//! REST API handlers for CI/CD Runners.

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};

use super::auth::extract_user_id;
use crate::error::AppError;
use crate::AppState;
use utoipa::{IntoParams, ToSchema};

/// Verify the current request is from an authenticated admin user.
/// Returns `Some(user_id)` on success, `None` otherwise.
async fn require_admin(state: &AppState, headers: &HeaderMap) -> Option<i64> {
    let user_id = extract_user_id(headers, &state.jwt_secret)?;
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .ok()??;
    if user.is_admin {
        Some(user_id)
    } else {
        None
    }
}

// ── Request/Response types ─────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct RegisterRunnerRequest {
    pub name: String,
    pub labels: Option<Vec<String>>,
    pub version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct RegisterRunnerResponse {
    id: i64,
    token: String,
    message: String,
}

#[derive(Serialize, ToSchema)]
pub struct HeartbeatResponse {
    status: String,
    server_time: String,
}

#[derive(Serialize, ToSchema)]
pub struct PollJobResponse {
    job_id: i64,
    pipeline_id: i64,
    stage_id: i64,
    name: String,
    script: Vec<String>,
    image: Option<String>,
    variables: Option<serde_json::Value>,
    cache_key: Option<String>,
    cache_paths: Option<Vec<String>>,
    timeout: i64,
}

#[derive(Deserialize, IntoParams)]
pub struct PollJobQuery {
    pub timeout: Option<u64>, // seconds, default 30
}

#[derive(Serialize, ToSchema)]
pub struct RunnerInfoResponse {
    id: i64,
    name: String,
    status: String,
    labels: String,
    last_seen_at: String,
    version: Option<String>,
    os: Option<String>,
    arch: Option<String>,
}

/// GET /api/v1/admin/runners/:id
/// Get runner details (admin only).
#[utoipa::path(
    get,
    path = "/admin/runners/{id}",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = RunnerInfoResponse),
        (status = 404, description = "Not found", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_runner_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(runner_id): Path<i64>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::unauthorized("admin authentication required").into_response();
    }

    match rg_db::ops::runner_ops::find_by_id(&state.db, runner_id).await {
        Ok(Some(r)) => (
            StatusCode::OK,
            Json(RunnerInfoResponse {
                id: r.id,
                name: r.name,
                status: r.status,
                labels: r.labels,
                last_seen_at: r.last_seen_at.to_string(),
                version: r.version,
                os: r.os,
                arch: r.arch,
            }),
        )
            .into_response(),
        Ok(None) => AppError::not_found("runner not found").into_response(),
        Err(e) => {
            tracing::error!(%e, "get_runner_admin failed");
            AppError::internal(e).into_response()
        }
    }
}

// ── Handlers ─────────────────────────────────────────────

/// POST /api/v1/runners/register
/// Register a new runner and receive a token.
#[utoipa::path(
    post,
    path = "/runners/register",
    tag = "Runners",
    request_body(content = RegisterRunnerRequest, description = "Runner registration info"),
    responses(
        (status = 201, description = "Created", body = RegisterRunnerResponse),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RegisterRunnerRequest>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::forbidden("admin authentication required to register runners")
            .into_response();
    }

    let labels_json =
        serde_json::to_string(&req.labels.unwrap_or_default()).unwrap_or_else(|_| "[]".to_string());

    match rg_db::ops::runner_ops::register_runner(
        &state.db,
        &req.name,
        &labels_json,
        req.version.as_deref(),
        req.os.as_deref(),
        req.arch.as_deref(),
    )
    .await
    {
        Ok(runner) => (
            StatusCode::CREATED,
            Json(RegisterRunnerResponse {
                id: runner.id,
                token: runner.token,
                message: "Runner registered successfully".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(%e, "register runner failed");
            AppError::internal(e).into_response()
        }
    }
}

/// POST /api/v1/runners/:id/heartbeat
/// Update runner heartbeat (called every 30 seconds).
/// Auth handled by `authenticate_runner` middleware.
#[utoipa::path(
    post,
    path = "/runners/{id}/heartbeat",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
    ),
    responses(
        (status = 200, description = "Success", body = HeartbeatResponse),
    ),
)]
pub async fn heartbeat(
    State(_state): State<AppState>,
    Path(_runner_id): Path<i64>,
) -> impl IntoResponse {
    // Heartbeat is already updated by the authenticate_runner middleware
    (
        StatusCode::OK,
        Json(HeartbeatResponse {
            status: "ok".to_string(),
            server_time: chrono::Utc::now().to_rfc3339(),
        }),
    )
        .into_response()
}

/// POST /api/v1/runners/:id/deregister
/// Deregister a runner — removes it from the pool.
/// Auth handled by `authenticate_runner` middleware.
#[utoipa::path(
    post,
    path = "/runners/{id}/deregister",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
    ),
    responses(
        (status = 200, description = "Runner deregistered"),
        (status = 404, description = "Runner not found"),
    ),
)]
pub async fn deregister(
    State(state): State<AppState>,
    Path(runner_id): Path<i64>,
) -> impl IntoResponse {
    // Reset any jobs assigned to this runner so they can be picked up by others
    if let Err(e) = rg_db::ops::pipeline_ops::reset_runner_jobs(&state.db, runner_id).await {
        tracing::warn!(runner_id, error = %e, "Failed to reset runner jobs during deregistration");
    }

    match rg_db::ops::runner_ops::delete_runner(&state.db, runner_id).await {
        Ok(true) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "deregistered"})),
        )
            .into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "runner not found"})),
        )
            .into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

/// GET /api/v1/runners/:id/jobs/poll?timeout=30
/// Long-polling endpoint for runners to fetch jobs.
/// Auth handled by `authenticate_runner` middleware.
#[utoipa::path(
    get,
    path = "/runners/{id}/jobs/poll",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("timeout" = Option<u64>, Query, description = "Poll timeout in seconds (default: 30, max: 300)"),
    ),
    responses(
        (status = 200, description = "Job assigned", body = PollJobResponse),
        (status = 204, description = "No job available (timeout)"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn poll_job(
    State(state): State<AppState>,
    Path(runner_id): Path<i64>,
    Query(query): Query<PollJobQuery>,
) -> impl IntoResponse {
    let timeout_secs = query.timeout.unwrap_or(30).min(300);

    // Use tokio::time::timeout to wrap a polling loop
    let poll_future = async {
        // Fetch runner labels for tag matching
        let runner_labels: Vec<String> =
            match rg_db::ops::runner_ops::find_by_id(&state.db, runner_id).await {
                Ok(Some(r)) => serde_json::from_str(&r.labels).unwrap_or_default(),
                _ => Vec::new(),
            };

        loop {
            match rg_db::ops::pipeline_ops::find_pending_job_matching_labels(
                &state.db,
                &runner_labels,
            )
            .await
            {
                Ok(Some(job)) => {
                    // Found a job — assign it to this runner
                    if let Err(e) =
                        rg_db::ops::pipeline_ops::assign_job(&state.db, job.id, runner_id).await
                    {
                        eprintln!("[poll_job] failed to assign job {}: {}", job.id, e);
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        continue;
                    }
                    // Mark job as assigned
                    let now = Some(chrono::Utc::now().naive_utc());
                    if let Err(e) = rg_db::ops::pipeline_ops::update_job_result(
                        &state.db, job.id, "assigned", None, None, now, None,
                    )
                    .await
                    {
                        tracing::error!(job_id = job.id, error = %e, "Failed to update job result to assigned");
                    }

                    // Fetch stage to get pipeline_id
                    let mut pipeline_id = 0i64;
                    let mut pipeline = None;
                    if let Ok(Some(stage)) =
                        rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await
                    {
                        pipeline_id = stage.pipeline_id;
                        pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, pipeline_id)
                            .await
                            .ok()
                            .flatten();
                    }

                    let mut variables = job
                        .variables
                        .as_deref()
                        .and_then(|json| {
                            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
                                .ok()
                        })
                        .unwrap_or_default();
                    for reserved in [
                        "CI",
                        "IRONFORGE",
                        "CI_PIPELINE_ID",
                        "CI_COMMIT_SHA",
                        "CI_SHA",
                        "CI_REF",
                        "CI_EVENT",
                        "CI_JOB_TOKEN",
                        "CI_OIDC_TOKEN_URL",
                    ] {
                        variables.remove(reserved);
                    }
                    if let Some(pipeline) = &pipeline {
                        match decrypted_repo_secrets(&state, pipeline.repo_id).await {
                            Ok(secrets) => {
                                for (name, value) in secrets {
                                    variables.insert(name, serde_json::json!(value));
                                }
                            }
                            Err(error) => {
                                tracing::error!(pipeline_id, %error, "failed to load CI secrets for external runner");
                                return Err(AppError::internal(
                                    "failed to prepare job environment",
                                )
                                .into_response());
                            }
                        }
                    }
                    variables.insert("CI".into(), serde_json::json!("true"));
                    variables.insert("IRONFORGE".into(), serde_json::json!("true"));
                    variables.insert("CI_PIPELINE_ID".into(), serde_json::json!(pipeline_id));
                    if let Some(pipeline) = &pipeline {
                        variables.insert(
                            "CI_COMMIT_SHA".into(),
                            serde_json::json!(pipeline.commit_sha),
                        );
                        variables.insert("CI_SHA".into(), serde_json::json!(pipeline.commit_sha));
                        variables.insert("CI_REF".into(), serde_json::json!(pipeline.ref_name));
                        variables
                            .insert("CI_EVENT".into(), serde_json::json!(pipeline.trigger_type));
                        if let Ok(token) = rg_core::auth::ci_token::generate_ci_job_token_with_ttl(
                            pipeline.repo_id,
                            pipeline.id,
                            job.id,
                            "repo:read packages:read",
                            &state.jwt_secret,
                            job.timeout_seconds
                                .unwrap_or(state.job_timeout_secs as i64)
                                .clamp(60, 86_400)
                                + 300,
                        ) {
                            variables.insert("CI_JOB_TOKEN".into(), serde_json::json!(token));
                        }
                        if let Some(url) = state.external_url.as_deref() {
                            variables.insert(
                                "CI_OIDC_TOKEN_URL".into(),
                                serde_json::json!(format!(
                                    "{}/api/v1/ci/oidc/token",
                                    url.trim_end_matches('/')
                                )),
                            );
                        }
                    }

                    let resp = PollJobResponse {
                        job_id: job.id,
                        pipeline_id,
                        stage_id: job.stage_id,
                        name: job.name,
                        script: job.script.lines().map(|s| s.to_string()).collect(),
                        image: job.image,
                        variables: Some(serde_json::Value::Object(variables)),
                        cache_key: job.cache_key,
                        cache_paths: job
                            .cache_paths
                            .as_deref()
                            .and_then(|json| serde_json::from_str(json).ok()),
                        timeout: job
                            .timeout_seconds
                            .unwrap_or(state.job_timeout_secs as i64)
                            .clamp(1, 86_400),
                    };
                    return Ok((StatusCode::OK, Json(resp)));
                }
                Ok(None) => {
                    // No job yet — wait and retry
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    continue;
                }
                Err(e) => {
                    // H-05: Log the full error, return sanitized response
                    tracing::error!(%e, "[poll_job] database error while finding pending job");
                    return Err(AppError::internal(e).into_response());
                }
            }
        }
    };

    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), poll_future).await {
        Ok(Ok(resp)) => resp.into_response(),
        Ok(Err(resp)) => resp.into_response(),
        Err(_elapsed) => (StatusCode::NO_CONTENT, Json(serde_json::json!({}))).into_response(),
    }
}

/// POST /api/v1/runners/:id/jobs/:job_id/start
/// Notify server that the runner has started executing a job.
#[utoipa::path(
    post,
    path = "/runners/{id}/jobs/{job_id}/start",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID"),
    ),
    responses(
        (status = 200, description = "Job started", body = serde_json::Value),
        (status = 403, description = "Forbidden - job not assigned to this runner", body = serde_json::Value),
        (status = 404, description = "Job not found", body = serde_json::Value),
    ),
)]
pub async fn start_job(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
) -> impl IntoResponse {
    // Verify the job is assigned to this runner
    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => {
            return AppError::not_found("job not found").into_response();
        }
        Err(e) => {
            tracing::error!(%e, "start_job: get_job failed");
            return AppError::internal(e).into_response();
        }
    };

    if job.runner_id != Some(runner_id) {
        return AppError::forbidden("job not assigned to this runner").into_response();
    }

    let now = Some(chrono::Utc::now().naive_utc());
    if let Err(e) = rg_db::ops::pipeline_ops::update_job_result(
        &state.db, job_id, "running", None, None, now, None,
    )
    .await
    {
        tracing::error!(%e, "start_job: update_job_result failed");
        return AppError::internal(e).into_response();
    }

    // Mark runner as busy
    if let Err(e) = rg_db::ops::runner_ops::update_status(&state.db, runner_id, "busy").await {
        tracing::error!(runner_id, error = %e, "Failed to mark runner as busy");
    }

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

/// POST /api/v1/runners/:id/jobs/:job_id/log
/// Upload job log (streaming or batch).
#[utoipa::path(
    post,
    path = "/runners/{id}/jobs/{job_id}/log",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID"),
    ),
    request_body(content = String, description = "Log content (plain text)"),
    responses(
        (status = 200, description = "Log uploaded", body = serde_json::Value),
        (status = 403, description = "Forbidden - job not assigned to this runner", body = serde_json::Value),
        (status = 404, description = "Job not found", body = serde_json::Value),
    ),
)]
pub async fn upload_log(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    body: String,
) -> impl IntoResponse {
    // Verify the job is assigned to this runner
    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => {
            return AppError::not_found("job not found").into_response();
        }
        Err(e) => {
            tracing::error!(%e, "upload_log: get_job failed");
            return AppError::internal(e).into_response();
        }
    };

    if job.runner_id != Some(runner_id) {
        return AppError::forbidden("job not assigned to this runner").into_response();
    }

    let body = match secrets_for_job(&state, job.stage_id).await {
        Ok(secrets) => rg_core::auth::encryption::mask_values(&body, &secrets),
        Err(error) => {
            tracing::error!(job_id, %error, "failed to load secrets while masking runner log");
            return AppError::internal("failed to sanitize job log").into_response();
        }
    };

    // Broadcast only the server-sanitized log via WebSocket to frontend.
    crate::ws::push_job_log(&state.notification_hub, job_id, &body).await;

    // Write log through the queue to serialise concurrent writes and
    // avoid SQLITE_BUSY under high concurrency.
    state.log_write_queue.write(job_id, &body).await;

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

/// Download a tar snapshot of the exact commit assigned to an external job.
pub async fn download_workspace(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
) -> impl IntoResponse {
    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(job)) if job.runner_id == Some(runner_id) => job,
        Ok(Some(_)) => {
            return AppError::forbidden("job not assigned to this runner").into_response()
        }
        Ok(None) => return AppError::not_found("job not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let stage = match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await {
        Ok(Some(stage)) => stage,
        Ok(None) => return AppError::not_found("pipeline stage not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id).await
    {
        Ok(Some(pipeline)) => pipeline,
        Ok(None) => return AppError::not_found("pipeline not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let repository = match rg_db::entities::repository::Entity::find_by_id(pipeline.repo_id)
        .one(&state.db)
        .await
    {
        Ok(Some(repository)) => repository,
        Ok(None) => return AppError::not_found("repository not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let namespace = if let Some(org_id) = repository.org_id {
        match rg_db::ops::org_ops::get_org(&state.db, org_id).await {
            Ok(Some(org)) => org.name,
            Ok(None) => {
                return AppError::not_found("repository organization not found").into_response()
            }
            Err(error) => return AppError::internal(error).into_response(),
        }
    } else {
        match rg_db::ops::user_ops::find_by_id(&state.db, repository.owner_id).await {
            Ok(Some(user)) => user.username,
            Ok(None) => return AppError::not_found("repository owner not found").into_response(),
            Err(error) => return AppError::internal(error).into_response(),
        }
    };
    let repo_path = state
        .repo_root
        .join(namespace)
        .join(format!("{}.git", repository.name));
    let commit_sha = pipeline.commit_sha.clone();
    let archive = match tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let gateway = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let output = gateway.run(&["archive", "--format=tar", &commit_sha], Some(&repo_path))?;
        if !output.success() {
            anyhow::bail!("git archive failed: {}", output.stderr_str().trim());
        }
        Ok(output.stdout)
    })
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => return AppError::internal(error).into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/x-tar")],
        archive,
    )
        .into_response()
}

pub async fn download_cache(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (job, repo_id) = match assigned_job_repo(&state, runner_id, job_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if job.cache_key.is_none() {
        return AppError::not_found("job has no cache configuration").into_response();
    }
    let key = match cache_key_header(&headers) {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    let path = cache_archive_path(&state, repo_id, key);
    let key_hash = cache_key_hash(key);
    if let Ok(Some(entry)) =
        rg_db::ops::ci_retention_ops::find_cache_entry(&state.db, repo_id, &key_hash).await
    {
        if entry.expires_at <= chrono::Utc::now() {
            let _ = tokio::fs::remove_file(&path).await;
            let _ = rg_db::ops::ci_retention_ops::delete_cache_entry(&state.db, entry.id).await;
            return AppError::not_found("cache entry expired").into_response();
        }
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let policy = match rg_db::ops::ci_retention_ops::get_policy(&state.db, repo_id).await {
                Ok(policy) => policy,
                Err(error) => return AppError::internal(error).into_response(),
            };
            if let Err(error) = rg_db::ops::ci_retention_ops::upsert_cache_entry(
                &state.db,
                repo_id,
                &key_hash,
                path.to_string_lossy().as_ref(),
                bytes.len() as i64,
                policy.cache_retention_days,
            )
            .await
            {
                return AppError::internal(error).into_response();
            }
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/x-tar")],
                bytes,
            )
                .into_response()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            AppError::not_found("cache entry not found").into_response()
        }
        Err(error) => AppError::internal(error).into_response(),
    }
}

pub async fn upload_cache(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let (job, repo_id) = match assigned_job_repo(&state, runner_id, job_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if job.cache_key.is_none() {
        return AppError::bad_request("job has no cache configuration").into_response();
    }
    if body.is_empty() || body.len() > 1024 * 1024 * 1024 {
        return AppError::bad_request("cache archive must contain 1 byte to 1 GiB").into_response();
    }
    let key = match cache_key_header(&headers) {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    let path = cache_archive_path(&state, repo_id, key);
    if let Some(parent) = path.parent() {
        if let Err(error) = tokio::fs::create_dir_all(parent).await {
            return AppError::internal(error).into_response();
        }
    }
    let temporary = path.with_extension("tar.tmp");
    if let Err(error) = tokio::fs::write(&temporary, body).await {
        return AppError::internal(error).into_response();
    }
    if let Err(error) = tokio::fs::rename(&temporary, &path).await {
        return AppError::internal(error).into_response();
    }
    let policy = match rg_db::ops::ci_retention_ops::get_policy(&state.db, repo_id).await {
        Ok(policy) => policy,
        Err(error) => return AppError::internal(error).into_response(),
    };
    let size = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta.len() as i64,
        Err(error) => return AppError::internal(error).into_response(),
    };
    if let Err(error) = rg_db::ops::ci_retention_ops::upsert_cache_entry(
        &state.db,
        repo_id,
        &cache_key_hash(key),
        path.to_string_lossy().as_ref(),
        size,
        policy.cache_retention_days,
    )
    .await
    {
        let _ = tokio::fs::remove_file(&path).await;
        return AppError::internal(error).into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn assigned_job_repo(
    state: &AppState,
    runner_id: i64,
    job_id: i64,
) -> Result<(rg_db::entities::pipeline_job::Model, i64), AppError> {
    let job = rg_db::ops::pipeline_ops::get_job(&state.db, job_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("job not found"))?;
    if job.runner_id != Some(runner_id) {
        return Err(AppError::forbidden("job not assigned to this runner"));
    }
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("pipeline stage not found"))?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("pipeline not found"))?;
    Ok((job, pipeline.repo_id))
}

fn cache_key_header(headers: &HeaderMap) -> Result<&str, AppError> {
    let key = headers
        .get("x-cache-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| AppError::bad_request("missing x-cache-key header"))?;
    if key.is_empty() || key.len() > 512 {
        return Err(AppError::bad_request("cache key must contain 1-512 bytes"));
    }
    Ok(key)
}

fn cache_archive_path(state: &AppState, repo_id: i64, key: &str) -> std::path::PathBuf {
    let name = cache_key_hash(key);
    state
        .repo_root
        .join("_ci_cache")
        .join(repo_id.to_string())
        .join(format!("{name}.tar"))
}

fn cache_key_hash(key: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(key.as_bytes()))
}

async fn decrypted_repo_secrets(
    state: &AppState,
    repo_id: i64,
) -> anyhow::Result<Vec<(String, String)>> {
    let key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
    let mut values = Vec::new();
    for secret in rg_db::ops::ci_secret_ops::list_by_repo(&state.db, repo_id).await? {
        values.push((
            secret.name,
            rg_core::auth::encryption::decrypt(&secret.encrypted_value, &key)?,
        ));
    }
    Ok(values)
}

async fn secrets_for_job(state: &AppState, stage_id: i64) -> anyhow::Result<Vec<String>> {
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, stage_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("pipeline stage not found"))?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("pipeline not found"))?;
    Ok(decrypted_repo_secrets(state, pipeline.repo_id)
        .await?
        .into_iter()
        .map(|(_, value)| value)
        .collect())
}

/// POST /api/v1/runners/:id/jobs/:job_id/finish
/// Notify server that the runner has finished executing a job.
#[utoipa::path(
    post,
    path = "/runners/{id}/jobs/{job_id}/finish",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID"),
    ),
    request_body(content = FinishJobRequest, description = "Job completion status"),
    responses(
        (status = 200, description = "Job finished", body = serde_json::Value),
        (status = 403, description = "Forbidden - job not assigned to this runner", body = serde_json::Value),
        (status = 404, description = "Job not found", body = serde_json::Value),
    ),
)]
pub async fn finish_job(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    Json(req): Json<FinishJobRequest>,
) -> impl IntoResponse {
    // Verify the job is assigned to this runner
    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => {
            return AppError::not_found("job not found").into_response();
        }
        Err(e) => {
            tracing::error!(%e, "finish_job: get_job failed");
            return AppError::internal(e).into_response();
        }
    };

    if job.runner_id != Some(runner_id) {
        return AppError::forbidden("job not assigned to this runner").into_response();
    }

    let now = Some(chrono::Utc::now().naive_utc());
    // log is managed via upload_log; not updated on finish
    if let Err(e) = rg_db::ops::pipeline_ops::update_job_result(
        &state.db,
        job_id,
        &req.status,
        Some(req.exit_code),
        None,
        None,
        now,
    )
    .await
    {
        tracing::error!(%e, "finish_job: update_job_result failed");
        return AppError::internal(e).into_response();
    }

    // Mark runner as online (ready for next job)
    if let Err(e) = rg_db::ops::runner_ops::update_status(&state.db, runner_id, "online").await {
        tracing::error!(runner_id, error = %e, "Failed to mark runner as online");
    }

    // Cascade: check if stage is done, then if pipeline is done
    if let Ok(Some(_stage_status)) =
        rg_db::ops::pipeline_ops::try_update_stage(&state.db, job.stage_id).await
    {
        // Stage is done — get pipeline_id and check pipeline
        if let Ok(Some(stage)) =
            rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await
        {
            match rg_db::ops::pipeline_ops::try_update_pipeline(&state.db, stage.pipeline_id).await
            {
                Ok(Some(status)) if status == "success" => {
                    if let Ok(Some(pipeline)) =
                        rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id).await
                    {
                        if let Err(error) = rg_core::pull_request::try_auto_merges_for_head_commit(
                            &state.db,
                            &state.repo_root,
                            pipeline.repo_id,
                            &pipeline.commit_sha,
                        )
                        .await
                        {
                            tracing::warn!(pipeline_id = pipeline.id, %error, "auto-merge evaluation after CI failed");
                        }
                        if let Err(error) =
                            rg_core::pull_request::merge_queue::process_for_head_commit_with_ci(
                                &state.db,
                                &state.repo_root,
                                pipeline.repo_id,
                                &pipeline.commit_sha,
                                &rg_core::pull_request::merge_queue::MergeQueueCi {
                                    trigger: &*state.ci_engine,
                                    docker_enabled: state.docker_enabled,
                                    external_runners: state.external_runners,
                                    jwt_secret: Some(&state.jwt_secret),
                                    external_url: state.external_url.as_deref(),
                                },
                            )
                            .await
                        {
                            tracing::warn!(pipeline_id = pipeline.id, %error, "merge queue evaluation after CI failed");
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(pipeline_id = stage.pipeline_id, error = %e, "Failed to update pipeline after stage completion");
                }
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

#[derive(Deserialize, ToSchema)]
pub struct FinishJobRequest {
    status: String, // success | failure | error
    exit_code: i32,
}

/// GET /api/v1/admin/runners
/// List all runners (admin only).
#[utoipa::path(
    get,
    path = "/admin/runners",
    tag = "Runners",
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_runners_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::unauthorized("admin authentication required").into_response();
    }
    match rg_db::ops::runner_ops::list_all(&state.db).await {
        Ok(runners) => {
            let resp: Vec<RunnerInfoResponse> = runners
                .into_iter()
                .map(|r| RunnerInfoResponse {
                    id: r.id,
                    name: r.name,
                    status: r.status,
                    labels: r.labels,
                    last_seen_at: r.last_seen_at.to_string(),
                    version: r.version,
                    os: r.os,
                    arch: r.arch,
                })
                .collect();
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => {
            tracing::error!(%e, "list_runners_admin failed");
            AppError::internal(e).into_response()
        }
    }
}

// ── Runner Token Authentication ──────────────────────────

/// Extract and validate runner Bearer token from Authorization header.
///
/// Used as a route-layer middleware via `from_fn_with_state`.
/// The runner_id is extracted from the path to verify token ownership.
/// Also updates heartbeat on every authenticated request.
pub async fn authenticate_runner(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let runner_id = match extract_runner_id_from_path(request.uri().path()) {
        Some(id) => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "missing runner ID in path"})),
            )
                .into_response();
        }
    };

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let token = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "missing or invalid Authorization header"})),
            )
                .into_response();
        }
    };

    match rg_db::ops::runner_ops::find_by_token(&state.db, token).await {
        Ok(Some(runner)) if runner.id == runner_id => {
            // Valid token — also update heartbeat
            if let Err(e) = rg_db::ops::runner_ops::update_heartbeat(&state.db, runner_id).await {
                tracing::error!(runner_id, error = %e, "Failed to update runner heartbeat");
            }
            next.run(request).await
        }
        Ok(Some(_)) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "token does not match runner ID"})),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid runner token"})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(%e, "authenticate_runner: find_by_token failed");
            AppError::internal(e).into_response()
        }
    }
}

fn extract_runner_id_from_path(path: &str) -> Option<i64> {
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    while let Some(part) = parts.next() {
        if part == "runners" {
            return parts.next()?.parse::<i64>().ok();
        }
    }
    None
}

/// DELETE /api/v1/admin/runners/:id
/// Delete a runner (admin only).
#[utoipa::path(
    delete,
    path = "/admin/runners/{id}",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_runner_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(runner_id): Path<i64>,
) -> impl IntoResponse {
    if require_admin(&state, &headers).await.is_none() {
        return AppError::unauthorized("admin authentication required").into_response();
    }
    match rg_db::ops::runner_ops::delete_runner(&state.db, runner_id).await {
        Ok(true) => (
            StatusCode::NO_CONTENT,
            Json(serde_json::json!({"deleted": true})),
        )
            .into_response(),
        Ok(false) => AppError::not_found("runner not found").into_response(),
        Err(e) => {
            tracing::error!(%e, "delete_runner_admin failed");
            AppError::internal(e).into_response()
        }
    }
}
