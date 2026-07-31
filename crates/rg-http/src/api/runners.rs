//! REST API handlers for CI/CD Runners.

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};

use crate::api::admin::InstanceAdmin;
use crate::error::AppError;
use crate::AppState;
use utoipa::{IntoParams, ToSchema};

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
    _admin: InstanceAdmin,
    Path(runner_id): Path<i64>,
) -> impl IntoResponse {
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
            tracing::error!(error = %format!("{e:#}"), "get_runner_admin failed");
            AppError::from(e).into_response()
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
    _admin: InstanceAdmin,
    Json(req): Json<RegisterRunnerRequest>,
) -> impl IntoResponse {
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
            tracing::error!(error = %format!("{e:#}"), "register runner failed");
            AppError::from(e).into_response()
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
        tracing::warn!(runner_id, error = %format!("{e:#}"), "Failed to reset runner jobs during deregistration");
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
        Err(e) => AppError::from(e).into_response(),
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
            match crate::metrics::time_db(
                "pipeline.find_pending_job",
                rg_db::ops::pipeline_ops::find_pending_job_matching_labels(
                    &state.db,
                    &runner_labels,
                ),
            )
            .await
            {
                Ok(Some(job)) => {
                    // Found a job — assign it to this runner
                    if let Err(e) =
                        rg_db::ops::pipeline_ops::assign_job(&state.db, job.id, runner_id).await
                    {
                        tracing::error!(
                            job_id = job.id,
                            runner_id,
                            error = %format!("{e:#}"),
                            "poll_job: failed to assign job"
                        );
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
                        tracing::error!(
                            job_id = job.id,
                            error = %format!("{e:#}"),
                            "Failed to update job result to assigned"
                        );
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
                        "FORGEKEEP",
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
                                tracing::error!(
                                    pipeline_id,
                                    error = %format!("{error:#}"),
                                    "failed to load CI secrets for external runner"
                                );
                                return Err(AppError::internal(
                                    "failed to prepare job environment",
                                )
                                .into_response());
                            }
                        }
                    }
                    variables.insert("CI".into(), serde_json::json!("true"));
                    variables.insert("FORGEKEEP".into(), serde_json::json!("true"));
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
                    tracing::error!(error = %format!("{e:#}"), "[poll_job] database error while finding pending job");
                    return Err(AppError::from(e).into_response());
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
        (status = 404, description = "Job not found", body = serde_json::Value),
    ),
)]
pub async fn start_job(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
) -> impl IntoResponse {
    // The job itself is not needed past the gate; the call is the gate.
    if let Err(error) = assigned_job(&state, runner_id, job_id).await {
        return error.into_response();
    }

    let now = Some(chrono::Utc::now().naive_utc());
    if let Err(e) = rg_db::ops::pipeline_ops::update_job_result(
        &state.db, job_id, "running", None, None, now, None,
    )
    .await
    {
        tracing::error!(error = %format!("{e:#}"), "start_job: update_job_result failed");
        return AppError::from(e).into_response();
    }

    // Metrics: a job is now executing on a runner.
    crate::metrics::recorder::ci_job_started();

    // Mark runner as busy
    if let Err(e) = rg_db::ops::runner_ops::update_status(&state.db, runner_id, "busy").await {
        tracing::error!(runner_id, error = %format!("{e:#}"), "Failed to mark runner as busy");
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
        (status = 404, description = "Job not found", body = serde_json::Value),
    ),
)]
pub async fn upload_log(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    body: String,
) -> impl IntoResponse {
    let job = match assigned_job(&state, runner_id, job_id).await {
        Ok(job) => job,
        Err(error) => return error.into_response(),
    };

    let body = match secrets_for_job(&state, job.stage_id).await {
        Ok(secrets) => rg_core::auth::encryption::mask_values(&body, &secrets),
        Err(error) => {
            tracing::error!(job_id, error = %format!("{error:#}"), "failed to load secrets while masking runner log");
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
#[utoipa::path(
    get,
    path = "/runners/{id}/jobs/{job_id}/workspace",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID, which must be assigned to this runner"),
    ),
    responses(
        (status = 200, description = "Tar archive of the commit assigned to the job", content_type = "application/x-tar"),
        (status = 404, description = "Job, stage, pipeline or repository not found", body = serde_json::Value),
    ),
)]
pub async fn download_workspace(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
) -> impl IntoResponse {
    let job = match assigned_job(&state, runner_id, job_id).await {
        Ok(job) => job,
        Err(error) => return error.into_response(),
    };
    let stage = match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await {
        Ok(Some(stage)) => stage,
        Ok(None) => return AppError::not_found("pipeline stage not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id).await
    {
        Ok(Some(pipeline)) => pipeline,
        Ok(None) => return AppError::not_found("pipeline not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    let repository = match rg_db::entities::repository::Entity::find_by_id(pipeline.repo_id)
        .one(&state.db)
        .await
    {
        Ok(Some(repository)) => repository,
        Ok(None) => return AppError::not_found("repository not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    let namespace = if let Some(org_id) = repository.org_id {
        match rg_db::ops::org_ops::get_org(&state.db, org_id).await {
            Ok(Some(org)) => org.name,
            Ok(None) => {
                return AppError::not_found("repository organization not found").into_response()
            }
            Err(error) => return AppError::from(error).into_response(),
        }
    } else {
        match rg_db::ops::user_ops::find_by_id(&state.db, repository.owner_id).await {
            Ok(Some(user)) => user.username,
            Ok(None) => return AppError::not_found("repository owner not found").into_response(),
            Err(error) => return AppError::from(error).into_response(),
        }
    };
    let repo_path = state
        .repo_root
        .join(namespace)
        .join(format!("{}.git", repository.name));
    let commit_sha = pipeline.commit_sha.clone();

    // Stream `git archive` stdout straight to the runner with an idle guard
    // instead of buffering the whole tar in memory (card_93d08fc4a67f). A large
    // repo's archive no longer sits in RAM, and a slow-drip runner that stops
    // reading is torn down at ~the idle window (killing git) rather than pinning
    // the buffer + connection until the kernel resets the socket.
    let gateway = match rg_git::cli_gateway::global_gateway().as_ref() {
        Ok(gateway) => gateway,
        Err(error) => return AppError::internal(format!("{error}")).into_response(),
    };
    let child = match gateway
        .spawn_async(&["archive", "--format=tar", &commit_sha], Some(&repo_path))
        .await
    {
        Ok(child) => child,
        Err(error) => return AppError::internal(format!("{error}")).into_response(),
    };
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/x-tar")],
        stream_git_archive_with_idle(child, state.git_idle_timeout_secs, job_id, repo_path),
    )
        .into_response()
}

/// Slice size for streaming `git archive` output to the runner.
const ARCHIVE_STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// Stream a spawned `git archive` child's stdout as an idle-guarded
/// `application/x-tar` body, holding the child for the life of the stream
/// (`kill_on_drop`).
///
/// This is the download-side slow-drip defense for the workspace endpoint, the
/// twin of `git_http::git_response_body_with_idle`: instead of buffering the
/// whole tar in memory, the producer pumps git's stdout through a bounded
/// channel in [`ARCHIVE_STREAM_CHUNK_BYTES`] slices. Two idle points are bounded
/// by `idle_secs`:
/// - the **read** from git's stdout — a hung `git archive` trips (it no longer
///   has the synchronous `git_cmd_secs` wall-clock, since we spawn it async);
/// - the **send** to the client — a stalled runner stops draining, so the
///   bounded channel fills and the blocked `send` trips.
///
/// On either trip (or the runner disconnecting) the producer returns, dropping
/// the child → `kill_on_drop` reaps git and frees the pipe. `idle_secs == 0`
/// disables the bound. git's stderr is drained concurrently so a chatty error
/// can't fill its pipe and stall the archive. A stdout read failure is logged
/// with the job and repository before the already-started response is cut short.
fn stream_git_archive_with_idle(
    mut child: tokio::process::Child,
    idle_secs: u64,
    job_id: i64,
    repo_path: std::path::PathBuf,
) -> axum::body::Body {
    use tokio::io::AsyncReadExt;

    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(4);
    let idle = (idle_secs > 0).then(|| std::time::Duration::from_secs(idle_secs));

    tokio::spawn(async move {
        // `git archive` reads no stdin; close it so git never waits on EOF.
        drop(child.stdin.take());

        // Drain stderr concurrently: it is tiny for archive, but an unread full
        // pipe would deadlock git. Kept for a diagnostic on a non-zero exit.
        let stderr = child.stderr.take();
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut se) = stderr {
                if let Err(error) = se.read_to_end(&mut buf).await {
                    tracing::warn!(%error, "failed to drain git archive stderr");
                }
            }
            buf
        });

        let mut stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                tracing::warn!(
                    job_id,
                    repo = %repo_path.display(),
                    "git archive stdout unavailable — workspace tar cannot be streamed"
                );
                return;
            }
        };

        pump_git_archive_stdout(&mut stdout, &tx, idle, job_id, &repo_path).await;

        // Reap git. On a trip we kill it; on clean EOF it has already exited.
        if let Err(error) = child.start_kill() {
            tracing::debug!(%error, "git archive process already exited before kill");
        }
        if let Ok(status) = child.wait().await {
            if !status.success() {
                let err = stderr_task.await.unwrap_or_default();
                tracing::warn!(
                    job_id,
                    repo = %repo_path.display(),
                    stderr = %String::from_utf8_lossy(&err).trim(),
                    "git archive exited non-zero after streaming workspace tar (truncated archive)"
                );
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let frame_stream = futures::StreamExt::map(stream, |item| item.map(http_body::Frame::data));
    axum::body::Body::new(http_body_util::StreamBody::new(frame_stream))
}

async fn pump_git_archive_stdout<R>(
    stdout: &mut R,
    tx: &tokio::sync::mpsc::Sender<std::io::Result<Bytes>>,
    idle: Option<std::time::Duration>,
    job_id: i64,
    repo_path: &std::path::Path,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut buf = vec![0u8; ARCHIVE_STREAM_CHUNK_BYTES];
    loop {
        // Read a chunk, bounded by the idle window (catches a hung git).
        let read = match idle {
            Some(dur) => match tokio::time::timeout(dur, stdout.read(&mut buf)).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    tracing::warn!(
                        job_id,
                        repo = %repo_path.display(),
                        idle_secs = dur.as_secs(),
                        "git archive read idle timeout — killing git"
                    );
                    break;
                }
            },
            None => stdout.read(&mut buf).await,
        };
        let n = match read {
            Ok(n) => n,
            Err(error) => {
                tracing::warn!(
                    job_id,
                    repo = %repo_path.display(),
                    %error,
                    "git archive stdout read failed — workspace tar truncated"
                );
                break;
            }
        };
        if n == 0 {
            break;
        }
        let chunk = Bytes::copy_from_slice(&buf[..n]);
        // Send, bounded by the idle window (catches a stalled runner: hyper
        // stops draining the stream → the channel fills → `send` blocks).
        let send = tx.send(Ok(chunk));
        let sent = match idle {
            Some(dur) => match tokio::time::timeout(dur, send).await {
                Ok(res) => res,
                Err(_elapsed) => {
                    tracing::warn!(
                        job_id,
                        repo = %repo_path.display(),
                        idle_secs = dur.as_secs(),
                        "git archive response idle timeout — slow runner stopped reading, killing git"
                    );
                    break;
                }
            },
            None => send.await,
        };
        if sent.is_err() {
            break; // receiver gone (runner disconnected)
        }
    }
}

/// Download the CI cache archive stored under the `x-cache-key` of an assigned job.
#[utoipa::path(
    get,
    path = "/runners/{id}/jobs/{job_id}/cache",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID, which must be assigned to this runner"),
        ("x-cache-key" = String, Header, description = "Cache key, 1-512 bytes"),
    ),
    responses(
        (status = 200, description = "Cache archive", content_type = "application/x-tar"),
        (status = 400, description = "Missing or malformed x-cache-key header", body = serde_json::Value),
        (status = 404, description = "Job has no cache configuration, or no cache entry for this key", body = serde_json::Value),
    ),
)]
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
    let existing = rg_db::ops::ci_retention_ops::find_cache_entry(&state.db, repo_id, &key_hash)
        .await
        .ok()
        .flatten();
    if let Some(entry) = &existing {
        if entry.expires_at <= chrono::Utc::now() {
            // Eviction is a side effect of answering 404 — it cannot change the
            // response, but a half-done eviction leaves residue that nothing
            // else will come back for.
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                // Already gone: eviction had nothing to do, not a failure.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(
                    repo_id,
                    cache_entry_id = entry.id,
                    path = %path.display(),
                    error = %error,
                    "expired CI cache archive not deleted — the file stays on disk after its entry expired"
                ),
            }
            if let Err(error) =
                rg_db::ops::ci_retention_ops::delete_cache_entry(&state.db, entry.id).await
            {
                tracing::warn!(
                    repo_id,
                    cache_entry_id = entry.id,
                    error = %format!("{error:#}"),
                    "expired CI cache entry not deleted — the row survives pointing at an archive that was just removed"
                );
            }
            return AppError::not_found("cache entry expired").into_response();
        }
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let sha256 = cache_content_hash(&bytes);
            // Integrity: the stored archive must still hash to the digest recorded
            // at upload — a tampered/corrupted cache would otherwise inject files
            // into a downstream build. Legacy entries carry no digest and are
            // served without this guard.
            if let Some(expected) = existing.as_ref().and_then(|e| e.sha256.as_deref()) {
                if sha256 != expected {
                    return AppError::internal(anyhow::anyhow!(
                        "cache integrity check failed: expected sha256 {expected}, got {sha256}"
                    ))
                    .into_response();
                }
            }
            let policy = match rg_db::ops::ci_retention_ops::get_policy(&state.db, repo_id).await {
                Ok(policy) => policy,
                Err(error) => return AppError::from(error).into_response(),
            };
            if let Err(error) = rg_db::ops::ci_retention_ops::upsert_cache_entry(
                &state.db,
                repo_id,
                &key_hash,
                path.to_string_lossy().as_ref(),
                bytes.len() as i64,
                Some(&sha256),
                policy.cache_retention_days,
            )
            .await
            {
                return AppError::from(error).into_response();
            }
            // The archive is already buffered (the integrity check above needs
            // it), so its exact length is known — advertise it so clients can
            // detect a truncated download.
            let content_length = bytes.len().to_string();
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, "application/x-tar"),
                    (axum::http::header::CONTENT_LENGTH, content_length.as_str()),
                    (
                        axum::http::HeaderName::from_static("x-checksum-sha256"),
                        sha256.as_str(),
                    ),
                ],
                // Idle-guarded stream of the already-verified buffer: a slow or
                // stalled CI client would otherwise pin this cache-sized `Vec` in
                // server memory until the kernel resets the dead connection
                // (card_16003d99e502). Reuses the git-streaming idle budget.
                crate::http_stream::buffered_body_with_idle(bytes, state.git_idle_timeout_secs),
            )
                .into_response()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            AppError::not_found("cache entry not found").into_response()
        }
        Err(error) => cache_path_error("CI cache archive", &path, &error).into_response(),
    }
}

/// Store a CI cache archive under the `x-cache-key` of an assigned job.
#[utoipa::path(
    put,
    path = "/runners/{id}/jobs/{job_id}/cache",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID, which must be assigned to this runner"),
        ("x-cache-key" = String, Header, description = "Cache key, 1-512 bytes"),
    ),
    request_body(
        content = String,
        description = "Cache archive, 1 byte to 1 GiB",
        content_type = "application/x-tar",
    ),
    responses(
        (status = 204, description = "Cache entry stored"),
        (status = 400, description = "Job has no cache configuration, bad x-cache-key, or archive outside 1 byte..1 GiB", body = serde_json::Value),
        (status = 404, description = "Job, stage or pipeline not found", body = serde_json::Value),
    ),
)]
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
            return cache_path_error("CI cache directory", parent, &error).into_response();
        }
    }
    // Digest the payload before the write consumes `body` — the archive is
    // already fully buffered in memory (≤ 1 GiB, bounded above), so hashing the
    // in-memory bytes costs nothing extra.
    let sha256 = cache_content_hash(body.as_ref());
    let temporary = path.with_extension("tar.tmp");
    // From here until `upsert_cache_entry` succeeds there is a file on disk that
    // no DB row points at. Retention walks rows, so every early exit below has to
    // take its file with it — otherwise the failure leaks a cache-sized archive
    // that nothing will ever come back for.
    if let Err(error) = tokio::fs::write(&temporary, body).await {
        discard_unreferenced_cache_file("CI cache staging file", &temporary, repo_id, job_id, key)
            .await;
        return cache_path_error("CI cache staging file", &temporary, &error).into_response();
    }
    if let Err(error) = tokio::fs::rename(&temporary, &path).await {
        discard_unreferenced_cache_file("CI cache staging file", &temporary, repo_id, job_id, key)
            .await;
        return cache_path_error("CI cache archive", &path, &error).into_response();
    }
    let policy = match rg_db::ops::ci_retention_ops::get_policy(&state.db, repo_id).await {
        Ok(policy) => policy,
        Err(error) => {
            discard_unreferenced_cache_file("CI cache archive", &path, repo_id, job_id, key).await;
            return AppError::from(error).into_response();
        }
    };
    let size = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta.len() as i64,
        Err(error) => {
            discard_unreferenced_cache_file("CI cache archive", &path, repo_id, job_id, key).await;
            return cache_path_error("CI cache archive", &path, &error).into_response();
        }
    };
    if let Err(error) = rg_db::ops::ci_retention_ops::upsert_cache_entry(
        &state.db,
        repo_id,
        &cache_key_hash(key),
        path.to_string_lossy().as_ref(),
        size,
        Some(&sha256),
        policy.cache_retention_days,
    )
    .await
    {
        discard_unreferenced_cache_file("CI cache archive", &path, repo_id, job_id, key).await;
        return AppError::from(error).into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Roll back a cache file that is on disk with no DB row pointing at it.
///
/// The caller still has to report the original failure to the runner, so a
/// failed rollback can only be logged: if the file survives, nothing references
/// it and retention (which walks DB rows) will never come back for it. An
/// already-absent file is the normal outcome of a write that failed before
/// creating anything, and is not worth a warning.
async fn discard_unreferenced_cache_file(
    what: &str,
    path: &std::path::Path,
    repo_id: i64,
    job_id: i64,
    key: &str,
) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => {}
        Err(cleanup_error) => tracing::warn!(
            repo_id,
            job_id,
            cache_key = %key,
            path = %path.display(),
            error = %cleanup_error,
            "orphaned {what}: the cache entry was not recorded and the rollback delete failed too — the file stays on disk with no row pointing at it"
        ),
    }
}

/// The job named by `{job_id}`, provided it is the one this runner was given.
///
/// `{job_id}` is an instance-wide primary key, so "somebody else's job" and "no
/// such job" have to be indistinguishable from the outside: answering `403` to
/// one and `404` to the other turns the pair into an existence oracle over every
/// pipeline on the instance, private repositories included, and a runner needs
/// nothing but a `for` loop to read it. A runner token is handed out by an
/// instance admin, so the perimeter is a trusted runner rather than any account
/// — which is why this was `low` and not why it was fine.
///
/// The rule is the project's, not this module's: `api::boards` states it as
/// "a mismatch answers 404, not 403: a 403 would confirm the id exists", and
/// the user- and repository-scoped axes already follow it. This is the runner
/// axis, in one function rather than the six copies it replaces — the copies
/// are how a rule ends up applied five times out of six.
///
/// The lookup's own failure stays a failure: `AppError::from` classifies a dead
/// pool as a retryable `503`, so a check that could not run is never reported
/// as a check that said no.
pub(crate) async fn assigned_job(
    state: &AppState,
    runner_id: i64,
    job_id: i64,
) -> Result<rg_db::entities::pipeline_job::Model, AppError> {
    let job = crate::metrics::time_db(
        "pipeline.get_job",
        rg_db::ops::pipeline_ops::get_job(&state.db, job_id),
    )
    .await
    .map_err(|error| {
        tracing::error!(runner_id, job_id, error = %format!("{error:#}"), "assigned_job: get_job failed");
        AppError::from(error)
    })?;
    match job {
        Some(job) if job.runner_id == Some(runner_id) => Ok(job),
        _ => Err(AppError::not_found("job not found")),
    }
}

async fn assigned_job_repo(
    state: &AppState,
    runner_id: i64,
    job_id: i64,
) -> Result<(rg_db::entities::pipeline_job::Model, i64), AppError> {
    let job = assigned_job(state, runner_id, job_id).await?;
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("pipeline stage not found"))?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
        .await
        .map_err(AppError::from)?
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

/// One actionable error for a filesystem failure on the server side of the CI
/// cache.
///
/// The archive path is `_ci_cache/<repo_id>/<sha256-of-cache-key>.tar` under
/// `repo_root`: it is derived inside the handler and never appears in the
/// request, so a bare `io::Error` hands the operator an errno for a file they
/// cannot locate. This is the same directory the runner reports through
/// `rg_ci`'s cache diagnostics — both halves quote one remedy, because one
/// mis-owned bind-mount is what breaks both.
fn cache_path_error(what: &str, path: &std::path::Path, error: &std::io::Error) -> AppError {
    AppError::internal(rg_core::platform::fs::describe_path_error(
        what,
        path,
        error,
        rg_core::platform::fs::CI_CACHE_DIR_HINT,
    ))
}

fn cache_key_hash(key: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// Hex-encoded SHA-256 of a cache archive's *contents* (distinct from
/// `cache_key_hash`, which digests the cache key). Used to record and later
/// verify the integrity of the stored archive.
fn cache_content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
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
        (status = 404, description = "Job not found", body = serde_json::Value),
    ),
)]
pub async fn finish_job(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    Json(req): Json<FinishJobRequest>,
) -> impl IntoResponse {
    let job = match assigned_job(&state, runner_id, job_id).await {
        Ok(job) => job,
        Err(error) => return error.into_response(),
    };

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
        tracing::error!(error = %format!("{e:#}"), "finish_job: update_job_result failed");
        return AppError::from(e).into_response();
    }

    // Metrics: job left the running set — count its outcome and, if we know when
    // it started, its execution duration.
    let job_duration = job
        .started_at
        .and_then(|started| (chrono::Utc::now().naive_utc() - started).to_std().ok());
    crate::metrics::recorder::ci_job_finished(&req.status, job_duration);

    // Mark runner as online (ready for next job)
    if let Err(e) = rg_db::ops::runner_ops::update_status(&state.db, runner_id, "online").await {
        tracing::error!(runner_id, error = %format!("{e:#}"), "Failed to mark runner as online");
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
                Ok(Some(status)) => {
                    // Metrics: the pipeline reached a terminal status.
                    crate::metrics::recorder::ci_pipeline_finished(&status);
                    if status == "success" {
                        if let Ok(Some(pipeline)) =
                            rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
                                .await
                        {
                            // "CI went green, so the PR goes in" is what
                            // auto-merge is for — and the merge commit it lands
                            // on the base branch owes the same post-push
                            // automation a push does. Until card_73a1ec5b32f3
                            // the merge happened here and its ref move was
                            // dropped, so that commit got no pipeline, no `push`
                            // webhook and no watch notification.
                            state
                                .evaluate_merges_and_spawn_hooks(
                                    pipeline.repo_id,
                                    &pipeline.commit_sha,
                                    None,
                                )
                                .await;
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(
                        pipeline_id = stage.pipeline_id,
                        error = %format!("{e:#}"),
                        "Failed to update pipeline after stage completion"
                    );
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
    _admin: InstanceAdmin,
) -> impl IntoResponse {
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
            tracing::error!(error = %format!("{e:#}"), "list_runners_admin failed");
            AppError::from(e).into_response()
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
                tracing::error!(runner_id, error = %format!("{e:#}"), "Failed to update runner heartbeat");
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
            tracing::error!(error = %format!("{e:#}"), "authenticate_runner: find_by_token failed");
            AppError::from(e).into_response()
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
    _admin: InstanceAdmin,
    Path(runner_id): Path<i64>,
) -> impl IntoResponse {
    match rg_db::ops::runner_ops::delete_runner(&state.db, runner_id).await {
        Ok(true) => (
            StatusCode::NO_CONTENT,
            Json(serde_json::json!({"deleted": true})),
        )
            .into_response(),
        Ok(false) => AppError::not_found("runner not found").into_response(),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "delete_runner_admin failed");
            AppError::from(e).into_response()
        }
    }
}

#[cfg(test)]
mod cache_path_error_tests {
    use super::*;

    /// The archive lives at `_ci_cache/<repo_id>/<sha256-of-cache-key>.tar`
    /// under `repo_root` — derived inside the handler, never echoed back — so a
    /// cache failure carrying only an errno names nothing an operator can act
    /// on. The runner half of this directory has said so since a42e694; the
    /// server half used to answer `Permission denied (os error 13)`.
    #[test]
    fn cache_failure_names_the_path_and_the_shared_remedy() {
        let temp = tempfile::tempdir().unwrap();
        // A regular file where `_ci_cache/` belongs fails directory creation
        // deterministically, independent of the uid the tests run as.
        let blocker = temp.path().join("_ci_cache");
        std::fs::write(&blocker, "not a directory").unwrap();
        let directory = blocker.join("7");
        let error = std::fs::create_dir_all(&directory).unwrap_err();

        let AppError::InternalError(rendered) =
            cache_path_error("CI cache directory", &directory, &error)
        else {
            panic!("a filesystem failure on the cache path must stay a 500");
        };

        assert!(
            rendered.contains(&directory.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("CI cache directory"), "{rendered}");
        // Byte-identical to what the runner writes into the job log, so one
        // mis-owned bind-mount never yields two different remedies.
        assert!(rendered.contains("_ci_cache/<repo_id>/"), "{rendered}");
    }
}

#[cfg(test)]
mod archive_stream_tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, ReadBuf};

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
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

    struct FailsAfterPartialChunk {
        yielded: bool,
    }

    impl AsyncRead for FailsAfterPartialChunk {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.yielded {
                return Poll::Ready(Err(std::io::Error::other(
                    "injected archive stdout failure",
                )));
            }
            self.yielded = true;
            buf.put_slice(b"partial tar");
            Poll::Ready(Ok(()))
        }
    }

    fn gw() -> &'static rg_git::cli_gateway::GitCommandGateway {
        rg_git::cli_gateway::global_gateway().as_ref().unwrap()
    }

    /// Run git via the sanctioned gateway (keeps the raw-git-invocation guard green).
    fn git(args: &[&str], cwd: Option<&std::path::Path>) {
        let out = gw().run(args, cwd).unwrap();
        assert!(out.success(), "git {args:?}: {}", out.stderr_str().trim());
    }

    fn seed_repo(repo: &std::path::Path, big: bool) -> Vec<u8> {
        git(&["init", "--initial-branch=main"], Some(repo));
        git(&["config", "user.name", "Archive Test"], Some(repo));
        git(&["config", "user.email", "archive@example.com"], Some(repo));
        let blob = if big {
            // Poorly-compressible content so the tar spans several 64 KiB reads,
            // exercising the multi-chunk streaming loop.
            let mut blob = Vec::with_capacity(300 * 1024);
            let mut x: u32 = 0x1234_5678;
            for _ in 0..(300 * 1024) {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                blob.push((x & 0xff) as u8);
            }
            blob
        } else {
            b"hello\n".to_vec()
        };
        std::fs::write(repo.join("big.bin"), &blob).unwrap();
        std::fs::write(repo.join("README.md"), "archive parity\n").unwrap();
        git(&["add", "."], Some(repo));
        git(&["commit", "-m", "content"], Some(repo));
        blob
    }

    /// The streamed tar must be byte-identical to a buffered `git archive` — a
    /// truncated or reordered stream would corrupt the runner's workspace.
    #[tokio::test]
    async fn streamed_archive_matches_buffered_git_archive_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        seed_repo(repo, true);

        let buffered = gw()
            .run(&["archive", "--format=tar", "HEAD"], Some(repo))
            .unwrap();
        assert!(buffered.success());
        let buffered = buffered.stdout;

        let child = gw()
            .spawn_async(&["archive", "--format=tar", "HEAD"], Some(repo))
            .await
            .unwrap();
        let streamed = stream_git_archive_with_idle(child, 30, 1, repo.to_path_buf())
            .collect()
            .await
            .unwrap()
            .to_bytes();

        assert_eq!(
            streamed.len(),
            buffered.len(),
            "streamed tar length must match buffered"
        );
        assert_eq!(
            &streamed[..],
            &buffered[..],
            "streamed tar must be byte-identical to buffered git archive"
        );
    }

    /// `idle_secs == 0` disables the idle bound; the full tar still streams.
    #[tokio::test]
    async fn streamed_archive_idle_disabled_delivers_full_tar() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        seed_repo(repo, false);

        let buffered = gw()
            .run(&["archive", "--format=tar", "HEAD"], Some(repo))
            .unwrap()
            .stdout;
        let child = gw()
            .spawn_async(&["archive", "--format=tar", "HEAD"], Some(repo))
            .await
            .unwrap();
        let streamed = stream_git_archive_with_idle(child, 0, 1, repo.to_path_buf())
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(&streamed[..], &buffered[..]);
    }

    /// Once response headers are on the wire, a read failure cannot become a
    /// different status. The server log is therefore the operator's only copy
    /// of the underlying errno and the job/repository it corrupted.
    #[tokio::test]
    async fn stdout_read_failure_logs_cause_and_context_with_or_without_idle_guard() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let repo_path = std::path::Path::new("/repos/acme/widgets.git");

        for idle in [None, Some(std::time::Duration::from_secs(30))] {
            let mut stdout = FailsAfterPartialChunk { yielded: false };
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);

            pump_git_archive_stdout(&mut stdout, &tx, idle, 42, repo_path).await;
            drop(tx);

            assert_eq!(
                rx.recv().await.unwrap().unwrap(),
                Bytes::from_static(b"partial tar")
            );
            assert!(rx.recv().await.is_none(), "the failed reader must stop");
        }

        let rendered = logs.text();
        assert_eq!(
            rendered
                .matches("git archive stdout read failed — workspace tar truncated")
                .count(),
            2,
            "{rendered}"
        );
        assert!(
            rendered.contains("injected archive stdout failure"),
            "{rendered}"
        );
        assert!(rendered.contains("job_id=42"), "{rendered}");
        assert!(rendered.contains("/repos/acme/widgets.git"), "{rendered}");
    }
}
