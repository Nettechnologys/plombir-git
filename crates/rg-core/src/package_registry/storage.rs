//! Package Registry storage layer.
//!
//! Manages file-system storage for package blobs.
//! Directory layout: `{root}/{owner}/{repo}/packages/{type}/{name}/{version}/{filename}`

use crate::blob_storage::{BlobKey, BlobStorage, LocalBlobStorage};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One actionable error for a filesystem failure on a pre-[`BlobKey`] package
/// path.
///
/// Rows written before the blob-storage migration hold an absolute path instead
/// of a key, and those paths bypass the backend entirely — so they also bypass
/// the diagnostics [`BlobStorageError`](crate::blob_storage::BlobStorageError)
/// attaches, and reach the operator as a naked errno.
fn legacy_path_error(what: &str, storage_path: &str, error: &std::io::Error) -> Error {
    crate::platform::fs::path_error(
        what,
        Path::new(storage_path),
        error,
        crate::platform::fs::BLOB_STORAGE_HINT,
    )
}

#[derive(Clone)]
pub struct PackageStorage {
    backend: Arc<dyn BlobStorage>,
}

impl PackageStorage {
    pub fn new(root: &Path) -> Self {
        Self {
            backend: Arc::new(LocalBlobStorage::new(root)),
        }
    }

    pub fn from_backend(backend: Arc<dyn BlobStorage>) -> Self {
        Self { backend }
    }

    fn version_key(
        &self,
        owner: &str,
        repo: &str,
        package_type: &str,
        name: &str,
        version: &str,
    ) -> Result<BlobKey> {
        BlobKey::from_segments(["packages", owner, repo, package_type, name, version])
            .map_err(Into::into)
    }

    fn file_key(
        &self,
        owner: &str,
        repo: &str,
        package_type: &str,
        name: &str,
        version: &str,
        filename: &str,
    ) -> Result<BlobKey> {
        // A publish request owns exactly the objects it wrote. A stable
        // `{version}/{filename}` key cannot express that ownership: two
        // concurrent publishes overwrite the same object, and the loser then
        // cannot roll its write back without deleting the winner's bytes.
        // Keep the version prefix for bulk deletion, but isolate every write
        // below it so compensation can safely delete the returned key.
        let object_id = uuid::Uuid::new_v4().simple().to_string();
        BlobKey::from_segments([
            "packages",
            owner,
            repo,
            package_type,
            name,
            version,
            "objects",
            object_id.as_str(),
            filename,
        ])
        .map_err(Into::into)
    }

    /// Store a file returning its storage path and digests.
    ///
    /// Three digests, because three is what the protocols ask for and a
    /// registry that keeps only one ends up publishing it under whatever name
    /// each protocol happens to use: npm and Composer both spell `dist.shasum`,
    /// and both define it as SHA-1, so a SHA-256 in that field is not a
    /// stronger answer but a wrong one — the client hashes the file it just
    /// downloaded with SHA-1 and refuses it. See [`FileDigests`].
    #[allow(clippy::too_many_arguments)]
    pub async fn store_file(
        &self,
        owner: &str,
        repo: &str,
        package_type: &str,
        name: &str,
        version: &str,
        filename: &str,
        data: &[u8],
    ) -> Result<StoredFile> {
        let key = self.file_key(owner, repo, package_type, name, version, filename)?;
        let metadata = self.backend.put(&key, data).await?;

        Ok(StoredFile {
            filename: filename.to_string(),
            size: metadata.size as i64,
            digests: FileDigests::of(data),
            storage_path: key.to_string(),
        })
    }

    /// Read a file from storage.
    pub async fn read_file(&self, storage_path: &str) -> Result<Vec<u8>> {
        match BlobKey::new(storage_path) {
            Ok(key) => self.backend.get(&key).await.map_err(Into::into),
            // Legacy rows hold an absolute path. It is right there in the
            // argument, yet a bare `io::Error` still reaches the operator as an
            // errno with no file attached.
            Err(_) => tokio::fs::read(storage_path)
                .await
                .map_err(|error| legacy_path_error("package file", storage_path, &error)),
        }
    }

    /// Stream a file from storage (returns the file path for serving).
    pub fn file_path(&self, storage_path: &str) -> Option<PathBuf> {
        match BlobKey::new(storage_path) {
            Ok(key) => self.backend.local_path(&key),
            Err(_) => Some(PathBuf::from(storage_path)),
        }
    }

    /// Delete a version directory and all its files.
    pub async fn delete_version(
        &self,
        owner: &str,
        repo: &str,
        package_type: &str,
        name: &str,
        version: &str,
    ) -> Result<()> {
        let prefix = self.version_key(owner, repo, package_type, name, version)?;
        for object in self.backend.list(Some(&prefix)).await? {
            self.backend.delete(&object.key).await?;
        }
        Ok(())
    }

    /// Delete a file by storage path.
    pub async fn delete_file(&self, storage_path: &str) -> Result<()> {
        match BlobKey::new(storage_path) {
            Ok(key) => {
                self.backend.delete(&key).await?;
            }
            Err(_) => {
                let path = Path::new(storage_path);
                if path.exists() {
                    tokio::fs::remove_file(path)
                        .await
                        .map_err(|error| legacy_path_error("package file", storage_path, &error))?;
                }
            }
        }
        Ok(())
    }

    /// Check if a version directory has any files (synchronous check for simplicity).
    pub async fn has_files(
        &self,
        owner: &str,
        repo: &str,
        package_type: &str,
        name: &str,
        version: &str,
    ) -> bool {
        let Ok(prefix) = self.version_key(owner, repo, package_type, name, version) else {
            return false;
        };
        self.backend
            .list(Some(&prefix))
            .await
            .map(|objects| !objects.is_empty())
            .unwrap_or(false)
    }
}

impl std::fmt::Debug for PackageStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackageStorage")
            .field("backend", &self.backend.backend_name())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct StoredFile {
    pub filename: String,
    pub size: i64,
    pub digests: FileDigests,
    pub storage_path: String,
}

/// Every digest of a stored file a package protocol may ask us to publish.
///
/// The algorithm is not ours to choose: `dist.shasum` is SHA-1 in npm and in
/// Composer, npm's `dist.integrity` is conventionally SHA-512, and cargo's
/// `cksum`, RubyGems' compact-index checksum, PyPI's `#sha256=` fragment and
/// Helm's `digest` are SHA-256. All three are computed once, at publish, over
/// the same bytes that were stored — recomputing one later means reading the
/// blob back on a metadata request, which is the one thing these routes must
/// not do.
#[derive(Debug, Clone)]
pub struct FileDigests {
    /// Lowercase hex, 40 chars. Only ever published where the protocol spells
    /// SHA-1 — never as a general-purpose identity.
    pub sha1: String,
    /// Lowercase hex, 64 chars.
    pub sha256: String,
    /// Lowercase hex, 128 chars.
    pub sha512: String,
}

impl FileDigests {
    /// Hash a blob with every algorithm the registries publish.
    pub fn of(data: &[u8]) -> Self {
        Self {
            sha1: hex::encode(Sha1::digest(data)),
            sha256: hex::encode(Sha256::digest(data)),
            sha512: hex::encode(Sha512::digest(data)),
        }
    }
}

/// Error type for storage operations.
pub type Error = anyhow::Error;
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::PackageStorage;

    #[tokio::test]
    async fn stores_portable_key_and_deletes_version_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let storage = PackageStorage::new(directory.path());
        let stored = storage
            .store_file(
                "alice",
                "demo",
                "npm",
                "@scope/pkg",
                "1.0.0",
                "package.tgz",
                b"package",
            )
            .await
            .unwrap();

        assert!(
            stored
                .storage_path
                .starts_with("packages/alice/demo/npm/%40scope%2Fpkg/1.0.0/objects/"),
            "{}",
            stored.storage_path
        );
        assert!(
            stored.storage_path.ends_with("/package.tgz"),
            "{}",
            stored.storage_path
        );
        assert_eq!(
            storage.read_file(&stored.storage_path).await.unwrap(),
            b"package"
        );
        assert!(
            storage
                .has_files("alice", "demo", "npm", "@scope/pkg", "1.0.0")
                .await
        );

        storage
            .delete_version("alice", "demo", "npm", "@scope/pkg", "1.0.0")
            .await
            .unwrap();
        assert!(
            !storage
                .has_files("alice", "demo", "npm", "@scope/pkg", "1.0.0")
                .await
        );
    }

    /// The legacy branch bypasses the blob backend, so it also bypasses the
    /// path the backend attaches to its own I/O errors.
    #[tokio::test]
    async fn failing_legacy_read_names_the_absolute_path() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("gone.bin");
        let storage = PackageStorage::new(directory.path());

        let error = storage
            .read_file(missing.to_string_lossy().as_ref())
            .await
            .expect_err("the legacy file does not exist");

        assert!(
            error.to_string().contains(&missing.display().to_string()),
            "{error}"
        );
    }

    #[tokio::test]
    async fn reads_legacy_absolute_path_during_migration_window() {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("legacy.bin");
        tokio::fs::write(&legacy, b"legacy").await.unwrap();
        let storage = PackageStorage::new(directory.path());

        assert_eq!(
            storage
                .read_file(legacy.to_string_lossy().as_ref())
                .await
                .unwrap(),
            b"legacy"
        );
    }
}
