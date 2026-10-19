use crate::api::repo_access::RepoAdmin;
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path as FsPath, PathBuf};
use std::time::{Duration, SystemTime};
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct RetentionPolicyResponse {
    pub artifact_retention_days: i32,
    pub cache_retention_days: i32,
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct RetentionPolicyRequest {
    pub artifact_retention_days: i32,
    pub cache_retention_days: i32,
}
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct CleanupResponse {
    pub artifacts_deleted: u64,
    pub caches_deleted: u64,
    /// Abandoned OCI blob-upload sessions whose 24h TTL has passed. Not a CI
    /// artifact, but it is the same question — storage whose retention window
    /// closed — and this is the pass that already asks it hourly.
    pub oci_uploads_deleted: u64,
    pub oci_blobs_deleted: u64,
    pub oci_orphan_objects_deleted: u64,
    pub failures: u64,
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/actions/retention", tag = "CI/CD", responses((status = 200, body = RetentionPolicyResponse)))]
pub async fn get_policy(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    match rg_db::ops::ci_retention_ops::get_policy(&state.db, repo.id).await {
        Ok(policy) => Json(RetentionPolicyResponse {
            artifact_retention_days: policy.artifact_retention_days,
            cache_retention_days: policy.cache_retention_days,
        })
        .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/actions/retention",
    tag = "CI/CD",
    request_body = RetentionPolicyRequest,
    responses(
        (status = 200, body = RetentionPolicyResponse),
        (status = 404, description = "Repository disappeared while saving the policy")
    )
)]
pub async fn update_policy(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
    Json(body): Json<RetentionPolicyRequest>,
) -> impl IntoResponse {
    if !(1..=3650).contains(&body.artifact_retention_days)
        || !(1..=3650).contains(&body.cache_retention_days)
    {
        return AppError::bad_request("retention days must be between 1 and 3650").into_response();
    }
    match rg_db::ops::ci_retention_ops::upsert_policy(
        &state.db,
        repo.id,
        body.artifact_retention_days,
        body.cache_retention_days,
    )
    .await
    {
        Ok(Some(policy)) => Json(RetentionPolicyResponse {
            artifact_retention_days: policy.artifact_retention_days,
            cache_retention_days: policy.cache_retention_days,
        })
        .into_response(),
        Ok(None) => AppError::from(rg_core::error::not_found("repository")).into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/actions/retention/expired", tag = "CI/CD", responses((status = 200, body = CleanupResponse)))]
pub async fn cleanup(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    match cleanup_expired_storage(&state, Some(repo.id)).await {
        Ok(summary) => (StatusCode::OK, Json(summary)).into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

pub async fn cleanup_expired_storage(
    state: &AppState,
    repo_filter: Option<i64>,
) -> anyhow::Result<CleanupResponse> {
    let mut summary = CleanupResponse::default();
    for artifact in rg_db::ops::artifact_ops::list_expired(&state.db).await? {
        if let Some(repo_id) = repo_filter {
            if artifact_repo_id(state, artifact.job_id).await? != Some(repo_id) {
                continue;
            }
        }
        // An expired artifact is deleted the same reversible way the routed
        // DELETE deletes one: bytes out of the live namespace first, metadata
        // second, tombstone last. A sweep is where the unreversible order hurts
        // most — nobody is watching the response, so a metadata failure after a
        // successful unlink would leave a row advertising bytes this loop
        // destroyed, and the next pass would report the same artifact again.
        let staging = match crate::api::artifacts::ArtifactDeletionStaging::prepare(
            state,
            artifact.id,
            &artifact.file_path,
        )
        .await
        {
            Ok(staging) => staging,
            Err(error) => {
                summary.failures += 1;
                tracing::error!(
                    artifact_id = artifact.id,
                    error = %format!("{error:#}"),
                    "refused to clean expired artifact"
                );
                continue;
            }
        };

        let deleted = match rg_db::ops::artifact_ops::delete_by_id(&state.db, artifact.id).await {
            Ok(deleted) => deleted,
            Err(error) => {
                staging.restore(state).await;
                summary.failures += 1;
                tracing::error!(
                    artifact_id = artifact.id,
                    error = %format!("{error:#}"),
                    "expired artifact kept: deleting its metadata failed, so its bytes were put back"
                );
                continue;
            }
        };

        // Retirement is what makes the artifact deleted. Counting it before the
        // tombstone is gone would report a cleanup that freed no space.
        if let Err(error) = staging.retire(state).await {
            summary.failures += 1;
            tracing::error!(
                artifact_id = artifact.id,
                error = %format!("{error:#}"),
                "expired artifact metadata is deleted, but its staged bytes remain"
            );
            continue;
        }
        if deleted {
            summary.artifacts_deleted += 1;
        }
    }
    for cache in rg_db::ops::ci_retention_ops::list_expired_cache(&state.db).await? {
        if repo_filter.is_some_and(|repo_id| repo_id != cache.repo_id) {
            continue;
        }
        // The cache half deletes on the same reversible terms as the artifact
        // half above: archive out of the live namespace first, row second,
        // tombstone last. Nothing here may be fatal to the sweep either — one
        // cache entry that cannot be cleaned is a `failures` line, not a reason
        // to abandon every entry behind it and drop the summary on the floor.
        let staging = match CacheDeletionStaging::prepare(state, &cache).await {
            Ok(staging) => staging,
            Err(error) => {
                summary.failures += 1;
                tracing::error!(cache_id = cache.id, error = %format!("{error:#}"), "refused to clean expired cache");
                continue;
            }
        };

        let deleted =
            match rg_db::ops::ci_retention_ops::delete_cache_entry_if_expired(&state.db, &cache)
                .await
            {
                Ok(deleted) => deleted,
                Err(error) => {
                    staging.restore(state).await;
                    summary.failures += 1;
                    tracing::error!(
                        cache_id = cache.id,
                        error = %format!("{error:#}"),
                        "expired cache kept: deleting its entry failed, so its archive was put back"
                    );
                    continue;
                }
            };

        if !deleted {
            // The row changed after `list_expired_cache`. If it still names the
            // staged publication, a reader merely refreshed its lifetime and
            // the archive must go back. If it points elsewhere (or the row is
            // already gone), the staged bytes are genuinely superseded and can
            // be retired without touching the live publication.
            let current = match rg_db::ops::ci_retention_ops::find_cache_entry(
                &state.db,
                cache.repo_id,
                &cache.key_hash,
            )
            .await
            {
                Ok(current) => current,
                Err(error) => {
                    staging.restore(state).await;
                    summary.failures += 1;
                    tracing::error!(
                        cache_id = cache.id,
                        error = %format!("{error:#}"),
                        "expired cache changed during cleanup and ownership could not be re-read; its archive was put back"
                    );
                    continue;
                }
            };
            if current
                .is_some_and(|entry| entry.id == cache.id && entry.file_path == cache.file_path)
            {
                staging.restore(state).await;
                continue;
            }
            if let Err(error) = staging.retire(state).await {
                summary.failures += 1;
                tracing::error!(
                    cache_id = cache.id,
                    error = %format!("{error:#}"),
                    "superseded expired cache entry is gone, but its staged archive remains"
                );
            }
            continue;
        }

        // Retirement is what frees the space. Counting the entry before the
        // tombstone is gone would report a cleanup that reclaimed nothing.
        if let Err(error) = staging.retire(state).await {
            summary.failures += 1;
            tracing::error!(
                cache_id = cache.id,
                error = %format!("{error:#}"),
                "expired cache entry is deleted, but its staged archive remains"
            );
            continue;
        }
        summary.caches_deleted += 1;
    }

    // Abandoned `docker push` sessions. `create_upload` has always stamped a
    // 24h `expires_at`, and until now nothing ever read it: a dropped
    // connection left both the `oci_upload` row and a staging directory holding
    // every layer byte already sent, and the disk leaked by exactly that much
    // forever (card_487dc1247247). This is the same sweep the CI retention
    // already runs, on the same schedule and behind the same route, because a
    // second scheduler for one more TTL is how the first one stops being the
    // place anybody looks.
    for (upload, oci_repo) in rg_db::ops::oci_ops::list_expired_uploads(&state.db).await? {
        // `oci_repository.repo_id` is the Plombir Git repository id, so the routed
        // per-repository cleanup filters on it directly — no second lookup that
        // could resolve differently from the one the row was written with.
        if repo_filter.is_some_and(|repo_id| repo_id != oci_repo.repo_id) {
            continue;
        }
        // The namespace column is `{owner}/{repo}` — the same string the push
        // built its staging path from. Splitting it is deliberate: deriving
        // `owner` and `repo` from anywhere else risks a path that points at
        // nothing, and `delete_upload` tolerates an absent directory (it has to,
        // so a retry is free), so a wrong path would report success and leave
        // the bytes exactly where they were.
        let Some((owner, repo)) = oci_repo.namespace.split_once('/') else {
            summary.failures += 1;
            tracing::error!(
                upload_uuid = %upload.uuid,
                namespace = %oci_repo.namespace,
                "expired OCI upload session kept: its repository namespace is not owner/repo, so \
                 the staging path cannot be built"
            );
            continue;
        };

        // Bytes first, row second, and the order is the whole argument. There is
        // nothing to compensate here — the session is dead either way — so the
        // only question is which failure is recoverable. A directory that will
        // not delete leaves the row, and the next pass tries again; a row
        // deleted ahead of the directory leaves bytes nothing in the database
        // names, and no pass ever comes back for them.
        if let Err(error) = state
            .oci_storage
            .delete_upload(owner, repo, &upload.uuid)
            .await
        {
            summary.failures += 1;
            tracing::error!(
                upload_uuid = %upload.uuid,
                repo_id = oci_repo.repo_id,
                error = %format!("{error:#}"),
                "expired OCI upload session kept: its staged chunks could not be removed, and dropping the row would strand them"
            );
            continue;
        }
        if let Err(error) =
            rg_db::ops::oci_ops::delete_upload(&state.db, upload.oci_repository_id, &upload.uuid)
                .await
        {
            summary.failures += 1;
            tracing::error!(
                upload_uuid = %upload.uuid,
                repo_id = oci_repo.repo_id,
                error = %format!("{error:#}"),
                "expired OCI upload session: its chunks are gone but the row remains, and the next pass will retry"
            );
            continue;
        }
        summary.oci_uploads_deleted += 1;
    }

    // Deleting a manifest retires metadata immediately, but shared layers are
    // collected only after a grace period. Repository metadata writers use the
    // same lease, and each object gets its publication lease before deletion.
    for oci_repo in rg_db::ops::oci_ops::list_repositories(&state.db, repo_filter).await? {
        let Some((owner, repo)) = oci_repo.namespace.split_once('/') else {
            summary.failures += 1;
            tracing::error!(namespace = %oci_repo.namespace, "OCI GC skipped malformed namespace");
            continue;
        };
        let key = format!("oci/metadata/{}", oci_repo.id);
        let lease = match rg_core::package_registry::oci::acquire_publication_lease(&state.db, &key)
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                summary.failures += 1;
                tracing::error!(repo_id = oci_repo.repo_id, error = %format!("{error:#}"), "OCI GC could not acquire metadata lease");
                continue;
            }
        };
        let result = if lease.exclusive() {
            cleanup_oci_repository(state, oci_repo.id, owner, repo).await
        } else {
            // The previous holder might still be writing after lease expiry.
            Err(anyhow::anyhow!(
                "OCI metadata lease was taken over; collection deferred"
            ))
        };
        rg_core::package_registry::oci::release_publication_lease(&state.db, &lease).await;
        match result {
            Ok((blobs, orphans)) => {
                summary.oci_blobs_deleted += blobs;
                summary.oci_orphan_objects_deleted += orphans;
            }
            Err(error) => {
                summary.failures += 1;
                tracing::error!(repo_id = oci_repo.repo_id, error = %format!("{error:#}"), "OCI GC failed for repository");
            }
        }
    }

    Ok(summary)
}

/// Reclaim only mature, unreferenced bytes. A manifest row is authoritative:
/// malformed historical JSON conservatively stops collection for this repo.
async fn cleanup_oci_repository(
    state: &AppState,
    oci_repo_id: i64,
    owner: &str,
    repo: &str,
) -> anyhow::Result<(u64, u64)> {
    use rg_core::package_registry::oci::{
        acquire_publication_lease, release_publication_lease, ParsedManifest,
    };
    let mut referenced = HashSet::new();
    let mut live = HashSet::new();
    let mut cursor = 0;
    loop {
        let page =
            rg_db::ops::oci_ops::list_manifest_page(&state.db, oci_repo_id, cursor, 10).await?;
        if page.is_empty() {
            break;
        }
        for manifest in page {
            cursor = manifest.id;
            let parsed = ParsedManifest::parse(manifest.manifest_json.as_bytes())?;
            referenced.extend(parsed.referenced_blobs());
            live.insert(
                state
                    .oci_storage
                    .manifest_storage_key(owner, repo, &manifest.digest)?,
            );
        }
    }
    let before = chrono::Utc::now() - chrono::Duration::hours(24);
    let mut removed_blobs = 0;
    for candidate in rg_db::ops::oci_ops::list_mature_blobs(&state.db, oci_repo_id, before).await? {
        if referenced.contains(&candidate.digest) {
            continue;
        }
        let lease = acquire_publication_lease(&state.db, &candidate.storage_path).await?;
        let result = async {
            if !lease.exclusive() {
                return Ok(false);
            }
            let current =
                rg_db::ops::oci_ops::find_blob(&state.db, oci_repo_id, &candidate.digest).await?;
            if current
                .as_ref()
                .is_none_or(|row| row.id != candidate.id || row.created_at >= before)
            {
                return Ok(false);
            }
            if !rg_db::ops::oci_ops::delete_blob_by_id(&state.db, candidate.id).await? {
                return Ok(false);
            }
            state
                .oci_storage
                .discard_published_object(&candidate.storage_path)
                .await?;
            Ok::<_, anyhow::Error>(true)
        }
        .await;
        release_publication_lease(&state.db, &lease).await;
        if result? {
            removed_blobs += 1;
        }
    }

    // A crash between metadata deletion and byte deletion can leave an object
    // with no row. Sweep only old objects and re-check under their key lease.
    for blob in
        rg_db::ops::oci_ops::list_mature_blobs(&state.db, oci_repo_id, chrono::Utc::now()).await?
    {
        live.insert(blob.storage_path);
    }
    let mut removed_orphans = 0;
    for object in state
        .oci_storage
        .list_published_objects(owner, repo)
        .await?
    {
        let key = object.key.as_str();
        if live.contains(key)
            || object.modified.is_none_or(|time| {
                SystemTime::now().duration_since(time).unwrap_or_default()
                    < Duration::from_secs(24 * 60 * 60)
            })
        {
            continue;
        }
        let lease = acquire_publication_lease(&state.db, key).await?;
        let result = async {
            if !lease.exclusive() {
                return Ok(false);
            }
            // A blob finalizer can publish while the repository lease is held,
            // but its key lease forces it to finish before this recheck.
            let current_blobs =
                rg_db::ops::oci_ops::list_mature_blobs(&state.db, oci_repo_id, chrono::Utc::now())
                    .await?;
            if current_blobs.iter().any(|blob| blob.storage_path == key) {
                return Ok(false);
            }
            if state
                .oci_storage
                .discard_published_object(key)
                .await
                .is_err()
            {
                return Err(anyhow::anyhow!("could not remove orphan OCI object {key}"));
            }
            Ok::<_, anyhow::Error>(true)
        }
        .await;
        release_publication_lease(&state.db, &lease).await;
        if result? {
            removed_orphans += 1;
        }
    }
    Ok((removed_blobs, removed_orphans))
}

async fn artifact_repo_id(state: &AppState, job_id: i64) -> anyhow::Result<Option<i64>> {
    let Some(job) = rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await? else {
        return Ok(None);
    };
    let Some(stage) = rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await?
    else {
        return Ok(None);
    };
    Ok(
        rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
            .await?
            .map(|pipeline| pipeline.repo_id),
    )
}

/// One actionable line for a filesystem failure on a CI cache archive.
///
/// The cleanup loop only logs `cache_id`, so a bare `io::Error` from the unlink
/// path leaves an operator with an errno against a `_ci_cache/<repo_id>/` file
/// that is never named — the same anonymous failure the write half of these
/// endpoints already spells out via `cache_path_error`.
fn cache_cleanup_error(what: &str, path: &FsPath, error: &std::io::Error) -> anyhow::Error {
    rg_core::platform::fs::path_error(what, path, error, rg_core::platform::fs::CI_CACHE_DIR_HINT)
}

/// An expired cache archive, renamed beside itself.
#[derive(Debug)]
struct StagedCacheArchive {
    live: PathBuf,
    staged: PathBuf,
}

/// An expired CI cache archive, taken out of the live namespace but not yet
/// destroyed.
///
/// Unlinking the archive first and deleting its row second is not compensable:
/// a metadata failure after a successful unlink leaves a live entry naming an
/// archive this sweep already destroyed, and every `download_cache` on that key
/// fails until the next pass — an hour later — comes back for the row. Cache is
/// recoverable data, so the damage is a stall rather than a loss, but the
/// reversible order costs one rename and removes the window entirely, which is
/// what the artifact half of this same sweep already does.
#[derive(Debug)]
struct CacheDeletionStaging {
    cache_id: i64,
    archive: Option<StagedCacheArchive>,
    /// The id this deletion's journal entry is filed under, once one has been
    /// opened. `None` is the staging that moved nothing — an archive an
    /// operator already removed by hand.
    deletion_id: Option<String>,
}

impl CacheDeletionStaging {
    /// Move the archive out of the live namespace.
    ///
    /// The archive is found the way every reader finds it, by the recorded file
    /// name inside the repository's cache directory under the current
    /// `repo_root` (`rg_core::ci_cache::recorded_archive`). Taking the recorded
    /// path literally used to disagree with them: after the data directory
    /// moved, the old spelling named nothing, the entry was deleted as if its
    /// archive were already gone, and the archive stayed on disk with no row.
    ///
    /// An archive that is already gone is the end state this call is asked for,
    /// so it stages nothing and succeeds — otherwise an entry whose file an
    /// operator removed by hand could never be cleaned at all. A path that
    /// resolves outside the repository's cache directory (through a symlink),
    /// or a filesystem failure, fails here — before the entry is touched.
    async fn prepare(
        state: &AppState,
        cache: &rg_db::entities::ci_cache_entry::Model,
    ) -> anyhow::Result<Self> {
        let mut staging = Self {
            cache_id: cache.id,
            archive: None,
            deletion_id: None,
        };
        let root = rg_core::ci_cache::archive_dir(&state.repo_root, cache.repo_id);
        let Some(live) =
            rg_core::ci_cache::recorded_archive(&state.repo_root, cache.repo_id, &cache.file_path)
        else {
            anyhow::bail!(
                "stored path {} does not name a CI cache archive",
                cache.file_path
            );
        };
        if !live.exists() {
            return Ok(staging);
        }
        let canonical_path = tokio::fs::canonicalize(&live)
            .await
            .map_err(|error| cache_cleanup_error("CI cache archive", &live, &error))?;
        let canonical_root = tokio::fs::canonicalize(&root)
            .await
            .map_err(|error| cache_cleanup_error("CI cache directory", &root, &error))?;
        if !canonical_path.starts_with(&canonical_root) {
            anyhow::bail!(
                "stored path {} is outside managed storage root {}",
                canonical_path.display(),
                canonical_root.display()
            );
        }
        let deletion_id = uuid::Uuid::new_v4().simple().to_string();
        let staged = match canonical_path.file_name() {
            Some(name) => canonical_path
                .with_file_name(format!("{}.deleted-{deletion_id}", name.to_string_lossy())),
            None => anyhow::bail!(
                "stored path {} does not name a CI cache archive",
                canonical_path.display()
            ),
        };
        // Declared before the rename: a stop between the two leaves a live entry
        // whose archive is one name away, and this entry is what a later
        // startup pass reads to put it back. See `rg_core::deletion_recovery`.
        rg_core::deletion_recovery::open(
            state.blob_storage.as_ref(),
            &deletion_id,
            "CI cache archive",
            vec![rg_core::deletion_recovery::StagedBytes::path(
                &canonical_path,
                &staged,
            )?],
        )
        .await?;
        staging.deletion_id = Some(deletion_id.clone());

        match tokio::fs::rename(&canonical_path, &staged).await {
            Ok(()) => {
                staging.archive = Some(StagedCacheArchive {
                    live: canonical_path,
                    staged,
                })
            }
            // Removed between the check and the rename: nothing left to stage.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(
                    cache_cleanup_error("CI cache archive", &canonical_path, &error).context(
                        format!(
                            "failed to stage expired CI cache archive at {}",
                            staged.display()
                        ),
                    ),
                );
            }
        }
        Ok(staging)
    }

    /// Put the archive back where the surviving entry expects it.
    ///
    /// Compensation on an error path: the sweep still has to report the original
    /// failure, so a failed restore can only be logged. When it fails the entry
    /// is live again while its archive sits under a name nothing records — which
    /// is exactly what the log line has to say.
    async fn restore(&self, state: &AppState) {
        if let Some(archive) = &self.archive {
            if let Err(error) = tokio::fs::rename(&archive.staged, &archive.live).await {
                tracing::warn!(
                    cache_id = self.cache_id,
                    staged_at = %archive.staged.display(),
                    belongs_at = %archive.live.display(),
                    %error,
                    "failed to restore an expired CI cache archive after cleanup aborted — the surviving entry now points at missing bytes until the file is moved back by hand"
                );
            }
        }
        if let Some(deletion_id) = &self.deletion_id {
            rg_core::deletion_recovery::close(state.blob_storage.as_ref(), deletion_id).await;
        }
    }

    /// Destroy the tombstone, once the entry is gone.
    ///
    /// After the entry commits there is nothing to roll back — the live name is
    /// already free — so a failure here is cleanup debt, not a lost deletion. It
    /// is still returned: counting a cache whose bytes are still parked would
    /// report space this pass never reclaimed.
    async fn retire(self, state: &AppState) -> anyhow::Result<()> {
        // Before the unlink, and before the early return: the marker is what
        // stops a startup pass restoring bytes whose entry is already gone, and
        // the journal has to be closed even when there was nothing to remove.
        // A failure to mark is reported rather than allowed to stop the
        // retirement — see `StagedPackageVersion::retire`.
        let mut cleanup_error = None;
        if let Some(deletion_id) = &self.deletion_id {
            if let Err(error) =
                rg_core::deletion_recovery::mark_committed(state.blob_storage.as_ref(), deletion_id)
                    .await
            {
                tracing::warn!(
                    cache_id = self.cache_id,
                    error = %format!("{error:#}"),
                    "CI cache entry is deleted, but the deletion could not be marked committed"
                );
                cleanup_error = Some(error);
            }
        }

        let removed = self.retire_archive().await;
        // Closed only once the tombstone is actually gone. A staged archive
        // that would not unlink used to be an operator's job forever; with the
        // marker already down, keeping the entry hands it to the next startup
        // pass instead, which destroys it and closes the entry then.
        if removed.is_ok() {
            if let Some(deletion_id) = &self.deletion_id {
                rg_core::deletion_recovery::close(state.blob_storage.as_ref(), deletion_id).await;
            }
        }
        match (removed, cleanup_error) {
            (Err(error), _) => Err(error),
            (Ok(()), Some(error)) => Err(error),
            (Ok(()), None) => Ok(()),
        }
    }

    async fn retire_archive(&self) -> anyhow::Result<()> {
        let Some(archive) = &self.archive else {
            return Ok(());
        };
        match tokio::fs::remove_file(&archive.staged).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                tracing::warn!(
                    cache_id = self.cache_id,
                    staged_at = %archive.staged.display(),
                    %error,
                    "CI cache entry is deleted, but its staged archive remains and must be removed by hand"
                );
                Err(
                    cache_cleanup_error("staged CI cache archive", &archive.staged, &error)
                        .context("failed to retire a deleted CI cache archive"),
                )
            }
        }
    }
}

/// One info line per pass that changed anything, carrying every counter.
///
/// The counters are destructured rather than read by name, so a new one cannot
/// be left out of the line or out of the "did anything happen" test. The OCI
/// blob collector shipped that way: it deleted blobs hourly and the line only
/// appeared when CI artifacts happened to expire in the same pass
/// (card_84a0006d0520).
fn log_cleanup_summary(summary: &CleanupResponse) {
    let &CleanupResponse {
        artifacts_deleted,
        caches_deleted,
        oci_uploads_deleted,
        oci_blobs_deleted,
        oci_orphan_objects_deleted,
        failures,
    } = summary;
    let changed = [
        artifacts_deleted,
        caches_deleted,
        oci_uploads_deleted,
        oci_blobs_deleted,
        oci_orphan_objects_deleted,
        failures,
    ]
    .iter()
    .any(|count| *count > 0);
    if changed {
        tracing::info!(
            artifacts = artifacts_deleted,
            caches = caches_deleted,
            oci_uploads = oci_uploads_deleted,
            oci_blobs = oci_blobs_deleted,
            oci_orphan_objects = oci_orphan_objects_deleted,
            failures,
            "CI retention cleanup completed"
        )
    }
}

pub async fn run_cleanup_loop(
    state: AppState,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        match cleanup_expired_storage(&state, None).await {
            Ok(summary) => log_cleanup_summary(&summary),
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "CI retention cleanup failed")
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(3600)) => {}
            _ = shutdown_rx.changed() => {
                tracing::info!("CI retention cleanup received shutdown, stopping");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{log_cleanup_summary, CleanupResponse};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogs {
        type Writer = CapturedLogs;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    fn logged(summary: &CleanupResponse) -> String {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || log_cleanup_summary(summary));
        let bytes = logs.0.lock().expect("log lock").clone();
        String::from_utf8(bytes).expect("logs are UTF-8")
    }

    #[test]
    fn a_pass_that_only_collected_oci_blobs_is_logged_with_both_counters() {
        let line = logged(&CleanupResponse {
            oci_blobs_deleted: 3,
            oci_orphan_objects_deleted: 2,
            ..CleanupResponse::default()
        });
        assert!(line.contains("CI retention cleanup completed"), "{line}");
        assert!(line.contains("oci_blobs=3"), "{line}");
        assert!(line.contains("oci_orphan_objects=2"), "{line}");

        let orphans_only = logged(&CleanupResponse {
            oci_orphan_objects_deleted: 1,
            ..CleanupResponse::default()
        });
        assert!(
            orphans_only.contains("oci_orphan_objects=1"),
            "{orphans_only}"
        );
    }

    #[test]
    fn an_idle_pass_stays_silent() {
        assert_eq!(logged(&CleanupResponse::default()), "");
    }
}
