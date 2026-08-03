//! REST API handlers for CI/CD pipelines.

use anyhow::Context;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::repo_access::{RepoRead, RepoWrite};
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

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListPipelinesQuery {
    #[serde(flatten)]
    #[param(ignore)]
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
        ListPipelinesQuery,
        PaginationParams,
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_pipelines(
    State(state): State<AppState>,
    RepoRead { repo }: RepoRead,
    Path((_, _)): Path<(String, String)>,
    Query(params): Query<ListPipelinesQuery>,
) -> impl IntoResponse {
    let pagination = params.pagination.clamp();
    let offset = pagination.offset();
    let limit = pagination.limit();

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
        Err(e) => AppError::from(e).into_response(),
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
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    let pipeline = match pipeline_in_repo(&state, &repo, id).await {
        Ok(pipeline) => pipeline,
        Err(error) => return error.into_response(),
    };

    let stages = match rg_db::ops::pipeline_ops::list_stages_by_pipeline(&state.db, id).await {
        Ok(s) => s,
        Err(e) => return AppError::from(e).into_response(),
    };

    let mut stages_with_jobs: Vec<StageWithJobsResponse> = Vec::new();

    for stage in stages {
        let jobs = match rg_db::ops::pipeline_ops::list_jobs_by_stage(&state.db, stage.id).await {
            Ok(j) => j,
            Err(e) => return AppError::from(e).into_response(),
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
    Path((_, _, pipeline_id, job_id)): Path<(String, String, i64, i64)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    let pipeline = match pipeline_in_repo(&state, &repo, pipeline_id).await {
        Ok(pipeline) => pipeline,
        Err(error) => return error.into_response(),
    };

    match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(j)) => {
            let belongs = match job_belongs_to_pipeline(&state.db, pipeline.id, j.stage_id).await {
                Ok(belongs) => belongs,
                Err(error) => return error.into_response(),
            };
            if !belongs {
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
        Err(e) => AppError::from(e).into_response(),
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
        (status = 409, description = "Job is not awaiting manual action, or was already released", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn play_job(
    State(state): State<AppState>,
    Path((owner, name, pipeline_id, job_id)): Path<(String, String, i64, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    let pipeline = match pipeline_in_repo(&state, &repo, pipeline_id).await {
        Ok(pipeline) => pipeline,
        Err(error) => return error.into_response(),
    };
    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(job)) => job,
        Ok(None) => return AppError::not_found("job not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    let belongs = match job_belongs_to_pipeline(&state.db, pipeline_id, job.stage_id).await {
        Ok(belongs) => belongs,
        Err(error) => return error.into_response(),
    };
    if !belongs {
        return AppError::not_found("job not found").into_response();
    }
    // Nothing is wrong with the request here: the path is valid, the caller is
    // allowed, the body parsed. The *job* is in a state that has no manual
    // action left to take. A 400 tells the client to fix a request that has
    // nothing to fix; a 409 tells it to re-read the state, which is the only
    // useful thing it can do.
    if pipeline.status != "manual" || job.status != "manual" || job.when_condition != "manual" {
        return AppError::conflict("job is not awaiting manual action").into_response();
    }
    let released = match rg_db::ops::pipeline_ops::play_manual_job(&state.db, job.id).await {
        Ok(released) => released,
        Err(error) => return AppError::from(error).into_response(),
    };
    if !released {
        // Losing this race is the textbook `Conflict`: a double click, or a
        // retry after a timeout whose first attempt actually landed.
        return AppError::conflict("manual job was already released").into_response();
    }
    if let Err(error) =
        rg_db::ops::pipeline_ops::resume_pipeline_chain(&state.db, pipeline_id, job.stage_id).await
    {
        return AppError::from(error).into_response();
    }

    let owner_display = match resolve_repo_storage_owner(&state, &repo, &owner).await {
        Ok(owner) => owner,
        Err(error) => return error.into_response(),
    };
    let repo_path = state
        .repo_root
        .join(format!("{}/{}.git", owner_display, name));
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
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
            allow_host_runner: state.allow_host_runner,
            jwt_secret: Some(&state.jwt_secret),
            encryption_key: Some(&state.encryption_key),
            external_url: state.external_url.as_deref(),
        })
        .await
    {
        return AppError::from(error).into_response();
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
    Path((owner, name)): Path<(String, String)>,
    RepoWrite { repo, actor_id }: RepoWrite,
    Json(body): Json<TriggerPipelineRequest>,
) -> impl IntoResponse {
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
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
    }

    // Resolve HEAD commit SHA
    let ref_name = body
        .ref_name
        .unwrap_or_else(|| "refs/heads/main".to_string());
    let commit_sha = match resolve_commit_sha(&repo_path, &ref_name) {
        Ok(Some(sha)) => sha,
        Ok(None) => {
            return AppError::bad_request("cannot resolve commit SHA for ref").into_response()
        }
        Err(e) => return AppError::from(e).into_response(),
    };

    // Check if CI config exists. The wording lives in `rg_core::ci` next to the
    // gate itself: this used to say `no .forgekeep-ci.yml found`, which lied to
    // every repository driving CI from `.gitea/workflows/`.
    if !state.ci_engine.has_ci_config(&repo_path, &commit_sha) {
        return AppError::bad_request(rg_core::ci::NO_CI_CONFIG_MESSAGE).into_response();
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
            base_branch: None,
            triggered_by: Some(actor_id),
            docker_enabled: state.docker_enabled,
            external_runners: state.external_runners,
            allow_host_runner: state.allow_host_runner,
            jwt_secret: Some(&state.jwt_secret),
            encryption_key: Some(&state.encryption_key),
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
        Err(e) => AppError::from(e).into_response(),
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
    Path((owner, name, id)): Path<(String, String, i64)>,
    RepoWrite { repo, actor_id }: RepoWrite,
) -> impl IntoResponse {
    let pipeline = match pipeline_in_repo(&state, &repo, id).await {
        Ok(pipeline) => pipeline,
        Err(error) => return error.into_response(),
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
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
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
            base_branch: None,
            triggered_by: Some(actor_id),
            docker_enabled: state.docker_enabled,
            external_runners: state.external_runners,
            allow_host_runner: state.allow_host_runner,
            jwt_secret: Some(&state.jwt_secret),
            encryption_key: Some(&state.encryption_key),
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
        Err(e) => AppError::from(e).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "Pipeline is not active", body = serde_json::Value),
    ),
)]
pub async fn cancel_pipeline(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    if let Err(error) = pipeline_in_repo(&state, &repo, id).await {
        return error.into_response();
    }

    match rg_db::ops::pipeline_ops::cancel_pipeline_chain(&state.db, id).await {
        Ok(true) => Json(serde_json::json!({"id": id, "status": "canceled"})).into_response(),
        // The pipeline finished on its own before the cancel landed. The
        // request was fine; the state moved.
        Ok(false) => AppError::conflict("pipeline is not active").into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

// ── Helpers ──────────────────────────────────────────────────────

/// Re-tie a pipeline id to the repository the gate admitted.
///
/// `{id}` / `{pipeline_id}` is an instance-wide `pipelines` primary key while
/// `RepoRead` / `RepoWrite` only ever prove something about `{owner}/{name}`, so
/// without this any pipeline on the instance could be read, retried or canceled
/// through the URL of a repository the caller happens to have access to. A
/// mismatch answers 404 rather than 403: a 403 would still confirm the id
/// exists, which is most of what an id-walking caller wants to learn.
///
/// Five handlers in this file spelled the comparison inline, in three different
/// shapes — `if pipeline.repo_id != repo.id`, a guarded `match` arm, and the
/// same arm again — and a comparison is the one form `global_id_anchor_guard`
/// cannot read: it sees a *call*, so an inline `if` is indistinguishable from no
/// anchor at all. Naming it is what puts `api/ci.rs` in that guard's `ANCHORED`
/// table, so the sixth pipeline route to be written is held to the anchor by the
/// build rather than by review.
///
/// It is `pub(crate)` because `api/artifacts.rs` and `api/ci_environments.rs`
/// need the identical rule and used to carry their own copies — the generator
/// this whole family of defects comes from is "the gate was copied into the next
/// module and drifted from the original". The copies had already drifted: the
/// artifacts one converted a database failure through `AppError::internal`,
/// which is an unconditional 500 and skips the `From<anyhow::Error>`
/// classification that turns a connection outage into a retryable 503.
pub(crate) async fn pipeline_in_repo(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    pipeline_id: i64,
) -> Result<rg_db::entities::pipeline::Model, AppError> {
    match rg_db::ops::pipeline_ops::get_pipeline(&state.db, pipeline_id).await {
        Ok(Some(pipeline)) if pipeline.repo_id == repo.id => Ok(pipeline),
        Ok(_) => Err(AppError::not_found("pipeline not found")),
        Err(error) => Err(AppError::from(error)),
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
        .map_err(AppError::from)?
        .map(|user| user.username)
        .ok_or_else(|| AppError::internal("repository owner not found"))
}

/// Keep "this stage belongs elsewhere" separate from "the ownership lookup
/// could not run". Callers may turn only the first outcome into a masked 404;
/// database failures keep their `AppError` classification (503 for an outage).
async fn job_belongs_to_pipeline(
    db: &rg_db::DatabaseConnection,
    pipeline_id: i64,
    stage_id: i64,
) -> Result<bool, AppError> {
    rg_db::ops::pipeline_ops::list_stages_by_pipeline(db, pipeline_id)
        .await
        .map(|stages| stages.iter().any(|stage| stage.id == stage_id))
        .map_err(AppError::from)
}

fn resolve_commit_sha(
    repo_path: &std::path::Path,
    ref_name: &str,
) -> anyhow::Result<Option<String>> {
    // Same class as the CI gate in `rg_core::ci::has_ci_config`: `.ok()?` turned
    // "the server cannot open this repository" into "your ref is wrong" (the
    // caller answers 400 `cannot resolve commit SHA for ref`), with nothing in
    // the log to tell the two apart.
    let repo = match gix::open(repo_path) {
        Ok(repo) => repo,
        Err(error) => {
            tracing::warn!(
                repo = %repo_path.display(),
                "cannot open repository while resolving a commit SHA: {:#}",
                error
            );
            return Err(crate::error::repository_storage_open_error(
                repo_path, error,
            ));
        }
    };

    // A reference lookup has the required three-valued contract: `None` is a
    // genuinely absent client ref, while I/O, malformed ref data and a missing
    // target object stay `Err` and become a retryable server failure.
    let ref_name_normalized = if ref_name.starts_with("refs/") {
        ref_name.to_string()
    } else {
        format!("refs/heads/{}", ref_name)
    };

    let mut reference = match repo
        .try_find_reference(ref_name_normalized.as_str())
        .with_context(|| format!("failed to look up ref {ref_name_normalized}"))?
    {
        Some(reference) => reference,
        None => return Ok(None),
    };
    let id = reference
        .peel_to_id()
        .with_context(|| format!("failed to peel ref {ref_name_normalized}"))?;
    id.object()
        .with_context(|| format!("failed to read object for ref {ref_name_normalized}"))?;
    Ok(Some(id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit_repo() -> (tempfile::TempDir, String, String) {
        let temp = tempfile::tempdir().expect("create repository tempdir");
        let git = rg_git::cli_gateway::GitCommandGateway::new().expect("git must be installed");
        git.run_or_bail(&["init", "-q", temp.path().to_str().unwrap()], None)
            .expect("init repository");
        for args in [
            vec!["config", "user.email", "ci@example.com"],
            vec!["config", "user.name", "CI"],
            vec!["commit", "--allow-empty", "-qm", "fixture"],
        ] {
            git.run_or_bail(&args, Some(temp.path()))
                .expect("create fixture commit");
        }
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .expect("resolve HEAD")
            .stdout_str()
            .trim()
            .to_string();
        let branch = git
            .run(&["symbolic-ref", "--short", "HEAD"], Some(temp.path()))
            .expect("resolve branch")
            .stdout_str()
            .trim()
            .to_string();
        (temp, sha, branch)
    }

    #[test]
    fn an_absent_ref_stays_a_client_negative_answer() {
        let (temp, _, _) = commit_repo();
        assert_eq!(
            resolve_commit_sha(temp.path(), "refs/heads/missing").unwrap(),
            None
        );
    }

    #[test]
    fn a_ref_with_a_missing_commit_object_is_an_error() {
        let (temp, sha, branch) = commit_repo();
        let object_path = temp
            .path()
            .join(".git/objects")
            .join(&sha[..2])
            .join(&sha[2..]);
        assert!(
            object_path.exists(),
            "fixture must keep HEAD as a loose object"
        );
        std::fs::remove_file(object_path).expect("remove commit object");

        let error = resolve_commit_sha(temp.path(), &branch)
            .expect_err("a dangling ref must not become an absent client ref");
        assert!(
            format!("{error:#}").contains("failed to peel ref"),
            "error must retain the object-store failure: {error:#}"
        );
    }

    #[tokio::test]
    async fn job_pipeline_ownership_connection_outage_is_retryable() {
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("connect test database");
        db.clone().close().await.expect("close test database");

        let error = job_belongs_to_pipeline(&db, 1, 1)
            .await
            .expect_err("a closed pool cannot answer the ownership check");

        assert_eq!(
            error.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a connection outage must stay retryable instead of becoming false"
        );
    }
}
