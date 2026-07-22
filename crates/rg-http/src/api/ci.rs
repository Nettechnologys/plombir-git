//! REST API handlers for CI/CD pipelines.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::AppState;

// ── Response types ───────────────────────────────────────────────

#[derive(Serialize)]
struct PipelineResponse {
    id: i64,
    repo_id: i64,
    commit_sha: String,
    ref_name: String,
    status: String,
    trigger_type: String,
    triggered_by: Option<i64>,
    started_at: Option<String>,
    finished_at: Option<String>,
    created_at: String,
}

#[derive(Serialize)]
struct StageResponse {
    id: i64,
    pipeline_id: i64,
    name: String,
    stage_order: i32,
    status: String,
    started_at: Option<String>,
    finished_at: Option<String>,
}

#[derive(Serialize)]
struct JobResponse {
    id: i64,
    stage_id: i64,
    name: String,
    image: Option<String>,
    script: String,
    when_condition: String,
    if_condition: Option<String>,
    allow_failure: bool,
    timeout_seconds: Option<i64>,
    environment_id: Option<i64>,
    environment_name: Option<String>,
    status: String,
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    log: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

#[derive(Serialize)]
struct PipelineDetailResponse {
    pipeline: PipelineResponse,
    stages: Vec<StageWithJobsResponse>,
}

#[derive(Serialize)]
struct StageWithJobsResponse {
    stage: StageResponse,
    jobs: Vec<JobResponse>,
}

#[derive(Deserialize)]
pub struct TriggerPipelineRequest {
    ref_name: Option<String>,
}

#[derive(Deserialize)]
pub struct ListPipelinesQuery {
    #[serde(flatten)]
    pagination: PaginationParams,
}

// ── Handlers ─────────────────────────────────────────────────────

/// GET /api/v1/repos/:owner/:name/pipelines
/// List all pipelines for a repository.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pipelines",
    tag = "CI/CD",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_pipelines(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<ListPipelinesQuery>,
) -> impl IntoResponse {
    let pagination = params.pagination.clamp();
    let offset = pagination.offset();
    let limit = pagination.limit();

    let repo = match resolve_repo_with_read_access(&state, &headers, &owner, &name).await {
        Ok(repo) => repo,
        Err(e) => return e.into_response(),
    };

    match rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(
        &state.db, repo.id, offset, limit,
    )
    .await
    {
        Ok((pipelines, total)) => {
            let resp: Vec<PipelineResponse> = pipelines
                .into_iter()
                .map(|p| PipelineResponse {
                    id: p.id,
                    repo_id: p.repo_id,
                    commit_sha: p.commit_sha,
                    ref_name: p.ref_name,
                    status: p.status,
                    trigger_type: p.trigger_type,
                    triggered_by: p.triggered_by,
                    started_at: p.started_at.map(|t| t.to_string()),
                    finished_at: p.finished_at.map(|t| t.to_string()),
                    created_at: p.created_at.to_string(),
                })
                .collect();
            Json(PaginatedResponse::new(resp, &pagination, total as u64)).into_response()
        }
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

/// GET /api/v1/repos/:owner/:name/pipelines/:id
/// Get pipeline detail with stages and jobs.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pipelines/{id}",
    tag = "CI/CD",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_pipeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, id)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let repo = match resolve_repo_with_read_access(&state, &headers, &owner, &name).await {
        Ok(repo) => repo,
        Err(e) => return e.into_response(),
    };

    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, id).await {
        Ok(Some(p)) => p,
        Ok(None) => return AppError::not_found("pipeline not found").into_response(),
        Err(e) => {
            return {
                tracing::error!(%e, "handler error");
                AppError::internal(e).into_response()
            };
        }
    };
    if pipeline.repo_id != repo.id {
        return AppError::not_found("pipeline not found").into_response();
    }

    let stages = match rg_db::ops::pipeline_ops::list_stages_by_pipeline(&state.db, id).await {
        Ok(s) => s,
        Err(e) => {
            return {
                tracing::error!(%e, "handler error");
                AppError::internal(e).into_response()
            };
        }
    };

    let mut stages_with_jobs: Vec<StageWithJobsResponse> = Vec::new();

    for stage in stages {
        let jobs = match rg_db::ops::pipeline_ops::list_jobs_by_stage(&state.db, stage.id).await {
            Ok(j) => j,
            Err(e) => {
                return {
                    tracing::error!(%e, "handler error");
                    AppError::internal(e).into_response()
                };
            }
        };

        stages_with_jobs.push(StageWithJobsResponse {
            stage: StageResponse {
                id: stage.id,
                pipeline_id: stage.pipeline_id,
                name: stage.name,
                stage_order: stage.stage_order,
                status: stage.status,
                started_at: stage.started_at.map(|t| t.to_string()),
                finished_at: stage.finished_at.map(|t| t.to_string()),
            },
            jobs: jobs
                .into_iter()
                .map(|j| JobResponse {
                    id: j.id,
                    stage_id: j.stage_id,
                    name: j.name,
                    image: j.image,
                    script: j.script,
                    when_condition: j.when_condition,
                    if_condition: j.if_condition,
                    allow_failure: j.allow_failure,
                    timeout_seconds: j.timeout_seconds,
                    environment_id: j.environment_id,
                    environment_name: j.environment_name,
                    status: j.status,
                    exit_code: j.exit_code,
                    log: j.log,
                    started_at: j.started_at.map(|t| t.to_string()),
                    finished_at: j.finished_at.map(|t| t.to_string()),
                })
                .collect(),
        });
    }

    let resp = PipelineDetailResponse {
        pipeline: PipelineResponse {
            id: pipeline.id,
            repo_id: pipeline.repo_id,
            commit_sha: pipeline.commit_sha,
            ref_name: pipeline.ref_name,
            status: pipeline.status,
            trigger_type: pipeline.trigger_type,
            triggered_by: pipeline.triggered_by,
            started_at: pipeline.started_at.map(|t| t.to_string()),
            finished_at: pipeline.finished_at.map(|t| t.to_string()),
            created_at: pipeline.created_at.to_string(),
        },
        stages: stages_with_jobs,
    };

    Json(resp).into_response()
}

/// GET /api/v1/repos/:owner/:name/pipelines/:id/jobs/:job_id
/// Get job detail with log.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}",
    tag = "CI/CD",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
        ("job_id" = i64, Path, description = "job_id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, pipeline_id, job_id)): Path<(String, String, i64, i64)>,
) -> impl IntoResponse {
    let repo = match resolve_repo_with_read_access(&state, &headers, &owner, &name).await {
        Ok(repo) => repo,
        Err(e) => return e.into_response(),
    };

    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, pipeline_id).await {
        Ok(Some(p)) if p.repo_id == repo.id => p,
        Ok(Some(_)) | Ok(None) => return AppError::not_found("pipeline not found").into_response(),
        Err(e) => {
            tracing::error!(%e, "handler error");
            return AppError::internal(e).into_response();
        }
    };

    match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(j)) => {
            if !job_belongs_to_pipeline(&state, pipeline.id, j.stage_id).await {
                return AppError::not_found("job not found").into_response();
            }
            Json(JobResponse {
                id: j.id,
                stage_id: j.stage_id,
                name: j.name,
                image: j.image,
                script: j.script,
                when_condition: j.when_condition,
                if_condition: j.if_condition,
                allow_failure: j.allow_failure,
                timeout_seconds: j.timeout_seconds,
                environment_id: j.environment_id,
                environment_name: j.environment_name,
                status: j.status,
                exit_code: j.exit_code,
                log: j.log,
                started_at: j.started_at.map(|t| t.to_string()),
                finished_at: j.finished_at.map(|t| t.to_string()),
            })
            .into_response()
        }
        Ok(None) => AppError::not_found("job not found").into_response(),
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

/// POST /api/v1/repos/:owner/:name/pipelines/:id/jobs/:job_id/play
/// Release a manual job and resume its persisted pipeline.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}/play",
    tag = "CI/CD",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "pipeline id"),
        ("job_id" = i64, Path, description = "manual job id"),
    ),
    responses(
        (status = 200, description = "Manual job released", body = serde_json::Value),
        (status = 400, description = "Job is not awaiting manual action", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn play_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, pipeline_id, job_id)): Path<(String, String, i64, i64)>,
) -> impl IntoResponse {
    let (repo, _) = match resolve_repo_with_write_access(&state, &headers, &owner, &name).await {
        Ok(access) => access,
        Err(error) => return error.into_response(),
    };
    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, pipeline_id).await {
        Ok(Some(pipeline)) if pipeline.repo_id == repo.id => pipeline,
        Ok(Some(_)) | Ok(None) => return AppError::not_found("pipeline not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(job)) if job_belongs_to_pipeline(&state, pipeline_id, job.stage_id).await => job,
        Ok(Some(_)) | Ok(None) => return AppError::not_found("job not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    if pipeline.status != "manual" || job.status != "manual" || job.when_condition != "manual" {
        return AppError::bad_request("job is not awaiting manual action").into_response();
    }
    let released = match rg_db::ops::pipeline_ops::play_manual_job(&state.db, job.id).await {
        Ok(released) => released,
        Err(error) => return AppError::internal(error).into_response(),
    };
    if !released {
        return AppError::bad_request("manual job was already released").into_response();
    }
    if let Err(error) =
        rg_db::ops::pipeline_ops::resume_pipeline_chain(&state.db, pipeline_id, job.stage_id).await
    {
        return AppError::internal(error).into_response();
    }

    let owner_display = match resolve_repo_storage_owner(&state, &repo, &owner).await {
        Ok(owner) => owner,
        Err(error) => return error.into_response(),
    };
    let repo_path = state
        .repo_root
        .join(format!("{}/{}.git", owner_display, name));
    if !repo_path.exists() {
        return AppError::not_found("repo path not found").into_response();
    }
    if let Err(error) = state
        .ci_engine
        .resume_pipeline(rg_core::ci::ResumePipelineParams {
            db: &state.db,
            repo_path: &repo_path,
            repo_id: repo.id,
            pipeline_id,
            docker_enabled: state.docker_enabled,
            external_runners: state.external_runners,
            jwt_secret: Some(&state.jwt_secret),
            external_url: state.external_url.as_deref(),
        })
        .await
    {
        return AppError::internal(error).into_response();
    }

    Json(serde_json::json!({
        "id": job_id,
        "pipeline_id": pipeline_id,
        "status": "pending"
    }))
    .into_response()
}

/// POST /api/v1/repos/:owner/:name/pipelines
/// Manually trigger a pipeline.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pipelines",
    tag = "CI/CD",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn trigger_pipeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<TriggerPipelineRequest>,
) -> impl IntoResponse {
    let (repo, actor_id) =
        match resolve_repo_with_write_access(&state, &headers, &owner, &name).await {
            Ok(access) => access,
            Err(e) => return e.into_response(),
        };

    let owner_display = match resolve_repo_storage_owner(&state, &repo, &owner).await {
        Ok(owner) => owner,
        Err(e) => return e.into_response(),
    };

    let repo_path = {
        // H-02: Validate owner/name before constructing repository path
        if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
            return AppError::bad_request(e.to_string()).into_response();
        }
        if let Err(e) = rg_core::platform::validate_repo_path(&name) {
            return AppError::bad_request(e.to_string()).into_response();
        }
        state
            .repo_root
            .join(format!("{}/{}.git", owner_display, name))
    };
    if !repo_path.exists() {
        return AppError::not_found("repo path not found").into_response();
    }

    // Resolve HEAD commit SHA
    let ref_name = body
        .ref_name
        .unwrap_or_else(|| "refs/heads/main".to_string());
    let commit_sha = match resolve_commit_sha(&repo_path, &ref_name) {
        Some(sha) => sha,
        None => return AppError::bad_request("cannot resolve commit SHA for ref").into_response(),
    };

    // Check if CI config exists
    if !state.ci_engine.has_ci_config(&repo_path, &commit_sha) {
        return AppError::bad_request("no .ironforge-ci.yml found").into_response();
    }

    match state
        .ci_engine
        .trigger_pipeline(rg_core::ci::TriggerPipelineParams {
            db: &state.db,
            repo_path: &repo_path,
            repo_id: repo.id,
            commit_sha: &commit_sha,
            ref_name: &ref_name,
            trigger_type: "manual",
            triggered_by: Some(actor_id),
            docker_enabled: state.docker_enabled,
            external_runners: state.external_runners,
            jwt_secret: Some(&state.jwt_secret),
            external_url: state.external_url.as_deref(),
        })
        .await
    {
        Ok(pipeline_id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": pipeline_id,
                "status": "pending",
                "commit_sha": commit_sha,
                "ref_name": ref_name,
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

/// POST /api/v1/repos/:owner/:name/pipelines/:id/retry
/// Retry a failed pipeline.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pipelines/{id}/retry",
    tag = "CI/CD",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn retry_pipeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, id)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let (repo, actor_id) =
        match resolve_repo_with_write_access(&state, &headers, &owner, &name).await {
            Ok(access) => access,
            Err(e) => return e.into_response(),
        };

    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, id).await {
        Ok(Some(p)) => p,
        Ok(None) => return AppError::not_found("pipeline not found").into_response(),
        Err(e) => {
            return {
                tracing::error!(%e, "handler error");
                AppError::internal(e).into_response()
            };
        }
    };
    if pipeline.repo_id != repo.id {
        return AppError::not_found("pipeline not found").into_response();
    }

    let owner_display = match resolve_repo_storage_owner(&state, &repo, &owner).await {
        Ok(owner) => owner,
        Err(e) => return e.into_response(),
    };

    let repo_path = {
        // H-02: Validate owner/name before constructing repository path
        if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
            return AppError::bad_request(e.to_string()).into_response();
        }
        if let Err(e) = rg_core::platform::validate_repo_path(&name) {
            return AppError::bad_request(e.to_string()).into_response();
        }
        state
            .repo_root
            .join(format!("{}/{}.git", owner_display, name))
    };
    if !repo_path.exists() {
        return AppError::not_found("repo path not found").into_response();
    }

    match state
        .ci_engine
        .trigger_pipeline(rg_core::ci::TriggerPipelineParams {
            db: &state.db,
            repo_path: &repo_path,
            repo_id: pipeline.repo_id,
            commit_sha: &pipeline.commit_sha,
            ref_name: &pipeline.ref_name,
            trigger_type: "retry",
            triggered_by: Some(actor_id),
            docker_enabled: state.docker_enabled,
            external_runners: state.external_runners,
            jwt_secret: Some(&state.jwt_secret),
            external_url: state.external_url.as_deref(),
        })
        .await
    {
        Ok(new_id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": new_id,
                "status": "pending",
                "original_pipeline_id": id,
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

/// POST /api/v1/repos/:owner/:name/pipelines/:id/cancel
/// Cancel a running pipeline.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pipelines/{id}/cancel",
    tag = "CI/CD",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn cancel_pipeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, id)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let (repo, _) = match resolve_repo_with_write_access(&state, &headers, &owner, &name).await {
        Ok(access) => access,
        Err(e) => return e.into_response(),
    };

    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, id).await {
        Ok(Some(p)) => p,
        Ok(None) => return AppError::not_found("pipeline not found").into_response(),
        Err(e) => {
            return {
                tracing::error!(%e, "handler error");
                AppError::internal(e).into_response()
            };
        }
    };
    if pipeline.repo_id != repo.id {
        return AppError::not_found("pipeline not found").into_response();
    }

    if pipeline.status != "running"
        && pipeline.status != "pending"
        && pipeline.status != "manual"
        && pipeline.status != "waiting_approval"
    {
        return AppError::bad_request("pipeline is not active").into_response();
    }

    let now = chrono::Utc::now().naive_utc();

    // Mark pipeline as canceled
    if let Err(e) =
        rg_db::ops::pipeline_ops::update_pipeline_status(&state.db, id, "canceled", None, Some(now))
            .await
    {
        return {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        };
    }

    // Mark all running stages/jobs as canceled
    let stages = match rg_db::ops::pipeline_ops::list_stages_by_pipeline(&state.db, id).await {
        Ok(s) => s,
        Err(e) => {
            return {
                tracing::error!(%e, "handler error");
                AppError::internal(e).into_response()
            };
        }
    };

    for stage in stages {
        if matches!(
            stage.status.as_str(),
            "running" | "pending" | "manual" | "waiting_approval"
        ) {
            if let Err(e) = rg_db::ops::pipeline_ops::update_stage_status(
                &state.db,
                stage.id,
                "canceled",
                None,
                Some(now),
            )
            .await
            {
                tracing::error!(stage_id = stage.id, error = %e, "Failed to cancel stage");
            }

            let jobs = match rg_db::ops::pipeline_ops::list_jobs_by_stage(&state.db, stage.id).await
            {
                Ok(j) => j,
                Err(_) => continue,
            };

            for job in jobs {
                if matches!(
                    job.status.as_str(),
                    "running" | "pending" | "manual" | "waiting_approval"
                ) {
                    if let Err(e) = rg_db::ops::pipeline_ops::update_job_result(
                        &state.db,
                        job.id,
                        "canceled",
                        None,
                        None,
                        None,
                        Some(now),
                    )
                    .await
                    {
                        tracing::error!(job_id = job.id, error = %e, "Failed to cancel job");
                    }
                }
            }
        }
    }

    Json(serde_json::json!({"id": id, "status": "canceled"})).into_response()
}

// ── Helpers ──────────────────────────────────────────────────────

async fn resolve_repo(
    state: &AppState,
    owner: &str,
    name: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, name)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("repository not found"))
}

async fn resolve_repo_with_read_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    let repo = resolve_repo(state, owner, name).await?;
    let actor_id = crate::api::auth::extract_user_id(headers, &state.jwt_secret);

    match rg_core::repo::service::can_read_repo(&state.db, &repo, actor_id).await {
        Ok(true) => Ok(repo),
        Ok(false) if repo.is_private && actor_id.is_none() => {
            Err(AppError::unauthorized("authentication required"))
        }
        Ok(false) => Err(AppError::forbidden("access denied")),
        Err(e) => Err(AppError::internal(e)),
    }
}

async fn resolve_repo_with_write_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<(rg_db::entities::repository::Model, i64), AppError> {
    let actor_id = crate::api::auth::extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let repo = resolve_repo(state, owner, name).await?;

    match rg_core::repo::service::can_write_repo(&state.db, &repo, Some(actor_id)).await {
        Ok(true) => Ok((repo, actor_id)),
        Ok(false) => Err(AppError::forbidden("write access denied")),
        Err(e) => Err(AppError::internal(e)),
    }
}

async fn resolve_repo_storage_owner(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    route_owner: &str,
) -> Result<String, AppError> {
    if repo.org_id.is_some() {
        return Ok(route_owner.to_string());
    }

    rg_db::ops::user_ops::find_by_id(&state.db, repo.owner_id)
        .await
        .map_err(AppError::internal)?
        .map(|user| user.username)
        .ok_or_else(|| AppError::internal("repository owner not found"))
}

async fn job_belongs_to_pipeline(state: &AppState, pipeline_id: i64, stage_id: i64) -> bool {
    match rg_db::ops::pipeline_ops::list_stages_by_pipeline(&state.db, pipeline_id).await {
        Ok(stages) => stages.iter().any(|stage| stage.id == stage_id),
        Err(e) => {
            tracing::error!(%e, pipeline_id, stage_id, "failed to verify job pipeline ownership");
            false
        }
    }
}

fn resolve_commit_sha(repo_path: &std::path::Path, ref_name: &str) -> Option<String> {
    let repo = gix::open(repo_path).ok()?;

    // Try to parse the ref directly
    let ref_name_normalized = if ref_name.starts_with("refs/") {
        ref_name.to_string()
    } else {
        format!("refs/heads/{}", ref_name)
    };

    match repo.rev_parse_single(ref_name_normalized.as_str()) {
        Ok(id) => Some(id.to_string()),
        Err(_) => {
            // Try without refs/heads/ prefix
            let short = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);
            repo.rev_parse_single(short).ok().map(|id| id.to_string())
        }
    }
}
