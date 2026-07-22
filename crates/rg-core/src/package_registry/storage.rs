//! Package Registry storage layer.
//!
//! Manages file-system storage for package blobs.
//! Directory layout: `{root}/{owner}/{repo}/packages/{type}/{name}/{version}/{filename}`

use crate::blob_storage::{BlobKey, BlobStorage, LocalBlobStorage};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
        BlobKey::from_segments([
            "packages",
            owner,
            repo,
            package_type,
            name,
            version,
            filename,
        ])
        .map_err(Into::into)
    }

    /// Store a file returning its storage path and sha256.
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

        let sha256 = hex::encode(Sha256::digest(data));

        Ok(StoredFile {
            filename: filename.to_string(),
            size: metadata.size as i64,
            sha256,
            storage_path: key.to_string(),
        })
    }

    /// Read a file from storage.
    pub async fn read_file(&self, storage_path: &str) -> Result<Vec<u8>> {
        match BlobKey::new(storage_path) {
            Ok(key) => self.backend.get(&key).await.map_err(Into::into),
            Err(_) => tokio::fs::read(storage_path).await.map_err(Into::into),
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
                    tokio::fs::remove_file(path).await?;
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
    pub sha256: String,
    pub storage_path: String,
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

        assert_eq!(
            stored.storage_path,
            "packages/alice/demo/npm/%40scope%2Fpkg/1.0.0/package.tgz"
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
