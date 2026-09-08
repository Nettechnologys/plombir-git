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
        let encoded: Vec<String> = segments.into_iter().map(encode_segment).collect();
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

fn encode_segment(segment: &str) -> String {
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
