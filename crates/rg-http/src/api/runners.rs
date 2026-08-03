//! REST API handlers for CI/CD Runners.

use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, Request, State};
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
        (status = 200, description = "Heartbeat recorded", body = HeartbeatResponse),
        (status = 503, description = "The heartbeat was not recorded: the database is unreachable", body = serde_json::Value),
    ),
)]
pub async fn heartbeat(
    State(_state): State<AppState>,
    // The id is deliberately not bound: this handler answers for a write it
    // does not perform and does not act on the id, and the middleware already
    // logs `runner_id` next to the error itself. Binding it here would enter
    // this route into `global_id_anchor_guard`'s census — the denominator of a
    // plan about handlers that *act* on an instance-wide id — for a log line.
    Path(_runner_id): Path<i64>,
    refresh: Option<Extension<HeartbeatRefresh>>,
) -> impl IntoResponse {
    // The write itself is `authenticate_runner`'s — every authenticated runner
    // request refreshes `last_seen_at`, so doing it again here would be a second
    // write for the same fact. What this handler owns is the *answer*: this is
    // the one route whose entire purpose is that write, and `{"status":"ok"}`
    // is a statement about it. The middleware records the outcome instead of
    // discarding it, so a refresh that never reached the database cannot be
    // reported as a heartbeat that landed.
    //
    // A missing extension means the middleware did not run at all — the route
    // is mounted behind it, so that is a wiring fault, and answering `200` to it
    // would be the same lie by another route.
    match refresh.map(|Extension(refresh)| refresh) {
        Some(HeartbeatRefresh::Persisted) => (
            StatusCode::OK,
            Json(HeartbeatResponse {
                status: "ok".to_string(),
                server_time: chrono::Utc::now().to_rfc3339(),
            }),
        )
            .into_response(),
        // Already logged with its full context by the middleware; the runner
        // gets the classification, not the database's words.
        Some(HeartbeatRefresh::Unavailable) => {
            AppError::service_unavailable("runner heartbeat was not recorded").into_response()
        }
        Some(HeartbeatRefresh::Failed) => {
            AppError::internal("runner heartbeat was not recorded").into_response()
        }
        None => {
            AppError::internal("runner heartbeat route reached without the runner-auth middleware")
                .into_response()
        }
    }
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
    // Handing the runner's in-flight jobs back to the pool and deleting the
    // runner row is one transaction, not two writes with a log line between
    // them: deleting a runner whose jobs were not reset strands those jobs on a
    // row that no longer exists. See `runner_ops::deregister_runner`.
    match rg_db::ops::runner_ops::deregister_runner(&state.db, runner_id).await {
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
        Err(e) => {
            tracing::error!(runner_id, error = %format!("{e:#}"), "Failed to deregister runner");
            AppError::from(e).into_response()
        }
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
                Ok(Some(runner)) => match serde_json::from_str(&runner.labels) {
                    Ok(labels) => labels,
                    Err(error) => {
                        tracing::error!(
                            runner_id,
                            error = %error,
                            "poll_job: stored runner labels are invalid JSON"
                        );
                        return Err(AppError::internal("invalid runner labels").into_response());
                    }
                },
                Ok(None) => {
                    return Err(AppError::not_found("runner not found").into_response());
                }
                Err(error) => {
                    tracing::error!(
                        runner_id,
                        error = %format!("{error:#}"),
                        "poll_job: runner lookup failed"
                    );
                    return Err(AppError::from(error).into_response());
                }
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
                    // Decode every runner-facing JSON column BEFORE claiming the
                    // job. Both used to be decoded after the assignment and with
                    // the error thrown away: a corrupt `variables` collapsed into
                    // an empty map and a corrupt `cache_paths` into `None`, so the
                    // runner got a `200` and started work with its mandatory
                    // environment missing and caching silently off — while the row
                    // was already `running` and owned by that runner, so no other
                    // runner could pick it up either. Refusing here answers 5xx and
                    // leaves the row `pending` and unassigned.
                    //
                    // `NULL` still means "this optional column was never set" and
                    // keeps its previous meaning; only undecodable text is refused.
                    let mut variables = match job.variables.as_deref() {
                        Some(json) => match serde_json::from_str::<
                            serde_json::Map<String, serde_json::Value>,
                        >(json)
                        {
                            Ok(variables) => variables,
                            Err(error) => {
                                tracing::error!(
                                    job_id = job.id,
                                    runner_id,
                                    error = %error,
                                    "poll_job: stored job variables are invalid JSON"
                                );
                                return Err(
                                    AppError::internal("invalid job variables").into_response()
                                );
                            }
                        },
                        None => serde_json::Map::new(),
                    };
                    let cache_paths = match job.cache_paths.as_deref() {
                        Some(json) => match serde_json::from_str::<Vec<String>>(json) {
                            Ok(cache_paths) => Some(cache_paths),
                            Err(error) => {
                                tracing::error!(
                                    job_id = job.id,
                                    runner_id,
                                    error = %error,
                                    "poll_job: stored job cache_paths are invalid JSON"
                                );
                                return Err(
                                    AppError::internal("invalid job cache paths").into_response()
                                );
                            }
                        },
                        None => None,
                    };

                    // Found a candidate — now *claim* it. The candidate came out
                    // of a snapshot, and two things can have happened since: the
                    // pipeline was canceled, or another runner polling at the
                    // same instant took this exact row. `assign_job` re-asserts
                    // the whole candidate condition (`pending` and unassigned)
                    // in its `WHERE`, so the database picks one winner and the
                    // rest are refused here rather than silently overwriting
                    // `runner_id` — which is what made two runners both receive
                    // `200` with the same job body, and left the loser's
                    // `/start` and `/finish` answered `404 job not found`.
                    //
                    // The retry needs no backoff and cannot spin: being refused
                    // means the row is no longer pending-and-unassigned, and the
                    // query above selects exactly that, so the same row cannot
                    // come back as a candidate.
                    match rg_db::ops::pipeline_ops::assign_job(&state.db, job.id, runner_id).await {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::info!(
                                job_id = job.id,
                                runner_id,
                                "poll_job: candidate was claimed or settled before this runner could take it"
                            );
                            continue;
                        }
                        Err(error) => {
                            tracing::error!(
                                job_id = job.id,
                                runner_id,
                                error = %format!("{error:#}"),
                                "poll_job: failed to assign job"
                            );
                            return Err(AppError::from(error).into_response());
                        }
                    }

                    // Fetch stage to get pipeline_id
                    let stage =
                        match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id)
                            .await
                        {
                            Ok(Some(stage)) => stage,
                            Ok(None) => {
                                tracing::error!(
                                    job_id = job.id,
                                    stage_id = job.stage_id,
                                    "poll_job: assigned job has no pipeline stage"
                                );
                                return Err(AppError::internal(
                                    "assigned job has no pipeline stage",
                                )
                                .into_response());
                            }
                            Err(error) => {
                                tracing::error!(
                                    job_id = job.id,
                                    stage_id = job.stage_id,
                                    error = %format!("{error:#}"),
                                    "poll_job: pipeline stage lookup failed after assignment"
                                );
                                return Err(AppError::from(error).into_response());
                            }
                        };
                    let pipeline_id = stage.pipeline_id;
                    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(
                        &state.db,
                        pipeline_id,
                    )
                    .await
                    {
                        Ok(Some(pipeline)) => pipeline,
                        Ok(None) => {
                            tracing::error!(
                                job_id = job.id,
                                stage_id = job.stage_id,
                                pipeline_id,
                                "poll_job: assigned job has no pipeline"
                            );
                            return Err(
                                AppError::internal("assigned job has no pipeline").into_response()
                            );
                        }
                        Err(error) => {
                            tracing::error!(
                                job_id = job.id,
                                stage_id = job.stage_id,
                                pipeline_id,
                                error = %format!("{error:#}"),
                                "poll_job: pipeline lookup failed after assignment"
                            );
                            return Err(AppError::from(error).into_response());
                        }
                    };

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
                            return Err(AppError::from(error).into_response());
                        }
                    }
                    variables.insert("CI".into(), serde_json::json!("true"));
                    variables.insert("FORGEKEEP".into(), serde_json::json!("true"));
                    variables.insert("CI_PIPELINE_ID".into(), serde_json::json!(pipeline_id));
                    variables.insert(
                        "CI_COMMIT_SHA".into(),
                        serde_json::json!(pipeline.commit_sha),
                    );
                    variables.insert("CI_SHA".into(), serde_json::json!(pipeline.commit_sha));
                    variables.insert("CI_REF".into(), serde_json::json!(pipeline.ref_name));
                    variables.insert("CI_EVENT".into(), serde_json::json!(pipeline.trigger_type));
                    match rg_core::auth::ci_token::generate_ci_job_token_with_ttl(
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
                        Ok(token) => {
                            variables.insert("CI_JOB_TOKEN".into(), serde_json::json!(token));
                        }
                        Err(error) => {
                            tracing::error!(
                                job_id = job.id,
                                pipeline_id,
                                error = %format!("{error:#}"),
                                "failed to generate CI job token for external runner"
                            );
                            return Err(AppError::internal("failed to prepare job environment")
                                .into_response());
                        }
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

                    let resp = PollJobResponse {
                        job_id: job.id,
                        pipeline_id,
                        stage_id: job.stage_id,
                        name: job.name,
                        script: job.script.lines().map(|s| s.to_string()).collect(),
                        image: job.image,
                        variables: Some(serde_json::Value::Object(variables)),
                        cache_key: job.cache_key,
                        cache_paths,
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
    match rg_db::ops::pipeline_ops::start_job_if_active(&state.db, job_id, now).await {
        Ok(true) => {}
        // The job settled while this runner was picking it up — a cancellation,
        // or the watchdog. Saying `200` here would tell the runner to go
        // execute work the server has already answered `canceled` for.
        Ok(false) => {
            tracing::info!(
                runner_id,
                job_id,
                "start_job: refused — the job settled before the runner started it"
            );
            return AppError::conflict("job is no longer active").into_response();
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "start_job: update_job_result failed");
            return AppError::from(e).into_response();
        }
    }

    // Mark runner as busy. This is not decoration: `busy` is what keeps the
    // scheduler from handing this runner a second job while it executes this
    // one, so a swallowed failure oversubscribes the runner — and answering
    // `200` tells it the whole transition landed. Reporting the failure is
    // retry-safe: the job stays assigned to this runner, so a repeated `start`
    // passes the same gate and rewrites the same `running` row.
    if let Err(e) = rg_db::ops::runner_ops::update_status(&state.db, runner_id, "busy").await {
        tracing::error!(runner_id, error = %format!("{e:#}"), "Failed to mark runner as busy");
        return AppError::from(e).into_response();
    }

    // Metrics: a job is now executing on a runner. After the transition above,
    // for the same reason `finish_job` counts after its roll-up — a 5xx makes
    // the runner retry `start`, and counting before it counts one job per retry.
    crate::metrics::recorder::ci_job_started();

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
    let key_hash = cache_key_hash(key);
    // The row is the handle on the archive, not a hint about it: every
    // publication is written under its own name, so a file this lookup cannot
    // reach is residue rather than a cache.
    let entry = match cache_entry_for_download(&state.db, repo_id, &key_hash).await {
        Ok(Some(entry)) => entry,
        Ok(None) => return AppError::not_found("cache entry not found").into_response(),
        Err(error) => return error.into_response(),
    };
    let directory = cache_archive_dir(&state, repo_id);
    let path = match recorded_cache_archive(&directory, &entry.file_path) {
        Some(path) => path,
        None => {
            return AppError::internal(anyhow::anyhow!(
                "CI cache entry {} names no archive file",
                entry.id
            ))
            .into_response()
        }
    };
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
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let sha256 = cache_content_hash(&bytes);
            // Integrity: the stored archive must still hash to the digest recorded
            // at upload — a tampered/corrupted cache would otherwise inject files
            // into a downstream build. Legacy entries carry no digest and are
            // served without this guard.
            if let Some(expected) = entry.sha256.as_deref() {
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
    let key_hash = cache_key_hash(key);
    let directory = cache_archive_dir(&state, repo_id);
    if let Err(error) = tokio::fs::create_dir_all(&directory).await {
        return cache_path_error("CI cache directory", &directory, &error).into_response();
    }
    // What the live entry names *before* this upload rewrites it. Read here
    // because the upsert below is the point of no return: afterwards the row
    // names this request's archive and the previous publication's bytes have no
    // handle left in the database at all.
    let replaced = match cache_entry_for_download(&state.db, repo_id, &key_hash).await {
        Ok(entry) => entry.map(|entry| entry.file_path),
        Err(error) => return error.into_response(),
    };
    // Digest the payload before the write consumes `body` — the archive is
    // already fully buffered in memory (≤ 1 GiB, bounded above), so hashing the
    // in-memory bytes costs nothing extra.
    let sha256 = cache_content_hash(body.as_ref());
    let size = body.len() as i64;
    // Every publication is written under a name of its own. Under the stable
    // `<key_hash>.tar` a retry wrote over the archive the live row still named,
    // so any failure below compensated by deleting bytes that belonged to the
    // *previous*, successful upload: one failed retry turned a working cache
    // into a row pointing at nothing. A request-private name makes the rollback
    // provable — the only file this request can ever remove is the one this
    // request created.
    let path = directory.join(format!("{key_hash}.{}.tar", uuid::Uuid::new_v4()));
    // From here until `upsert_cache_entry` succeeds there is a file on disk that
    // no DB row points at. Retention walks rows, so every early exit below has to
    // take its file with it — otherwise the failure leaks a cache-sized archive
    // that nothing will ever come back for.
    if let Err(error) = tokio::fs::write(&path, body).await {
        discard_unreferenced_cache_file("CI cache archive", &path, repo_id, job_id, key).await;
        return cache_path_error("CI cache archive", &path, &error).into_response();
    }
    let policy = match rg_db::ops::ci_retention_ops::get_policy(&state.db, repo_id).await {
        Ok(policy) => policy,
        Err(error) => {
            discard_unreferenced_cache_file("CI cache archive", &path, repo_id, job_id, key).await;
            return AppError::from(error).into_response();
        }
    };
    if let Err(error) = rg_db::ops::ci_retention_ops::upsert_cache_entry(
        &state.db,
        repo_id,
        &key_hash,
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
    // The row names this request's archive now, and that — not the write that
    // returned `Ok` earlier — is what makes the archive it named before ours to
    // retire. Only a request that got this far may touch it; a failure above
    // leaves it exactly where the previous upload put it, still restorable.
    //
    // Two uploads of one key racing here can leave the loser's archive behind
    // with no row naming it: waste that retention (which walks rows) will not
    // reclaim, and the deliberate side of the trade — the ordering that avoids
    // it is the one that risks deleting a cache somebody is still restoring.
    if let Some(previous) = replaced
        .as_deref()
        .and_then(|recorded| recorded_cache_archive(&directory, recorded))
    {
        if previous != path {
            discard_replaced_cache_file(&previous, repo_id, job_id, key).await;
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Retire the archive a previous publication left behind, once this request's
/// own archive is the one the row names.
///
/// Best-effort for the same reason as the rollback below: the upload succeeded,
/// so a failure here cannot be reported to the runner without lying about the
/// cache it just stored. What stays behind is waste, not loss.
async fn discard_replaced_cache_file(path: &std::path::Path, repo_id: i64, job_id: i64, key: &str) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            repo_id,
            job_id,
            cache_key = %key,
            path = %path.display(),
            error = %error,
            "superseded CI cache archive not deleted — the file stays on disk after the entry moved to the newly uploaded archive"
        ),
    }
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

/// Read the metadata that makes a cache archive safe to serve.
///
/// A lookup error is not a cache miss: without this row we cannot enforce the
/// expiry or compare the archive against its recorded content digest.
async fn cache_entry_for_download(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    key_hash: &str,
) -> Result<Option<rg_db::entities::ci_cache_entry::Model>, AppError> {
    rg_db::ops::ci_retention_ops::find_cache_entry(db, repo_id, key_hash)
        .await
        .map_err(AppError::from)
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

/// Where one repository's cache archives live.
fn cache_archive_dir(state: &AppState, repo_id: i64) -> std::path::PathBuf {
    state.repo_root.join("_ci_cache").join(repo_id.to_string())
}

/// Resolve the archive a cache row names, inside `directory` and nowhere else.
///
/// A row records the full path it was written under, and both writers — this
/// module and the in-process runner in `rg_ci` — spell that root differently
/// (configured `repo_root` versus a path derived from the repository), so the
/// prefix is not something either side can compare against. The file name is:
/// every archive of a repository sits directly in this one directory, which is
/// also what keeps a row from ever addressing a file outside it.
fn recorded_cache_archive(
    directory: &std::path::Path,
    recorded: &str,
) -> Option<std::path::PathBuf> {
    std::path::Path::new(recorded)
        .file_name()
        .map(|name| directory.join(name))
}

/// One actionable error for a filesystem failure on the server side of the CI
/// cache.
///
/// The archive path is `_ci_cache/<repo_id>/<sha256-of-cache-key>.<publication>.tar`
/// under `repo_root`: it is built inside the handler and never appears in the
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
    let key = rg_core::auth::encryption::derive_key(&state.encryption_key);
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
    //
    // Conditional, and that is the whole point of this call: a cancellation is
    // transactional at the moment it answers, but it cannot stop a runner that
    // already holds the job. The report arriving now was computed from a
    // snapshot older than the cascade, and writing it unconditionally walked
    // job → stage → pipeline back out of `canceled` — the caller who got
    // `200 {"status":"canceled"}` would watch the pipeline turn green, success
    // hooks and auto-merge included.
    //
    // Rewriting the status the row already carries still counts as landing, so
    // the runner's `finish` retry stays idempotent and keeps driving the
    // roll-up below.
    let job_settled = rg_db::ops::pipeline_ops::settle_job_if_active(
        &state.db,
        job_id,
        &req.status,
        Some(req.exit_code),
        None,
        now,
    )
    .await;

    // Mark runner as online (ready for next job). The mirror of `start_job`'s
    // `busy`: a runner left `busy` after finishing is a runner the scheduler
    // skips until the watchdog's 90-second sweep, so this transition is part of
    // what `{"status":"ok"}` claims. It stays ahead of the roll-up below so a
    // failure here returns before any completion metric is emitted, and the
    // runner's retry (`finish` is the one report it retries) redoes the whole
    // sequence against the same job row.
    //
    // It also runs before the late-completion answer below: a runner whose job
    // was canceled under it is still a free runner, and leaving it `busy` would
    // park it until the watchdog sweep.
    if let Err(e) = rg_db::ops::runner_ops::update_status(&state.db, runner_id, "online").await {
        tracing::error!(runner_id, error = %format!("{e:#}"), "Failed to mark runner as online");
        return AppError::from(e).into_response();
    }

    match job_settled {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(
                runner_id,
                job_id,
                reported = %req.status,
                "finish_job: late completion for a job that already settled — not applied"
            );
            // Answering `200 {"status":"ok"}` here would be a false receipt for
            // a state change that did not happen. The runner has nothing to
            // retry, so this is a conflict, not a server error.
            return AppError::conflict(
                "job already settled (canceled or completed) — completion not applied",
            )
            .into_response();
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "finish_job: update_job_result failed");
            return AppError::from(e).into_response();
        }
    }

    // Cascade: check if stage is done, then if pipeline is done
    let stage_status =
        match rg_db::ops::pipeline_ops::try_update_stage(&state.db, job.stage_id).await {
            Ok(status) => status,
            Err(error) => {
                tracing::error!(
                    stage_id = job.stage_id,
                    error = %format!("{error:#}"),
                    "Failed to update stage after job completion"
                );
                return AppError::from(error).into_response();
            }
        };
    if stage_status.is_some() {
        // Stage is done — get pipeline_id and check pipeline
        let stage = match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await {
            Ok(Some(stage)) => stage,
            Ok(None) => {
                tracing::error!(
                    job_id,
                    stage_id = job.stage_id,
                    "finish_job: completed stage disappeared before pipeline roll-up"
                );
                return AppError::internal("pipeline stage not found after job completion")
                    .into_response();
            }
            Err(error) => {
                tracing::error!(
                    job_id,
                    stage_id = job.stage_id,
                    error = %format!("{error:#}"),
                    "finish_job: failed to reload completed stage"
                );
                return AppError::from(error).into_response();
            }
        };
        match rg_db::ops::pipeline_ops::try_update_pipeline(&state.db, stage.pipeline_id).await {
            Ok(Some(status)) => {
                if status == "success" {
                    let pipeline =
                        match rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
                            .await
                        {
                            Ok(Some(pipeline)) => pipeline,
                            Ok(None) => {
                                tracing::error!(
                                job_id,
                                pipeline_id = stage.pipeline_id,
                                "finish_job: completed pipeline disappeared before post-push hooks"
                            );
                                return AppError::internal(
                                    "pipeline not found after job completion",
                                )
                                .into_response();
                            }
                            Err(error) => {
                                tracing::error!(
                                    job_id,
                                    pipeline_id = stage.pipeline_id,
                                    error = %format!("{error:#}"),
                                    "finish_job: failed to reload completed pipeline"
                                );
                                return AppError::from(error).into_response();
                            }
                        };
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
                // Metrics: the pipeline reached a terminal status. Emit only
                // after all mandatory context was loaded, so a runner retry
                // after a failed lookup does not double-count the completion.
                crate::metrics::recorder::ci_pipeline_finished(&status);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(
                    pipeline_id = stage.pipeline_id,
                    error = %format!("{error:#}"),
                    "Failed to update pipeline after stage completion"
                );
                return AppError::from(error).into_response();
            }
        }
    }

    // Metrics: job left the running set — count its outcome and, if we know when
    // it started, its execution duration. This is deliberately after roll-up:
    // a failed roll-up makes the runner retry `finish`, and recording before it
    // would count the same job once per retry.
    let job_duration = job
        .started_at
        .and_then(|started| (chrono::Utc::now().naive_utc() - started).to_std().ok());
    crate::metrics::recorder::ci_job_finished(&req.status, job_duration);

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
/// What became of the `last_seen_at` refresh `authenticate_runner` performs on
/// every authenticated runner request.
///
/// The refresh is opportunistic for every route but `/heartbeat`: a runner
/// reporting a finished job has done the work whether or not its liveness
/// timestamp could be rewritten, and failing that report would throw away a
/// result the runner cannot reproduce. So the middleware carries the outcome
/// forward in the request extensions rather than deciding for the handler, and
/// only `heartbeat` — where the refresh *is* the request — turns a failure into
/// a failed response.
#[derive(Clone, Copy, Debug)]
pub enum HeartbeatRefresh {
    /// `last_seen_at` was written.
    Persisted,
    /// The write failed against an unreachable database — retryable, so the
    /// runner's next heartbeat may well land (503).
    Unavailable,
    /// The write failed for a reason retrying will not fix (500).
    Failed,
}

pub async fn authenticate_runner(
    State(state): State<AppState>,
    mut request: Request,
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
            // Valid token — also update heartbeat. The outcome travels with the
            // request instead of being dropped here: `heartbeat` answers for
            // this write, every other handler is free to ignore it.
            let refresh = match rg_db::ops::runner_ops::update_heartbeat(&state.db, runner_id).await
            {
                Ok(()) => HeartbeatRefresh::Persisted,
                Err(error) => {
                    tracing::error!(runner_id, error = %format!("{error:#}"), "Failed to update runner heartbeat");
                    // Same outage predicate the `AppError` conversions use, so a
                    // dead pool is a retryable 503 on `/heartbeat` exactly as it
                    // is on every other route — classified here, where the
                    // `DbErr` still exists.
                    if error
                        .downcast_ref::<sea_orm::DbErr>()
                        .is_some_and(AppError::is_db_outage)
                    {
                        HeartbeatRefresh::Unavailable
                    } else {
                        HeartbeatRefresh::Failed
                    }
                }
            };
            request.extensions_mut().insert(refresh);
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
mod cache_entry_lookup_tests {
    use super::*;

    /// A failed lookup must stay distinguishable from `Ok(None)`: otherwise the
    /// download path serves an archive without its expiry and digest metadata.
    #[tokio::test]
    async fn a_closed_pool_is_not_a_missing_cache_entry() {
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("open lookup test database");
        db.clone().close().await.expect("close lookup test pool");

        let error = cache_entry_for_download(&db, 1, "cache-key")
            .await
            .expect_err("a closed pool must not become an absent cache entry");

        assert_eq!(
            error.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a failed cache-entry lookup must be retryable, not a cache miss"
        );
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
