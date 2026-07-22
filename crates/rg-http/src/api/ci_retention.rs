use super::repo_access::require_admin;
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
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
    pub failures: u64,
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/actions/retention", tag = "CI/CD", responses((status = 200, body = RetentionPolicyResponse)))]
pub async fn get_policy(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match rg_db::ops::ci_retention_ops::get_policy(&state.db, repo.id).await {
        Ok(policy) => Json(RetentionPolicyResponse {
            artifact_retention_days: policy.artifact_retention_days,
            cache_retention_days: policy.cache_retention_days,
        })
        .into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[utoipa::path(put, path = "/repos/{owner}/{name}/actions/retention", tag = "CI/CD", request_body = RetentionPolicyRequest, responses((status = 200, body = RetentionPolicyResponse)))]
pub async fn update_policy(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<RetentionPolicyRequest>,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
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
        Ok(policy) => Json(RetentionPolicyResponse {
            artifact_retention_days: policy.artifact_retention_days,
            cache_retention_days: policy.cache_retention_days,
        })
        .into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/actions/retention/expired", tag = "CI/CD", responses((status = 200, body = CleanupResponse)))]
pub async fn cleanup(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match cleanup_expired_storage(&state, Some(repo.id)).await {
        Ok(summary) => (StatusCode::OK, Json(summary)).into_response(),
        Err(error) => AppError::internal(error).into_response(),
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
        match crate::api::artifacts::delete_artifact_blob(state, &artifact.file_path).await {
            Ok(()) => {
                rg_db::ops::artifact_ops::delete_by_id(&state.db, artifact.id).await?;
                summary.artifacts_deleted += 1;
            }
            Err(error) => {
                summary.failures += 1;
                tracing::error!(artifact_id = artifact.id, %error, "refused to clean expired artifact");
            }
        }
    }
    for cache in rg_db::ops::ci_retention_ops::list_expired_cache(&state.db).await? {
        if repo_filter.is_some_and(|repo_id| repo_id != cache.repo_id) {
            continue;
        }
        match safe_remove_file(PathBuf::from(&cache.file_path), &cache_root).await {
            Ok(()) => {
                rg_db::ops::ci_retention_ops::delete_cache_entry(&state.db, cache.id).await?;
                summary.caches_deleted += 1;
            }
            Err(error) => {
                summary.failures += 1;
                tracing::error!(cache_id = cache.id, %error, "refused to clean expired cache");
            }
        }
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

async fn safe_remove_file(path: PathBuf, root: &FsPath) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let canonical_path = tokio::fs::canonicalize(&path).await?;
    let canonical_root = tokio::fs::canonicalize(root).await?;
    if !canonical_path.starts_with(&canonical_root) {
        anyhow::bail!("stored path is outside managed storage root");
    }
    tokio::fs::remove_file(canonical_path).await?;
    Ok(())
}

pub async fn run_cleanup_loop(state: AppState) {
    loop {
        match cleanup_expired_storage(&state, None).await {
            Ok(summary)
                if summary.artifacts_deleted > 0
                    || summary.caches_deleted > 0
                    || summary.failures > 0 =>
            {
                tracing::info!(
                    artifacts = summary.artifacts_deleted,
                    caches = summary.caches_deleted,
                    failures = summary.failures,
                    "CI retention cleanup completed"
                )
            }
            Ok(_) => {}
            Err(error) => tracing::error!(%error, "CI retention cleanup failed"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
