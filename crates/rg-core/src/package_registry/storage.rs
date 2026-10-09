//! Package Registry storage layer.
//!
//! Manages file-system storage for package blobs.
//! Directory layout: `{root}/{owner}/{repo}/packages/{type}/{name}/{version}/{filename}`

use crate::blob_storage::{BlobKey, BlobStorage, LocalBlobStorage};
use crate::deletion_recovery;
use crate::package_registry::artifact::PackageArtifact;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The window a streaming digest reads through.
///
/// Same order as the release-asset hasher: large enough that the syscall count
/// is irrelevant next to the copy, small enough that it is a constant rather
/// than a fraction of the artifact.
const DIGEST_BUFFER_BYTES: usize = 128 * 1024;

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

/// Where a stored package file can be read from.
///
/// The default local backend exposes a path, so downloads can hash and stream
/// the file with a fixed-size window. A backend with no local path falls back
/// to the buffered `get` contract it exposes today.
pub enum PackageFileSource {
    LocalFile { path: PathBuf, size: u64 },
    Buffered(Vec<u8>),
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
    ///
    /// A spooled artifact is published with a file-to-file copy and digested by
    /// streaming its own spool, so publishing one costs a fixed buffer rather
    /// than the artifact's size — the whole reason [`PackageArtifact`] carries a
    /// path instead of a `Vec`.
    #[allow(clippy::too_many_arguments)]
    pub async fn store_file(
        &self,
        owner: &str,
        repo: &str,
        package_type: &str,
        name: &str,
        version: &str,
        filename: &str,
        artifact: PackageArtifact,
    ) -> Result<StoredFile> {
        let key = self.file_key(owner, repo, package_type, name, version, filename)?;
        // Digest first: the copy below is what makes the object readable, and
        // hashing an artifact we have already published means a failure here
        // would leave bytes nothing describes. The artifact is owned by the
        // task and returned with its digests, keeping a spool alive without
        // cloning an in-memory upload merely to satisfy the `'static` bound.
        let (artifact, digests) = artifact
            .run_blocking(FileDigests::of_artifact)
            .await
            .map_err(|error| anyhow::anyhow!("package digest task did not complete: {error}"))?;
        let digests = digests?;

        let publication_id = uuid::Uuid::new_v4().simple().to_string();
        deletion_recovery::open_package_file_creation(self.backend.as_ref(), &publication_id, &key)
            .await?;

        let stored = match &artifact {
            PackageArtifact::Bytes(data) => self.backend.put(&key, data).await,
            PackageArtifact::Spooled { path, .. } => self.backend.put_file(&key, path).await,
        };
        let metadata = match stored {
            Ok(metadata) => metadata,
            Err(error) => {
                self.cleanup_uncommitted_blob(&key, &publication_id, "blob write")
                    .await;
                return Err(error.into());
            }
        };

        Ok(StoredFile {
            filename: filename.to_string(),
            size: metadata.size as i64,
            digests,
            storage_path: key.to_string(),
            publication_id,
        })
    }

    /// Close every recovery intent after the transaction owning these blobs committed.
    pub(crate) async fn finish_publication(&self, files: &[StoredFile]) {
        for file in files {
            if let Err(error) =
                deletion_recovery::mark_committed(self.backend.as_ref(), &file.publication_id).await
            {
                tracing::warn!(
                    filename = %file.filename,
                    storage_path = %file.storage_path,
                    error = %format!("{error:#}"),
                    "package file publication committed, but its recovery entry could not be \
                     marked committed"
                );
            }
            deletion_recovery::close(self.backend.as_ref(), &file.publication_id).await;
        }
    }

    /// Remove one uncommitted request-private blob and close its intent only
    /// when the delete proves there is nothing left for startup to recover.
    pub(crate) async fn discard_uncommitted_file(&self, file: &StoredFile) -> Result<()> {
        self.delete_file(&file.storage_path).await?;
        deletion_recovery::close(self.backend.as_ref(), &file.publication_id).await;
        Ok(())
    }

    async fn cleanup_uncommitted_blob(
        &self,
        key: &BlobKey,
        publication_id: &str,
        failed_step: &'static str,
    ) {
        match self.backend.delete(key).await {
            Ok(_) => deletion_recovery::close(self.backend.as_ref(), publication_id).await,
            Err(cleanup_error) => tracing::warn!(
                blob_key = %key,
                failed_step,
                error = %cleanup_error,
                "package file publication failed and cleanup could not prove the blob absent; \
                 the recovery entry remains for startup"
            ),
        }
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

    /// Locate a file without pulling a local blob through heap.
    ///
    /// `metadata` is deliberately read through the backend before returning
    /// its path: the local implementation performs the same canonical-path and
    /// file-kind checks as `get`, while decorators can withdraw `local_path`
    /// and force their faulted `get` branch.
    pub async fn resolve_file_source(&self, storage_path: &str) -> Result<PackageFileSource> {
        match BlobKey::new(storage_path) {
            Ok(key) => {
                if let Some(path) = self.backend.local_path(&key) {
                    let metadata = self.backend.metadata(&key).await?;
                    Ok(PackageFileSource::LocalFile {
                        path,
                        size: metadata.size,
                    })
                } else {
                    Ok(PackageFileSource::Buffered(self.backend.get(&key).await?))
                }
            }
            Err(_) => {
                let path = PathBuf::from(storage_path);
                let metadata = tokio::fs::metadata(&path)
                    .await
                    .map_err(|error| legacy_path_error("package file", storage_path, &error))?;
                Ok(PackageFileSource::LocalFile {
                    path,
                    size: metadata.len(),
                })
            }
        }
    }

    /// Stream a file from storage (returns the file path for serving).
    pub fn file_path(&self, storage_path: &str) -> Option<PathBuf> {
        match BlobKey::new(storage_path) {
            Ok(key) => self.backend.local_path(&key),
            Err(_) => Some(PathBuf::from(storage_path)),
        }
    }

    /// Move a version's whole object prefix out of the live namespace.
    ///
    /// Deleting the objects one by one is not a deletion boundary: a backend
    /// failure halfway through leaves a still-published version whose files are
    /// partly destroyed, and a metadata failure after the loop finished leaves
    /// the full metadata on bytes that no longer exist. Neither is reversible.
    /// One atomic prefix move is, and a backend that cannot provide one refuses
    /// here — before the live version has changed at all.
    ///
    /// `legacy_paths` are the absolute paths of rows written before the
    /// blob-storage migration; they sit outside the prefix, so they are renamed
    /// beside themselves and travel with the same tombstone.
    #[allow(clippy::too_many_arguments)]
    pub async fn stage_version_deletion(
        &self,
        owner: &str,
        repo: &str,
        package_type: &str,
        name: &str,
        version: &str,
        legacy_paths: &[String],
        deletion_id: &str,
    ) -> Result<StagedPackageVersion> {
        let live = self.version_key(owner, repo, package_type, name, version)?;
        let staged = BlobKey::from_segments([
            "_deleted",
            "package-deletions",
            owner,
            repo,
            package_type,
            name,
            version,
            deletion_id,
        ])?;

        // Every name this deletion is about to move is derivable before it
        // moves any of them, so the journal entry can be complete rather than
        // grown as the moves succeed. A planned pair that turns out to have
        // nothing behind it costs a recovery pass one `Ok(false)`.
        let legacy: Vec<StagedLegacyPackageFile> = legacy_paths
            .iter()
            .filter_map(|path| {
                let live = PathBuf::from(path);
                let file_name = live.file_name()?.to_string_lossy().into_owned();
                let staged = live.with_file_name(format!("{file_name}.deleted-{deletion_id}"));
                Some(StagedLegacyPackageFile { live, staged })
            })
            .collect();

        let mut journal = vec![deletion_recovery::StagedBytes::blob_prefix(&live, &staged)];
        for file in &legacy {
            journal.push(deletion_recovery::StagedBytes::path(
                &file.live,
                &file.staged,
            )?);
        }
        deletion_recovery::open(
            self.backend.as_ref(),
            deletion_id,
            "package version",
            journal,
        )
        .await?;

        let mut staging = StagedPackageVersion {
            live: live.clone(),
            staged: staged.clone(),
            deletion_id: deletion_id.to_string(),
            moved: false,
            legacy: Vec::new(),
        };

        match self.backend.move_prefix(&live, &staged).await {
            Ok(true) => staging.moved = true,
            // A version whose objects are already gone still owns its metadata,
            // and removing that is the end state this call is asked for.
            Ok(false) => {}
            Err(error) => {
                deletion_recovery::close(self.backend.as_ref(), deletion_id).await;
                return Err(Error::new(error).context(format!(
                    "failed to stage package version prefix {live} at {staged}"
                )));
            }
        }

        for file in legacy {
            match tokio::fs::rename(&file.live, &file.staged).await {
                Ok(()) => staging.legacy.push(file),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    let failure =
                        legacy_path_error("package file", &file.live.to_string_lossy(), &error);
                    let staged_path = file.staged.clone();
                    staging.restore(self).await;
                    return Err(failure.context(format!(
                        "failed to stage legacy package file at {}",
                        staged_path.display()
                    )));
                }
            }
        }

        Ok(staging)
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

    /// Whether the version's live prefix currently holds any object.
    ///
    /// Test-only, and deliberately so: it had no production caller in the whole
    /// tree while being `pub` and returning a bare `bool`, which is the shape
    /// that made it dangerous rather than merely unused (card_cc376dd3605e). An
    /// unusable key and an unreachable backend both collapsed into `false` —
    /// "this version has no files" — with nothing logged, so whoever eventually
    /// wired it to a real decision ("show this version?", "is it safe to
    /// delete?") would have inherited a storage outage answering as an empty
    /// version.
    ///
    /// It answers `Result` now, so every caller — the staging assertions below
    /// and the publish-rollback ones in `rg-http`'s `fault_injection_tests` —
    /// fails loudly on a backend error instead of passing because the failure
    /// looked like the absence it was checking for. The collapse is what must
    /// not come back if a production consumer ever appears.
    pub async fn has_files(
        &self,
        owner: &str,
        repo: &str,
        package_type: &str,
        name: &str,
        version: &str,
    ) -> Result<bool> {
        let prefix = self.version_key(owner, repo, package_type, name, version)?;
        Ok(!self.backend.list(Some(&prefix)).await?.is_empty())
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

/// A pre-migration package file, renamed beside itself.
#[derive(Debug)]
struct StagedLegacyPackageFile {
    live: PathBuf,
    staged: PathBuf,
}

/// A package version's objects, parked under a private prefix.
///
/// Produced by [`PackageStorage::stage_version_deletion`] and consumed exactly
/// once: [`restore`](Self::restore) puts the version back when the metadata
/// delete fails, [`retire`](Self::retire) destroys the tombstone after it
/// commits. Everything the version owns — the portable prefix and any legacy
/// absolute paths — travels together, so a rollback restores a whole version
/// rather than the part that happened to be reached first.
#[derive(Debug)]
pub struct StagedPackageVersion {
    live: BlobKey,
    staged: BlobKey,
    /// The id the journal entry of this deletion is filed under, so whichever
    /// of [`restore`](Self::restore) / [`retire`](Self::retire) runs can close
    /// it — and so a run that reaches neither leaves it open for the startup
    /// pass to finish.
    deletion_id: String,
    moved: bool,
    legacy: Vec<StagedLegacyPackageFile>,
}

impl StagedPackageVersion {
    /// Put every staged representation back where the surviving metadata
    /// expects it.
    ///
    /// Compensation on an error path: the caller must still see the original
    /// failure, so a failed restore can only be reported — and it has to be,
    /// because the outcome it leaves is a published version pointing at bytes
    /// parked under a name nothing else records.
    pub async fn restore(&self, storage: &PackageStorage) {
        self.restore_representations(storage).await;
        self.close_journal(storage).await;
    }

    async fn restore_representations(&self, storage: &PackageStorage) {
        for file in self.legacy.iter().rev() {
            if let Err(error) = tokio::fs::rename(&file.staged, &file.live).await {
                tracing::warn!(
                    staged_at = %file.staged.display(),
                    belongs_at = %file.live.display(),
                    %error,
                    "failed to restore a legacy package file after deletion aborted — the surviving version now points at missing bytes until the file is moved back by hand"
                );
            }
        }
        if !self.moved {
            return;
        }
        match storage.backend.move_prefix(&self.staged, &self.live).await {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                staged_prefix = %self.staged,
                live_prefix = %self.live,
                "failed to restore a package version prefix after deletion aborted — the staged prefix disappeared and the surviving version now points at missing bytes"
            ),
            Err(error) => tracing::warn!(
                staged_prefix = %self.staged,
                live_prefix = %self.live,
                %error,
                "failed to restore a package version prefix after deletion aborted — the surviving version cannot reach it until the prefix is moved back by hand"
            ),
        }
    }

    /// Forget the journal entry this deletion opened.
    ///
    /// Split out so both endings close it: the compensation above and the
    /// retirement below are the only two ways a staged version stops being
    /// staged, and an entry that outlives either would have a later startup
    /// pass reach for bytes that are no longer anywhere.
    async fn close_journal(&self, storage: &PackageStorage) {
        deletion_recovery::close(storage.backend.as_ref(), &self.deletion_id).await;
    }

    /// Destroy the tombstone, once the metadata is gone.
    ///
    /// There is nothing left to roll back at this point — the live prefix is
    /// already free — so a failure here is cleanup debt rather than a lost
    /// deletion. It is still returned: reporting `204` while a version's bytes
    /// remain parked under a private prefix is the silent half of the failure.
    pub async fn retire(self, storage: &PackageStorage) -> Result<()> {
        let mut cleanup_error = None;

        // The marker goes down before the first unlink, because it is the only
        // thing that tells a later startup pass which side of the commit this
        // tombstone is on. A marker that cannot be written is reported, but the
        // retirement still runs: bytes destroyed with the entry still open cost
        // that pass one no-op restore, while bytes kept would be restored into
        // a live namespace whose metadata is already gone.
        if let Err(error) =
            deletion_recovery::mark_committed(storage.backend.as_ref(), &self.deletion_id).await
        {
            tracing::warn!(
                deletion_id = self.deletion_id,
                error = %format!("{error:#}"),
                "package version metadata is deleted, but the deletion could not be marked committed"
            );
            cleanup_error = Some(error);
        }

        for file in self.legacy {
            match tokio::fs::remove_file(&file.staged).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(
                        staged_at = %file.staged.display(),
                        %error,
                        "package version metadata is deleted, but a staged legacy file remains and must be removed by hand"
                    );
                    if cleanup_error.is_none() {
                        cleanup_error = Some(
                            legacy_path_error(
                                "staged package file",
                                &file.staged.to_string_lossy(),
                                &error,
                            )
                            .context("failed to retire a deleted legacy package file"),
                        );
                    }
                }
            }
        }

        if self.moved {
            if let Err(error) = storage.backend.delete_prefix(&self.staged).await {
                tracing::warn!(
                    staged_prefix = %self.staged,
                    live_prefix = %self.live,
                    %error,
                    "package version metadata is deleted and its live prefix is free, but the staged objects remain and must be removed by hand"
                );
                if cleanup_error.is_none() {
                    cleanup_error = Some(Error::new(error).context(format!(
                        "failed to retire a staged package version prefix at {}",
                        self.staged
                    )));
                }
            }
        }

        // Closed only when nothing is left staged. The marker is already
        // down, so an entry that survives a failed removal is what lets the
        // next startup pass destroy the tombstone rather than leaving it to an
        // operator who has to be told about it first.
        if cleanup_error.is_none() {
            deletion_recovery::close(storage.backend.as_ref(), &self.deletion_id).await;
        }

        cleanup_error.map_or(Ok(()), Err)
    }
}

#[derive(Debug, Clone)]
pub struct StoredFile {
    pub filename: String,
    pub size: i64,
    pub digests: FileDigests,
    pub storage_path: String,
    pub(crate) publication_id: String,
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

    /// The same three digests, computed in one pass over an artifact that may
    /// be far too large to hold.
    ///
    /// One pass rather than three: reading the spool once and feeding every
    /// hasher from the same buffer is what keeps the cost a fixed
    /// [`DIGEST_BUFFER_BYTES`] window instead of three walks of the file.
    pub fn of_artifact(artifact: &PackageArtifact) -> Result<Self> {
        let mut reader = artifact.reader()?;
        let mut sha1 = Sha1::new();
        let mut sha256 = Sha256::new();
        let mut sha512 = Sha512::new();
        let mut buffer = vec![0_u8; DIGEST_BUFFER_BYTES];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            sha1.update(&buffer[..read]);
            sha256.update(&buffer[..read]);
            sha512.update(&buffer[..read]);
        }
        Ok(Self {
            sha1: hex::encode(sha1.finalize()),
            sha256: hex::encode(sha256.finalize()),
            sha512: hex::encode(sha512.finalize()),
        })
    }
}

/// Error type for storage operations.
pub type Error = anyhow::Error;
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::{
        FileDigests, PackageArtifact, PackageFileSource, PackageStorage, DIGEST_BUFFER_BYTES,
    };
    use crate::blob_storage::{BlobKey, BlobMetadata, BlobStorage, LocalBlobStorage};
    use crate::storage_quota::StorageLimits;
    use futures::future::BoxFuture;
    use sea_orm::ActiveValue::Set;
    use std::path::Path as StdPath;
    use std::sync::Mutex;

    struct RecordingStorage {
        inner: LocalBlobStorage,
        operations: Mutex<Vec<String>>,
    }

    impl RecordingStorage {
        fn new(root: &StdPath) -> Self {
            Self {
                inner: LocalBlobStorage::new(root),
                operations: Mutex::new(Vec::new()),
            }
        }

        fn record(&self, operation: &str, key: &BlobKey) {
            self.operations
                .lock()
                .unwrap()
                .push(format!("{operation}:{key}"));
        }

        fn take_operations(&self) -> Vec<String> {
            std::mem::take(&mut *self.operations.lock().unwrap())
        }
    }

    impl BlobStorage for RecordingStorage {
        fn backend_name(&self) -> &'static str {
            "recording"
        }

        fn put<'a>(
            &'a self,
            key: &'a BlobKey,
            data: &'a [u8],
        ) -> BoxFuture<'a, crate::blob_storage::Result<BlobMetadata>> {
            self.record("put", key);
            self.inner.put(key, data)
        }

        fn put_file<'a>(
            &'a self,
            key: &'a BlobKey,
            source: &'a StdPath,
        ) -> BoxFuture<'a, crate::blob_storage::Result<BlobMetadata>> {
            self.record("put_file", key);
            self.inner.put_file(key, source)
        }

        fn get<'a>(
            &'a self,
            key: &'a BlobKey,
        ) -> BoxFuture<'a, crate::blob_storage::Result<Vec<u8>>> {
            self.inner.get(key)
        }

        fn metadata<'a>(
            &'a self,
            key: &'a BlobKey,
        ) -> BoxFuture<'a, crate::blob_storage::Result<BlobMetadata>> {
            self.inner.metadata(key)
        }

        fn exists<'a>(
            &'a self,
            key: &'a BlobKey,
        ) -> BoxFuture<'a, crate::blob_storage::Result<bool>> {
            self.inner.exists(key)
        }

        fn delete<'a>(
            &'a self,
            key: &'a BlobKey,
        ) -> BoxFuture<'a, crate::blob_storage::Result<bool>> {
            self.record("delete", key);
            self.inner.delete(key)
        }

        fn list<'a>(
            &'a self,
            prefix: Option<&'a BlobKey>,
        ) -> BoxFuture<'a, crate::blob_storage::Result<Vec<BlobMetadata>>> {
            self.inner.list(prefix)
        }
    }

    async fn setup_publish_db() -> (rg_db::DatabaseConnection, i64) {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "package-boundary",
            "package-boundary@example.invalid",
            "",
            "Package Boundary",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                owner_id: Set(user.id),
                name: Set("publication".to_string()),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                stars_count: Set(0),
                forks_count: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        (db, user.id)
    }

    fn publish_info(
        author_id: i64,
        files: Vec<(String, PackageArtifact)>,
    ) -> crate::package_registry::PublishInfo {
        crate::package_registry::PublishInfo {
            owner: "package-boundary".to_string(),
            repo: "publication".to_string(),
            package_type: "generic".to_string(),
            name: "widget".to_string(),
            version: "1.0.0".to_string(),
            semver: None,
            metadata: None,
            description: None,
            homepage: None,
            repository_url: None,
            npm_dist_tag: None,
            author_id,
            files,
        }
    }

    fn assert_multi_file_publication_boundary(operations: &[String]) {
        let journals: Vec<_> = operations
            .iter()
            .enumerate()
            .filter(|(_, event)| event.starts_with("put:_deleted/journal/"))
            .map(|(index, _)| index)
            .collect();
        let blobs: Vec<_> = operations
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                event.starts_with("put:packages/") || event.starts_with("put_file:packages/")
            })
            .map(|(index, _)| index)
            .collect();
        let committed: Vec<_> = operations
            .iter()
            .enumerate()
            .filter(|(_, event)| event.starts_with("put:_deleted/committed/"))
            .map(|(index, _)| index)
            .collect();
        let closes: Vec<_> = operations
            .iter()
            .enumerate()
            .filter(|(_, event)| event.starts_with("delete:_deleted/journal/"))
            .map(|(index, _)| index)
            .collect();

        assert_eq!(journals.len(), 2, "one intent per blob: {operations:?}");
        assert_eq!(blobs.len(), 2, "both blobs must be visible: {operations:?}");
        assert_eq!(
            committed.len(),
            2,
            "one commit marker per blob: {operations:?}"
        );
        assert_eq!(closes.len(), 2, "every intent must close: {operations:?}");
        assert!(journals[0] < blobs[0] && journals[1] < blobs[1]);
        assert!(
            blobs[1] < committed[0] && blobs[1] < closes[0],
            "a multi-file publication closed an intent before all transaction-owned blobs were written: {operations:?}"
        );
    }

    fn two_files() -> Vec<(String, PackageArtifact)> {
        vec![
            (
                "first.bin".to_string(),
                PackageArtifact::from_bytes(b"first".to_vec()),
            ),
            (
                "second.bin".to_string(),
                PackageArtifact::from_bytes(b"second".to_vec()),
            ),
        ]
    }

    #[tokio::test]
    async fn new_and_existing_version_publishes_keep_all_intents_open_until_commit() {
        let root = tempfile::tempdir().unwrap();
        let backend = std::sync::Arc::new(RecordingStorage::new(root.path()));
        let storage = PackageStorage::from_backend(backend.clone());
        let (db, author_id) = setup_publish_db().await;

        let created = crate::package_registry::service::publish(
            &db,
            &storage,
            publish_info(author_id, two_files()),
            &StorageLimits::default(),
        )
        .await
        .unwrap();
        assert!(!created.existing);
        assert_multi_file_publication_boundary(&backend.take_operations());

        let added = crate::package_registry::service::publish(
            &db,
            &storage,
            publish_info(
                author_id,
                vec![
                    (
                        "third.bin".to_string(),
                        PackageArtifact::from_bytes(b"third".to_vec()),
                    ),
                    (
                        "fourth.bin".to_string(),
                        PackageArtifact::from_bytes(b"fourth".to_vec()),
                    ),
                ],
            ),
            &StorageLimits::default(),
        )
        .await
        .unwrap();
        assert!(added.existing);
        assert_multi_file_publication_boundary(&backend.take_operations());
    }

    /// The two variants must be indistinguishable to everything downstream.
    ///
    /// Every protocol publishes one of these digests as the artifact's identity
    /// — npm's `dist.shasum`, cargo's `cksum`, Helm's `digest` — and a client
    /// that hashes what it downloaded refuses a mismatch. A windowed digest
    /// that dropped or double-counted a chunk boundary would therefore not fail
    /// here, it would fail at `npm install`. The payload is deliberately not a
    /// multiple of the read window, so the last short read is exercised.
    #[test]
    fn a_spooled_artifact_digests_exactly_like_the_same_bytes_in_memory() {
        use std::io::Write as _;

        let payload: Vec<u8> = (0..DIGEST_BUFFER_BYTES * 2 + 7)
            .map(|index| (index % 251) as u8)
            .collect();

        let mut spool = tempfile::NamedTempFile::new().expect("create spool");
        spool.write_all(&payload).expect("write spool");
        spool.flush().expect("flush spool");
        let spooled = PackageArtifact::spooled(spool.into_temp_path(), payload.len() as u64);

        let collected = FileDigests::of(payload.as_slice());
        let streamed = FileDigests::of_artifact(&spooled).expect("digest the spool");

        assert_eq!(streamed.sha1, collected.sha1);
        assert_eq!(streamed.sha256, collected.sha256);
        assert_eq!(streamed.sha512, collected.sha512);
    }

    /// And the object the backend ends up holding is the same one either way.
    #[tokio::test]
    async fn a_spooled_artifact_stores_the_same_object_as_an_in_memory_one() {
        use std::io::Write as _;

        let payload = b"the same bytes, two ways in".to_vec();
        let mut spool = tempfile::NamedTempFile::new().expect("create spool");
        spool.write_all(&payload).expect("write spool");
        spool.flush().expect("flush spool");

        let directory = tempfile::tempdir().unwrap();
        let storage = PackageStorage::new(directory.path());

        let store = async |artifact: PackageArtifact| {
            storage
                .store_file(
                    "alice", "demo", "generic", "pkg", "1.0.0", "a.bin", artifact,
                )
                .await
                .expect("store the artifact")
        };

        let from_memory = store(PackageArtifact::from_bytes(payload.clone())).await;
        let from_spool = store(PackageArtifact::spooled(
            spool.into_temp_path(),
            payload.len() as u64,
        ))
        .await;

        assert_eq!(from_spool.size, from_memory.size);
        assert_eq!(from_spool.digests.sha256, from_memory.digests.sha256);
        assert_eq!(
            storage.read_file(&from_spool.storage_path).await.unwrap(),
            payload,
            "the published object must be the artifact's bytes, not a truncated copy"
        );
    }

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
                PackageArtifact::from_bytes(b"package".to_vec()),
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
        assert!(storage
            .has_files("alice", "demo", "npm", "@scope/pkg", "1.0.0")
            .await
            .unwrap());

        let staged = storage
            .stage_version_deletion("alice", "demo", "npm", "@scope/pkg", "1.0.0", &[], "abc123")
            .await
            .unwrap();
        // Staging alone frees the live prefix — that is what makes the metadata
        // delete safe to attempt — but the bytes are still recoverable.
        assert!(!storage
            .has_files("alice", "demo", "npm", "@scope/pkg", "1.0.0")
            .await
            .unwrap());
        staged.restore(&storage).await;
        assert_eq!(
            storage.read_file(&stored.storage_path).await.unwrap(),
            b"package"
        );

        let staged = storage
            .stage_version_deletion("alice", "demo", "npm", "@scope/pkg", "1.0.0", &[], "def456")
            .await
            .unwrap();
        staged.retire(&storage).await.unwrap();
        assert!(!storage
            .has_files("alice", "demo", "npm", "@scope/pkg", "1.0.0")
            .await
            .unwrap());
        assert!(
            !directory
                .path()
                .join("_deleted/package-deletions/alice/demo/npm/%40scope%2Fpkg/1.0.0/def456")
                .exists(),
            "retirement left the private tombstone behind"
        );
    }

    /// A backend that cannot move a namespace atomically has no safe way to
    /// take a version out of the live namespace, so it must refuse *before*
    /// touching it — never fall back to deleting the objects one by one, which
    /// is the unreversible order this staging exists to replace.
    #[tokio::test]
    async fn a_backend_without_atomic_prefix_move_refuses_before_touching_the_version() {
        use crate::blob_storage::{BlobKey, BlobMetadata, BlobStorage, LocalBlobStorage};
        use futures::future::BoxFuture;
        use std::path::Path as StdPath;

        /// Everything `LocalBlobStorage` does, minus the atomic prefix move —
        /// the shape of an object store that only speaks per-key operations.
        struct NoPrefixMove(LocalBlobStorage);

        impl BlobStorage for NoPrefixMove {
            fn backend_name(&self) -> &'static str {
                "no-prefix-move"
            }
            fn put<'a>(
                &'a self,
                key: &'a BlobKey,
                data: &'a [u8],
            ) -> BoxFuture<'a, crate::blob_storage::Result<BlobMetadata>> {
                self.0.put(key, data)
            }
            fn put_file<'a>(
                &'a self,
                key: &'a BlobKey,
                source: &'a StdPath,
            ) -> BoxFuture<'a, crate::blob_storage::Result<BlobMetadata>> {
                self.0.put_file(key, source)
            }
            fn get<'a>(
                &'a self,
                key: &'a BlobKey,
            ) -> BoxFuture<'a, crate::blob_storage::Result<Vec<u8>>> {
                self.0.get(key)
            }
            fn metadata<'a>(
                &'a self,
                key: &'a BlobKey,
            ) -> BoxFuture<'a, crate::blob_storage::Result<BlobMetadata>> {
                self.0.metadata(key)
            }
            fn exists<'a>(
                &'a self,
                key: &'a BlobKey,
            ) -> BoxFuture<'a, crate::blob_storage::Result<bool>> {
                self.0.exists(key)
            }
            fn delete<'a>(
                &'a self,
                key: &'a BlobKey,
            ) -> BoxFuture<'a, crate::blob_storage::Result<bool>> {
                self.0.delete(key)
            }
            fn list<'a>(
                &'a self,
                prefix: Option<&'a BlobKey>,
            ) -> BoxFuture<'a, crate::blob_storage::Result<Vec<BlobMetadata>>> {
                self.0.list(prefix)
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let storage = PackageStorage::from_backend(std::sync::Arc::new(NoPrefixMove(
            LocalBlobStorage::new(directory.path()),
        )));
        let stored = storage
            .store_file(
                "alice",
                "demo",
                "npm",
                "pkg",
                "1.0.0",
                "package.tgz",
                PackageArtifact::from_bytes(b"bytes".to_vec()),
            )
            .await
            .unwrap();

        let refusal = storage
            .stage_version_deletion("alice", "demo", "npm", "pkg", "1.0.0", &[], "abc123")
            .await
            .expect_err("a backend with no atomic prefix move must refuse");
        assert!(
            format!("{refusal:#}").contains("atomic prefix move"),
            "{refusal:#}"
        );
        assert_eq!(
            storage.read_file(&stored.storage_path).await.unwrap(),
            b"bytes",
            "a refused staging destroyed part of the live version"
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

    /// The default backend must hand downloads a path, not pull the object
    /// through `BlobStorage::get`. This is the branch the HTTP handler relies
    /// on to keep package-sized files out of heap.
    #[tokio::test]
    async fn local_package_files_resolve_to_a_streamable_path() {
        let directory = tempfile::tempdir().unwrap();
        let storage = PackageStorage::new(directory.path());
        let stored = storage
            .store_file(
                "alice",
                "demo",
                "generic",
                "pkg",
                "1.0.0",
                "package.bin",
                PackageArtifact::from_bytes(b"package bytes".to_vec()),
            )
            .await
            .unwrap();

        match storage
            .resolve_file_source(&stored.storage_path)
            .await
            .unwrap()
        {
            PackageFileSource::LocalFile { path, size } => {
                assert!(path.starts_with(directory.path()), "{}", path.display());
                assert_eq!(size, b"package bytes".len() as u64);
            }
            PackageFileSource::Buffered(data) => panic!(
                "local package resolved through BlobStorage::get into {} buffered bytes",
                data.len()
            ),
        }
    }
}
