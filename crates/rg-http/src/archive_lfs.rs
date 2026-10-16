//! Repository archives that carry Git LFS files as the files they stand for.
//!
//! `git archive` writes every blob as committed, and an LFS file is committed
//! as a three-line pointer — so "Download ZIP" of a repository whose images or
//! models live in LFS handed over text stubs (card_aeeb6fc892f2). A smudge
//! filter would not help: git reads a filter's whole output into memory before
//! it writes the entry, so the server's footprint would grow with the largest
//! object in the tree.
//!
//! Instead the server asks git for a tar stream and rewrites it on the way out:
//! a regular file whose content parses as a pointer, and whose object this
//! repository stores, is written with the object's bytes streamed from LFS
//! storage; everything else passes through. The result is re-encoded as the
//! format the caller asked for — gzip-compressed tar, or a zip written here,
//! because a zip has to be produced without seeking back over what has already
//! been sent. Memory is bounded by a pointer (at most
//! [`rg_core::lfs::pointer::POINTER_SIZE_LIMIT`] bytes is ever held per entry)
//! plus the encoders' windows, whatever the objects weigh.
//!
//! A pointer whose object the server does not have stays a pointer, and the
//! archive ends with [`MISSING_NOTE`] naming every such path — the reader of the
//! archive learns it from the archive, not from a header nobody sees.

use std::io::{self, Read, Write};

/// The name of the note appended when some LFS objects could not be included.
pub(crate) const MISSING_NOTE: &str = "LFS-OBJECTS-MISSING.txt";

/// What the caller asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArchiveFormat {
    TarGz,
    Zip,
}

/// What a pointer's object turned out to be.
pub(crate) enum LfsContent {
    /// The object's bytes, `size` of them — the size the upload was verified
    /// against, not the one the pointer claims.
    Object {
        size: u64,
        reader: Box<dyn Read + Send>,
    },
    /// This repository does not store the object.
    Missing,
}

/// Where pointer objects come from.
pub(crate) trait LfsObjects {
    fn resolve(&mut self, pointer: &rg_core::lfs::pointer::LfsPointer) -> io::Result<LfsContent>;
}

/// Read the tar stream `git archive --format=tar` writes, and write the same
/// archive with LFS objects in place of their pointers, as `format`.
pub(crate) fn rewrite_archive<R, W, O>(
    input: R,
    output: W,
    format: ArchiveFormat,
    objects: &mut O,
) -> io::Result<W>
where
    R: Read,
    W: Write,
    O: LfsObjects,
{
    let mut sink = match format {
        ArchiveFormat::TarGz => Sink::Tar(tar::Builder::new(flate2::write::GzEncoder::new(
            output,
            flate2::Compression::default(),
        ))),
        ArchiveFormat::Zip => Sink::Zip(StreamingZip::new(output)),
    };
    let mut missing: Vec<(String, String)> = Vec::new();
    let mut archive = tar::Archive::new(input);
    let mut last_mtime = 0;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let header = entry.header().clone();
        let kind = header.entry_type();

        if kind.is_pax_global_extensions() {
            // `git archive` records the commit here (`comment=<oid>`), which is
            // what `git get-tar-commit-id` reads back. Small by construction,
            // and a header with no file fields of its own: its mode and mtime
            // are not read.
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            sink.global_header(&header, &data)?;
            continue;
        }
        let mtime = header.mtime()?;
        last_mtime = mtime;

        let path = entry.path()?.to_string_lossy().into_owned();
        match kind {
            tar::EntryType::Directory => sink.directory(&path, header.mode()?, mtime)?,
            tar::EntryType::Symlink => {
                let target = entry
                    .link_name()?
                    .ok_or_else(|| io::Error::other(format!("symlink {path} has no target")))?
                    .to_string_lossy()
                    .into_owned();
                sink.symlink(&path, &target, mtime)?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mode = header.mode()?;
                let size = entry.size();
                if size >= rg_core::lfs::pointer::POINTER_SIZE_LIMIT {
                    sink.file(&path, mode, mtime, size, &mut entry)?;
                    continue;
                }
                let mut small = Vec::with_capacity(size as usize);
                entry.read_to_end(&mut small)?;
                let Some(pointer) = rg_core::lfs::pointer::parse(&small) else {
                    sink.file(&path, mode, mtime, size, &mut small.as_slice())?;
                    continue;
                };
                match objects.resolve(&pointer)? {
                    LfsContent::Object { size, reader } => {
                        let mut exact = ExactLength::new(reader, size, &path);
                        sink.file(&path, mode, mtime, size, &mut exact)?;
                        exact.finish()?;
                    }
                    LfsContent::Missing => {
                        missing.push((path.clone(), pointer.oid));
                        sink.file(&path, mode, mtime, size, &mut small.as_slice())?;
                    }
                }
            }
            other => {
                // `git archive` writes nothing else; anything here is a format
                // this rewrite does not know how to carry faithfully.
                return Err(io::Error::other(format!(
                    "unexpected tar entry type {other:?} at {path}"
                )));
            }
        }
    }

    if !missing.is_empty() {
        let note = missing_note(&missing);
        sink.file(
            MISSING_NOTE,
            0o644,
            last_mtime,
            note.len() as u64,
            &mut note.as_bytes(),
        )?;
    }
    sink.finish()
}

fn missing_note(missing: &[(String, String)]) -> String {
    let mut note = String::from(
        "These files are stored in Git LFS, and this server does not have their objects.\n\
         They are included as LFS pointer files, not as their content.\n\n",
    );
    for (path, oid) in missing {
        note.push_str(&format!("{path}\tsha256:{oid}\n"));
    }
    note
}

/// The output half: one of the two encodings.
enum Sink<W: Write> {
    Tar(tar::Builder<flate2::write::GzEncoder<W>>),
    Zip(StreamingZip<W>),
}

impl<W: Write> Sink<W> {
    fn global_header(&mut self, header: &tar::Header, data: &[u8]) -> io::Result<()> {
        match self {
            Self::Tar(builder) => builder.append(header, data),
            Self::Zip(zip) => {
                if let Some(commit) = std::str::from_utf8(data)
                    .ok()
                    .and_then(pax_comment)
                    .map(str::to_string)
                {
                    zip.comment = commit.into_bytes();
                }
                Ok(())
            }
        }
    }

    fn directory(&mut self, path: &str, mode: u32, mtime: u64) -> io::Result<()> {
        match self {
            Self::Tar(builder) => {
                let mut header = base_header(tar::EntryType::Directory, mode, mtime);
                header.set_size(0);
                builder.append_data(&mut header, path, io::empty())
            }
            Self::Zip(zip) => zip.add(path, ZipKind::Directory(mode), mtime, 0, &mut io::empty()),
        }
    }

    fn symlink(&mut self, path: &str, target: &str, mtime: u64) -> io::Result<()> {
        match self {
            Self::Tar(builder) => {
                let mut header = base_header(tar::EntryType::Symlink, 0o777, mtime);
                header.set_size(0);
                builder.append_link(&mut header, path, target)
            }
            Self::Zip(zip) => zip.add(
                path,
                ZipKind::Symlink,
                mtime,
                target.len() as u64,
                &mut target.as_bytes(),
            ),
        }
    }

    fn file(
        &mut self,
        path: &str,
        mode: u32,
        mtime: u64,
        size: u64,
        data: &mut dyn Read,
    ) -> io::Result<()> {
        match self {
            Self::Tar(builder) => {
                let mut header = base_header(tar::EntryType::Regular, mode, mtime);
                header.set_size(size);
                builder.append_data(&mut header, path, data)
            }
            Self::Zip(zip) => zip.add(path, ZipKind::File(mode), mtime, size, data),
        }
    }

    fn finish(self) -> io::Result<W> {
        match self {
            Self::Tar(builder) => builder.into_inner()?.finish(),
            Self::Zip(zip) => zip.finish(),
        }
    }
}

/// The header fields `git archive` writes for every entry: owned by root,
/// stamped with the commit time.
fn base_header(kind: tar::EntryType, mode: u32, mtime: u64) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(mode);
    header.set_mtime(mtime);
    header.set_uid(0);
    header.set_gid(0);
    // Infallible for these short ASCII names.
    let _ = header.set_username("root");
    let _ = header.set_groupname("root");
    header
}

/// The `comment` record of a pax global header, where `git archive` puts the
/// commit id.
fn pax_comment(records: &str) -> Option<&str> {
    // Each record is `<len> <key>=<value>\n`.
    records.lines().find_map(|record| {
        record
            .split_once(' ')
            .and_then(|(_, pair)| pair.strip_prefix("comment="))
    })
}

/// A reader that has to produce exactly `expected` bytes.
///
/// The tar header and the zip sizes are written from the object row's size
/// before the bytes are read; a stored object that turned out shorter or
/// longer would leave a structurally broken archive that looks complete. Either
/// way is an error, which breaks the download instead.
struct ExactLength<'a> {
    inner: Box<dyn Read + Send>,
    remaining: u64,
    path: &'a str,
}

impl<'a> ExactLength<'a> {
    fn new(inner: Box<dyn Read + Send>, expected: u64, path: &'a str) -> Self {
        Self {
            inner,
            remaining: expected,
            path,
        }
    }

    /// After the entry is written: the object must have had nothing left.
    fn finish(mut self) -> io::Result<()> {
        let mut probe = [0u8; 1];
        if self.inner.read(&mut probe)? != 0 {
            return Err(io::Error::other(format!(
                "the LFS object for {} is longer than its recorded size",
                self.path
            )));
        }
        Ok(())
    }
}

impl Read for ExactLength<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let limit = buf
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buf[..limit])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "the LFS object for {} ended {} bytes short of its recorded size",
                    self.path, self.remaining
                ),
            ));
        }
        self.remaining -= read as u64;
        Ok(read)
    }
}

// ── The async edge ─────────────────────────────────────────────────────────

/// A repository's LFS store as the rewrite reads it: the row says whether the
/// object was uploaded and how large it is, the store hands over its bytes.
pub(crate) struct StoredLfsObjects {
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) db: rg_db::DatabaseConnection,
    pub(crate) storage: std::sync::Arc<dyn rg_core::blob_storage::BlobStorage>,
    pub(crate) legacy_root: std::path::PathBuf,
    pub(crate) repo_id: i64,
    pub(crate) owner: String,
    pub(crate) name: String,
}

impl LfsObjects for StoredLfsObjects {
    fn resolve(&mut self, pointer: &rg_core::lfs::pointer::LfsPointer) -> io::Result<LfsContent> {
        let row = self
            .runtime
            .block_on(rg_db::ops::lfs_object_ops::find_by_repo_and_oid(
                &self.db,
                self.repo_id,
                &pointer.oid,
            ))
            .map_err(|error| io::Error::other(format!("{error:#}")))?;
        let Some(row) = row.filter(|row| row.uploaded) else {
            return Ok(LfsContent::Missing);
        };
        let size = u64::try_from(row.size)
            .map_err(|_| io::Error::other(format!("LFS object {} has a negative size", row.oid)))?;
        let source = self
            .runtime
            .block_on(rg_core::lfs::service::read_object_source(
                self.storage.as_ref(),
                &self.legacy_root,
                &self.owner,
                &self.name,
                &pointer.oid,
            ));
        let source = match source {
            Ok(source) => source,
            // The row says uploaded and the store says gone: the same answer a
            // download of the file would get, so the archive keeps the pointer
            // and names it rather than failing as a whole.
            Err(error) if proves_absent(&error) => return Ok(LfsContent::Missing),
            Err(error) => return Err(io::Error::other(format!("{error:#}"))),
        };
        let reader: Box<dyn Read + Send> = match source {
            rg_core::lfs::service::LfsObjectSource::Local { path, compressed } => {
                let file = std::fs::File::open(&path)?;
                if compressed {
                    Box::new(zstd::stream::Decoder::new(file)?)
                } else {
                    Box::new(file)
                }
            }
            rg_core::lfs::service::LfsObjectSource::Bytes { data, compressed } => {
                let data = io::Cursor::new(data);
                if compressed {
                    Box::new(zstd::stream::Decoder::new(data)?)
                } else {
                    Box::new(data)
                }
            }
        };
        Ok(LfsContent::Object { size, reader })
    }
}

/// Whether a failed object read proved the object is gone, as
/// `api::lfs::lfs_object_read_error` classifies it; anything else is the
/// storage failing, which must not pass for a missing object.
fn proves_absent(error: &anyhow::Error) -> bool {
    error.downcast_ref::<rg_core::error::NotFound>().is_some()
        || matches!(
            error.downcast_ref::<rg_core::blob_storage::BlobStorageError>(),
            Some(rg_core::blob_storage::BlobStorageError::NotFound(_))
        )
}

/// Rewrite `stream`'s tar into `format` on a blocking worker, and hand back a
/// stream whose stdout is the rewritten archive.
///
/// The child stays in the returned stream, so `git_child_body_with_idle` still
/// reaps git and still breaks the body when git exits non-zero; a failure of
/// the rewrite itself — an object that cannot be read, or one shorter than its
/// row — arrives as a read error, which breaks the body the same way. The idle
/// bounds hold too: a git that hangs starves the rewrite, which then has
/// nothing to read out, and a client that stops reading fills the channel the
/// rewrite writes into.
pub(crate) fn rewrite_git_tar(
    mut stream: crate::http_stream::GitChildStream,
    format: ArchiveFormat,
    mut objects: StoredLfsObjects,
) -> crate::http_stream::GitChildStream {
    let (tx, rx) = tokio::sync::mpsc::channel::<io::Result<axum::body::Bytes>>(4);
    let rewritten =
        tokio_util::io::StreamReader::new(tokio_stream::wrappers::ReceiverStream::new(rx));
    let git_tar = std::mem::replace(&mut stream.stdout, Box::new(rewritten));
    let head = std::mem::take(&mut stream.head);
    let input = io::Cursor::new(head).chain(BlockingRead {
        runtime: objects.runtime.clone(),
        inner: git_tar,
    });
    tokio::task::spawn_blocking(move || {
        let out = ChannelWriter {
            tx: tx.clone(),
            buf: Vec::with_capacity(crate::http_stream::RESPONSE_CHUNK_BYTES),
        };
        let written =
            rewrite_archive(input, out, format, &mut objects).and_then(|mut out| out.flush());
        if let Err(error) = written {
            tracing::warn!(
                repo = %format!("{}/{}", objects.owner, objects.name),
                error = %error,
                "repository archive could not be rewritten with its LFS objects — breaking the body"
            );
            drop(tx.blocking_send(Err(error)));
        }
    });
    stream
}

/// git's async stdout, read from the blocking worker.
struct BlockingRead {
    runtime: tokio::runtime::Handle,
    inner: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
}

impl Read for BlockingRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use tokio::io::AsyncReadExt as _;
        self.runtime.block_on(self.inner.read(buf))
    }
}

/// The rewritten archive, out to the body in response-sized chunks.
struct ChannelWriter {
    tx: tokio::sync::mpsc::Sender<io::Result<axum::body::Bytes>>,
    buf: Vec<u8>,
}

impl ChannelWriter {
    fn send(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = axum::body::Bytes::from(std::mem::replace(
            &mut self.buf,
            Vec::with_capacity(crate::http_stream::RESPONSE_CHUNK_BYTES),
        ));
        self.tx
            .blocking_send(Ok(chunk))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the client stopped reading"))
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let room = crate::http_stream::RESPONSE_CHUNK_BYTES - self.buf.len();
        let taken = data.len().min(room);
        self.buf.extend_from_slice(&data[..taken]);
        if self.buf.len() >= crate::http_stream::RESPONSE_CHUNK_BYTES {
            self.send()?;
        }
        Ok(taken)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send()
    }
}

// ── Zip, written forward only ──────────────────────────────────────────────

const ZIP_U32_MAX: u64 = 0xFFFF_FFFF;
/// Entries at least this large are written as zip64 from the start: the sizes
/// are only known once the data is behind us, and deflate can grow
/// incompressible input by a little, so the margin keeps a 32-bit entry from
/// discovering too late that it needed 64 bits.
const ZIP64_ENTRY_THRESHOLD: u64 = 0xFF00_0000;

enum ZipKind {
    File(u32),
    Directory(u32),
    Symlink,
}

struct ZipRecord {
    name: Vec<u8>,
    crc: u32,
    compressed: u64,
    uncompressed: u64,
    offset: u64,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    mtime: u32,
    external: u32,
    zip64: bool,
}

/// A zip written front to back: each entry carries a data descriptor after
/// its data, so nothing already sent has to be rewritten.
struct StreamingZip<W: Write> {
    out: Counting<W>,
    records: Vec<ZipRecord>,
    comment: Vec<u8>,
}

impl<W: Write> StreamingZip<W> {
    fn new(out: W) -> Self {
        Self {
            out: Counting {
                inner: out,
                written: 0,
            },
            records: Vec::new(),
            comment: Vec::new(),
        }
    }

    fn add(
        &mut self,
        path: &str,
        kind: ZipKind,
        mtime: u64,
        size: u64,
        data: &mut dyn Read,
    ) -> io::Result<()> {
        let (name, external, deflate) = match kind {
            ZipKind::File(mode) => (path.to_string(), (0o100000 | (mode & 0o7777)) << 16, true),
            // MS-DOS directory bit alongside the unix mode, as zip tools expect.
            ZipKind::Directory(mode) => (
                format!("{}/", path.trim_end_matches('/')),
                ((0o040000 | (mode & 0o7777)) << 16) | 0x10,
                false,
            ),
            ZipKind::Symlink => (path.to_string(), (0o120000 | 0o777) << 16, false),
        };
        let zip64 = size >= ZIP64_ENTRY_THRESHOLD;
        let method: u16 = if deflate { 8 } else { 0 };
        let (dos_time, dos_date) = dos_datetime(mtime);
        let mtime32 = u32::try_from(mtime).unwrap_or(u32::MAX);
        let offset = self.out.written;
        let version: u16 = if zip64 { 45 } else { 20 };
        // Bit 3: sizes and CRC follow the data. Bit 11: the name is UTF-8.
        let flags: u16 = (1 << 3) | (1 << 11);

        let mut extra = Vec::new();
        if zip64 {
            extra.extend_from_slice(&0x0001u16.to_le_bytes());
            extra.extend_from_slice(&16u16.to_le_bytes());
            extra.extend_from_slice(&0u64.to_le_bytes());
            extra.extend_from_slice(&0u64.to_le_bytes());
        }
        push_timestamp_extra(&mut extra, mtime32);

        let mut local = Vec::with_capacity(30 + name.len() + extra.len());
        local.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        local.extend_from_slice(&version.to_le_bytes());
        local.extend_from_slice(&flags.to_le_bytes());
        local.extend_from_slice(&method.to_le_bytes());
        local.extend_from_slice(&dos_time.to_le_bytes());
        local.extend_from_slice(&dos_date.to_le_bytes());
        local.extend_from_slice(&0u32.to_le_bytes()); // crc, in the descriptor
        let placeholder: u32 = if zip64 { u32::MAX } else { 0 };
        local.extend_from_slice(&placeholder.to_le_bytes());
        local.extend_from_slice(&placeholder.to_le_bytes());
        local.extend_from_slice(&(name.len() as u16).to_le_bytes());
        local.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        local.extend_from_slice(name.as_bytes());
        local.extend_from_slice(&extra);
        self.out.write_all(&local)?;

        let data_start = self.out.written;
        let mut crc = flate2::Crc::new();
        let mut uncompressed = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        if deflate {
            let mut encoder =
                flate2::write::DeflateEncoder::new(&mut self.out, flate2::Compression::default());
            loop {
                let read = data.read(&mut buf)?;
                if read == 0 {
                    break;
                }
                crc.update(&buf[..read]);
                uncompressed += read as u64;
                encoder.write_all(&buf[..read])?;
            }
            encoder.finish()?;
        } else {
            loop {
                let read = data.read(&mut buf)?;
                if read == 0 {
                    break;
                }
                crc.update(&buf[..read]);
                uncompressed += read as u64;
                self.out.write_all(&buf[..read])?;
            }
        }
        let compressed = self.out.written - data_start;
        if uncompressed != size {
            return Err(io::Error::other(format!(
                "{path}: {uncompressed} bytes written where {size} were announced"
            )));
        }
        if !zip64 && (compressed > ZIP_U32_MAX || uncompressed > ZIP_U32_MAX) {
            return Err(io::Error::other(format!(
                "{path}: entry outgrew a 32-bit zip record"
            )));
        }

        let mut descriptor = Vec::with_capacity(24);
        descriptor.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
        descriptor.extend_from_slice(&crc.sum().to_le_bytes());
        if zip64 {
            descriptor.extend_from_slice(&compressed.to_le_bytes());
            descriptor.extend_from_slice(&uncompressed.to_le_bytes());
        } else {
            descriptor.extend_from_slice(&(compressed as u32).to_le_bytes());
            descriptor.extend_from_slice(&(uncompressed as u32).to_le_bytes());
        }
        self.out.write_all(&descriptor)?;

        self.records.push(ZipRecord {
            name: name.into_bytes(),
            crc: crc.sum(),
            compressed,
            uncompressed,
            offset,
            method,
            dos_time,
            dos_date,
            mtime: mtime32,
            external,
            zip64,
        });
        Ok(())
    }

    fn finish(mut self) -> io::Result<W> {
        let directory_start = self.out.written;
        for record in &self.records {
            let needs64 = record.zip64
                || record.offset >= ZIP_U32_MAX
                || record.compressed >= ZIP_U32_MAX
                || record.uncompressed >= ZIP_U32_MAX;
            let mut extra = Vec::new();
            if needs64 {
                extra.extend_from_slice(&0x0001u16.to_le_bytes());
                extra.extend_from_slice(&24u16.to_le_bytes());
                extra.extend_from_slice(&record.uncompressed.to_le_bytes());
                extra.extend_from_slice(&record.compressed.to_le_bytes());
                extra.extend_from_slice(&record.offset.to_le_bytes());
            }
            push_timestamp_extra(&mut extra, record.mtime);
            let version: u16 = if needs64 { 45 } else { 20 };
            let clamp = |value: u64| if needs64 { u32::MAX } else { value as u32 };

            let mut central = Vec::with_capacity(46 + record.name.len() + extra.len());
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&((3u16 << 8) | version).to_le_bytes()); // made by: unix
            central.extend_from_slice(&version.to_le_bytes());
            central.extend_from_slice(&((1u16 << 3) | (1 << 11)).to_le_bytes());
            central.extend_from_slice(&record.method.to_le_bytes());
            central.extend_from_slice(&record.dos_time.to_le_bytes());
            central.extend_from_slice(&record.dos_date.to_le_bytes());
            central.extend_from_slice(&record.crc.to_le_bytes());
            central.extend_from_slice(&clamp(record.compressed).to_le_bytes());
            central.extend_from_slice(&clamp(record.uncompressed).to_le_bytes());
            central.extend_from_slice(&(record.name.len() as u16).to_le_bytes());
            central.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // comment
            central.extend_from_slice(&0u16.to_le_bytes()); // disk
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            central.extend_from_slice(&record.external.to_le_bytes());
            central.extend_from_slice(&clamp(record.offset).to_le_bytes());
            central.extend_from_slice(&record.name);
            central.extend_from_slice(&extra);
            self.out.write_all(&central)?;
        }
        let directory_size = self.out.written - directory_start;
        let entries = self.records.len() as u64;
        let needs64 =
            entries >= 0xFFFF || directory_start >= ZIP_U32_MAX || directory_size >= ZIP_U32_MAX;

        if needs64 {
            let record_offset = self.out.written;
            let mut end64 = Vec::with_capacity(56 + 20);
            end64.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
            end64.extend_from_slice(&44u64.to_le_bytes());
            end64.extend_from_slice(&((3u16 << 8) | 45).to_le_bytes());
            end64.extend_from_slice(&45u16.to_le_bytes());
            end64.extend_from_slice(&0u32.to_le_bytes());
            end64.extend_from_slice(&0u32.to_le_bytes());
            end64.extend_from_slice(&entries.to_le_bytes());
            end64.extend_from_slice(&entries.to_le_bytes());
            end64.extend_from_slice(&directory_size.to_le_bytes());
            end64.extend_from_slice(&directory_start.to_le_bytes());
            end64.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
            end64.extend_from_slice(&0u32.to_le_bytes());
            end64.extend_from_slice(&record_offset.to_le_bytes());
            end64.extend_from_slice(&1u32.to_le_bytes());
            self.out.write_all(&end64)?;
        }

        let short = |value: u64, max: u64| if needs64 { max } else { value };
        let comment = &self.comment[..self.comment.len().min(0xFFFF)];
        let mut end = Vec::with_capacity(22 + comment.len());
        end.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&(short(entries, 0xFFFF) as u16).to_le_bytes());
        end.extend_from_slice(&(short(entries, 0xFFFF) as u16).to_le_bytes());
        end.extend_from_slice(&(short(directory_size, ZIP_U32_MAX) as u32).to_le_bytes());
        end.extend_from_slice(&(short(directory_start, ZIP_U32_MAX) as u32).to_le_bytes());
        end.extend_from_slice(&(comment.len() as u16).to_le_bytes());
        end.extend_from_slice(comment);
        self.out.write_all(&end)?;
        self.out.flush()?;
        Ok(self.out.inner)
    }
}

/// The "UT" extended timestamp: the modification time in Unix seconds, which
/// unzip prefers over the DOS fields' two-second, timezone-less resolution.
fn push_timestamp_extra(extra: &mut Vec<u8>, mtime: u32) {
    extra.extend_from_slice(&0x5455u16.to_le_bytes());
    extra.extend_from_slice(&5u16.to_le_bytes());
    extra.push(1);
    extra.extend_from_slice(&mtime.to_le_bytes());
}

/// MS-DOS time and date fields for a Unix time, in UTC, clamped to the
/// format's 1980–2107 range.
fn dos_datetime(mtime: u64) -> (u16, u16) {
    let Some(at) = chrono::DateTime::from_timestamp(i64::try_from(mtime).unwrap_or(0), 0) else {
        return (0, (1 << 5) | 1);
    };
    use chrono::{Datelike as _, Timelike as _};
    let year = at.year().clamp(1980, 2107);
    if at.year() < 1980 {
        return (0, (1 << 5) | 1);
    }
    let time = ((at.hour() as u16) << 11) | ((at.minute() as u16) << 5) | (at.second() as u16 / 2);
    let date = (((year - 1980) as u16) << 9) | ((at.month() as u16) << 5) | at.day() as u16;
    (time, date)
}

/// A writer that knows how many bytes it has passed on — the zip offsets.
struct Counting<W: Write> {
    inner: W,
    written: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const OID: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    fn pointer(oid: &str, size: u64) -> String {
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
    }

    /// The tar `git archive --format=tar` would write for this tree.
    fn git_like_tar(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut global = tar::Header::new_ustar();
        let records = "52 comment=0123456789abcdef0123456789abcdef01234567\n";
        global.set_entry_type(tar::EntryType::XGlobalHeader);
        global.set_size(records.len() as u64);
        global.set_path("pax_global_header").unwrap();
        global.set_cksum();
        builder.append(&global, records.as_bytes()).unwrap();
        let mut dir = tar::Header::new_ustar();
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_mode(0o775);
        dir.set_mtime(1_700_000_000);
        dir.set_size(0);
        builder
            .append_data(&mut dir, "assets/", io::empty())
            .unwrap();
        for (path, data, mode) in files {
            let mut header = tar::Header::new_ustar();
            header.set_mode(*mode);
            header.set_mtime(1_700_000_000);
            header.set_size(data.len() as u64);
            builder.append_data(&mut header, path, *data).unwrap();
        }
        let mut link = tar::Header::new_ustar();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_mode(0o777);
        link.set_mtime(1_700_000_000);
        link.set_size(0);
        builder
            .append_link(&mut link, "latest", "assets/model.bin")
            .unwrap();
        builder.into_inner().unwrap()
    }

    struct Store(HashMap<String, Vec<u8>>);

    impl LfsObjects for Store {
        fn resolve(
            &mut self,
            pointer: &rg_core::lfs::pointer::LfsPointer,
        ) -> io::Result<LfsContent> {
            Ok(match self.0.get(&pointer.oid) {
                Some(bytes) => LfsContent::Object {
                    size: bytes.len() as u64,
                    reader: Box::new(io::Cursor::new(bytes.clone())),
                },
                None => LfsContent::Missing,
            })
        }
    }

    fn fixture() -> (Vec<u8>, Store, Vec<u8>) {
        let object: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let tar = git_like_tar(&[
            ("README.md", b"plain text\n", 0o664),
            ("run.sh", b"#!/bin/sh\n", 0o775),
            (
                "assets/model.bin",
                pointer(OID, object.len() as u64).as_bytes(),
                0o664,
            ),
        ]);
        let store = Store(HashMap::from([(OID.to_string(), object.clone())]));
        (tar, store, object)
    }

    fn untar_gz(bytes: &[u8]) -> HashMap<String, (tar::EntryType, Vec<u8>, u32)> {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        let mut seen = HashMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let kind = entry.header().entry_type();
            let mode = entry.header().mode().unwrap_or(0);
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut data = Vec::new();
            if kind == tar::EntryType::Symlink {
                data = entry
                    .link_name()
                    .unwrap()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
                    .into_bytes();
            } else {
                entry.read_to_end(&mut data).unwrap();
            }
            seen.insert(path, (kind, data, mode));
        }
        seen
    }

    #[test]
    fn a_tar_gz_carries_the_object_where_the_pointer_was() {
        let (tar, mut store, object) = fixture();
        let out =
            rewrite_archive(tar.as_slice(), Vec::new(), ArchiveFormat::TarGz, &mut store).unwrap();
        let entries = untar_gz(&out);
        assert_eq!(entries["assets/model.bin"].1, object);
        assert_eq!(entries["README.md"].1, b"plain text\n");
        assert_eq!(
            entries["run.sh"].2 & 0o111,
            0o111,
            "the executable bit survives"
        );
        assert_eq!(entries["latest"].0, tar::EntryType::Symlink);
        assert_eq!(entries["latest"].1, b"assets/model.bin");
        assert_eq!(
            entries
                .get("assets/")
                .or_else(|| entries.get("assets"))
                .expect("the directory entry")
                .0,
            tar::EntryType::Directory
        );
        assert!(
            entries
                .values()
                .any(|(kind, data, _)| *kind == tar::EntryType::XGlobalHeader
                    && data.ends_with(b"0123456789abcdef0123456789abcdef01234567\n")),
            "the commit id `git get-tar-commit-id` reads must survive"
        );
        assert!(!entries.contains_key(MISSING_NOTE));
    }

    #[test]
    fn a_zip_carries_the_object_and_reads_back_with_a_stock_reader() {
        let (tar, mut store, object) = fixture();
        let out =
            rewrite_archive(tar.as_slice(), Vec::new(), ArchiveFormat::Zip, &mut store).unwrap();
        let mut zip = zip::ZipArchive::new(io::Cursor::new(out)).unwrap();
        assert_eq!(
            zip.comment(),
            b"0123456789abcdef0123456789abcdef01234567",
            "the commit id travels as the zip comment, as git archive writes it"
        );
        let mut read = |name: &str| {
            let mut file = zip.by_name(name).unwrap();
            let mode = file.unix_mode().unwrap();
            let mut data = Vec::new();
            file.read_to_end(&mut data).unwrap();
            (data, mode)
        };
        assert_eq!(read("assets/model.bin").0, object);
        assert_eq!(read("README.md").0, b"plain text\n");
        assert_eq!(read("run.sh").1 & 0o111, 0o111);
        let (target, mode) = read("latest");
        assert_eq!(target, b"assets/model.bin");
        assert_eq!(mode & 0o170000, 0o120000, "a symlink stays a symlink");
        assert!(zip.by_name("assets/").unwrap().is_dir());
    }

    #[test]
    fn a_missing_object_stays_a_pointer_and_the_archive_says_so() {
        for format in [ArchiveFormat::TarGz, ArchiveFormat::Zip] {
            let (tar, _, object) = fixture();
            let mut empty = Store(HashMap::new());
            let out = rewrite_archive(tar.as_slice(), Vec::new(), format, &mut empty).unwrap();
            let (model, note) = match format {
                ArchiveFormat::TarGz => {
                    let entries = untar_gz(&out);
                    (
                        entries["assets/model.bin"].1.clone(),
                        entries[MISSING_NOTE].1.clone(),
                    )
                }
                ArchiveFormat::Zip => {
                    let mut zip = zip::ZipArchive::new(io::Cursor::new(out)).unwrap();
                    let mut take = |name: &str| {
                        let mut data = Vec::new();
                        zip.by_name(name).unwrap().read_to_end(&mut data).unwrap();
                        data
                    };
                    (take("assets/model.bin"), take(MISSING_NOTE))
                }
            };
            assert_eq!(model, pointer(OID, object.len() as u64).as_bytes());
            let note = String::from_utf8(note).unwrap();
            assert!(
                note.contains("assets/model.bin") && note.contains(OID),
                "{note}"
            );
        }
    }

    #[test]
    fn an_object_shorter_than_its_row_breaks_the_archive() {
        struct Short;
        impl LfsObjects for Short {
            fn resolve(
                &mut self,
                _pointer: &rg_core::lfs::pointer::LfsPointer,
            ) -> io::Result<LfsContent> {
                Ok(LfsContent::Object {
                    size: 1000,
                    reader: Box::new(io::Cursor::new(vec![0u8; 999])),
                })
            }
        }
        for format in [ArchiveFormat::TarGz, ArchiveFormat::Zip] {
            let (tar, _, _) = fixture();
            let error = rewrite_archive(tar.as_slice(), Vec::new(), format, &mut Short)
                .err()
                .expect("a short object must fail the archive");
            assert!(error.to_string().contains("short"), "{error}");
        }
    }

    #[test]
    fn a_zip_past_the_32_bit_entry_count_reads_back() {
        let mut zip = StreamingZip::new(Vec::new());
        // Directories: the subject is the zip64 end record, not deflate.
        for index in 0..70_000u32 {
            zip.add(
                &format!("d{index}"),
                ZipKind::Directory(0o755),
                1_700_000_000,
                0,
                &mut io::empty(),
            )
            .unwrap();
        }
        let out = zip.finish().unwrap();
        let read = zip::ZipArchive::new(io::Cursor::new(out)).unwrap();
        assert_eq!(read.len(), 70_000);
    }
}
