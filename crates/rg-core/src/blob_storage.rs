//! Backend-neutral storage for durable binary objects.
//!
//! Database rows store [`BlobKey`] values, never backend-specific absolute paths.
//! A backend owns atomic writes, lookup, deletion and inventory under its root.

use futures::future::BoxFuture;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

/// Maximum serialized object-key length accepted by all backends.
pub const MAX_BLOB_KEY_LEN: usize = 1024;

/// Stable, backend-neutral identifier for one stored object.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlobKey(String);

impl BlobKey {
    /// Validate a serialized key.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_key(&value)?;
        Ok(Self(value))
    }

    /// Build a key from logical path segments.
    ///
    /// Segments are percent-encoded so user-controlled names cannot introduce
    /// separators, traversal components or backend-specific path syntax.
    pub fn from_segments<'a>(segments: impl IntoIterator<Item = &'a str>) -> Result<Self> {
        let raw: Vec<&str> = segments.into_iter().collect();
        let last = raw.len().saturating_sub(1);
        let encoded: Vec<String> = raw
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                let limit = if index == last {
                    MAX_FILE_SEGMENT_LEN
                } else {
                    MAX_DIRECTORY_SEGMENT_LEN
                };
                encode_segment(segment, limit)
            })
            .collect();
        if encoded.is_empty() || encoded.iter().any(String::is_empty) {
            return Err(BlobStorageError::InvalidKey(
                "blob key requires non-empty segments".to_string(),
            ));
        }
        Self::new(encoded.join("/"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BlobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&str> for BlobKey {
    type Error = BlobStorageError;

    fn try_from(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

/// Metadata common to local and object-store backends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobMetadata {
    pub key: BlobKey,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, thiserror::Error)]
pub enum BlobStorageError {
    #[error("invalid blob key: {0}")]
    InvalidKey(String),
    #[error("blob not found: {0}")]
    NotFound(BlobKey),
    #[error("blob path escaped storage root: {0}")]
    OutsideRoot(PathBuf),
    #[error("blob storage backend {backend} does not support {operation}")]
    UnsupportedOperation {
        backend: &'static str,
        operation: &'static str,
    },
    /// A filesystem failure, carrying the path the bare `io::Error` dropped.
    ///
    /// There is deliberately no `#[from] std::io::Error`: automatic conversion
    /// is what let every `?` in this file report `Permission denied (os error
    /// 13)` about a path only the backend knows. Build the variant through
    /// [`BlobStorageError::io`] so each site has to name what it was touching.
    #[error("blob storage I/O error: {message}")]
    Io {
        /// The file or directory the operation failed on.
        path: PathBuf,
        /// Rendered diagnostic: the path, the errno and the remediation.
        message: String,
        #[source]
        source: std::io::Error,
    },
}

impl BlobStorageError {
    /// Attach the failing path — and, on a permission error, the uid/ownership
    /// diagnostic — to a raw [`std::io::Error`].
    ///
    /// `what` names the thing in operator terms (`"blob storage root"`,
    /// `"blob temporary file"`).
    pub fn io(what: &str, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        let path = path.into();
        let message = crate::platform::fs::describe_path_error(
            what,
            &path,
            &source,
            crate::platform::fs::BLOB_STORAGE_HINT,
        );
        Self::Io {
            path,
            message,
            source,
        }
    }
}

/// `map_err` adaptor for the sites that only have a raw io error to convert.
///
/// Every path in this backend is built from the storage root plus a [`BlobKey`],
/// so the failing directory appears in neither the request nor the database row
/// that names the object.
fn io_at<'a>(
    what: &'static str,
    path: &'a Path,
) -> impl Fn(std::io::Error) -> BlobStorageError + 'a {
    move |error| BlobStorageError::io(what, path, error)
}

pub type Result<T> = std::result::Result<T, BlobStorageError>;

/// Object-safe storage contract used by LFS, packages, OCI, CI artifacts and
/// future attachments/archive caches.
///
/// Implementations must make `put` and `put_file` atomic for readers: callers
/// either observe the previous complete object or the new complete object.
pub trait BlobStorage: Send + Sync {
    fn backend_name(&self) -> &'static str;

    fn put<'a>(&'a self, key: &'a BlobKey, data: &'a [u8]) -> BoxFuture<'a, Result<BlobMetadata>>;

    fn put_file<'a>(
        &'a self,
        key: &'a BlobKey,
        source: &'a Path,
    ) -> BoxFuture<'a, Result<BlobMetadata>>;

    /// Store the file at `source` under `key`, sharing its bytes rather than
    /// copying them when the backend can.
    ///
    /// Only for content that is never rewritten in place — content-addressed
    /// objects such as LFS, whose key already names their hash — because a
    /// shared copy is the same bytes under two names. Readers see the same
    /// atomicity as [`BlobStorage::put_file`], and a backend with no way to
    /// share simply copies.
    fn put_file_shared<'a>(
        &'a self,
        key: &'a BlobKey,
        source: &'a Path,
    ) -> BoxFuture<'a, Result<BlobMetadata>> {
        self.put_file(key, source)
    }

    fn get<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, Result<Vec<u8>>>;

    fn metadata<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, Result<BlobMetadata>>;

    fn exists<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, Result<bool>>;

    fn delete<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, Result<bool>>;

    fn list<'a>(&'a self, prefix: Option<&'a BlobKey>) -> BoxFuture<'a, Result<Vec<BlobMetadata>>>;

    /// Atomically move every object below `source` to `destination` without
    /// materialising object contents in the caller.
    ///
    /// Repository deletion uses this as its prepare/rollback primitive. A
    /// backend that cannot provide an atomic namespace move must reject the
    /// operation before changing either prefix; deleting objects one by one
    /// could otherwise leave an active repository only partly readable.
    fn move_prefix<'a>(
        &'a self,
        _source: &'a BlobKey,
        _destination: &'a BlobKey,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            Err(BlobStorageError::UnsupportedOperation {
                backend: self.backend_name(),
                operation: "atomic prefix move",
            })
        })
    }

    /// Delete every object below `prefix` after it has left the live namespace.
    fn delete_prefix<'a>(&'a self, prefix: &'a BlobKey) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let objects = self.list(Some(prefix)).await?;
            let found = !objects.is_empty();
            for object in objects {
                self.delete(&object.key).await?;
            }
            Ok(found)
        })
    }

    /// Local backends expose a path for zero-copy protocol handlers. Portable
    /// callers must use the other methods and handle `None` for S3-like stores.
    fn local_path(&self, _key: &BlobKey) -> Option<PathBuf> {
        None
    }
}

/// The blob store of the instance whose storage root is `repo_root`.
///
/// The server builds its request-path handle (`AppState::blob_storage`) from
/// this, and so does work that runs where no such handle reaches — a merge
/// started by the merge queue or by auto-merge after CI, which has to give the
/// base repository the LFS objects of a fork. One function says which backend
/// that is, so the two can never be pointed at different stores.
pub fn instance_blob_storage(repo_root: &Path) -> LocalBlobStorage {
    LocalBlobStorage::new(repo_root.to_path_buf())
}

/// Atomic filesystem implementation used by the current single-node runtime.
#[derive(Clone, Debug)]
pub struct LocalBlobStorage {
    root: PathBuf,
}

impl LocalBlobStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn lexical_path(&self, key: &BlobKey) -> PathBuf {
        key.as_str()
            .split('/')
            .fold(self.root.clone(), |path, segment| path.join(segment))
    }

    async fn prepare_parent(&self, path: &Path) -> Result<()> {
        // First filesystem touch of any write, and the one that fails on a
        // bind-mount owned by a host uid the container does not share.
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(io_at("blob storage root", &self.root))?;
        let parent = path
            .parent()
            .ok_or_else(|| BlobStorageError::InvalidKey("blob key has no parent".to_string()))?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(io_at("blob parent directory", parent))?;
        self.ensure_canonical_under_root(parent).await
    }

    async fn ensure_canonical_under_root(&self, path: &Path) -> Result<()> {
        let root = tokio::fs::canonicalize(&self.root)
            .await
            .map_err(io_at("blob storage root", &self.root))?;
        let candidate = tokio::fs::canonicalize(path)
            .await
            .map_err(io_at("blob path", path))?;
        if candidate.starts_with(&root) {
            Ok(())
        } else {
            Err(BlobStorageError::OutsideRoot(candidate))
        }
    }

    async fn atomic_copy(&self, key: &BlobKey, source: &Path) -> Result<BlobMetadata> {
        let destination = self.lexical_path(key);
        self.prepare_parent(&destination).await?;
        let temporary = temporary_sibling(&destination);

        let result = async {
            let staged = io_at("blob temporary file", &temporary);
            let mut src = tokio::fs::File::open(source)
                .await
                .map_err(io_at("blob source file", source))?;
            let mut dst = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .await
                .map_err(&staged)?;
            tokio::io::copy(&mut src, &mut dst).await.map_err(&staged)?;
            dst.flush().await.map_err(&staged)?;
            dst.sync_all().await.map_err(&staged)?;
            drop(dst);
            tokio::fs::rename(&temporary, &destination)
                .await
                .map_err(io_at("blob file", &destination))?;
            self.metadata(key).await
        }
        .await;

        if result.is_err() {
            discard_temporary(&temporary).await;
        }
        result
    }

    /// Publish `source` under `key` as a second hard link to the same inode.
    ///
    /// The link is made under a temporary sibling and renamed into place, so a
    /// reader sees the previous object or the whole new one, as with
    /// [`Self::atomic_copy`]. Sharing the inode is safe because nothing in this
    /// backend writes into an existing file: `put` and `put_file` stage a fresh
    /// file and rename it over the name, which detaches the name and leaves
    /// every other link to the old bytes untouched, and `delete` unlinks one
    /// name only.
    ///
    /// Anything that cannot be linked falls back to a real copy: a symlink
    /// (linking would publish the symlink itself, which the canonical-root
    /// check then refuses), another filesystem, a filesystem without hard
    /// links, or an inode at its link-count ceiling.
    async fn atomic_link(&self, key: &BlobKey, source: &Path) -> Result<BlobMetadata> {
        let source_kind = tokio::fs::symlink_metadata(source)
            .await
            .map_err(io_at("blob source file", source))?;
        if !source_kind.is_file() {
            return self.atomic_copy(key, source).await;
        }

        let destination = self.lexical_path(key);
        self.prepare_parent(&destination).await?;
        let temporary = temporary_sibling(&destination);
        if let Err(error) = tokio::fs::hard_link(source, &temporary).await {
            tracing::debug!(
                source = %source.display(),
                destination = %destination.display(),
                error = %error,
                "blob could not be hard-linked; copying it instead"
            );
            discard_temporary(&temporary).await;
            return self.atomic_copy(key, source).await;
        }

        // The linked sibling carries the *source's* mtime, so to the stale-spool
        // sweep (`staging::sweep_stale_sibling_spools`) it can look abandoned the
        // moment it exists. That sweep runs at startup, but a second process
        // starting beside this one is enough — and losing the sibling costs
        // only the link, never the bytes, so a failed rename takes the copy
        // path instead of failing the write.
        if let Err(error) = tokio::fs::rename(&temporary, &destination).await {
            tracing::debug!(
                path = %temporary.display(),
                error = %error,
                "hard-linked blob could not be renamed into place; copying it instead"
            );
            discard_temporary(&temporary).await;
            return self.atomic_copy(key, source).await;
        }
        self.metadata_checked(key).await
    }

    async fn read_checked(&self, key: &BlobKey) -> Result<Vec<u8>> {
        let path = self.lexical_path(key);
        match tokio::fs::File::open(&path).await {
            Ok(mut file) => {
                self.ensure_canonical_under_root(&path).await?;
                let mut data = Vec::new();
                file.read_to_end(&mut data)
                    .await
                    .map_err(io_at("blob file", &path))?;
                Ok(data)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(BlobStorageError::NotFound(key.clone()))
            }
            Err(error) => Err(BlobStorageError::io("blob file", &path, error)),
        }
    }

    async fn metadata_checked(&self, key: &BlobKey) -> Result<BlobMetadata> {
        let path = self.lexical_path(key);
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => return Err(BlobStorageError::NotFound(key.clone())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(BlobStorageError::NotFound(key.clone()));
            }
            Err(error) => return Err(BlobStorageError::io("blob file", &path, error)),
        };
        self.ensure_canonical_under_root(&path).await?;
        Ok(BlobMetadata {
            key: key.clone(),
            size: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }

    async fn list_checked(&self, prefix: Option<&BlobKey>) -> Result<Vec<BlobMetadata>> {
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(io_at("blob storage root", &self.root))?;
        let root = self.root.clone();
        let start = prefix
            .map(|key| self.lexical_path(key))
            .unwrap_or_else(|| root.clone());
        if !tokio::fs::try_exists(&start)
            .await
            .map_err(io_at("blob prefix directory", &start))?
        {
            return Ok(Vec::new());
        }
        self.ensure_canonical_under_root(&start).await?;

        tokio::task::spawn_blocking(move || collect_local_metadata(&root, &start))
            .await
            .map_err(|error| {
                // A panicked scan says nothing about which tree it was walking.
                BlobStorageError::io(
                    "blob storage root",
                    &self.root,
                    std::io::Error::other(error.to_string()),
                )
            })?
    }

    async fn move_prefix_checked(&self, source: &BlobKey, destination: &BlobKey) -> Result<bool> {
        let source_path = self.lexical_path(source);
        let metadata = match tokio::fs::symlink_metadata(&source_path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(BlobStorageError::io("blob prefix", &source_path, error)),
        };
        if metadata.file_type().is_symlink() {
            return Err(BlobStorageError::io(
                "blob prefix",
                &source_path,
                std::io::Error::other("refusing to move a symlinked blob prefix"),
            ));
        }
        self.ensure_canonical_under_root(&source_path).await?;

        let destination_path = self.lexical_path(destination);
        self.prepare_parent(&destination_path).await?;
        if tokio::fs::try_exists(&destination_path)
            .await
            .map_err(io_at("blob staging prefix", &destination_path))?
        {
            return Err(BlobStorageError::io(
                "blob staging prefix",
                &destination_path,
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "destination prefix already exists",
                ),
            ));
        }

        tokio::fs::rename(&source_path, &destination_path)
            .await
            .map_err(io_at("blob prefix", &source_path))?;
        Ok(true)
    }

    async fn delete_prefix_checked(&self, prefix: &BlobKey) -> Result<bool> {
        let path = self.lexical_path(prefix);
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(BlobStorageError::io("blob prefix", &path, error)),
        };
        if metadata.file_type().is_symlink() {
            return Err(BlobStorageError::io(
                "blob prefix",
                &path,
                std::io::Error::other("refusing to delete a symlinked blob prefix"),
            ));
        }
        self.ensure_canonical_under_root(&path).await?;
        if metadata.is_dir() {
            tokio::fs::remove_dir_all(&path)
                .await
                .map_err(io_at("blob prefix", &path))?;
        } else if metadata.is_file() {
            tokio::fs::remove_file(&path)
                .await
                .map_err(io_at("blob prefix", &path))?;
        } else {
            return Err(BlobStorageError::NotFound(prefix.clone()));
        }
        Ok(true)
    }
}

impl BlobStorage for LocalBlobStorage {
    fn backend_name(&self) -> &'static str {
        "local"
    }

    fn put<'a>(&'a self, key: &'a BlobKey, data: &'a [u8]) -> BoxFuture<'a, Result<BlobMetadata>> {
        Box::pin(async move {
            let destination = self.lexical_path(key);
            self.prepare_parent(&destination).await?;
            let temporary = temporary_sibling(&destination);

            let result = async {
                let staged = io_at("blob temporary file", &temporary);
                let mut file = tokio::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&temporary)
                    .await
                    .map_err(&staged)?;
                file.write_all(data).await.map_err(&staged)?;
                file.flush().await.map_err(&staged)?;
                file.sync_all().await.map_err(&staged)?;
                drop(file);
                tokio::fs::rename(&temporary, &destination)
                    .await
                    .map_err(io_at("blob file", &destination))?;
                self.metadata_checked(key).await
            }
            .await;

            if result.is_err() {
                discard_temporary(&temporary).await;
            }
            result
        })
    }

    fn put_file<'a>(
        &'a self,
        key: &'a BlobKey,
        source: &'a Path,
    ) -> BoxFuture<'a, Result<BlobMetadata>> {
        Box::pin(self.atomic_copy(key, source))
    }

    fn put_file_shared<'a>(
        &'a self,
        key: &'a BlobKey,
        source: &'a Path,
    ) -> BoxFuture<'a, Result<BlobMetadata>> {
        Box::pin(self.atomic_link(key, source))
    }

    fn get<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(self.read_checked(key))
    }

    fn metadata<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, Result<BlobMetadata>> {
        Box::pin(self.metadata_checked(key))
    }

    fn exists<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            match self.metadata_checked(key).await {
                Ok(_) => Ok(true),
                Err(BlobStorageError::NotFound(_)) => Ok(false),
                Err(error) => Err(error),
            }
        })
    }

    fn delete<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let path = self.lexical_path(key);
            match tokio::fs::metadata(&path).await {
                Ok(metadata) if metadata.is_file() => {
                    self.ensure_canonical_under_root(&path).await?;
                    tokio::fs::remove_file(&path)
                        .await
                        .map_err(io_at("blob file", &path))?;
                    Ok(true)
                }
                Ok(_) => Err(BlobStorageError::NotFound(key.clone())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(BlobStorageError::io("blob file", &path, error)),
            }
        })
    }

    fn list<'a>(&'a self, prefix: Option<&'a BlobKey>) -> BoxFuture<'a, Result<Vec<BlobMetadata>>> {
        Box::pin(self.list_checked(prefix))
    }

    fn move_prefix<'a>(
        &'a self,
        source: &'a BlobKey,
        destination: &'a BlobKey,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.move_prefix_checked(source, destination))
    }

    fn delete_prefix<'a>(&'a self, prefix: &'a BlobKey) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.delete_prefix_checked(prefix))
    }

    fn local_path(&self, key: &BlobKey) -> Option<PathBuf> {
        Some(self.lexical_path(key))
    }
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_BLOB_KEY_LEN {
        return Err(BlobStorageError::InvalidKey(format!(
            "length must be 1..={MAX_BLOB_KEY_LEN}"
        )));
    }
    if key.starts_with('/')
        || key.ends_with('/')
        || key.contains('\\')
        || key.chars().any(char::is_control)
    {
        return Err(BlobStorageError::InvalidKey(key.to_string()));
    }
    if key.split('/').any(|segment| {
        segment.is_empty() || segment == "." || segment == ".." || segment.starts_with('.')
    }) {
        return Err(BlobStorageError::InvalidKey(key.to_string()));
    }
    Ok(())
}

/// The longest encoded directory segment: one file name on every filesystem
/// the local backend runs on.
const MAX_DIRECTORY_SEGMENT_LEN: usize = 255;

/// The longest encoded final segment. A write lands in a spool sibling first,
/// `.{name}.{uuid}.tmp` (`staging::blob_write_spool_name`), which adds 42
/// bytes — so this is the longest name a write has ever been able to finish.
const MAX_FILE_SEGMENT_LEN: usize = MAX_DIRECTORY_SEGMENT_LEN - 42;

/// How much of an over-long segment survives in front of its digest, so the
/// stored name still says what the blob was.
const LONG_SEGMENT_PREFIX_LEN: usize = 120;

/// One key segment, percent-encoded and short enough to be a file name.
///
/// Encoding triples every byte outside `[A-Za-z0-9._-]`, so a 255-byte upload
/// name — 85 Cyrillic letters — became a 510-byte segment, and the write
/// failed with ENAMETOOLONG: a 500 for a name every validator had accepted.
/// A segment past `limit` keeps a readable prefix and appends a digest of the
/// whole raw segment, so it stays deterministic (a read rebuilds the same key
/// from the stored name) and two long names that share a prefix stay apart.
/// The limits are exactly where writes used to start failing, so every
/// segment a write could ever store is untouched, byte for byte, and keys
/// already in storage still resolve.
fn encode_segment(segment: &str, limit: usize) -> String {
    let encoded = percent_encode_segment(segment);
    if encoded.len() <= limit {
        return encoded;
    }
    use sha2::Digest as _;
    let digest = hex::encode(sha2::Sha256::digest(segment.as_bytes()));
    // Never cut through a `%XX` escape.
    let mut cut = LONG_SEGMENT_PREFIX_LEN;
    while encoded[..cut].ends_with('%') || encoded[..cut - 1].ends_with('%') {
        cut -= 1;
    }
    format!("{}-{digest}", &encoded[..cut])
}

fn percent_encode_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for (index, byte) in segment.as_bytes().iter().enumerate() {
        if byte.is_ascii_alphanumeric()
            || matches!(*byte, b'-' | b'_')
            || (*byte == b'.' && index > 0)
        {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

/// Drop the staging file of a write that failed.
///
/// The write's own error is what the caller needs, so a failed cleanup must not
/// replace it — but discarding it silently makes a leaked `.tmp` sibling
/// invisible until the volume fills up, and the name is a UUID nothing else
/// records.
///
/// This runs on every path out of a write the *process* survives. The one it
/// cannot cover — a `SIGKILL` between creating the sibling and renaming it —
/// is `staging::sweep_stale_sibling_spools`'s, which recognises the name
/// through `staging::blob_write_spool_name`'s matching half.
async fn discard_temporary(temporary: &Path) {
    match tokio::fs::remove_file(temporary).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            path = %temporary.display(),
            %error,
            "failed to remove the temporary file of an aborted blob write"
        ),
    }
}

fn temporary_sibling(destination: &Path) -> PathBuf {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("blob");
    destination.with_file_name(crate::staging::blob_write_spool_name(name, Uuid::new_v4()))
}

fn collect_local_metadata(root: &Path, start: &Path) -> Result<Vec<BlobMetadata>> {
    let canonical_root = root
        .canonicalize()
        .map_err(io_at("blob storage root", root))?;
    let mut pending = vec![start.to_path_buf()];
    let mut objects = Vec::new();

    while let Some(path) = pending.pop() {
        let entry_error = io_at("blob inventory entry", &path);
        let metadata = std::fs::symlink_metadata(&path).map_err(&entry_error)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path).map_err(&entry_error)? {
                pending.push(entry.map_err(&entry_error)?.path());
            }
            continue;
        }
        if !metadata.is_file() {
            continue;
        }

        let canonical = path.canonicalize().map_err(&entry_error)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(BlobStorageError::OutsideRoot(canonical));
        }
        let relative = canonical
            .strip_prefix(&canonical_root)
            .map_err(|_| BlobStorageError::OutsideRoot(canonical.clone()))?;
        let serialized = relative
            .iter()
            .map(|segment| segment.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        // Temporary files are an implementation detail and are never inventory
        // objects — their names are not valid `BlobKey`s, so listing one would
        // fail the whole inventory rather than describe it. That makes this
        // skip the reason a leaked sibling used to be invisible even to an
        // operator counting the store by hand; what answers for them now is
        // `staging::sweep_stale_sibling_spools`, not this listing.
        if serialized
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with('.') && name.ends_with(".tmp"))
        {
            continue;
        }
        let key = BlobKey::new(serialized)?;
        objects.push(BlobMetadata {
            key,
            size: metadata.len(),
            modified: metadata.modified().ok(),
        });
    }

    objects.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::{BlobKey, BlobStorage, BlobStorageError, LocalBlobStorage};

    #[test]
    fn keys_reject_traversal_and_encode_user_segments() {
        for invalid in ["", "/absolute", "a/../b", "a/.hidden", "a\\b", "a//b"] {
            assert!(matches!(
                BlobKey::new(invalid),
                Err(BlobStorageError::InvalidKey(_))
            ));
        }

        let key = BlobKey::from_segments(["packages", "alice", "@scope/pkg", "a b.tgz"])
            .expect("encoded key");
        assert_eq!(key.as_str(), "packages/alice/%40scope%2Fpkg/a%20b.tgz");
    }

    #[tokio::test]
    async fn a_long_non_ascii_name_is_a_key_the_local_backend_can_write() {
        // 120 Cyrillic letters: 240 bytes, inside every 255-byte name limit,
        // and 720 once percent-encoded.
        let long = format!("{}.tar.gz", "ж".repeat(120));
        let key = BlobKey::from_segments(["releases", "alice", "app", "1", "2", &long])
            .expect("a long name still makes a key");
        assert!(
            key.as_str().split('/').all(|segment| segment.len() <= 213),
            "{key}"
        );
        assert_eq!(
            key,
            BlobKey::from_segments(["releases", "alice", "app", "1", "2", &long]).unwrap(),
            "a read must rebuild the key the write used"
        );
        let sibling = format!("{}.zip", "ж".repeat(120));
        assert_ne!(
            key,
            BlobKey::from_segments(["releases", "alice", "app", "1", "2", &sibling]).unwrap(),
            "two long names sharing a prefix must not share a blob"
        );

        let dir = tempfile::tempdir().unwrap();
        let storage = LocalBlobStorage::new(dir.path());
        storage
            .put(&key, b"bytes")
            .await
            .expect("write under the long name");
        assert_eq!(storage.get(&key).await.unwrap(), b"bytes");
    }

    #[tokio::test]
    async fn every_name_a_write_could_store_keeps_its_old_key() {
        // The longest final segment a write has always been able to finish is
        // left exactly as it was — keys already in storage depend on it — and
        // one byte more is the first to be shortened.
        let dir = tempfile::tempdir().unwrap();
        let storage = LocalBlobStorage::new(dir.path());
        let longest = "a".repeat(213);
        let key = BlobKey::from_segments(["x", &longest]).unwrap();
        assert_eq!(key.as_str(), format!("x/{longest}"));
        storage
            .put(&key, b"ok")
            .await
            .expect("the longest old name still writes");

        let over = "a".repeat(214);
        let key = BlobKey::from_segments(["x", &over]).unwrap();
        assert_ne!(key.as_str(), format!("x/{over}"));
        storage
            .put(&key, b"ok")
            .await
            .expect("one byte more writes too, shortened");

        // A directory segment may be a whole file name long.
        let directory = "d".repeat(255);
        let key = BlobKey::from_segments([directory.as_str(), "f"]).unwrap();
        assert_eq!(key.as_str(), format!("{directory}/f"));
    }

    #[test]
    fn a_shortened_segment_never_splits_an_escape() {
        for filler in ["a", "ab", ""] {
            let name = format!("{filler}{}", "ж".repeat(150));
            let key = BlobKey::from_segments(["x", &name]).unwrap();
            let segment = key.as_str().rsplit('/').next().unwrap();
            let prefix = segment.rsplit_once('-').unwrap().0.as_bytes();
            assert!(
                prefix[prefix.len() - 1] != b'%' && prefix[prefix.len() - 2] != b'%',
                "{filler:?}: the prefix ends inside a `%XX` escape: {segment}"
            );
        }
    }

    #[tokio::test]
    async fn local_backend_round_trip_inventory_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let storage = LocalBlobStorage::new(dir.path());
        let first = BlobKey::new("artifacts/1/one.bin").unwrap();
        let second = BlobKey::new("artifacts/2/two.bin").unwrap();

        let stored = storage.put(&first, b"first").await.unwrap();
        assert_eq!(stored.size, 5);
        assert_eq!(storage.get(&first).await.unwrap(), b"first");
        assert!(storage.exists(&first).await.unwrap());

        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("source");
        tokio::fs::write(&source, b"second").await.unwrap();
        storage.put_file(&second, &source).await.unwrap();

        let prefix = BlobKey::new("artifacts/1").unwrap();
        let prefixed = storage.list(Some(&prefix)).await.unwrap();
        assert_eq!(prefixed.len(), 1);
        assert_eq!(prefixed[0].key, first);
        assert_eq!(storage.list(None).await.unwrap().len(), 2);

        let staged = BlobKey::new("_deleted/artifacts/1").unwrap();
        assert!(storage.move_prefix(&prefix, &staged).await.unwrap());
        assert!(storage.list(Some(&prefix)).await.unwrap().is_empty());
        assert_eq!(storage.list(Some(&staged)).await.unwrap().len(), 1);
        assert!(storage.move_prefix(&staged, &prefix).await.unwrap());

        assert!(storage.delete(&first).await.unwrap());
        assert!(!storage.delete(&first).await.unwrap());
        assert!(!storage.exists(&first).await.unwrap());

        let artifacts = BlobKey::new("artifacts").unwrap();
        assert!(storage.delete_prefix(&artifacts).await.unwrap());
        assert!(storage.list(None).await.unwrap().is_empty());
        assert!(!storage.delete_prefix(&artifacts).await.unwrap());
    }

    /// A shared copy is the same inode under a second name — and stays the
    /// bytes it was when either name is later replaced or removed, because
    /// nothing in the backend writes into an existing file.
    #[tokio::test]
    async fn a_shared_copy_shares_the_inode_and_outlives_changes_to_its_source() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let storage = LocalBlobStorage::new(dir.path());
        let source = BlobKey::new("lfs/alice/assets/ab/source.zst").unwrap();
        let copy = BlobKey::new("lfs/bob/assets/ab/source.zst").unwrap();
        storage.put(&source, b"original bytes").await.unwrap();

        let source_path = storage.local_path(&source).unwrap();
        let stored = storage.put_file_shared(&copy, &source_path).await.unwrap();
        assert_eq!(stored.size, 14);
        let copy_path = storage.local_path(&copy).unwrap();
        assert_eq!(
            std::fs::metadata(&source_path).unwrap().ino(),
            std::fs::metadata(&copy_path).unwrap().ino(),
            "the shared copy must be a hard link, not a second set of bytes"
        );

        storage.put(&source, b"replacement").await.unwrap();
        assert_eq!(storage.get(&copy).await.unwrap(), b"original bytes");
        assert!(storage.delete(&source).await.unwrap());
        assert_eq!(storage.get(&copy).await.unwrap(), b"original bytes");
        let leftovers: Vec<_> = std::fs::read_dir(copy_path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            leftovers.len(),
            1,
            "a temporary link was left beside the copy: {leftovers:?}"
        );
    }

    /// A symlinked source is never linked as a symlink — that would publish a
    /// name the canonical-root check refuses to read — but copied by content.
    #[tokio::test]
    async fn a_shared_copy_of_a_symlink_copies_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let storage = LocalBlobStorage::new(dir.path().join("blobs"));
        let target = dir.path().join("target");
        std::fs::write(&target, b"behind a symlink").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let copy = BlobKey::new("lfs/bob/assets/ab/linked").unwrap();
        storage.put_file_shared(&copy, &link).await.unwrap();
        let copy_path = storage.local_path(&copy).unwrap();
        assert!(
            !std::fs::symlink_metadata(&copy_path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the stored object must be a regular file"
        );
        assert_eq!(storage.get(&copy).await.unwrap(), b"behind a symlink");
    }

    /// The failing directory is derived from the storage root and the key, so
    /// it appears in neither the request nor the database row that names the
    /// object — a bare `io::Error` leaves the operator with an errno and
    /// nothing to act on.
    ///
    /// The trigger is a regular file where the backend needs a directory
    /// (`ENOTDIR`) rather than a permission error: `describe_path_error` returns
    /// early on `PermissionDenied` and never appends the remediation, and a
    /// `chmod`-based setup does not bite when the tests run as root.
    #[tokio::test]
    async fn io_failures_name_the_path_the_errno_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let occupied = dir.path().join("artifacts");
        tokio::fs::write(&occupied, b"not a directory")
            .await
            .unwrap();
        let storage = LocalBlobStorage::new(dir.path());
        let key = BlobKey::new("artifacts/1/one.bin").unwrap();

        let error = storage
            .put(&key, b"payload")
            .await
            .expect_err("a regular file cannot host a subdirectory");

        let BlobStorageError::Io { path, source, .. } = &error else {
            panic!("expected an I/O error, got {error:?}");
        };
        assert_eq!(path, &occupied.join("1"));
        assert!(source.raw_os_error().is_some(), "{source}");
        let rendered = error.to_string();
        assert!(
            rendered.contains(&occupied.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("[server].repo_root"), "{rendered}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_backend_refuses_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        let storage = LocalBlobStorage::new(root.path());
        let key = BlobKey::new("escape/object").unwrap();

        assert!(matches!(
            storage.put(&key, b"nope").await,
            Err(BlobStorageError::OutsideRoot(_))
        ));
        assert!(!outside.path().join("object").exists());
    }
}
