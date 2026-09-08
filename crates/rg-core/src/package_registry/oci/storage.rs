//! OCI content-addressed storage backed by the shared [`BlobStorage`] contract.
//!
//! Completed blobs and manifests use durable backend-neutral keys. Chunked
//! uploads remain local temporary files until their digest has been verified,
//! then `put_file` atomically publishes them to the configured backend.

use crate::blob_storage::{BlobKey, BlobStorage};
use crate::platform::fs::discard_dir_async;
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

/// Where the registry's own directories live, for an error that names one.
///
/// Retiring a repository touches both the upload staging tree and — on an
/// instance still carrying the pre-[`BlobStorage`] layout — the legacy
/// per-repository directory. Both hang off the one root the operator can
/// actually set, so the hint names that root and nothing else.
const REGISTRY_DIR_HINT: &str =
    "the OCI registry's directories live under the `[server].repo_root` directory; that \
     directory must be writable by the user running forgekeep";

/// A blob that has reached its content-addressed key, and how it got there.
///
/// `published` is the part the caller cannot work out for itself, and it decides
/// whether a failure further along may roll the bytes back. Finalizing and
/// cross-repository copying are deduplicating: a key that already holds these bytes is accepted as-is and
/// nothing is written. Rolling *that* back would delete the object an earlier,
/// successful push already recorded a row for — turning a leaked blob into an
/// unpullable image. Only the caller that actually published may compensate.
#[derive(Debug, Clone)]
pub struct FinalizedBlob {
    pub digest: String,
    pub size: i64,
    /// The backend key the bytes live under, as a string.
    pub storage_path: String,
    /// Whether this call wrote the bytes, as opposed to finding them already there.
    pub published: bool,
}

/// A manifest stored at its content-addressed key, and whether this request
/// created the object.
///
/// The distinction is what makes compensation safe. A failed database write
/// may delete bytes this request published, but must not delete a manifest an
/// earlier successful push already recorded.
#[derive(Debug, Clone)]
pub struct StoredManifest {
    /// The backend key the bytes live under, as a string.
    pub storage_path: String,
    /// Whether this call wrote the bytes, as opposed to finding them already there.
    pub published: bool,
}

/// Everything one repository owns in the registry, moved out of the live
/// namespace but not yet removed.
///
/// The registry's storage and the database cannot share a transaction, so a
/// repository deletion renames the registry's data aside first and discards it
/// only once the metadata row is gone. Until that point every part is
/// reversible — see [`OciStorage::restore_repository`].
///
/// A part is absent when there was nothing to move, which is the ordinary case:
/// most repositories never receive a `docker push`, and the legacy layout only
/// exists on instances that predate [`BlobStorage`].
#[derive(Debug, Default)]
pub struct StagedOciRepository {
    /// Content-addressed blobs and manifests: `(live, staged)` backend keys.
    blobs: Option<(BlobKey, BlobKey)>,
    /// The chunked-upload tree: `(live, staged)` filesystem paths.
    uploads: Option<(PathBuf, PathBuf)>,
    /// The pre-[`BlobStorage`] on-disk layout: `(live, staged)` paths.
    legacy: Option<(PathBuf, PathBuf)>,
    /// The directory the two filesystem parts were staged under.
    ///
    /// Kept so that retiring the tombstone leaves nothing behind: removing the
    /// staged leaves alone would accumulate one empty deletion-id directory per
    /// repository that ever had an upload in flight.
    directories: Option<PathBuf>,
}

/// Everything one repository owns in the registry, moved to another live
/// namespace while its ForgeKeep repository row is being transferred.
///
/// The database and both registry backends cannot share a transaction.  Keep
/// each successful move so the caller can put it back if a later storage move
/// or the database update fails.
#[derive(Debug, Default)]
pub struct TransferredOciRepository {
    /// Content-addressed blobs and manifests: `(source, destination)` backend keys.
    blobs: Option<(BlobKey, BlobKey)>,
    /// The chunked-upload tree: `(source, destination)` filesystem paths.
    uploads: Option<(PathBuf, PathBuf)>,
    /// The pre-[`BlobStorage`] on-disk layout: `(source, destination)` paths.
    legacy: Option<(PathBuf, PathBuf)>,
}

/// Where every registry namespace of one repository goes when its repository is
/// transferred, in the order it moves.
struct OciRepositoryTransferPlan {
    /// `(source, destination)` backend keys for the content-addressed tree.
    blobs: (BlobKey, BlobKey),
    /// `(source, destination)` paths for the chunked-upload tree.
    uploads: (PathBuf, PathBuf),
    /// `(source, destination)` paths for the pre-[`BlobStorage`] on-disk
    /// layout, on an instance still carrying one.
    legacy: Option<(PathBuf, PathBuf)>,
}

/// One actionable error for a filesystem failure on an OCI upload path.
fn upload_path_error(what: &str, path: &Path, error: &std::io::Error) -> anyhow::Error {
    crate::platform::fs::path_error(what, path, error, UPLOAD_DIR_HINT)
}

/// One actionable error for a filesystem failure on a registry directory.
fn registry_path_error(what: &str, path: &Path, error: &std::io::Error) -> anyhow::Error {
    crate::platform::fs::path_error(what, path, error, REGISTRY_DIR_HINT)
}

/// Where every registry namespace of one repository goes when it is deleted.
///
/// Produced once per deletion and consumed twice — by the journal entry that
/// declares the move, and by the moves themselves.
struct OciRepositoryDeletionPlan {
    /// `(live, aside)` blob keys for the content-addressed tree.
    blobs: (BlobKey, BlobKey),
    /// `(live, aside)` paths for the chunked-upload tree.
    uploads: (PathBuf, PathBuf),
    /// `(live, aside)` paths for the pre-[`BlobStorage`] on-disk layout, on an
    /// instance still carrying one.
    legacy: Option<(PathBuf, PathBuf)>,
    /// The directory both filesystem halves are parked under.
    staging_root: PathBuf,
}

/// Rename `live` aside, answering `false` when there was nothing there.
///
/// The parent of `aside` is created first: the staging tree is keyed by a
/// deletion id that has never existed before, so nothing else can have made it.
async fn stage_directory(what: &str, live: &Path, aside: &Path) -> anyhow::Result<bool> {
    match tokio::fs::try_exists(live).await {
        Ok(true) => {}
        Ok(false) => return Ok(false),
        Err(error) => return Err(registry_path_error(what, live, &error)),
    }
    if let Some(parent) = aside.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| registry_path_error(what, parent, &error))?;
    }
    tokio::fs::rename(live, aside)
        .await
        .map_err(|error| registry_path_error(what, live, &error))?;
    Ok(true)
}

/// Move one optional registry directory to another live namespace.
///
/// Unlike deletion staging, the destination is not a fresh UUID tree.  Refuse
/// to replace anything already there: a database row saying the namespace is
/// free is not permission to merge a prior orphan into the transferred
/// repository.
async fn move_directory(what: &str, source: &Path, destination: &Path) -> anyhow::Result<bool> {
    match tokio::fs::try_exists(source).await {
        Ok(true) => {}
        Ok(false) => return Ok(false),
        Err(error) => return Err(registry_path_error(what, source, &error)),
    }
    match tokio::fs::try_exists(destination).await {
        Ok(false) => {}
        Ok(true) => {
            return Err(registry_path_error(
                what,
                destination,
                &std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "destination directory already exists",
                ),
            ));
        }
        Err(error) => return Err(registry_path_error(what, destination, &error)),
    }
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| registry_path_error(what, parent, &error))?;
    }
    tokio::fs::rename(source, destination)
        .await
        .map_err(|error| registry_path_error(what, source, &error))?;
    Ok(true)
}

/// The bytes the client sent do not hash to the digest it named.
///
/// Finalizing an upload fails for three unrelated reasons — this one, a staging
/// file the registry cannot read, and a blob store that refuses the publish —
/// and only this one is the client's fault. Flattened into an `anyhow::Error`
/// all three look alike, so the HTTP layer had nothing to classify on and
/// answered `400 DIGEST_INVALID` to every one of them. The distinction has to
/// travel inside the error, which is what this type is for.
///
/// The size travels with it because the two are read together and only one of
/// them is in the message otherwise. `digest mismatch: expected X, got Y` is
/// true of a layer that arrived corrupt, of one that arrived twice, and of a
/// staging file that is no longer there at all — three different problems, one
/// of which is not the client's. The byte count is what tells them apart, and
/// it is the only part of the reply a `docker push` operator can compare
/// against the layer they pushed.
#[derive(Debug, thiserror::Error)]
#[error("digest mismatch: expected {expected}, got {actual} over {staged_bytes} staged byte(s)")]
pub struct DigestMismatch {
    pub expected: String,
    pub actual: String,
    /// How many bytes the staging file held when it was hashed.
    pub staged_bytes: i64,
}

/// The digest string itself is not a well-formed `sha256:<64 hex digits>`.
///
/// Also the client's fault, and also `400 DIGEST_INVALID` — see
/// [`DigestMismatch`] for why it needs a type rather than a message.
#[derive(Debug, thiserror::Error)]
#[error("unsupported or invalid OCI digest: {digest}")]
pub struct InvalidDigest {
    pub digest: String,
}

/// Whether a failed blob operation is the client's fault (`400`) or the
/// registry's (`500`).
///
/// `downcast_ref` sees through the `.context()` layers a caller may have added,
/// the same way [`crate::blob_storage`] errors are classified elsewhere. The
/// default is deliberately "ours": `docker push` does not retry a `4xx`, it
/// prints `digest invalid` and stops — so mislabelling a broken `repo_root` as
/// a corrupt layer sends the operator to inspect an image that was never the
/// problem.
pub fn is_client_digest_fault(error: &anyhow::Error) -> bool {
    error.downcast_ref::<DigestMismatch>().is_some()
        || error.downcast_ref::<InvalidDigest>().is_some()
}

#[derive(Clone)]
pub struct OciStorage {
    backend: Arc<dyn BlobStorage>,
    upload_root: PathBuf,
    legacy_root: Option<PathBuf>,
}

impl OciStorage {
    /// Build a registry over `backend`, staging chunked uploads under
    /// `upload_root`.
    ///
    /// `legacy_root` is the directory an instance carrying the
    /// pre-[`BlobStorage`] layout keeps `<owner>/<repo>/oci/` under — the
    /// server's `[server].repo_root`, the same root `<owner>/<repo>.releases`
    /// and `<owner>.lfs/` hang off. It is a read-side fallback plus the
    /// deletion sweep; `None` declares that this registry has no predecessor
    /// layout to reach for.
    pub fn from_backend(
        backend: Arc<dyn BlobStorage>,
        upload_root: impl Into<PathBuf>,
        legacy_root: Option<PathBuf>,
    ) -> Self {
        Self {
            backend,
            upload_root: upload_root.into(),
            legacy_root,
        }
    }

    fn blob_key(&self, owner: &str, repo: &str, digest: &str) -> anyhow::Result<BlobKey> {
        let (algorithm, hash) = digest_parts(digest)?;
        BlobKey::from_segments(["oci", owner, repo, "blobs", algorithm, &hash[..2], hash])
            .map_err(Into::into)
    }

    /// The backend key a blob of `digest` lives under, as stored in
    /// `oci_blobs.storage_path`.
    ///
    /// Published so a caller can name the key *before* publishing into it —
    /// which is what taking a publication lease requires. See
    /// [`super::publication`].
    pub fn blob_storage_key(
        &self,
        owner: &str,
        repo: &str,
        digest: &str,
    ) -> anyhow::Result<String> {
        Ok(self.blob_key(owner, repo, digest)?.to_string())
    }

    /// The backend key a manifest of `digest` lives under.
    pub fn manifest_storage_key(
        &self,
        owner: &str,
        repo: &str,
        digest: &str,
    ) -> anyhow::Result<String> {
        Ok(self.manifest_key(owner, repo, digest)?.to_string())
    }

    /// Remove an object this request published but could not record.
    ///
    /// The delete goes through the registry's own backend rather than whatever
    /// other handle the caller happens to hold. The registry is constructed
    /// with a backend of its own, and a compensation aimed at a different one
    /// deletes nothing while reporting that it cleaned up — the layer leaks
    /// and nobody hears about it.
    ///
    /// Callers must satisfy themselves that the bytes are theirs to take back;
    /// this only performs the delete.
    pub async fn discard_published_object(&self, storage_path: &str) -> anyhow::Result<()> {
        let key = BlobKey::new(storage_path)?;
        self.backend.delete(&key).await?;
        Ok(())
    }

    fn manifest_key(&self, owner: &str, repo: &str, digest: &str) -> anyhow::Result<BlobKey> {
        let (algorithm, hash) = digest_parts(digest)?;
        BlobKey::from_segments(["oci", owner, repo, "manifests", algorithm, hash])
            .map_err(Into::into)
    }

    /// Turn logical segments into a path under [`Self::upload_root`].
    ///
    /// Every segment goes through [`BlobKey::from_segments`] first, so a
    /// client-supplied upload UUID cannot contribute a separator or a
    /// traversal component to the path that is about to be opened.
    fn upload_path<'a>(&self, segments: impl IntoIterator<Item = &'a str>) -> PathBuf {
        let key = BlobKey::from_segments(segments)
            .expect("validated OCI namespace and generated upload UUID");
        key.as_str()
            .split('/')
            .fold(self.upload_root.clone(), |path, segment| path.join(segment))
    }

    fn upload_dir(&self, owner: &str, repo: &str, uuid: &str) -> PathBuf {
        self.upload_path(["oci-uploads", owner, repo, uuid])
    }

    /// The directory holding every unfinished upload of `owner/repo`.
    ///
    /// One level above [`Self::upload_dir`]. A repository being deleted has to
    /// retire the chunks of pushes still in flight, and those are named by
    /// UUIDs that live in `oci_upload` rows the deletion never reads — so the
    /// only way to reach them all is the directory that contains them.
    fn repository_upload_dir(&self, owner: &str, repo: &str) -> PathBuf {
        self.upload_path(["oci-uploads", owner, repo])
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

    /// The prefix every completed blob and manifest of `owner/repo` lives
    /// under, in whichever backend this registry was built on.
    fn repository_blob_prefix(owner: &str, repo: &str) -> anyhow::Result<BlobKey> {
        BlobKey::from_segments(["oci", owner, repo]).map_err(Into::into)
    }

    /// The legacy per-repository directory, on an instance still carrying one.
    fn legacy_repository_dir(&self, owner: &str, repo: &str) -> Option<PathBuf> {
        let root = self.legacy_root.as_ref()?;
        Some(root.join(owner).join(repo).join("oci"))
    }

    /// Move every registry object that belongs to `source_owner/source_repo`
    /// into `destination_owner/destination_repo`.
    ///
    /// A repository transfer changes the namespace used by every OCI read, but
    /// it keeps the same repository id and therefore the same OCI metadata.
    /// Moving only the bare Git directory would leave layers, manifests and
    /// chunked uploads behind under a name that another repository can claim.
    /// Each successful move is retained for [`Self::restore_repository_transfer`]
    /// until the caller has committed the database update.
    pub async fn transfer_repository(
        &self,
        source_owner: &str,
        source_repo: &str,
        destination_owner: &str,
        destination_repo: &str,
    ) -> anyhow::Result<TransferredOciRepository> {
        let plan = self.plan_repository_transfer(
            source_owner,
            source_repo,
            destination_owner,
            destination_repo,
        )?;
        let mut moved = TransferredOciRepository::default();

        let (source, destination) = plan.blobs;
        match self.backend.move_prefix(&source, &destination).await {
            Ok(true) => moved.blobs = Some((source, destination)),
            Ok(false) => {}
            Err(error) => {
                return Err(anyhow::Error::new(error).context(format!(
                    "failed to move OCI registry data from {source_owner}/{source_repo} to \
                     {destination_owner}/{destination_repo}"
                )));
            }
        }

        let (source, destination) = plan.uploads;
        match move_directory("OCI upload directory", &source, &destination).await {
            Ok(true) => moved.uploads = Some((source, destination)),
            Ok(false) => {}
            Err(error) => {
                self.restore_repository_transfer(moved).await;
                return Err(error);
            }
        }

        if let Some((source, destination)) = plan.legacy {
            match move_directory("legacy OCI repository directory", &source, &destination).await {
                Ok(true) => moved.legacy = Some((source, destination)),
                Ok(false) => {}
                Err(error) => {
                    self.restore_repository_transfer(moved).await;
                    return Err(error);
                }
            }
        }

        Ok(moved)
    }

    /// Where every registry namespace of one repository goes when the
    /// repository is transferred to another owner.
    ///
    /// Produced once per transfer and consumed twice — by the journal entry
    /// that declares the move, and by the moves themselves — for exactly the
    /// reason [`Self::plan_repository_deletion`] is: a namespace named in one
    /// place and not the other is a namespace the recovery pass cannot finish.
    fn plan_repository_transfer(
        &self,
        source_owner: &str,
        source_repo: &str,
        destination_owner: &str,
        destination_repo: &str,
    ) -> anyhow::Result<OciRepositoryTransferPlan> {
        Ok(OciRepositoryTransferPlan {
            blobs: (
                Self::repository_blob_prefix(source_owner, source_repo)?,
                Self::repository_blob_prefix(destination_owner, destination_repo)?,
            ),
            uploads: (
                self.repository_upload_dir(source_owner, source_repo),
                self.repository_upload_dir(destination_owner, destination_repo),
            ),
            legacy: self
                .legacy_repository_dir(source_owner, source_repo)
                .map(|source| {
                    (
                        source,
                        self.legacy_repository_dir(destination_owner, destination_repo)
                            .expect("legacy OCI root is present for both transfer paths"),
                    )
                }),
        })
    }

    /// What a repository transfer is about to move within the registry, in the
    /// shape the recovery journal records.
    ///
    /// `live` is the source namespace throughout: while the row has not moved,
    /// that is still where every read looks, so an interrupted transfer that
    /// never committed is undone by moving each pair back the way it came.
    pub fn planned_repository_transfer_journal(
        &self,
        source_owner: &str,
        source_repo: &str,
        destination_owner: &str,
        destination_repo: &str,
    ) -> anyhow::Result<Vec<crate::deletion_recovery::StagedBytes>> {
        use crate::deletion_recovery::StagedBytes;

        let plan = self.plan_repository_transfer(
            source_owner,
            source_repo,
            destination_owner,
            destination_repo,
        )?;
        let mut journal = vec![StagedBytes::blob_prefix(&plan.blobs.0, &plan.blobs.1)];
        journal.push(StagedBytes::path(&plan.uploads.0, &plan.uploads.1)?);
        if let Some((source, destination)) = &plan.legacy {
            journal.push(StagedBytes::path(source, destination)?);
        }
        Ok(journal)
    }

    /// Put a registry moved by [`Self::transfer_repository`] back under its
    /// source namespace after the repository transfer aborts.
    pub async fn restore_repository_transfer(&self, moved: TransferredOciRepository) {
        if let Some((source, destination)) = moved.legacy {
            if let Err(error) = tokio::fs::rename(&destination, &source).await {
                tracing::warn!(
                    moved_to = %destination.display(),
                    belongs_at = %source.display(),
                    %error,
                    "failed to restore the legacy OCI directory after a repository transfer aborted — the active repository can no longer reach these blobs until the directory is moved back by hand"
                );
            }
        }
        if let Some((source, destination)) = moved.uploads {
            if let Err(error) = tokio::fs::rename(&destination, &source).await {
                tracing::warn!(
                    moved_to = %destination.display(),
                    belongs_at = %source.display(),
                    %error,
                    "failed to restore the OCI upload directory after a repository transfer aborted — uploads in flight will report a session the registry can no longer write to"
                );
            }
        }
        if let Some((source, destination)) = moved.blobs {
            match self.backend.move_prefix(&destination, &source).await {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    moved_to = %destination,
                    belongs_at = %source,
                    "failed to restore the OCI blob namespace after a repository transfer aborted — the active repository can no longer reach these layers until the prefix is moved back by hand"
                ),
                Err(error) => tracing::warn!(
                    moved_to = %destination,
                    belongs_at = %source,
                    %error,
                    "failed to restore the OCI blob namespace after a repository transfer aborted — the active repository can no longer reach these layers until the prefix is moved back by hand"
                ),
            }
        }
    }

    /// Move everything `owner/repo` owns in the registry out of the live
    /// namespace, reversibly.
    ///
    /// Registry keys are shaped `oci/<owner>/<repo>/...`, and the namespace is
    /// released the moment the repository row is soft-deleted — so a
    /// re-created `<owner>/<repo>` inherits the physical keys of its
    /// predecessor. That is not merely a retention leak: `HEAD
    /// /v2/{owner}/{repo}/blobs/{digest}` answers off storage and then looks
    /// for the `oci_blob` row, so the first `docker push` into the re-created
    /// namespace meets a `500` for a layer it never uploaded. Unfinished
    /// uploads leak the same way, minus the collision — nothing ever collects
    /// them, because the sessions that named them are gone with the repository.
    ///
    /// A failure part-way through puts back whatever had already moved and
    /// returns the error: a deletion that cannot retire the registry is a
    /// failed deletion, not one that quietly keeps the layers of a repository
    /// nobody can reach any more.
    pub async fn stage_repository_deletion(
        &self,
        owner: &str,
        repo: &str,
        repo_id: i64,
        deletion_id: &str,
    ) -> anyhow::Result<StagedOciRepository> {
        let mut staged = StagedOciRepository::default();
        let plan = self.plan_repository_deletion(owner, repo, repo_id, deletion_id)?;

        let (live, aside) = plan.blobs;
        match self.backend.move_prefix(&live, &aside).await {
            Ok(true) => staged.blobs = Some((live, aside)),
            Ok(false) => {}
            Err(error) => {
                let context = format!(
                    "failed to stage the registry data of {owner}/{repo} at {aside} — the \
                     repository cannot be deleted while its layers stay under a name it no \
                     longer holds"
                );
                return Err(anyhow::Error::new(error).context(context));
            }
        }

        let staging_root = plan.staging_root;

        let (live, aside) = plan.uploads;
        match stage_directory("OCI upload directory", &live, &aside).await {
            Ok(true) => {
                staged.uploads = Some((live, aside));
                staged.directories = Some(staging_root.clone());
            }
            Ok(false) => {}
            Err(error) => {
                self.restore_repository(staged).await;
                return Err(error);
            }
        }

        if let Some((live, aside)) = plan.legacy {
            match stage_directory("legacy OCI repository directory", &live, &aside).await {
                Ok(true) => {
                    staged.legacy = Some((live, aside));
                    staged.directories = Some(staging_root);
                }
                Ok(false) => {}
                Err(error) => {
                    self.restore_repository(staged).await;
                    return Err(error);
                }
            }
        }

        Ok(staged)
    }

    /// Name every registry namespace [`Self::stage_repository_deletion`] will
    /// move, without moving any of them.
    ///
    /// The staging call goes through this too, which is the point: a repository
    /// deletion has to declare its whole tombstone to
    /// [`crate::deletion_recovery`] before the first rename, and a second copy
    /// of these names would be a copy that drifts. What the journal describes
    /// and what the renames do are the same expressions.
    fn plan_repository_deletion(
        &self,
        owner: &str,
        repo: &str,
        repo_id: i64,
        deletion_id: &str,
    ) -> anyhow::Result<OciRepositoryDeletionPlan> {
        let repo_id_segment = repo_id.to_string();
        let staging_root = self
            .upload_root
            .join("_deleted")
            .join("repositories")
            .join(&repo_id_segment)
            .join(deletion_id);
        Ok(OciRepositoryDeletionPlan {
            blobs: (
                Self::repository_blob_prefix(owner, repo)?,
                BlobKey::from_segments([
                    "_deleted",
                    "repositories",
                    repo_id_segment.as_str(),
                    deletion_id,
                    "oci",
                ])?,
            ),
            uploads: (
                self.repository_upload_dir(owner, repo),
                staging_root.join("oci-uploads"),
            ),
            legacy: self
                .legacy_repository_dir(owner, repo)
                .map(|live| (live, staging_root.join("oci-legacy"))),
            staging_root,
        })
    }

    /// What a repository deletion is about to move out of the registry, in the
    /// shape the deletion journal records.
    pub fn planned_repository_deletion_journal(
        &self,
        owner: &str,
        repo: &str,
        repo_id: i64,
        deletion_id: &str,
    ) -> anyhow::Result<Vec<crate::deletion_recovery::StagedBytes>> {
        use crate::deletion_recovery::StagedBytes;

        let plan = self.plan_repository_deletion(owner, repo, repo_id, deletion_id)?;
        let mut journal = vec![StagedBytes::blob_prefix(&plan.blobs.0, &plan.blobs.1)];
        journal.push(StagedBytes::path(&plan.uploads.0, &plan.uploads.1)?);
        if let Some((live, aside)) = &plan.legacy {
            journal.push(StagedBytes::path(live, aside)?);
        }
        Ok(journal)
    }

    /// Put a staged registry back where a live repository expects it.
    ///
    /// Best-effort by construction: the caller is already returning a failure,
    /// and that failure is the one worth reading. What a botched restore must
    /// not do is stay quiet — the repository is live again with part of its
    /// registry parked under a name nothing else records.
    pub async fn restore_repository(&self, staged: StagedOciRepository) {
        if let Some((live, aside)) = staged.legacy {
            if let Err(error) = tokio::fs::rename(&aside, &live).await {
                tracing::warn!(
                    staged_at = %aside.display(),
                    belongs_at = %live.display(),
                    %error,
                    "failed to restore the legacy OCI directory after a repository deletion aborted — the active repository can no longer reach these blobs until the directory is moved back by hand"
                );
            }
        }
        if let Some((live, aside)) = staged.uploads {
            if let Err(error) = tokio::fs::rename(&aside, &live).await {
                tracing::warn!(
                    staged_at = %aside.display(),
                    belongs_at = %live.display(),
                    %error,
                    "failed to restore the OCI upload directory after a repository deletion aborted — uploads in flight will report a session the registry can no longer write to"
                );
            }
        }
        if let Some(root) = staged.directories {
            // Both leaves are back where they belong, so this is an empty
            // directory. `remove_dir` rather than `remove_dir_all` on purpose:
            // if a rename above failed, whatever is left is the operator's to
            // move by hand and the warning above already names it.
            if let Err(error) = tokio::fs::remove_dir(&root).await {
                tracing::debug!(
                    path = %root.display(),
                    %error,
                    "left the OCI deletion staging directory in place after a rollback — it is empty unless a restore above failed, and that failure has its own warning naming what is still in it"
                );
            }
        }
        if let Some((live, aside)) = staged.blobs {
            match self.backend.move_prefix(&aside, &live).await {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    staged_prefix = %aside,
                    live_prefix = %live,
                    "failed to restore the OCI blob namespace after a repository deletion aborted — the staged prefix disappeared and the active repository may now reference missing layers"
                ),
                Err(error) => tracing::warn!(
                    staged_prefix = %aside,
                    live_prefix = %live,
                    %error,
                    "failed to restore the OCI blob namespace after a repository deletion aborted — the active repository can no longer reach these layers until the prefix is moved back by hand"
                ),
            }
        }
    }

    /// Remove a staged registry for good, once the metadata row is gone.
    ///
    /// Every part is attempted even after one fails, because each leftover
    /// costs the operator a separate manual cleanup. The first error is
    /// returned so the caller can report that the deletion finished with data
    /// still on disk rather than claim a clean one.
    pub async fn discard_repository(&self, staged: StagedOciRepository) -> anyhow::Result<()> {
        let mut first_error = None;

        if let Some((_, aside)) = staged.blobs {
            if let Err(error) = self.backend.delete_prefix(&aside).await {
                tracing::warn!(
                    staged_prefix = %aside,
                    %error,
                    "the repository is deleted and its registry namespace is free, but its staged layers remain and must be removed by hand"
                );
                first_error = Some(
                    anyhow::Error::new(error)
                        .context(format!("failed to retire the staged OCI layers at {aside}")),
                );
            }
        }

        // One `remove_dir_all` for both filesystem parts: they were staged as
        // siblings under a directory named by this deletion, so removing that
        // directory is what leaves nothing behind.
        if let Some(path) = staged.directories {
            match tokio::fs::remove_dir_all(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "the repository is deleted, but its staged OCI directories remain on disk and must be removed by hand"
                    );
                    if first_error.is_none() {
                        first_error =
                            Some(registry_path_error("staged OCI directories", &path, &error));
                    }
                }
            }
        }

        first_error.map_or(Ok(()), Err)
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

    /// Read a blob's bytes, falling back to the legacy on-disk layout.
    ///
    /// "The registry does not have this blob" is typed
    /// ([`crate::error::NotFound`]) and everything else is left as a plain
    /// error, because the caller has to answer `404 BLOB_UNKNOWN` to the first
    /// and `500` to the second. Flattened together they used to be one
    /// `anyhow::Error`, and `get_blob` answered `BLOB_UNKNOWN` to both — so an
    /// unreadable blob store told `docker pull` the image referenced a layer
    /// that does not exist.
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
                let Some(path) = self.legacy_blob_path(owner, repo, digest) else {
                    return Err(crate::error::not_found("blob"));
                };
                tokio::fs::read(&path).await.map_err(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        crate::error::not_found("blob")
                    } else {
                        upload_path_error("legacy OCI blob", &path, &error)
                    }
                })
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
    ) -> anyhow::Result<FinalizedBlob> {
        let source = self.blob_key(src_owner, src_repo, digest)?;
        let destination = self.blob_key(dst_owner, dst_repo, digest)?;
        if self.backend.exists(&destination).await? {
            let size = self.backend.metadata(&destination).await?.size as i64;
            return Ok(FinalizedBlob {
                digest: digest.to_string(),
                size,
                storage_path: destination.to_string(),
                published: false,
            });
        }

        let metadata = if self.backend.exists(&source).await? {
            if let Some(path) = self.backend.local_path(&source) {
                self.backend.put_file(&destination, &path).await?
            } else {
                let data = self.backend.get(&source).await?;
                self.backend.put(&destination, &data).await?
            }
        } else if let Some(path) = self
            .legacy_blob_path(src_owner, src_repo, digest)
            .filter(|path| path.is_file())
        {
            self.backend.put_file(&destination, &path).await?
        } else {
            anyhow::bail!("source blob not found: {digest}");
        };
        Ok(FinalizedBlob {
            digest: digest.to_string(),
            size: metadata.size as i64,
            storage_path: destination.to_string(),
            published: true,
        })
    }

    pub async fn store_manifest(
        &self,
        owner: &str,
        repo: &str,
        digest: &str,
        data: &[u8],
    ) -> anyhow::Result<StoredManifest> {
        let key = self.manifest_key(owner, repo, digest)?;
        let published = !self.backend.exists(&key).await?;
        if published {
            self.backend.put(&key, data).await?;
        }
        Ok(StoredManifest {
            storage_path: key.to_string(),
            published,
        })
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
                // Same split as `read_blob`: absent is a 404, unreadable is ours.
                let Some(path) = self.legacy_manifest_path(owner, repo, digest) else {
                    return Err(crate::error::not_found("manifest"));
                };
                tokio::fs::read(&path).await.map_err(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        crate::error::not_found("manifest")
                    } else {
                        upload_path_error("legacy OCI manifest", &path, &error)
                    }
                })
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
    ) -> anyhow::Result<FinalizedBlob> {
        let upload_path = self.upload_file_path(owner, repo, uuid);
        let key = self.blob_key(owner, repo, expected_digest)?;

        if self.backend.exists(&key).await? {
            let size = self.backend.metadata(&key).await?.size as i64;
            discard_dir_async("OCI upload directory", &self.upload_dir(owner, repo, uuid)).await;
            return Ok(FinalizedBlob {
                digest: expected_digest.to_string(),
                size,
                storage_path: key.to_string(),
                published: false,
            });
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
            return Err(DigestMismatch {
                expected: expected_digest.to_string(),
                actual,
                staged_bytes: size,
            }
            .into());
        }

        self.backend.put_file(&key, &upload_path).await?;
        discard_dir_async("OCI upload directory", &self.upload_dir(owner, repo, uuid)).await;
        Ok(FinalizedBlob {
            digest: expected_digest.to_string(),
            size,
            storage_path: key.to_string(),
            published: true,
        })
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

fn digest_parts(digest: &str) -> Result<(&str, &str), InvalidDigest> {
    let malformed = || InvalidDigest {
        digest: digest.to_string(),
    };
    let (algorithm, hash) = digest.split_once(':').ok_or_else(malformed)?;
    if algorithm != "sha256"
        || hash.len() != 64
        || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(malformed());
    }
    Ok((algorithm, hash))
}

fn verify_digest(expected: &str, data: &[u8]) -> anyhow::Result<()> {
    digest_parts(expected)?;
    let actual = format!("sha256:{}", hex::encode(Sha256::digest(data)));
    if actual != expected {
        return Err(DigestMismatch {
            expected: expected.to_string(),
            actual,
            staged_bytes: data.len() as i64,
        }
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::OciStorage;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;

    /// A registry built the way `forgekeep serve` builds one: the shared local
    /// backend, the `_oci_uploads` staging tree, and `root` as the legacy
    /// layout to fall back on.
    fn local_storage(root: &std::path::Path) -> OciStorage {
        let backend = Arc::new(crate::blob_storage::LocalBlobStorage::new(root));
        OciStorage::from_backend(backend, root.join("_oci_uploads"), Some(root.to_path_buf()))
    }

    /// Build a storage whose upload root cannot host directories, so every
    /// upload-side filesystem call fails for a reason an operator has to fix.
    fn storage_with_unusable_upload_root(root: &std::path::Path) -> OciStorage {
        let upload_root = root.join("uploads");
        std::fs::write(&upload_root, b"not a directory").unwrap();
        let backend = Arc::new(crate::blob_storage::LocalBlobStorage::new(root));
        OciStorage::from_backend(backend, upload_root, Some(root.to_path_buf()))
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
        let started = local_storage(directory.path());
        assert_eq!(started.upload_size("alice", "demo", "no-such").unwrap(), 0);

        let broken = storage_with_unusable_upload_root(directory.path());
        let error = broken
            .upload_size("alice", "demo", "no-such")
            .expect_err("upload root is a file, not a missing upload");

        assert!(format!("{error:#}").contains("uploads"), "{error:#}");
    }

    /// The three ways `finalize_upload` fails are not interchangeable: two of
    /// them are the registry's own, and a client told `400 DIGEST_INVALID` for
    /// those goes looking for a corrupt layer instead of an unwritable
    /// directory.
    #[tokio::test]
    async fn finalize_blames_the_client_only_for_a_digest_it_got_wrong() {
        let directory = tempfile::tempdir().unwrap();
        let storage = local_storage(directory.path());
        let data = b"oci layer";
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(data)));
        let (upload, _) = storage.create_upload("alice", "demo").await.unwrap();
        storage
            .append_to_upload("alice", "demo", &upload, data)
            .await
            .unwrap();

        // The bytes are fine, the digest naming them is not — the client's.
        let wrong = format!("sha256:{}", "0".repeat(64));
        let mismatch = storage
            .finalize_upload("alice", "demo", &upload, &wrong)
            .await
            .expect_err("the staged bytes do not hash to that digest");
        assert!(super::is_client_digest_fault(&mismatch), "{mismatch:#}");

        // A digest that is not a digest at all — also the client's.
        let malformed = storage
            .finalize_upload("alice", "demo", &upload, "not-a-digest")
            .await
            .expect_err("malformed digest");
        assert!(super::is_client_digest_fault(&malformed), "{malformed:#}");

        // The staging file the registry itself owns is unreadable — ours, and
        // the message has to carry the path the io error dropped.
        let broken = storage_with_unusable_upload_root(directory.path());
        let ours = broken
            .finalize_upload("alice", "demo", &upload, &digest)
            .await
            .expect_err("upload root is a file, not a directory");
        assert!(!super::is_client_digest_fault(&ours), "{ours:#}");
        assert!(format!("{ours:#}").contains("uploads"), "{ours:#}");
    }

    #[tokio::test]
    async fn publishes_verified_upload_under_stable_key() {
        let directory = tempfile::tempdir().unwrap();
        let storage = local_storage(directory.path());
        let data = b"oci layer";
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(data)));
        let (upload, _) = storage.create_upload("alice", "demo").await.unwrap();
        storage
            .append_to_upload("alice", "demo", &upload, data)
            .await
            .unwrap();

        let blob = storage
            .finalize_upload("alice", "demo", &upload, &digest)
            .await
            .unwrap();
        assert_eq!(blob.size, data.len() as i64);
        assert!(blob
            .storage_path
            .starts_with("oci/alice/demo/blobs/sha256/"));
        assert!(
            blob.published,
            "the first finalize is what wrote the bytes and must say so"
        );
        assert_eq!(
            storage.read_blob("alice", "demo", &digest).await.unwrap(),
            data
        );
    }

    /// Finalizing a layer whose key already holds the bytes must not claim to
    /// have published them.
    ///
    /// The flag is what the HTTP layer rolls back on: told `true` here, a failed
    /// `insert_blob` on a re-push would delete the object the *first* push
    /// recorded a row for, and that image stops pulling. The dedup branch writes
    /// nothing, so it has nothing to take back.
    #[tokio::test]
    async fn a_deduplicated_finalize_does_not_claim_to_have_published() {
        let directory = tempfile::tempdir().unwrap();
        let storage = local_storage(directory.path());
        let data = b"oci layer";
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(data)));

        let mut finalized = Vec::new();
        for _ in 0..2 {
            let (upload, _) = storage.create_upload("alice", "demo").await.unwrap();
            storage
                .append_to_upload("alice", "demo", &upload, data)
                .await
                .unwrap();
            finalized.push(
                storage
                    .finalize_upload("alice", "demo", &upload, &digest)
                    .await
                    .unwrap(),
            );
        }

        assert!(finalized[0].published, "the first push wrote the bytes");
        assert!(
            !finalized[1].published,
            "the second push found them already there and wrote nothing"
        );
        assert_eq!(finalized[0].storage_path, finalized[1].storage_path);
        assert_eq!(finalized[0].size, finalized[1].size);
        assert_eq!(
            storage.read_blob("alice", "demo", &digest).await.unwrap(),
            data,
            "the deduplicated finalize must leave the stored bytes intact"
        );
    }

    /// An image pushed under the pre-`BlobStorage` layout still pulls.
    ///
    /// The fallback that reaches those bytes hangs off `legacy_root`, and until
    /// card_04cc26cbe976 the only production caller that ever set it sat behind
    /// a `[server]` key no shipped binary could produce — so every instance
    /// built its registry with `None` and the whole legacy path was dead code
    /// that read as live. `serve` now passes `repo_root`, which is what this
    /// asserts is enough to find a blob written the old way.
    #[tokio::test]
    async fn a_blob_left_by_the_previous_layout_is_still_readable() {
        let directory = tempfile::tempdir().unwrap();
        let storage = local_storage(directory.path());
        let data = b"layer from the old layout";
        let hash = hex::encode(Sha256::digest(data));
        let digest = format!("sha256:{hash}");

        let legacy = directory
            .path()
            .join("alice")
            .join("demo")
            .join("oci")
            .join("_blobs")
            .join("sha256")
            .join(&hash[..2])
            .join(&hash);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, data).unwrap();

        assert!(
            storage.blob_exists("alice", "demo", &digest).await.unwrap(),
            "a blob only the legacy layout holds still belongs to the registry"
        );
        assert_eq!(
            storage.read_blob("alice", "demo", &digest).await.unwrap(),
            data
        );
    }
}
