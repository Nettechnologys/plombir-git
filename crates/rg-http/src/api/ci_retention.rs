use crate::api::repo_access::RepoAdmin;
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::{Path as FsPath, PathBuf};
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
    let cache_root = state.repo_root.join("_ci_cache");
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
        let staging = match CacheDeletionStaging::prepare(
            state,
            cache.id,
            &cache.file_path,
            &cache_root,
        )
        .await
        {
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

    Ok(summary)
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
    /// An archive that is already gone is the end state this call is asked for,
    /// so it stages nothing and succeeds — otherwise an entry whose file an
    /// operator removed by hand could never be cleaned at all. A recorded path
    /// outside the managed cache root, or a filesystem failure, fails here —
    /// before the entry is touched.
    async fn prepare(
        state: &AppState,
        cache_id: i64,
        recorded: &str,
        root: &FsPath,
    ) -> anyhow::Result<Self> {
        let mut staging = Self {
            cache_id,
            archive: None,
            deletion_id: None,
        };
        let live = PathBuf::from(recorded);
        if !live.exists() {
            return Ok(staging);
        }
        let canonical_path = tokio::fs::canonicalize(&live)
            .await
            .map_err(|error| cache_cleanup_error("CI cache archive", &live, &error))?;
        let canonical_root = tokio::fs::canonicalize(root)
            .await
            .map_err(|error| cache_cleanup_error("CI cache directory", root, &error))?;
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

pub async fn run_cleanup_loop(
    state: AppState,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        match cleanup_expired_storage(&state, None).await {
            Ok(summary)
                if summary.artifacts_deleted > 0
                    || summary.caches_deleted > 0
                    || summary.oci_uploads_deleted > 0
                    || summary.failures > 0 =>
            {
                tracing::info!(
                    artifacts = summary.artifacts_deleted,
                    caches = summary.caches_deleted,
                    oci_uploads = summary.oci_uploads_deleted,
                    failures = summary.failures,
                    "CI retention cleanup completed"
                )
            }
            Ok(_) => {}
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
