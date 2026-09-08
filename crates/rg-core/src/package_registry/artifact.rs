//! The bytes of one publishable artifact, wherever they happen to live.
//!
//! A publish route used to hand the rest of the registry a `Vec<u8>`, which
//! made the configured artifact ceiling — half a gigabyte by default — the
//! amount of heap one request could claim, on every protocol at once. The
//! ingress already spooled the body to a temporary file; materialising it again
//! for the adapter and for storage threw that away.
//!
//! [`PackageArtifact`] is the shape that keeps the spool: the adapters read it
//! through [`PackageArtifact::reader`], storage publishes it with a streaming
//! copy, and nothing on the path has to hold the whole artifact. The in-memory
//! variant is still there because some artifacts genuinely are small and
//! already decoded — an npm attachment out of a packument, a PyPI provenance
//! document, a fixture in a test — and spooling those would buy nothing.

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

/// How much of an artifact a magic-number or preview check may look at.
///
/// Every adapter that sniffs a format reads a handful of leading bytes (`PK\x03\x04`,
/// the gzip `1f 8b`, the first line of a POM). Capping the sniff keeps that
/// check the same cheap operation on a spooled artifact that it was on a slice.
const MAX_HEAD_BYTES: usize = 8 * 1024;

/// One artifact's bytes: either already in memory, or spooled on disk.
///
/// The spooled variant owns its [`tempfile::TempPath`], so the temporary file
/// is retired when the artifact is dropped — including on every early return of
/// a publish that refuses the upload.
#[derive(Debug)]
pub enum PackageArtifact {
    /// Bytes the server already holds: a decoded attachment, a generated
    /// sidecar document, or a test fixture.
    Bytes(Vec<u8>),
    /// Bytes spooled to a request-private temporary file by the ingress.
    Spooled { path: tempfile::TempPath, len: u64 },
}

impl PackageArtifact {
    /// Wrap bytes the caller already holds.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Bytes(bytes.into())
    }

    /// Adopt a spooled upload, taking ownership of its temporary file.
    ///
    /// `len` is the byte count the ingress counted as it wrote, and it is what
    /// [`len`](Self::len) answers — so a size the caller already enforced a
    /// limit against cannot be re-read as something else here.
    pub fn spooled(path: tempfile::TempPath, len: u64) -> Self {
        Self::Spooled { path, len }
    }

    /// The artifact's size in bytes.
    pub fn len(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::Spooled { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The spool file backing this artifact, if it has one.
    ///
    /// Storage uses it to publish the artifact with a file-to-file copy instead
    /// of reading it into the process first.
    pub fn spool_path(&self) -> Option<&Path> {
        match self {
            Self::Bytes(_) => None,
            Self::Spooled { path, .. } => Some(path),
        }
    }

    /// The first `n` bytes, or the whole artifact when it is shorter.
    ///
    /// Capped at [`MAX_HEAD_BYTES`]: this is the format-sniffing path, and a
    /// caller that wants more than a header wants [`reader`](Self::reader).
    pub fn head(&self, n: usize) -> std::io::Result<Vec<u8>> {
        let n = n.min(MAX_HEAD_BYTES);
        match self {
            Self::Bytes(bytes) => Ok(bytes[..n.min(bytes.len())].to_vec()),
            Self::Spooled { path, .. } => {
                let mut file = std::fs::File::open(path)?;
                let mut head = vec![0_u8; n];
                let mut filled = 0;
                while filled < n {
                    match file.read(&mut head[filled..])? {
                        0 => break,
                        read => filled += read,
                    }
                }
                head.truncate(filled);
                Ok(head)
            }
        }
    }

    /// Whether the artifact opens with `magic`.
    ///
    /// A short artifact answers `false` rather than erroring: "this is not a
    /// zip" is the same verdict whether the file is truncated or is something
    /// else entirely, and every caller reports it that way.
    pub fn starts_with(&self, magic: &[u8]) -> std::io::Result<bool> {
        Ok(self.head(magic.len())? == magic)
    }

    /// A fresh reader positioned at the start of the artifact.
    ///
    /// It is `Seek` as well as `Read` because the zip-based formats need to
    /// find a central directory at the end of the file; the tar-based ones only
    /// ever read forward.
    pub fn reader(&self) -> std::io::Result<ArtifactReader<'_>> {
        match self {
            Self::Bytes(bytes) => Ok(ArtifactReader::Bytes(Cursor::new(bytes.as_slice()))),
            Self::Spooled { path, .. } => Ok(ArtifactReader::File(
                std::fs::File::open(path)
                    .map(|file| std::io::BufReader::with_capacity(64 * 1024, file))?,
            )),
        }
    }

    /// The whole artifact in memory.
    ///
    /// Deliberately explicit, and deliberately rare: every call is a decision
    /// to hold the artifact ceiling in heap. Only formats whose *entire*
    /// payload is a manifest — a Maven POM, a checksum sidecar — may use it.
    pub fn to_bytes(&self) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Bytes(bytes) => Ok(bytes.clone()),
            Self::Spooled { path, .. } => std::fs::read(path),
        }
    }
}

/// The largest archive member an adapter will read as a manifest.
///
/// Streaming the artifact off disk is only half a memory bound: every adapter
/// then pulls one member out of the archive — `package.json`, `Cargo.toml`, a
/// `.nuspec`, `METADATA`, `metadata.gz` — and a `read_to_string` on a tar or
/// zip entry is bounded by nothing but the entry's declared size. A crafted
/// artifact whose "manifest" is the whole upload would put the artifact ceiling
/// back in heap through that door. Real manifests are kilobytes; this is three
/// orders of magnitude above them.
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

/// Read one archive member that is supposed to be a manifest, refusing an
/// oversized one instead of allocating it.
///
/// `what` names the member in the refusal, because by this point the caller is
/// deep inside an archive and the operator has no other way to tell which file
/// was the problem.
pub fn read_manifest(reader: &mut impl Read, what: &str) -> anyhow::Result<Vec<u8>> {
    let mut manifest = Vec::new();
    // One byte past the limit: enough to tell "exactly at the limit" from
    // "over it" without reading the rest of an oversized member.
    reader
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut manifest)?;
    if manifest.len() as u64 > MAX_MANIFEST_BYTES {
        anyhow::bail!("{what} is larger than the {MAX_MANIFEST_BYTES}-byte manifest limit");
    }
    Ok(manifest)
}

/// [`read_manifest`], for the manifests that are text.
pub fn read_manifest_to_string(reader: &mut impl Read, what: &str) -> anyhow::Result<String> {
    String::from_utf8(read_manifest(reader, what)?)
        .map_err(|error| anyhow::anyhow!("{what} is not valid UTF-8: {error}"))
}

impl From<Vec<u8>> for PackageArtifact {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Bytes(bytes)
    }
}

impl From<&[u8]> for PackageArtifact {
    fn from(bytes: &[u8]) -> Self {
        Self::Bytes(bytes.to_vec())
    }
}

/// A `Read + Seek` view over a [`PackageArtifact`].
pub enum ArtifactReader<'a> {
    Bytes(Cursor<&'a [u8]>),
    File(std::io::BufReader<std::fs::File>),
}

impl Read for ArtifactReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Bytes(cursor) => cursor.read(buf),
            Self::File(file) => file.read(buf),
        }
    }
}

impl Seek for ArtifactReader<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        match self {
            Self::Bytes(cursor) => cursor.seek(position),
            Self::File(file) => file.seek(position),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn spooled(bytes: &[u8]) -> PackageArtifact {
        let mut file = tempfile::NamedTempFile::new().expect("create spool");
        file.write_all(bytes).expect("write spool");
        file.flush().expect("flush spool");
        PackageArtifact::spooled(file.into_temp_path(), bytes.len() as u64)
    }

    #[test]
    fn both_variants_answer_the_same_questions() {
        let payload = b"PK\x03\x04rest of the archive".to_vec();
        for artifact in [
            PackageArtifact::from_bytes(payload.clone()),
            spooled(&payload),
        ] {
            assert_eq!(artifact.len(), payload.len() as u64);
            assert!(!artifact.is_empty());
            assert!(artifact.starts_with(b"PK\x03\x04").unwrap());
            assert!(!artifact.starts_with(b"\x1f\x8b").unwrap());
            assert_eq!(artifact.head(4).unwrap(), b"PK\x03\x04");
            assert_eq!(artifact.to_bytes().unwrap(), payload);

            let mut read_back = Vec::new();
            artifact
                .reader()
                .unwrap()
                .read_to_end(&mut read_back)
                .unwrap();
            assert_eq!(read_back, payload);
        }
    }

    /// A magic check against an artifact shorter than the magic is a verdict,
    /// not an error — the short-read loop must not report a truncated file as
    /// a match either.
    #[test]
    fn a_short_artifact_does_not_match_a_longer_magic() {
        for artifact in [PackageArtifact::from_bytes(b"PK".to_vec()), spooled(b"PK")] {
            assert!(!artifact.starts_with(b"PK\x03\x04").unwrap());
            assert_eq!(artifact.head(64).unwrap(), b"PK");
        }
    }

    #[test]
    fn an_empty_artifact_reads_as_empty() {
        for artifact in [PackageArtifact::from_bytes(Vec::new()), spooled(b"")] {
            assert!(artifact.is_empty());
            assert_eq!(artifact.len(), 0);
            assert!(artifact.head(4).unwrap().is_empty());
            assert!(!artifact.starts_with(b"PK\x03\x04").unwrap());
        }
    }

    /// The reader has to be usable twice: an adapter validates the artifact and
    /// then reads its manifest out of it, and a reader left at EOF by the first
    /// pass would make the second one see an empty archive.
    #[test]
    fn every_reader_starts_at_the_beginning() {
        let artifact = spooled(b"abcdefgh");
        for _ in 0..2 {
            let mut first = [0_u8; 3];
            artifact.reader().unwrap().read_exact(&mut first).unwrap();
            assert_eq!(&first, b"abc");
        }
    }

    /// Zip-based formats seek to the central directory at the end of the file.
    #[test]
    fn a_spooled_artifact_seeks() {
        let artifact = spooled(b"0123456789");
        let mut reader = artifact.reader().unwrap();
        reader.seek(SeekFrom::End(-3)).unwrap();
        let mut tail = String::new();
        reader.read_to_string(&mut tail).unwrap();
        assert_eq!(tail, "789");
    }

    /// The spool belongs to the artifact: dropping it must retire the file, or
    /// a refused publish leaves the upload on disk for good.
    #[test]
    fn dropping_a_spooled_artifact_retires_its_file() {
        let artifact = spooled(b"transient");
        let path = artifact.spool_path().expect("spooled").to_path_buf();
        assert!(path.exists());
        drop(artifact);
        assert!(!path.exists(), "the spool outlived the artifact");
    }

    /// A manifest read is bounded too, or an archive whose "manifest" is the
    /// whole upload puts the artifact ceiling back in heap.
    #[test]
    fn an_oversized_manifest_is_refused_rather_than_allocated() {
        let oversized = vec![b'x'; MAX_MANIFEST_BYTES as usize + 1];
        let error = read_manifest(&mut oversized.as_slice(), "Cargo.toml")
            .expect_err("an oversized manifest must be refused");
        assert!(
            error.to_string().contains("Cargo.toml"),
            "the refusal must name the member: {error}"
        );

        let at_limit = vec![b'x'; MAX_MANIFEST_BYTES as usize];
        assert_eq!(
            read_manifest(&mut at_limit.as_slice(), "Cargo.toml")
                .unwrap()
                .len(),
            MAX_MANIFEST_BYTES as usize,
            "a manifest exactly at the limit is still a manifest"
        );
    }

    /// `head` is a sniff, not a way to smuggle the whole artifact into heap.
    #[test]
    fn head_is_capped() {
        let artifact = spooled(&vec![7_u8; MAX_HEAD_BYTES * 2]);
        assert_eq!(artifact.head(usize::MAX).unwrap().len(), MAX_HEAD_BYTES);
    }
}
