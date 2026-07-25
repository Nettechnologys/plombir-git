//! OCI content-addressed storage backed by the shared [`BlobStorage`] contract.
//!
//! Completed blobs and manifests use durable backend-neutral keys. Chunked
//! uploads remain local temporary files until their digest has been verified,
//! then `put_file` atomically publishes them to the configured backend.

use crate::blob_storage::{BlobKey, BlobStorage, LocalBlobStorage};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

/// Where a chunked OCI upload is staged and what has to be true about it.
///
/// The directory is derived from `repo_root` and a generated UUID, so nothing
/// in a failed `docker push` names it — the client only ever sees the errno the
/// registry handed back.
const UPLOAD_DIR_HINT: &str =
    "chunked OCI uploads are staged in `_oci_uploads/` under the `[server].repo_root` directory; \
     that directory must be writable by the user running forgekeep";

/// One actionable error for a filesystem failure on an OCI upload path.
fn upload_path_error(what: &str, path: &Path, error: &std::io::Error) -> anyhow::Error {
    crate::platform::fs::path_error(what, path, error, UPLOAD_DIR_HINT)
}

#[derive(Clone)]
pub struct OciStorage {
    backend: Arc<dyn BlobStorage>,
    upload_root: PathBuf,
    legacy_root: Option<PathBuf>,
}

impl OciStorage {
    /// Backwards-compatible local constructor.
    pub fn new(root: &Path) -> Self {
        Self {
            backend: Arc::new(LocalBlobStorage::new(root)),
            upload_root: root.join("_uploads"),
            legacy_root: Some(root.to_path_buf()),
        }
    }

    pub fn from_backend(backend: Arc<dyn BlobStorage>, upload_root: impl Into<PathBuf>) -> Self {
        Self {
            backend,
            upload_root: upload_root.into(),
            legacy_root: None,
        }
    }

    fn blob_key(&self, owner: &str, repo: &str, digest: &str) -> anyhow::Result<BlobKey> {
        let (algorithm, hash) = digest_parts(digest)?;
        BlobKey::from_segments(["oci", owner, repo, "blobs", algorithm, &hash[..2], hash])
            .map_err(Into::into)
    }

    fn manifest_key(&self, owner: &str, repo: &str, digest: &str) -> anyhow::Result<BlobKey> {
        let (algorithm, hash) = digest_parts(digest)?;
        BlobKey::from_segments(["oci", owner, repo, "manifests", algorithm, hash])
            .map_err(Into::into)
    }

    fn upload_dir(&self, owner: &str, repo: &str, uuid: &str) -> PathBuf {
        let key = BlobKey::from_segments(["oci-uploads", owner, repo, uuid])
            .expect("validated OCI namespace and generated upload UUID");
        key.as_str()
            .split('/')
            .fold(self.upload_root.clone(), |path, segment| path.join(segment))
    }

    fn upload_file_path(&self, owner: &str, repo: &str, uuid: &str) -> PathBuf {
        self.upload_dir(owner, repo, uuid).join("data")
    }

    fn legacy_blob_path(&self, owner: &str, repo: &str, digest: &str) -> Option<PathBuf> {
        let root = self.legacy_root.as_ref()?;
        let (algorithm, hash) = digest_parts(digest).ok()?;
        Some(
            root.join(owner)
                .join(repo)
                .join("oci")
                .join("_blobs")
                .join(algorithm)
                .join(&hash[..2])
                .join(hash),
        )
    }

    fn legacy_manifest_path(&self, owner: &str, repo: &str, digest: &str) -> Option<PathBuf> {
        let root = self.legacy_root.as_ref()?;
        let (algorithm, _) = digest_parts(digest).ok()?;
        Some(
            root.join(owner)
                .join(repo)
                .join("oci")
                .join("_manifests")
                .join(algorithm)
                .join(digest.replace(':', "_")),
        )
    }

    pub async fn blob_exists(&self, owner: &str, repo: &str, digest: &str) -> anyhow::Result<bool> {
        let key = self.blob_key(owner, repo, digest)?;
        if self.backend.exists(&key).await? {
            return Ok(true);
        }
        Ok(self
            .legacy_blob_path(owner, repo, digest)
            .is_some_and(|path| path.is_file()))
    }

    pub async fn store_blob(
        &self,
        owner: &str,
        repo: &str,
        digest: &str,
        data: &[u8],
    ) -> anyhow::Result<String> {
        verify_digest(digest, data)?;
        let key = self.blob_key(owner, repo, digest)?;
        self.backend.put(&key, data).await?;
        Ok(key.to_string())
    }

    pub async fn read_blob(
        &self,
        owner: &str,
        repo: &str,
        digest: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let key = self.blob_key(owner, repo, digest)?;
        match self.backend.get(&key).await {
            Ok(data) => Ok(data),
            Err(crate::blob_storage::BlobStorageError::NotFound(_)) => {
                let path = self
                    .legacy_blob_path(owner, repo, digest)
                    .ok_or_else(|| anyhow::anyhow!("blob not found: {digest}"))?;
                tokio::fs::read(&path)
                    .await
                    .map_err(|error| upload_path_error("legacy OCI blob", &path, &error))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn blob_local_path(
        &self,
        owner: &str,
        repo: &str,
        digest: &str,
    ) -> anyhow::Result<Option<PathBuf>> {
        let key = self.blob_key(owner, repo, digest)?;
        if let Some(path) = self.backend.local_path(&key) {
            if path.is_file() {
                return Ok(Some(path));
            }
        }
        Ok(self
            .legacy_blob_path(owner, repo, digest)
            .filter(|path| path.is_file()))
    }

    pub async fn copy_blob_file(
        &self,
        src_owner: &str,
        src_repo: &str,
        dst_owner: &str,
        dst_repo: &str,
        digest: &str,
    ) -> anyhow::Result<String> {
        let source = self.blob_key(src_owner, src_repo, digest)?;
        let destination = self.blob_key(dst_owner, dst_repo, digest)?;
        if self.backend.exists(&destination).await? {
            return Ok(destination.to_string());
        }

        if self.backend.exists(&source).await? {
            if let Some(path) = self.backend.local_path(&source) {
                self.backend.put_file(&destination, &path).await?;
            } else {
                let data = self.backend.get(&source).await?;
                self.backend.put(&destination, &data).await?;
            }
        } else if let Some(path) = self
            .legacy_blob_path(src_owner, src_repo, digest)
            .filter(|path| path.is_file())
        {
            self.backend.put_file(&destination, &path).await?;
        } else {
            anyhow::bail!("source blob not found: {digest}");
        }
        Ok(destination.to_string())
    }

    pub async fn store_manifest(
        &self,
        owner: &str,
        repo: &str,
        digest: &str,
        data: &[u8],
    ) -> anyhow::Result<String> {
        let key = self.manifest_key(owner, repo, digest)?;
        self.backend.put(&key, data).await?;
        Ok(key.to_string())
    }

    pub async fn read_manifest(
        &self,
        owner: &str,
        repo: &str,
        digest: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let key = self.manifest_key(owner, repo, digest)?;
        match self.backend.get(&key).await {
            Ok(data) => Ok(data),
            Err(crate::blob_storage::BlobStorageError::NotFound(_)) => {
                let path = self
                    .legacy_manifest_path(owner, repo, digest)
                    .ok_or_else(|| anyhow::anyhow!("manifest not found: {digest}"))?;
                tokio::fs::read(&path)
                    .await
                    .map_err(|error| upload_path_error("legacy OCI manifest", &path, &error))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn create_upload(&self, owner: &str, repo: &str) -> anyhow::Result<(String, String)> {
        let uuid = Uuid::new_v4().to_string();
        let directory = self.upload_dir(owner, repo, &uuid);
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|error| upload_path_error("OCI upload directory", &directory, &error))?;
        let file = self.upload_file_path(owner, repo, &uuid);
        tokio::fs::write(&file, &[])
            .await
            .map_err(|error| upload_path_error("OCI upload file", &file, &error))?;
        Ok((uuid, file.to_string_lossy().to_string()))
    }

    pub async fn append_to_upload(
        &self,
        owner: &str,
        repo: &str,
        uuid: &str,
        data: &[u8],
    ) -> anyhow::Result<i64> {
        use tokio::io::AsyncWriteExt;
        let path = self.upload_file_path(owner, repo, uuid);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|error| upload_path_error("OCI upload file", &path, &error))?;
        file.write_all(data)
            .await
            .map_err(|error| upload_path_error("OCI upload file", &path, &error))?;
        file.flush()
            .await
            .map_err(|error| upload_path_error("OCI upload file", &path, &error))?;
        let metadata = file
            .metadata()
            .await
            .map_err(|error| upload_path_error("OCI upload file", &path, &error))?;
        Ok(metadata.len() as i64)
    }

    /// Bytes already staged for `uuid`, or `0` when the upload has not been
    /// written to yet.
    ///
    /// Any other failure is an error rather than a zero: an unreadable chunk
    /// file used to be indistinguishable from an empty one, and the size feeds
    /// the `Range` header a client resumes from — so answering `0` for a
    /// directory it cannot stat tells the client to re-send a blob the
    /// registry will fail on again.
    pub fn upload_size(&self, owner: &str, repo: &str, uuid: &str) -> anyhow::Result<i64> {
        let path = self.upload_file_path(owner, repo, uuid);
        match std::fs::metadata(&path) {
            Ok(metadata) => Ok(metadata.len() as i64),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(upload_path_error("OCI upload file", &path, &error)),
        }
    }

    pub async fn finalize_upload(
        &self,
        owner: &str,
        repo: &str,
        uuid: &str,
        expected_digest: &str,
    ) -> anyhow::Result<(String, i64, String)> {
        let upload_path = self.upload_file_path(owner, repo, uuid);
        let key = self.blob_key(owner, repo, expected_digest)?;

        if self.backend.exists(&key).await? {
            let size = self.backend.metadata(&key).await?.size as i64;
            let _ = tokio::fs::remove_dir_all(self.upload_dir(owner, repo, uuid)).await;
            return Ok((expected_digest.to_string(), size, key.to_string()));
        }

        let mut source = tokio::fs::File::open(&upload_path)
            .await
            .map_err(|error| upload_path_error("OCI upload file", &upload_path, &error))?;
        let mut hasher = Sha256::new();
        let mut size = 0_i64;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = source
                .read(&mut buffer)
                .await
                .map_err(|error| upload_path_error("OCI upload file", &upload_path, &error))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            size += read as i64;
        }
        let actual = format!("sha256:{}", hex::encode(hasher.finalize()));
        if actual != expected_digest {
            anyhow::bail!("digest mismatch: expected {expected_digest}, got {actual}");
        }

        self.backend.put_file(&key, &upload_path).await?;
        let _ = tokio::fs::remove_dir_all(self.upload_dir(owner, repo, uuid)).await;
        Ok((expected_digest.to_string(), size, key.to_string()))
    }

    pub async fn delete_upload(&self, owner: &str, repo: &str, uuid: &str) -> anyhow::Result<()> {
        let directory = self.upload_dir(owner, repo, uuid);
        let exists = tokio::fs::try_exists(&directory)
            .await
            .map_err(|error| upload_path_error("OCI upload directory", &directory, &error))?;
        if exists {
            tokio::fs::remove_dir_all(&directory)
                .await
                .map_err(|error| upload_path_error("OCI upload directory", &directory, &error))?;
        }
        Ok(())
    }

    pub fn upload_file(&self, owner: &str, repo: &str, uuid: &str) -> PathBuf {
        self.upload_file_path(owner, repo, uuid)
    }
}

impl std::fmt::Debug for OciStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OciStorage")
            .field("backend", &self.backend.backend_name())
            .field("upload_root", &self.upload_root)
            .field("legacy_root", &self.legacy_root)
            .finish()
    }
}

fn digest_parts(digest: &str) -> anyhow::Result<(&str, &str)> {
    let (algorithm, hash) = digest
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid OCI digest"))?;
    if algorithm != "sha256"
        || hash.len() != 64
        || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        anyhow::bail!("unsupported or invalid OCI digest: {digest}");
    }
    Ok((algorithm, hash))
}

fn verify_digest(expected: &str, data: &[u8]) -> anyhow::Result<()> {
    digest_parts(expected)?;
    let actual = format!("sha256:{}", hex::encode(Sha256::digest(data)));
    if actual != expected {
        anyhow::bail!("digest mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::OciStorage;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;

    /// Build a storage whose upload root cannot host directories, so every
    /// upload-side filesystem call fails for a reason an operator has to fix.
    fn storage_with_unusable_upload_root(root: &std::path::Path) -> OciStorage {
        let upload_root = root.join("uploads");
        std::fs::write(&upload_root, b"not a directory").unwrap();
        let backend = Arc::new(crate::blob_storage::LocalBlobStorage::new(root));
        OciStorage::from_backend(backend, upload_root)
    }

    /// `docker push` against an unwritable registry directory used to answer
    /// with the errno alone — the staging path is built from `repo_root` and a
    /// generated UUID, so nothing else in the exchange could name it.
    #[tokio::test]
    async fn create_upload_names_the_directory_and_the_setting_behind_it() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with_unusable_upload_root(directory.path());

        let error = storage
            .create_upload("alice", "demo")
            .await
            .expect_err("upload root is a file");
        let rendered = format!("{error:#}");

        assert!(rendered.contains("uploads"), "{rendered}");
        assert!(rendered.contains("[server].repo_root"), "{rendered}");
    }

    /// An upload nobody has written to yet is genuinely zero bytes; anything
    /// else has to be an error, because the size becomes the `Range` a client
    /// resumes from.
    #[tokio::test]
    async fn upload_size_separates_a_missing_upload_from_an_unreadable_one() {
        let directory = tempfile::tempdir().unwrap();
        let started = OciStorage::new(directory.path());
        assert_eq!(started.upload_size("alice", "demo", "no-such").unwrap(), 0);

        let broken = storage_with_unusable_upload_root(directory.path());
        let error = broken
            .upload_size("alice", "demo", "no-such")
            .expect_err("upload root is a file, not a missing upload");

        assert!(format!("{error:#}").contains("uploads"), "{error:#}");
    }

    #[tokio::test]
    async fn publishes_verified_upload_under_stable_key() {
        let directory = tempfile::tempdir().unwrap();
        let storage = OciStorage::new(directory.path());
        let data = b"oci layer";
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(data)));
        let (upload, _) = storage.create_upload("alice", "demo").await.unwrap();
        storage
            .append_to_upload("alice", "demo", &upload, data)
            .await
            .unwrap();

        let (_, size, key) = storage
            .finalize_upload("alice", "demo", &upload, &digest)
            .await
            .unwrap();
        assert_eq!(size, data.len() as i64);
        assert!(key.starts_with("oci/alice/demo/blobs/sha256/"));
        assert_eq!(
            storage.read_blob("alice", "demo", &digest).await.unwrap(),
            data
        );
    }
}
