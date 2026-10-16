//! Receiving one LFS object, whichever transport carried it.
//!
//! An object arrives as the body of `PUT .../lfs/objects/{oid}` or as the data
//! of a `git-lfs-transfer` `put-object` over SSH. Both go through the same two
//! steps, so the checks cannot drift between the doors:
//!
//! 1. [`ObjectSpool`] writes the bytes to a file of this upload's own while
//!    counting and hashing them, and refuses the first byte past the ceiling;
//! 2. [`accept_spooled`] checks the digest against the oid, the size against
//!    the one the batch registered, the repository's storage budget, and hands
//!    the file to [`store_object_from_file`] — or retires it.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use sea_orm::DatabaseConnection;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::blob_storage::BlobStorage;
use crate::lfs::service::store_object_from_file;
use crate::platform::fs::{discard_file_async, path_error, LFS_STORAGE_HINT};

/// An upload longer than the ceiling its spool was opened with.
#[derive(Debug, thiserror::Error)]
#[error("LFS upload exceeds the configured {max_bytes}-byte limit")]
pub struct LfsObjectTooLarge {
    pub max_bytes: usize,
}

/// Where one upload of `oid` is spooled: a name of its own under the
/// repository's LFS root, so two uploads of the same object never write — or
/// delete — each other's file.
pub fn spool_path(lfs_root: &Path, oid: &str) -> PathBuf {
    lfs_root.join(crate::staging::lfs_object_spool_name(
        oid,
        uuid::Uuid::new_v4(),
    ))
}

/// An upload being written to its spool, measured and hashed as it goes.
///
/// A spool that fails or is abandoned must be [`discard`](Self::discard)ed:
/// the file is this upload's alone, and nothing else will remove it before
/// the startup sweep.
pub struct ObjectSpool {
    file: tokio::fs::File,
    path: PathBuf,
    written: usize,
    max_bytes: usize,
    hasher: Sha256,
}

/// A complete spool: the file, its length and the SHA-256 of its bytes.
#[derive(Debug)]
pub struct SpooledObject {
    pub path: PathBuf,
    pub written: usize,
    pub sha256: String,
}

impl ObjectSpool {
    /// Open a new spool at `path`, whose directory must exist.
    pub async fn create(path: PathBuf, max_bytes: usize) -> Result<Self> {
        // `create_new`: the name is this upload's own, so finding a file there
        // means something else wrote it, and truncating it is not ours to do.
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .map_err(|error| staging_error(&path, &error))?;
        Ok(Self {
            file,
            path,
            written: 0,
            max_bytes,
            hasher: Sha256::new(),
        })
    }

    /// Append `data`, refusing it whole when it would cross the ceiling.
    pub async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.written = self
            .written
            .checked_add(data.len())
            .filter(|size| *size <= self.max_bytes)
            .ok_or_else(|| {
                anyhow::Error::new(LfsObjectTooLarge {
                    max_bytes: self.max_bytes,
                })
            })?;
        self.file
            .write_all(data)
            .await
            .map_err(|error| staging_error(&self.path, &error))?;
        self.hasher.update(data);
        Ok(())
    }

    /// The bytes written so far.
    pub fn written(&self) -> usize {
        self.written
    }

    /// Close the spool with every byte in the file.
    pub async fn finish(mut self) -> Result<SpooledObject> {
        // `tokio::fs::File` buffers: `write_all` returns once the bytes are
        // queued for the blocking pool, not once they are in the file, and
        // dropping the handle does not wait for that queue either. The spool is
        // handed straight to `store_object_from_file`, which opens it with
        // `std::fs` and compresses whatever is there — so without this the
        // object that reaches storage is the upload minus whatever had not
        // landed yet, filed under a size that says the whole thing arrived
        // (`sol_07eb75f8fb62`).
        if let Err(error) = self.file.flush().await {
            let error = staging_error(&self.path, &error);
            self.discard().await;
            return Err(error);
        }
        drop(self.file);
        Ok(SpooledObject {
            path: self.path,
            written: self.written,
            sha256: hex::encode(self.hasher.finalize()),
        })
    }

    /// Give up on the upload and remove its file.
    pub async fn discard(self) {
        drop(self.file);
        discard_file_async("LFS staging file", &self.path).await;
    }
}

fn staging_error(path: &Path, error: &std::io::Error) -> anyhow::Error {
    path_error("LFS staging file", path, error, LFS_STORAGE_HINT)
}

/// The repository an upload is for, as the store addresses it.
#[derive(Clone, Copy, Debug)]
pub struct UploadTarget<'a> {
    pub repo_id: i64,
    pub owner: &'a str,
    pub repo: &'a str,
    pub oid: &'a str,
}

/// Publish a complete spool as `target.oid`, or refuse it and remove it.
///
/// Refusals are [`crate::error::InvalidRequest`] (digest or size disagree with
/// what the client said) or the storage budget's own error; anything else is
/// the server's. The spool is gone afterwards either way: published, or
/// discarded.
pub async fn accept_spooled(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    limits: &crate::storage_quota::StorageLimits,
    target: UploadTarget<'_>,
    spooled: SpooledObject,
) -> Result<()> {
    let UploadTarget {
        repo_id,
        owner,
        repo,
        oid,
    } = target;
    let spool = &spooled.path;
    let refuse = |error: anyhow::Error| async move {
        discard_file_async("LFS staging file", spool).await;
        Err::<(), _>(error)
    };

    // An LFS object id is a SHA-256 of its uncompressed bytes, not just a
    // well-formed name chosen by the client. The digest was taken while the
    // spool was written, before the file can become visible under the
    // object's stable key.
    if spooled.sha256 != oid {
        return refuse(crate::error::invalid_request(format!(
            "LFS object content digest does not match oid: expected {oid}, got {}",
            spooled.sha256
        )))
        .await;
    }

    // A batch registers the size the client promised before it lets the upload
    // through. A short or long body must not become a live object whose row
    // says something else. An upload with no batch before it has no row and
    // records the size that actually arrived.
    let already_stored = match rg_db::ops::lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid)
        .await
        .context("db: read the LFS object row an upload completes")
    {
        Ok(Some(object)) if object.size != spooled.written as i64 => {
            return refuse(crate::error::invalid_request(format!(
                "LFS object size does not match batch declaration: expected {} bytes, got {}",
                object.size, spooled.written
            )))
            .await;
        }
        Ok(object) => object.is_some_and(|object| object.uploaded),
        Err(error) => return refuse(error).await,
    };

    // The batch checked the budget before it let the upload through; two
    // uploads that both passed a batch meet here, with the bytes already
    // spooled, so this can only refuse. An object already stored adds
    // nothing — a re-upload at the limit is not charged twice.
    let incoming = if already_stored {
        0
    } else {
        spooled.written as u64
    };
    if let Err(error) = crate::storage_quota::check_room(db, repo_id, incoming, limits).await {
        return refuse(error.into_service_error()).await;
    }

    // Ownership of the spool passes to the store, which retires it.
    store_object_from_file(
        db,
        repo_id,
        storage,
        owner,
        repo,
        oid,
        &spooled.path,
        spooled.written as i64,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_spool_refuses_the_byte_past_its_ceiling_and_discards_cleanly() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("spool");
        let mut spool = ObjectSpool::create(path.clone(), 4).await.unwrap();
        spool.write(b"123").await.unwrap();
        let error = spool.write(b"45").await.unwrap_err();
        assert!(error.downcast_ref::<LfsObjectTooLarge>().is_some());
        spool.discard().await;
        assert!(!path.exists(), "a refused spool was left behind");
    }

    #[tokio::test]
    async fn a_finished_spool_carries_its_length_and_digest() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("spool");
        let mut spool = ObjectSpool::create(path.clone(), 1024).await.unwrap();
        spool.write(b"hello ").await.unwrap();
        spool.write(b"world").await.unwrap();
        let spooled = spool.finish().await.unwrap();
        assert_eq!(spooled.written, 11);
        assert_eq!(
            spooled.sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
    }
}
