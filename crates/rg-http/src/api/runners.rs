//! REST API handlers for CI/CD Runners.

use axum::body::Body;
use axum::extract::{Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};

use crate::api::access_audit::{grant_actor, record_instance_credential, InstanceResource};
use crate::api::admin::InstanceAdmin;
use crate::api::auth::SudoUser;
use crate::error::AppError;
use crate::AppState;
use utoipa::{IntoParams, ToSchema};

/// The declared ceiling for one job-log upload.
///
/// The runner posts a job's whole output in a single request, so this is the
/// ceiling on an entire build's log rather than on a chunk of one — and a
/// verbose build (`cargo build -v`, `npm ci`, `docker build`) clears Axum's
/// hidden 2 MiB `DefaultBodyLimit` without trying. Left unstated, that default
/// turned a long build's diagnosis into a job with no log at all.
///
/// The number is bounded by what the log then costs downstream rather than by
/// what a build can print: the body is masked in memory, broadcast whole to
/// every websocket subscriber of the job, and stored by rewriting the job row's
/// whole `log` column. `rg_runner::api` trims to the same ceiling before it
/// sends, so an over-long log arrives shortened and marked instead of being
/// refused.
pub(crate) const JOB_LOG_MAX_BYTES: usize = 8 * 1024 * 1024;

/// The declared ceiling for one CI cache archive.
///
/// The same number the route's `Wrap::runner_auth_with_body_limit` layers, kept
/// here so the handler's own backstop and the transport ceiling are one value
/// rather than two literals that agree today. The archive never becomes a heap
/// buffer of this size — [`stage_cache_archive`] spools it — so the ceiling
/// bounds what a job may store, not what a request may cost this process.
pub(crate) const CACHE_ARCHIVE_MAX_BYTES: usize = 1024 * 1024 * 1024;

// ── Request/Response types ─────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct RegisterRunnerRequest {
    /// Repository scope in human-addressable `owner/name` form.
    pub repository: String,
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
    repository: String,
    message: String,
}

#[derive(Serialize, ToSchema)]
pub struct HeartbeatResponse {
    status: String,
    server_time: String,
}

/// What the server holds about a job its runner is executing.
#[derive(Serialize, ToSchema)]
pub struct RunnerJobStatusResponse {
    job_id: i64,
    /// The job row's status, verbatim.
    status: String,
    /// Whether the job is still work this runner should be doing. `false`
    /// means it was canceled or otherwise settled behind the runner's back,
    /// and the runner should stop executing it and publish nothing.
    active: bool,
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
    /// Name the artifact this job publishes is stored under, and the
    /// workspace-relative paths packed into it. Both are `None` when the job
    /// declares no artifact — the runner uploads nothing in that case.
    artifact_name: Option<String>,
    artifact_paths: Option<Vec<String>>,
    timeout: i64,
    /// The job's steps, for a job translated from an Actions workflow: the
    /// runner executes them one by one and reports each through
    /// `POST .../steps/{number}`. `None` for a native job, whose `script` is
    /// the one program it runs. Only a runner that speaks
    /// [`rg_core::ci::STEPS_PROTOCOL`] is ever handed one.
    #[schema(value_type = Option<Vec<Object>>)]
    steps: Option<Vec<serde_json::Value>>,
}

/// The artifact declaration as `pipeline_jobs.artifacts` stores it.
#[derive(Deserialize)]
struct StoredJobArtifacts {
    name: String,
    paths: Vec<String>,
}

#[derive(Deserialize, IntoParams)]
pub struct PollJobQuery {
    pub timeout: Option<u64>, // seconds, default 30
}

#[derive(Serialize, ToSchema)]
pub struct RunnerInfoResponse {
    id: i64,
    repo_id: Option<i64>,
    repository: Option<String>,
    name: String,
    status: String,
    labels: String,
    last_seen_at: String,
    version: Option<String>,
    os: Option<String>,
    arch: Option<String>,
}

async fn runner_info_response(
    state: &AppState,
    runner: rg_db::entities::runner::Model,
) -> Result<RunnerInfoResponse, AppError> {
    let repository = match runner.repo_id {
        Some(repo_id) => {
            let (owner, name) = rg_core::repo::service::repository_identity(&state.db, repo_id)
                .await
                .map_err(AppError::from)?;
            Some(format!("{owner}/{name}"))
        }
        None => None,
    };
    Ok(RunnerInfoResponse {
        id: runner.id,
        repo_id: runner.repo_id,
        repository,
        name: runner.name,
        status: runner.status,
        labels: runner.labels,
        last_seen_at: runner.last_seen_at.to_string(),
        version: runner.version,
        os: runner.os,
        arch: runner.arch,
    })
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
        Ok(Some(runner)) => match runner_info_response(&state, runner).await {
            Ok(response) => (StatusCode::OK, Json(response)).into_response(),
            Err(error) => error.into_response(),
        },
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
        (status = 403, description = "A login session is required to create credentials", body = serde_json::Value),
    ),
)]
pub async fn register(
    State(state): State<AppState>,
    InstanceAdmin(actor_id): InstanceAdmin,
    SudoUser(_session_user): SudoUser,
    headers: HeaderMap,
    Json(req): Json<RegisterRunnerRequest>,
) -> impl IntoResponse {
    let repository_scope = req.repository.trim();
    let Some((owner, repo_name)) = repository_scope.split_once('/') else {
        return AppError::bad_request("repository must use owner/name format").into_response();
    };
    if owner.is_empty()
        || repo_name.is_empty()
        || repo_name.contains('/')
        || owner.trim() != owner
        || repo_name.trim() != repo_name
    {
        return AppError::bad_request("repository must use owner/name format").into_response();
    }
    let repository =
        match rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, repo_name).await {
            Ok(Some(repository)) => repository,
            Ok(None) => return AppError::not_found("repository not found").into_response(),
            Err(error) => {
                tracing::error!(
                    repository = repository_scope,
                    error = %format!("{error:#}"),
                    "register runner repository lookup failed"
                );
                return AppError::from(error).into_response();
            }
        };
    let labels_json =
        serde_json::to_string(&req.labels.unwrap_or_default()).unwrap_or_else(|_| "[]".to_string());

    // Before the token exists, per the rule in `access_audit`: a failed name
    // lookup afterwards would leave the widest credential this instance issues
    // with a blank author, and this way it is a 5xx from a request that issued
    // nothing.
    let actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };

    match rg_db::ops::runner_ops::register_runner(
        &state.db,
        repository.id,
        &req.name,
        &labels_json,
        req.version.as_deref(),
        req.os.as_deref(),
        req.arch.as_deref(),
    )
    .await
    {
        // The only place the plaintext token exists after generation: the row
        // carries its hash, so this response is the operator's one chance to
        // copy it into the runner's config.
        Ok((runner, token)) => {
            record_instance_credential(
                &state,
                &actor,
                "admin.register_runner",
                InstanceResource {
                    kind: "runner",
                    id: runner.id,
                    name: &runner.name,
                },
                &headers,
                runner_details(&runner),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(RegisterRunnerResponse {
                    id: runner.id,
                    token,
                    repository: repository_scope.to_string(),
                    message: "Runner registered successfully".to_string(),
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "register runner failed");
            AppError::from(e).into_response()
        }
    }
}

/// What the journal says about a runner, and what it deliberately leaves out.
///
/// The repository scope and labels are what make the entry worth reading:
/// `repo_id` is the hard credential boundary, while labels select compatible
/// jobs only inside it. An entry naming only the runner would answer "a runner
/// appeared" and not "which repository entrusted this machine with its jobs and
/// secrets", which is the question an incident review is asking.
///
/// Never the token. `register` returns it once and the row keeps only its hash,
/// so the response is the only copy — a journal operators read must not become
/// a second one.
fn runner_details(runner: &rg_db::entities::runner::Model) -> serde_json::Value {
    serde_json::json!({
        "name": runner.name,
        "repo_id": runner.repo_id,
        // The column is JSON text. Rendered as the array it is when it parses,
        // so the entry reads the same way the runner's own configuration does;
        // a value this server did not write stays whatever it is rather than
        // being dropped.
        "labels": serde_json::from_str::<serde_json::Value>(&runner.labels)
            .unwrap_or_else(|_| serde_json::Value::String(runner.labels.clone())),
        "version": runner.version,
        "os": runner.os,
        "arch": runner.arch,
    })
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
        // The middleware always writes on this route; a skipped write here is
        // the same wiring fault as a missing extension, and `200` would claim
        // a write nobody made.
        Some(HeartbeatRefresh::Recent) => {
            AppError::internal("runner heartbeat route skipped its write").into_response()
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
    headers: HeaderMap,
) -> impl IntoResponse {
    // Read before the delete, for the same reason as on the admin route: after
    // it there is nothing left that says which machine stopped being able to
    // take jobs. A failed lookup is not fatal here — this request is the
    // runner's own orderly exit and refusing it would leave the row behind —
    // so the entry is written with what is known.
    let runner = match rg_db::ops::runner_ops::find_by_id(&state.db, runner_id).await {
        Ok(runner) => runner,
        Err(error) => {
            tracing::warn!(
                runner_id,
                error = %format!("{error:#}"),
                "could not read the runner about to deregister itself; its journal entry will                  name the id and nothing else"
            );
            None
        }
    };

    // Handing the runner's in-flight jobs back to the pool and deleting the
    // runner row is one transaction, not two writes with a log line between
    // them: deleting a runner whose jobs were not reset strands those jobs on a
    // row that no longer exists. See `runner_ops::deregister_runner`.
    match rg_db::ops::runner_ops::deregister_runner(&state.db, runner_id).await {
        Ok(true) => {
            // No account performed this: the request is authenticated by the
            // runner's own token, so the actor columns stay `NULL` rather than
            // naming whoever happened to register the machine months ago. The
            // resource is what identifies it.
            record_instance_credential(
                &state,
                &rg_core::audit::AuditActor::none(),
                "runner.deregister",
                InstanceResource {
                    kind: "runner",
                    id: runner_id,
                    name: runner
                        .as_ref()
                        .map(|runner| runner.name.as_str())
                        .unwrap_or_default(),
                },
                &headers,
                match runner.as_ref() {
                    Some(runner) => runner_details(runner),
                    None => serde_json::json!({"runner_id": runner_id}),
                },
            )
            .await;
            (
                StatusCode::OK,
                Json(serde_json::json!({"status": "deregistered"})),
            )
                .into_response()
        }
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
        (status = 426, description = "The only work this runner could take is a job of steps, and it did not send the steps protocol header", body = serde_json::Value),
    ),
)]
pub async fn poll_job(
    State(state): State<AppState>,
    Path(runner_id): Path<i64>,
    Query(query): Query<PollJobQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let timeout_secs = query.timeout.unwrap_or(30).min(300);
    let runs_steps = runner_runs_steps(&headers);

    // Use tokio::time::timeout to wrap a polling loop
    let poll_future = async {
        let (runner_repo_id, runner_labels) = poll_scope(&state, runner_id).await?;

        loop {
            match crate::metrics::time_db(
                "pipeline.find_pending_job",
                rg_db::ops::pipeline_ops::find_pending_job_for_runner(
                    &state.db,
                    runner_repo_id,
                    &runner_labels,
                    runs_steps,
                ),
            )
            .await
            {
                Ok(rg_db::ops::pipeline_ops::PendingPick::NeedsSteps(job)) => {
                    return Err(steps_protocol_refusal(runner_id, &job));
                }
                Ok(rg_db::ops::pipeline_ops::PendingPick::Job(job)) => {
                    let (resp, encrypted_mask) = prepare_dispatch(&state, runner_id, job).await?;
                    // Everything the response needs is prepared before the
                    // irreversible claim. The long-poll future can be dropped
                    // at any `.await` (its own timeout, a disconnected client,
                    // or server shutdown); claiming earlier stranded the row as
                    // `assigned` while the only response carrying it vanished.
                    // Keep this as the final await before returning the body.
                    if claim_dispatched_job(&state, runner_id, resp.job_id, &encrypted_mask).await?
                    {
                        return Ok((StatusCode::OK, Json(resp)));
                    }
                    continue;
                }
                Ok(rg_db::ops::pipeline_ops::PendingPick::Nothing) => {
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
        Err(_elapsed) => {
            tracing::debug!(
                runner_id,
                timeout_secs,
                "poll_job: long-poll elapsed without an available job"
            );
            (StatusCode::NO_CONTENT, Json(serde_json::json!({}))).into_response()
        }
    }
}

/// Whether the polling runner runs a job's steps one by one — it says so with
/// [`rg_core::ci::RUNNER_PROTOCOL_HEADER`].
///
/// A runner built before the step model sends nothing, and would read a job of
/// steps through the fields it knows: it would run the flat `script` as one
/// program, with every step's `continue-on-error`, `timeout-minutes` and
/// `shell` silently gone. So it is never handed one.
fn runner_runs_steps(headers: &HeaderMap) -> bool {
    headers
        .get(rg_core::ci::RUNNER_PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u32>().ok())
        .is_some_and(|protocol| protocol >= rg_core::ci::STEPS_PROTOCOL)
}

/// What a runner too old for the step model is told when the only work it
/// could take is a job of steps: `426`, naming the job and the remedy, rather
/// than the job half-run or a silence that looks like an empty queue. The job
/// stays `pending` for a runner that can run it.
fn steps_protocol_refusal(runner_id: i64, job: &rg_db::entities::pipeline_job::Model) -> Response {
    tracing::warn!(
        runner_id,
        job_id = job.id,
        "poll_job: refused a job of steps to a runner that does not run steps"
    );
    (
        StatusCode::UPGRADE_REQUIRED,
        Json(serde_json::json!({
            "error": format!(
                "job #{} ({}) runs its Actions steps one by one, and this runner runs a job as one \
                 script: it would ignore every step's continue-on-error, timeout-minutes and \
                 shell. Upgrade plombir-git-runner; the job waits for a runner that can run it.",
                job.id, job.name
            ),
        })),
    )
        .into_response()
}

/// The repository a polling runner serves and the labels it matches jobs by.
///
/// Fetched together: the repository is the runner's immutable capability. A
/// legacy row has no trustworthy scope and is therefore revoked until the
/// operator explicitly re-registers it.
async fn poll_scope(state: &AppState, runner_id: i64) -> Result<(i64, Vec<String>), Response> {
    match rg_db::ops::runner_ops::find_by_id(&state.db, runner_id).await {
        Ok(Some(runner)) => {
            let Some(repo_id) = runner.repo_id else {
                return Err(AppError::forbidden(
                    "runner is not repository-scoped; re-register it for owner/repo",
                )
                .into_response());
            };
            match serde_json::from_str(&runner.labels) {
                Ok(labels) => Ok((repo_id, labels)),
                Err(error) => {
                    tracing::error!(
                        runner_id,
                        error = %error,
                        "poll_job: stored runner labels are invalid JSON"
                    );
                    Err(AppError::internal("invalid runner labels").into_response())
                }
            }
        }
        Ok(None) => Err(AppError::not_found("runner not found").into_response()),
        Err(error) => {
            tracing::error!(
                runner_id,
                error = %format!("{error:#}"),
                "poll_job: runner lookup failed"
            );
            Err(AppError::from(error).into_response())
        }
    }
}

/// The runner-facing JSON columns of a candidate job, decoded.
struct DecodedJobColumns {
    variables: serde_json::Map<String, serde_json::Value>,
    cache_paths: Option<Vec<String>>,
    artifact_name: Option<String>,
    artifact_paths: Option<Vec<String>>,
}

/// Decode every runner-facing JSON column BEFORE the job is claimed.
///
/// Both used to be decoded after the assignment and with the error thrown
/// away: a corrupt `variables` collapsed into an empty map and a corrupt
/// `cache_paths` into `None`, so the runner got a `200` and started work with
/// its mandatory environment missing and caching silently off — while the row
/// was already `running` and owned by that runner, so no other runner could
/// pick it up either. Refusing here answers 5xx and leaves the row `pending`
/// and unassigned.
///
/// `NULL` still means "this optional column was never set" and keeps its
/// previous meaning; only undecodable text is refused.
fn decode_job_columns(
    job: &rg_db::entities::pipeline_job::Model,
    runner_id: i64,
) -> Result<DecodedJobColumns, AppError> {
    let variables = match job.variables.as_deref() {
        Some(json) => {
            match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json) {
                Ok(variables) => variables,
                Err(error) => {
                    tracing::error!(
                        job_id = job.id,
                        runner_id,
                        error = %error,
                        "poll_job: stored job variables are invalid JSON"
                    );
                    return Err(AppError::internal("invalid job variables"));
                }
            }
        }
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
                return Err(AppError::internal("invalid job cache paths"));
            }
        },
        None => None,
    };
    // Decoded on the same terms as `cache_paths` above, and for a sharper
    // reason: an artifact declaration that collapses into `None` produces a
    // green job that publishes nothing, and the artifact list a user opens
    // afterwards is empty with no failure anywhere to explain it.
    let (artifact_name, artifact_paths) = match job.artifacts.as_deref() {
        Some(json) => match serde_json::from_str::<StoredJobArtifacts>(json) {
            Ok(artifacts) => (Some(artifacts.name), Some(artifacts.paths)),
            Err(error) => {
                tracing::error!(
                    job_id = job.id,
                    runner_id,
                    error = %error,
                    "poll_job: stored job artifacts are invalid JSON"
                );
                return Err(AppError::internal("invalid job artifacts"));
            }
        },
        None => (None, None),
    };
    Ok(DecodedJobColumns {
        variables,
        cache_paths,
        artifact_name,
        artifact_paths,
    })
}

/// The pipeline a candidate job belongs to, through its stage.
async fn candidate_pipeline(
    state: &AppState,
    job: &rg_db::entities::pipeline_job::Model,
) -> Result<rg_db::entities::pipeline::Model, Response> {
    let stage = match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await {
        Ok(Some(stage)) => stage,
        Ok(None) => {
            tracing::error!(
                job_id = job.id,
                stage_id = job.stage_id,
                "poll_job: candidate job has no pipeline stage"
            );
            return Err(AppError::internal("candidate job has no pipeline stage").into_response());
        }
        Err(error) => {
            tracing::error!(
                job_id = job.id,
                stage_id = job.stage_id,
                error = %format!("{error:#}"),
                "poll_job: candidate pipeline stage lookup failed"
            );
            return Err(AppError::from(error).into_response());
        }
    };
    let pipeline_id = stage.pipeline_id;
    match rg_db::ops::pipeline_ops::get_pipeline(&state.db, pipeline_id).await {
        Ok(Some(pipeline)) => Ok(pipeline),
        Ok(None) => {
            tracing::error!(
                job_id = job.id,
                stage_id = job.stage_id,
                pipeline_id,
                "poll_job: candidate job has no pipeline"
            );
            Err(AppError::internal("candidate job has no pipeline").into_response())
        }
        Err(error) => {
            tracing::error!(
                job_id = job.id,
                stage_id = job.stage_id,
                pipeline_id,
                error = %format!("{error:#}"),
                "poll_job: candidate pipeline lookup failed"
            );
            Err(AppError::from(error).into_response())
        }
    }
}

/// Lay the server-owned environment over the job's declared variables: the
/// repository's secrets (each one also a value the log mask hides), the
/// built-in `CI_*` variables and the job token. Returns the secrets to mask.
async fn add_server_variables(
    state: &AppState,
    job: &rg_db::entities::pipeline_job::Model,
    pipeline: &rg_db::entities::pipeline::Model,
    timeout_secs: u64,
    variables: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<String>, Response> {
    let pipeline_id = pipeline.id;
    for reserved in rg_core::ci::BUILTIN_CI_VARIABLES {
        variables.remove(reserved);
    }
    let mut mask_secrets = Vec::new();
    match decrypted_repo_secrets(state, pipeline.repo_id, job.environment_id).await {
        Ok(secrets) => {
            for (name, value) in secrets {
                mask_secrets.push(value.clone());
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
    variables.insert("PLOMBIR_GIT".into(), serde_json::json!("true"));
    variables.insert("CI_PIPELINE_ID".into(), serde_json::json!(pipeline_id));
    variables.insert(
        "CI_COMMIT_SHA".into(),
        serde_json::json!(pipeline.commit_sha),
    );
    variables.insert("CI_SHA".into(), serde_json::json!(pipeline.commit_sha));
    variables.insert("CI_REF".into(), serde_json::json!(pipeline.ref_name));
    variables.insert("CI_EVENT".into(), serde_json::json!(pipeline.trigger_type));
    if let Some(cron) = pipeline.schedule_cron.as_deref() {
        variables.insert("CI_EVENT_SCHEDULE".into(), serde_json::json!(cron));
    }
    let (repository_owner, repository_name) =
        match rg_core::repo::service::repository_identity(&state.db, pipeline.repo_id).await {
            Ok(identity) => identity,
            Err(error) => {
                tracing::error!(
                    pipeline_id,
                    error = %format!("{error:#}"),
                    "poll_job: repository identity lookup failed after assignment"
                );
                return Err(AppError::from(error).into_response());
            }
        };
    variables.insert(
        "CI_REPOSITORY".into(),
        serde_json::json!(format!("{repository_owner}/{repository_name}")),
    );
    variables.insert(
        "CI_REPOSITORY_OWNER".into(),
        serde_json::json!(repository_owner),
    );
    match rg_core::auth::ci_token::generate_ci_job_token_with_ttl(
        pipeline.repo_id,
        pipeline.id,
        job.id,
        "repo:read packages:read",
        &state.jwt_secret,
        rg_core::ci::ci_job_token_ttl_secs(timeout_secs),
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
            return Err(AppError::internal("failed to prepare job environment").into_response());
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
    Ok(mask_secrets)
}

/// Everything a runner is handed for `job`, and the sealed log mask the claim
/// records with it — all of it prepared before anything irreversible happens.
async fn prepare_dispatch(
    state: &AppState,
    runner_id: i64,
    job: rg_db::entities::pipeline_job::Model,
) -> Result<(PollJobResponse, String), Response> {
    let DecodedJobColumns {
        mut variables,
        cache_paths,
        artifact_name,
        artifact_paths,
    } = decode_job_columns(&job, runner_id).map_err(IntoResponse::into_response)?;
    let pipeline = candidate_pipeline(state, &job).await?;
    // One resolution for both the token's lifetime and the deadline the runner
    // is handed, sharing the range the config validator enforces.
    let timeout_secs =
        rg_core::ci::resolve_job_timeout_secs(job.id, job.timeout_seconds, state.job_timeout_secs);
    let mask_secrets =
        add_server_variables(state, &job, &pipeline, timeout_secs, &mut variables).await?;

    let encrypted_mask = match encrypt_job_mask(state, &mask_secrets) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::error!(job_id = job.id, error = %format!("{error:#}"), "failed to seal runner log mask");
            return Err(AppError::internal("failed to prepare job log mask").into_response());
        }
    };

    // A job of steps whose stored steps this server cannot read is refused
    // before the claim, like every other undecodable runner-facing column: a
    // runner handed `steps: null` would run the flat script instead.
    let steps = match job
        .steps
        .as_deref()
        .map(serde_json::from_str::<Vec<serde_json::Value>>)
        .transpose()
    {
        Ok(steps) => steps,
        Err(error) => {
            tracing::error!(
                job_id = job.id,
                runner_id,
                %error,
                "poll_job: stored job steps are not valid JSON"
            );
            return Err(AppError::internal("invalid stored job steps").into_response());
        }
    };
    let resp = PollJobResponse {
        steps,
        job_id: job.id,
        pipeline_id: pipeline.id,
        stage_id: job.stage_id,
        name: job.name,
        script: job.script.lines().map(|s| s.to_string()).collect(),
        image: job.image,
        variables: Some(serde_json::Value::Object(variables)),
        cache_key: job.cache_key,
        cache_paths,
        artifact_name,
        artifact_paths,
        timeout: rg_core::ci::dispatched_job_timeout_secs(timeout_secs),
    };
    Ok((resp, encrypted_mask))
}

/// Claim a prepared job for `runner_id`: `Ok(false)` when another poller or a
/// cancellation got there first.
///
/// The candidate came from a snapshot, so `assign_job` re-asserts `pending AND
/// runner_id IS NULL` and makes the database choose between a concurrent
/// cancellation or poller. A refused claim needs no backoff: the candidate
/// query can no longer return that row in the same state.
async fn claim_dispatched_job(
    state: &AppState,
    runner_id: i64,
    job_id: i64,
    encrypted_mask: &str,
) -> Result<bool, Response> {
    match rg_db::ops::pipeline_ops::assign_job_with_mask_snapshot(
        &state.db,
        job_id,
        runner_id,
        encrypted_mask,
    )
    .await
    {
        Ok(true) => Ok(true),
        Ok(false) => {
            tracing::info!(
                job_id,
                runner_id,
                "poll_job: candidate was claimed or settled before this runner could take it"
            );
            Ok(false)
        }
        Err(error) => {
            tracing::error!(
                job_id,
                runner_id,
                error = %format!("{error:#}"),
                "poll_job: failed to assign job"
            );
            Err(AppError::from(error).into_response())
        }
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

    // A job that is run again — handed back, reclaimed, retried — runs its
    // steps again, and what the previous run reported is not this run's.
    if let Err(e) = rg_db::ops::pipeline_step_ops::reset_job_steps(&state.db, job_id).await {
        tracing::error!(job_id, error = %format!("{e:#}"), "start_job: failed to reset the job's steps");
        return AppError::from(e).into_response();
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

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

/// GET /api/v1/runners/:id/jobs/:job_id/status
/// Whether the job a runner is executing is still its to run.
///
/// Cancellation is a write on the server; the work it is meant to stop runs on
/// another machine. Before this route the runner had no way to hear about it
/// while a job executed — `start` was its last question — so a canceled deploy
/// ran on to its timeout, and a `concurrency: cancel-in-progress` group had the
/// old run and its replacement going at once (card_a0377b61860e). The runner
/// asks here on a period while the job runs, and once more before it publishes
/// a cache or an artifact.
///
/// A job that is not this runner's answers `404`, exactly as an unknown id
/// does — see [`assigned_job`].
#[utoipa::path(
    get,
    path = "/runners/{id}/jobs/{job_id}/status",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID, which must be assigned to this runner"),
    ),
    responses(
        (status = 200, description = "The job's current status", body = RunnerJobStatusResponse),
        (status = 404, description = "Job not found", body = serde_json::Value),
    ),
)]
pub async fn job_status(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
) -> impl IntoResponse {
    match assigned_job(&state, runner_id, job_id).await {
        Ok(job) => (
            StatusCode::OK,
            Json(RunnerJobStatusResponse {
                job_id,
                active: rg_db::ops::pipeline_ops::is_active_pipeline_work(&job.status),
                status: job.status,
            }),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

/// POST /api/v1/runners/:id/jobs/:job_id/log
/// Upload job log. A runner posting its complete final log may request
/// replacement, making retries idempotent; legacy chunk uploads still append.
#[utoipa::path(
    post,
    path = "/runners/{id}/jobs/{job_id}/log",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID"),
    ),
    request_body(
        content = String,
        description = "Log content (plain text), up to 8 MiB per request",
    ),
    responses(
        (status = 200, description = "Log uploaded", body = serde_json::Value),
        (status = 404, description = "Job not found", body = serde_json::Value),
        (status = 413, description = "Log above the 8 MiB per-request ceiling"),
        (status = 503, description = "Log write queue is full or unavailable; retry the upload"),
    ),
)]
pub async fn upload_log(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let job = match assigned_job(&state, runner_id, job_id).await {
        Ok(job) => job,
        Err(error) => return error.into_response(),
    };

    let body = match sanitized_runner_log(&state, &job, &body).await {
        Ok(body) => body,
        Err(response) => return response,
    };

    let result = if headers
        .get("x-job-log-mode")
        .is_some_and(|value| value == "replace")
    {
        state.log_write_queue.replace(job_id, &body).await
    } else {
        state.log_write_queue.write(job_id, &body).await
    };
    if let Err(error) = result {
        tracing::warn!(job_id, error = %format!("{error:#}"), "failed to persist runner log");
        return log_write_failure_response(error);
    }

    // Broadcast only the server-sanitized log via WebSocket to frontend.
    crate::ws::push_job_log(&state.notification_hub, job_id, &body).await;

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

/// One step's report, as a runner sends it.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepReportRequest {
    /// `running` when the step starts; `success`, `failure`, `skipped` or
    /// `cancelled` when it ends.
    status: String,
    /// What the step counts as for the job; required once it ended.
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    exit_code: Option<i32>,
    /// The step's own log, once it ended. Masked here like the job log.
    #[serde(default)]
    log: Option<String>,
}

const STEP_OUTCOMES: [&str; 4] = ["success", "failure", "skipped", "cancelled"];

/// POST /api/v1/runners/:id/jobs/:job_id/steps/:number
/// Report one step of a job of steps: when it starts, and how it ended.
#[utoipa::path(
    post,
    path = "/runners/{id}/jobs/{job_id}/steps/{number}",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID"),
        ("number" = i32, Path, description = "Step number, from 1"),
    ),
    request_body = StepReportRequest,
    responses(
        (status = 200, description = "Step recorded", body = serde_json::Value),
        (status = 400, description = "Not a step status this route records", body = serde_json::Value),
        (status = 404, description = "Job not found", body = serde_json::Value),
        (status = 409, description = "The job is no longer running, or has no such step", body = serde_json::Value),
        (status = 413, description = "Report above the 8 MiB per-request ceiling"),
    ),
)]
pub async fn report_step(
    State(state): State<AppState>,
    Path((runner_id, job_id, number)): Path<(i64, i64, i32)>,
    Json(report): Json<StepReportRequest>,
) -> impl IntoResponse {
    let job = match assigned_job(&state, runner_id, job_id).await {
        Ok(job) => job,
        Err(error) => return error.into_response(),
    };
    let ended = STEP_OUTCOMES.contains(&report.status.as_str());
    if report.status != "running" && !ended {
        return AppError::bad_request(
            "a step's status is `running`, or one of success, failure, skipped, cancelled",
        )
        .into_response();
    }
    if ended
        && !report
            .conclusion
            .as_deref()
            .is_some_and(|conclusion| STEP_OUTCOMES.contains(&conclusion))
    {
        return AppError::bad_request(
            "a step that ended reports its conclusion: success, failure, skipped or cancelled",
        )
        .into_response();
    }
    let log = match report.log.as_deref() {
        Some(log) if ended => match sanitized_runner_log(&state, &job, log).await {
            Ok(log) => Some(log),
            Err(response) => return response,
        },
        _ => None,
    };
    match rg_db::ops::pipeline_step_ops::report_job_step(
        &state.db,
        job_id,
        &rg_db::ops::pipeline_step_ops::StepReport {
            number,
            status: &report.status,
            conclusion: report.conclusion.as_deref().filter(|_| ended),
            exit_code: report.exit_code.filter(|_| ended),
            log: log.as_deref(),
        },
    )
    .await
    {
        Ok(true) => (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response(),
        Ok(false) => {
            AppError::conflict("the job is no longer running, or has no such step").into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

/// Mask a log a runner sent, with every secret the job was issued and every
/// secret it could read now — the same mask `upload_log` applies.
async fn sanitized_runner_log(
    state: &AppState,
    job: &rg_db::entities::pipeline_job::Model,
    log: &str,
) -> Result<String, Response> {
    let mut secrets = match decrypt_job_mask(state, job) {
        Ok(secrets) => secrets,
        Err(error) => {
            tracing::error!(job_id = job.id, error = %format!("{error:#}"), "failed to load issued secret snapshot while masking runner log");
            return Err(AppError::internal("failed to sanitize job log").into_response());
        }
    };
    match secrets_for_job(state, job.stage_id).await {
        Ok(current) => secrets.extend(current),
        Err(error) => {
            tracing::error!(job_id = job.id, error = %format!("{error:#}"), "failed to load current secrets while masking runner log");
            return Err(AppError::internal("failed to sanitize job log").into_response());
        }
    }
    let log = rg_core::auth::ci_token::mask_job_tokens_in_log(log, &state.jwt_secret, job.id);
    Ok(rg_core::auth::encryption::mask_values(&log, &secrets))
}

fn log_write_failure_response(
    error: rg_core::ci::log_write_queue::LogWriteError,
) -> axum::response::Response {
    match error {
        rg_core::ci::log_write_queue::LogWriteError::Full
        | rg_core::ci::log_write_queue::LogWriteError::Closed => (
            StatusCode::SERVICE_UNAVAILABLE,
            "log write queue unavailable",
        )
            .into_response(),
        rg_core::ci::log_write_queue::LogWriteError::Persistence(error) => {
            AppError::from(error).into_response()
        }
    }
}

#[cfg(test)]
mod log_queue_failure_tests {
    use super::*;

    #[test]
    fn a_full_queue_is_retryable_instead_of_looking_like_a_saved_log() {
        let response =
            log_write_failure_response(rg_core::ci::log_write_queue::LogWriteError::Full);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
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
    let stream = match crate::http_stream::split_git_child(child) {
        Ok(stream) => stream,
        Err(error) => return AppError::internal(format!("{error}")).into_response(),
    };
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/x-tar")],
        crate::http_stream::git_child_body_with_idle(
            stream,
            state.git_idle_timeout_secs,
            crate::http_stream::GitStreamSource {
                job_id: Some(job_id),
                repo_path,
                what: "workspace tar",
            },
        ),
    )
        .into_response()
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
    let (job, repo_id, pipeline_id) = match assigned_job_repo(&state, runner_id, job_id).await {
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
    // This ref's entry, then the default branch's — never another branch's
    // (card_b7a25458b98b).
    let scope = match job_cache_scope(&state, pipeline_id).await {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let mut expired_seen = false;
    let mut live = None;
    for key_hash in scope.restore_hashes(key) {
        // The row is the handle on the archive, not a hint about it: every
        // publication is written under its own name, so a file this lookup
        // cannot reach is residue rather than a cache.
        let entry = match cache_entry_for_download(&state.db, repo_id, &key_hash).await {
            Ok(Some(entry)) => entry,
            Ok(None) => continue,
            Err(error) => return error.into_response(),
        };
        let path = match rg_core::ci_cache::recorded_archive(
            &state.repo_root,
            repo_id,
            &entry.file_path,
        ) {
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
            evict_expired_cache_entry(&state, repo_id, &entry, &path).await;
            expired_seen = true;
            continue;
        }
        live = Some((entry, path));
        break;
    }
    let Some((entry, path)) = live else {
        return if expired_seen {
            AppError::not_found("cache entry expired").into_response()
        } else {
            AppError::not_found("cache entry not found").into_response()
        };
    };
    // Integrity: hash the archive off disk so a mismatch is a `500` decided
    // BEFORE the first byte of body leaves. The previous shape buffered the
    // whole archive into a `Vec` to hash it (card_f357f874d69e) — the ceiling
    // the route declares (1 GiB) was also the per-request heap this process
    // paid, and N parallel restores of one instance were N archives in memory.
    // Legacy entries carry no recorded digest and are served without the
    // check, but they still stream off disk rather than into memory.
    let sha256 = match crate::http_stream::hash_local_file(&path).await {
        Ok(sha) => sha,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return AppError::not_found("cache entry not found").into_response();
        }
        Err(error) => {
            return cache_path_error("CI cache archive", &path, &error).into_response();
        }
    };
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
    if let Err(error) = rg_db::ops::ci_retention_ops::refresh_cache_entry(
        &state.db,
        &entry,
        policy.cache_retention_days,
    )
    .await
    {
        return AppError::from(error).into_response();
    }
    // Second pass: open the file again and hand it to the socket. The archive
    // is on local disk, so a second `open` is cheap; the round trip is what
    // buys the memory bound — a mismatched digest above never reaches this
    // point, so a broken transfer here cannot be read as a valid short one.
    let (file, size) = match crate::http_stream::open_local_file_for_stream(&path).await {
        Ok(pair) => pair,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return AppError::not_found("cache entry not found").into_response();
        }
        Err(error) => {
            return cache_path_error("CI cache archive", &path, &error).into_response();
        }
    };
    let content_length = size.to_string();
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
        // Stream the archive straight off disk, so the memory bound is the
        // hashing window rather than the cache size (card_f357f874d69e). The
        // earlier idle-guard fix (card_16003d99e502) still bites — a slow
        // client trips the idle window and the file handle is released.
        crate::http_stream::file_body_with_idle(file, state.git_idle_timeout_secs),
    )
        .into_response()
}

/// Retire an expired cache row and its archive.
///
/// Deletes the observed expired row first. If a concurrent upload or restore
/// refreshed/replaced it, the conditional delete returns false and its archive
/// must stay exactly where the now-live row expects it.
async fn evict_expired_cache_entry(
    state: &AppState,
    repo_id: i64,
    entry: &rg_db::entities::ci_cache_entry::Model,
    path: &std::path::Path,
) {
    match rg_db::ops::ci_retention_ops::delete_cache_entry_if_expired(&state.db, entry).await {
        Ok(true) => match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            // Already gone: eviction had nothing to do, not a failure.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                repo_id,
                cache_entry_id = entry.id,
                path = %path.display(),
                error = %error,
                "expired CI cache row is gone, but its archive could not be deleted"
            ),
        },
        Ok(false) => {}
        Err(error) => tracing::warn!(
            repo_id,
            cache_entry_id = entry.id,
            error = %format!("{error:#}"),
            "expired CI cache entry kept because its conditional delete failed"
        ),
    }
}

/// One CI cache archive that has been received in full but is not yet the
/// publication any row names.
#[derive(Debug)]
struct StagedCacheArchive {
    path: tempfile::TempPath,
    len: u64,
    sha256: String,
}

/// Stream one cache archive into a request-private spool beside where it will
/// live, digesting it as it goes.
///
/// The route declares a gigabyte, and before this the handler took the body as
/// `Bytes`: the declared ceiling was also the amount of heap one request could
/// hold, so N runners publishing at once cost N gigabytes of this process.
/// Spooling makes the ceiling a disk number rather than a memory number, and the
/// digest is folded into the same pass so the archive is never read twice.
///
/// The spool is created in `directory` rather than under a `.tmp` sibling so the
/// rename that publishes it is a same-directory one, which cannot fail across
/// devices — and until that rename the `TempPath` retires the partial file on
/// every path out of the handler, including the refusals below.
///
/// Over-ceiling bodies answer `413`: `max_bytes` is the handler's own backstop
/// for a chunked upload that never declared a `Content-Length`, and the
/// transport layer's `LengthLimitError` — which surfaces here as a body error
/// rather than as a rejection — is mapped to the same status instead of being
/// reported as a malformed request.
async fn stage_cache_archive(
    body: Body,
    directory: &std::path::Path,
    max_bytes: usize,
) -> Result<StagedCacheArchive, AppError> {
    use futures::StreamExt;
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;

    let staged = rg_core::ci_cache::spool_in(directory)
        .map_err(|error| cache_path_error("CI cache staging file", directory, &error))?;
    let (file, path) = staged.into_parts();
    let mut file = tokio::fs::File::from_std(file);
    let mut stream = body.into_data_stream();
    let mut hasher = Sha256::new();
    let mut len = 0_usize;

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let inner = error.into_inner();
                if crate::body_limit::is_length_limit_error(&*inner) {
                    return Err(AppError::payload_too_large(format!(
                        "cache archive exceeds the configured {max_bytes}-byte request limit"
                    )));
                }
                return Err(AppError::bad_request(format!(
                    "failed to read cache archive body: {inner}"
                )));
            }
        };
        len = len
            .checked_add(chunk.len())
            .filter(|size| *size <= max_bytes)
            .ok_or_else(|| {
                AppError::payload_too_large(format!(
                    "cache archive exceeds the configured {max_bytes}-byte request limit"
                ))
            })?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|error| cache_path_error("CI cache staging file", &path, &error))?;
    }
    file.flush()
        .await
        .map_err(|error| cache_path_error("CI cache staging file", &path, &error))?;
    drop(file);

    Ok(StagedCacheArchive {
        path,
        len: len as u64,
        sha256: hex::encode(hasher.finalize()),
    })
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
        (status = 400, description = "Job has no cache configuration, bad x-cache-key, or empty archive", body = serde_json::Value),
        (status = 404, description = "Job, stage or pipeline not found", body = serde_json::Value),
        (status = 409, description = "The job is no longer active (canceled or settled); nothing was stored", body = serde_json::Value),
        (status = 413, description = "Cache archive exceeds 1 GiB, or a `[limits]` ceiling of the repository has no room for it", body = serde_json::Value),
    ),
)]
pub async fn upload_cache(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    let (job, repo_id, pipeline_id) = match assigned_job_repo(&state, runner_id, job_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    // The next run restores what this one stores, so a canceled run must not
    // be the one that stores it.
    if let Err(error) = refuse_settled_job(runner_id, &job) {
        return error.into_response();
    }
    if job.cache_key.is_none() {
        return AppError::bad_request("job has no cache configuration").into_response();
    }
    let key = match cache_key_header(&headers) {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    // Only ever this ref's entry: a branch must not write the entry the
    // default branch restores (card_b7a25458b98b).
    let key_hash = match job_cache_scope(&state, pipeline_id).await {
        Ok(scope) => scope.save_hash(key),
        Err(error) => return error.into_response(),
    };
    let directory = rg_core::ci_cache::archive_dir(&state.repo_root, repo_id);
    if let Err(error) = tokio::fs::create_dir_all(&directory).await {
        return cache_path_error("CI cache directory", &directory, &error).into_response();
    }
    // Spool the archive to disk as it arrives, digesting it on the way. The
    // request may be a gigabyte; the ceiling bounds what a job may store, and
    // this is what keeps that number off the heap — an over-ceiling body is
    // refused mid-stream rather than after a gigabyte has been collected, and
    // the spool retires itself on every path that does not persist it.
    let staged = match stage_cache_archive(body, &directory, CACHE_ARCHIVE_MAX_BYTES).await {
        Ok(staged) => staged,
        Err(error) => return error.into_response(),
    };
    let StagedCacheArchive {
        path: spool,
        len,
        sha256,
    } = staged;
    if len == 0 {
        return AppError::bad_request("cache archive must contain 1 byte to 1 GiB").into_response();
    }
    let size = len as i64;
    if let Err(error) = rg_core::ci_cache::publish_from_spool(
        &state.db,
        &state.repo_root,
        repo_id,
        &key_hash,
        spool,
        size,
        &sha256,
        &state.storage_limits,
    )
    .await
    {
        return AppError::from(error).into_response();
    }
    StatusCode::NO_CONTENT.into_response()
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

/// [`assigned_job`], for a call that *publishes* something on the job's behalf.
///
/// A cache entry or an artifact outlives the job that made it: the next run
/// restores the cache, and whoever downloads `bundle` gets the artifact. A job
/// that was canceled while it ran must not ship either — a canceled deploy's
/// build is exactly the output nobody asked for (card_a0377b61860e). The
/// runner is told `409`, the same answer `start` gives a job that settled
/// before it began, and a runner older than the job-status route is held to it
/// here whether or not it asks first.
pub(crate) async fn assigned_active_job(
    state: &AppState,
    runner_id: i64,
    job_id: i64,
) -> Result<rg_db::entities::pipeline_job::Model, AppError> {
    let job = assigned_job(state, runner_id, job_id).await?;
    refuse_settled_job(runner_id, &job)?;
    Ok(job)
}

/// The half of [`assigned_active_job`] for a handler that already holds the job.
fn refuse_settled_job(
    runner_id: i64,
    job: &rg_db::entities::pipeline_job::Model,
) -> Result<(), AppError> {
    if rg_db::ops::pipeline_ops::is_active_pipeline_work(&job.status) {
        return Ok(());
    }
    tracing::info!(
        runner_id,
        job_id = job.id,
        status = %job.status,
        "refused to publish output for a job that is no longer active"
    );
    Err(AppError::conflict("job is no longer active"))
}

async fn assigned_job_repo(
    state: &AppState,
    runner_id: i64,
    job_id: i64,
) -> Result<(rg_db::entities::pipeline_job::Model, i64, i64), AppError> {
    let job = assigned_job(state, runner_id, job_id).await?;
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("pipeline stage not found"))?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("pipeline not found"))?;
    Ok((job, pipeline.repo_id, pipeline.id))
}

/// The cache scope of the job's pipeline — see [`rg_core::ci_cache::CacheScope`].
async fn job_cache_scope(
    state: &AppState,
    pipeline_id: i64,
) -> Result<rg_core::ci_cache::CacheScope, AppError> {
    rg_core::ci_cache::scope_of_pipeline(&state.db, pipeline_id)
        .await
        .map_err(AppError::from)
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

/// Hex-encoded SHA-256 of a cache archive's *contents* (distinct from the
/// scoped key hash of [`rg_core::ci_cache::CacheScope`]). Used to record and later
/// verify the integrity of the stored archive.
///
/// Kept for tests that spot-check the digest recorded by
/// [`stage_cache_archive`]. The download path never hashes the whole archive
/// as one buffer — it streams the file through
/// [`crate::http_stream::hash_local_file`] instead (card_f357f874d69e).
#[cfg(test)]
fn cache_content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// The secrets one job is handed: the repository-wide scope, plus its
/// environment's own when it declares one, one value per name.
async fn decrypted_repo_secrets(
    state: &AppState,
    repo_id: i64,
    environment_id: Option<i64>,
) -> anyhow::Result<Vec<(String, String)>> {
    let key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let mut values = Vec::new();
    for secret in
        rg_db::ops::ci_secret_ops::list_for_job(&state.db, repo_id, environment_id).await?
    {
        values.push((
            secret.name,
            rg_core::auth::encryption::decrypt(&secret.encrypted_value, &key)?,
        ));
    }
    Ok(values)
}

fn encrypt_job_mask(state: &AppState, secrets: &[String]) -> anyhow::Result<String> {
    let values = serde_json::to_string(secrets)?;
    let key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    rg_core::auth::encryption::encrypt(&values, &key)
}

fn decrypt_job_mask(
    state: &AppState,
    job: &rg_db::entities::pipeline_job::Model,
) -> anyhow::Result<Vec<String>> {
    let snapshot = job
        .mask_secrets_encrypted
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("job has no issued secret snapshot"))?;
    let key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let values = rg_core::auth::encryption::decrypt(snapshot, &key)?;
    Ok(serde_json::from_str(&values)?)
}

/// Every secret value of the job's repository, in every scope, for masking
/// its log.
///
/// Wider than what the job was handed on purpose: masking a value the job
/// never saw costs nothing, while scoping the mask the way delivery is scoped
/// would let a value through the day those two rules drift apart.
async fn secrets_for_job(state: &AppState, stage_id: i64) -> anyhow::Result<Vec<String>> {
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, stage_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("pipeline stage not found"))?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("pipeline not found"))?;
    let key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    rg_db::ops::ci_secret_ops::list_by_repo(&state.db, pipeline.repo_id)
        .await?
        .into_iter()
        .map(|secret| rg_core::auth::encryption::decrypt(&secret.encrypted_value, &key))
        .collect()
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
    // Ahead of the lookup: a status this server cannot classify must not reach
    // the column at all. Stored verbatim it settled the job while every roll-up
    // read it as unfinished, so the stage and the pipeline stayed `running` for
    // good and the PR's required checks never unblocked — all behind a
    // `200 OK` (card_39bf6a755499).
    let reported = match rg_core::ci::JobStatus::parse_runner_report(&req.status) {
        Ok(status) => status,
        Err(error) => return AppError::from(error).into_response(),
    };

    let job = match assigned_job(&state, runner_id, job_id).await {
        Ok(job) => job,
        Err(error) => return error.into_response(),
    };

    // The mandatory state transition is one transaction: job outcome, runner
    // availability and any stage/pipeline roll-up either all commit or all
    // remain retryable. The operation preserves the conditional-write fencing
    // against a cancellation that settled the job while it was executing.
    let transition = match rg_db::ops::pipeline_ops::finish_runner_job(
        &state.db,
        runner_id,
        job_id,
        job.stage_id,
        reported.as_str(),
        Some(req.exit_code),
        None,
        Some(chrono::Utc::now().naive_utc()),
    )
    .await
    {
        Ok(transition) => transition,
        Err(error) => {
            tracing::error!(
                runner_id,
                job_id,
                error = %format!("{error:#}"),
                "finish_job: atomic graph transition failed"
            );
            return AppError::from(error).into_response();
        }
    };

    if !transition.job_settled {
        tracing::info!(
            runner_id,
            job_id,
            reported = %req.status,
            "finish_job: late completion for a job that already settled — not applied"
        );
        return AppError::conflict(
            "job already settled (canceled or completed) — completion not applied",
        )
        .into_response();
    }

    // The job is over, so whatever it staged and never published is over too.
    // A staged archive is an artifact-sized file that no row names and no
    // retention sweep walks — retention reads rows — so a runner that died
    // between staging and publishing would otherwise leave one behind for good.
    crate::api::artifacts::discard_job_staging(&state, job_id).await;

    if let Some(pipeline) = transition.completed_pipeline {
        if pipeline.status == "success" {
            // The graph is committed before success follow-ups can move refs
            // or spawn new work. Re-running this hook remains guarded by the
            // merge/queue operations it evaluates.
            state
                .evaluate_merges_and_spawn_hooks(pipeline.repo_id, &pipeline.commit_sha, None)
                .await;
        }
        if transition.pipeline_completed_now {
            crate::metrics::recorder::ci_pipeline_finished(&pipeline.status);
            if let Err(hook_error) = rg_core::webhook::service::trigger_ci_status(
                &state.db_pools(),
                pipeline.repo_id,
                pipeline.id,
                &pipeline.status,
            )
            .await
            {
                tracing::warn!(pipeline_id = pipeline.id, error = %hook_error, "failed to trigger CI webhook");
            }
            if pipeline.status == "failed" {
                rg_core::notification::thread::notify_ci_failed(&state.db_pools(), pipeline.id);
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
    if transition.job_completed_now {
        crate::metrics::recorder::ci_job_finished(&req.status, job_duration);
    }

    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

#[derive(Deserialize, ToSchema)]
pub struct FinishJobRequest {
    /// Parsed by [`rg_core::ci::JobStatus::parse_runner_report`] — the domain
    /// lives in that type, not in a comment here.
    status: String,
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
            let mut resp = Vec::with_capacity(runners.len());
            for runner in runners {
                match runner_info_response(&state, runner).await {
                    Ok(response) => resp.push(response),
                    Err(error) => return error.into_response(),
                }
            }
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
/// Also refreshes the heartbeat — on `/heartbeat` always, elsewhere at most
/// once per [`OPPORTUNISTIC_REFRESH_WINDOW`].
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
    /// Not attempted: `last_seen_at` was written less than
    /// [`OPPORTUNISTIC_REFRESH_WINDOW`] ago, and this request is not
    /// `/heartbeat`. Never produced for `/heartbeat`.
    Recent,
}

/// How stale `last_seen_at` must be before a runner request other than
/// `/heartbeat` rewrites it.
///
/// A runner polls every few seconds, and each refresh is a write transaction —
/// on SQLite, a turn on the instance's one write lock — so N idle runners used
/// to cost N writes every poll interval for a fact nothing reads at that
/// resolution (card_c6dd2bed5613). Its readers are coarse: a runner goes
/// offline after 90 s without a refresh (`find_offline_runners`) and a running
/// job is reclaimed after 600 s without one (`find_stuck_jobs`), and the
/// runner's own `/heartbeat` — which always writes — comes every 30 s. Fifteen
/// seconds of staleness moves neither deadline.
const OPPORTUNISTIC_REFRESH_WINDOW: chrono::Duration = chrono::Duration::seconds(15);

/// Whether this request has to rewrite `last_seen_at`: always on
/// `/heartbeat`, whose answer is a statement about that write; otherwise only
/// once the stored value is older than [`OPPORTUNISTIC_REFRESH_WINDOW`]. A
/// value in the future (the clock stepped back) counts as stale, so a skewed
/// row is repaired rather than trusted until the clock catches up.
fn heartbeat_write_due(
    path: &str,
    last_seen_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    if path.trim_end_matches('/').ends_with("/heartbeat") {
        return true;
    }
    let age = now - last_seen_at;
    age < chrono::Duration::zero() || age >= OPPORTUNISTIC_REFRESH_WINDOW
}

pub async fn authenticate_runner(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    // Every refusal below travels as `AppError` — the same `ErrorResponse`
    // envelope every other `/api/v1` handler emits, so a runner client can
    // branch on `error.code` (`BAD_REQUEST`, `UNAUTHORIZED`, `FORBIDDEN`)
    // instead of pattern-matching a free-form string. The `/api/v1`
    // `api_rejection_envelope` layer deliberately leaves `401`/`403` alone
    // (package protocols mounted under the same prefix have their own
    // formats), so this middleware has to produce the envelope itself.
    let runner_id = match extract_runner_id_from_path(request.uri().path()) {
        Some(id) => id,
        None => return AppError::bad_request("missing runner ID in path").into_response(),
    };

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let token = match auth_header {
        Some(h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            return AppError::unauthorized("missing or invalid Authorization header")
                .into_response();
        }
    };

    match rg_db::ops::runner_ops::find_by_token(&state.db, token).await {
        Ok(Some(runner)) if runner.id == runner_id => {
            let Some(repo_id) = runner.repo_id else {
                return AppError::forbidden(
                    "runner is not repository-scoped; re-register it for owner/repo",
                )
                .into_response();
            };
            // Older installations may already hold runners orphaned by a
            // soft-delete. Refuse those credentials even before a future
            // retirement can clean up their rows.
            match rg_db::ops::repo_ops::find_by_id(&state.db, repo_id).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return AppError::unauthorized("runner repository is deleted").into_response()
                }
                Err(error) => return AppError::from(error).into_response(),
            }
            // Valid token — also update heartbeat, unless a recent one already
            // stands (see `OPPORTUNISTIC_REFRESH_WINDOW`). The outcome travels
            // with the request instead of being dropped here: `heartbeat`
            // answers for this write, every other handler is free to ignore it.
            let due = heartbeat_write_due(
                request.uri().path(),
                runner.last_seen_at,
                chrono::Utc::now(),
            );
            let refresh = if !due {
                HeartbeatRefresh::Recent
            } else {
                match rg_db::ops::runner_ops::update_heartbeat(&state.db_write, runner_id).await {
                    Ok(()) => HeartbeatRefresh::Persisted,
                    Err(error) => {
                        tracing::error!(runner_id, error = %format!("{error:#}"), "Failed to update runner heartbeat");
                        // Same predicate the `AppError` conversions use, so a dead
                        // pool — or a heartbeat write the backend refused because
                        // somebody else held the write lock — is a retryable 503 on
                        // `/heartbeat` exactly as it is on every other route,
                        // classified here, where the `DbErr` still exists. A runner
                        // told 500 stops reporting; one told 503 comes back.
                        if error
                            .downcast_ref::<sea_orm::DbErr>()
                            .is_some_and(AppError::is_db_retryable)
                        {
                            HeartbeatRefresh::Unavailable
                        } else {
                            HeartbeatRefresh::Failed
                        }
                    }
                }
            };
            request.extensions_mut().insert(refresh);
            next.run(request).await
        }
        Ok(Some(_)) => AppError::forbidden("token does not match runner ID").into_response(),
        Ok(None) => AppError::unauthorized("invalid runner token").into_response(),
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
    InstanceAdmin(actor_id): InstanceAdmin,
    Path(runner_id): Path<i64>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Read before the delete, because afterwards there is nothing left to read:
    // "runner #4 was removed" does not say which machine stopped being able to
    // take jobs, and the revocation is the half of the pair a review needs
    // most.
    let runner = match rg_db::ops::runner_ops::find_by_id(&state.db, runner_id).await {
        Ok(Some(runner)) => runner,
        Ok(None) => return AppError::not_found("runner not found").into_response(),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "delete_runner_admin lookup failed");
            return AppError::from(e).into_response();
        }
    };
    let actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };

    match rg_db::ops::runner_ops::deregister_runner(&state.db, runner_id).await {
        Ok(true) => {
            record_instance_credential(
                &state,
                &actor,
                "admin.delete_runner",
                InstanceResource {
                    kind: "runner",
                    id: runner.id,
                    name: &runner.name,
                },
                &headers,
                runner_details(&runner),
            )
            .await;
            (
                StatusCode::NO_CONTENT,
                Json(serde_json::json!({"deleted": true})),
            )
                .into_response()
        }
        // The lookup above and this statement are separate, so a concurrent
        // delete can take the row in between. That request owns the revocation
        // and journalled it; this one must not record a second one.
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
mod poll_job_delivery_tests {
    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// The one call of `callee` inside `function`.
    fn only_call(
        source: &str,
        function: &str,
        callee: &str,
    ) -> Result<rust_source::CallSite, String> {
        let calls = rust_source::production_function_call_sites(source, function, &[callee]);
        match calls.as_slice() {
            [call] => Ok(*call),
            _ => Err(format!(
                "expected one production `{callee}` in `{function}`, found {}",
                calls.len()
            )),
        }
    }

    fn claim_is_the_last_await_before_delivery(source: &str) -> Result<(), String> {
        let code = rust_source::production_rust_code_only(source);

        // The irreversible write lives in one helper and is that helper's last
        // await: nothing fallible runs after it before the helper returns.
        let claim = only_call(
            source,
            "claim_dispatched_job",
            "assign_job_with_mask_snapshot",
        )?;
        let helper_end = code[claim.open_paren..]
            .find("\n}\n")
            .map(|end| claim.open_paren + end)
            .ok_or("`claim_dispatched_job` has no end")?;
        let helper_awaits = code[claim.open_paren..helper_end].matches(".await").count();
        if helper_awaits != 1 {
            return Err(format!(
                "expected only the claim's own await in `claim_dispatched_job`, found \
                 {helper_awaits} after runners.rs:{}",
                claim.line
            ));
        }
        for function in ["poll_job", "prepare_dispatch"] {
            if !rust_source::production_function_call_sites(
                source,
                function,
                &["assign_job_with_mask_snapshot"],
            )
            .is_empty()
            {
                return Err(format!(
                    "`{function}` claims the job itself instead of through `claim_dispatched_job`"
                ));
            }
        }

        // `poll_job` prepares the whole body first, claims, and hands the
        // prepared body over with no await in between.
        let prepared = only_call(source, "poll_job", "prepare_dispatch")?;
        let claimed = only_call(source, "poll_job", "claim_dispatched_job")?;
        if prepared.open_paren >= claimed.open_paren {
            return Err("the response is prepared after the irreversible claim".to_string());
        }
        let responses = code.matches("let resp = PollJobResponse {").count();
        if responses != 1 {
            return Err(format!(
                "expected one production PollJobResponse construction, found {responses}"
            ));
        }

        let return_marker = "return Ok((StatusCode::OK, Json(resp)))";
        let Some(relative_return) = code[claimed.open_paren..].find(return_marker) else {
            return Err(format!(
                "the successful response is not returned after the claim at runners.rs:{}",
                claimed.line
            ));
        };
        let delivery_at = claimed.open_paren + relative_return;
        let awaits = code[claimed.open_paren..delivery_at]
            .match_indices(".await")
            .count();
        if awaits != 1 {
            return Err(format!(
                "expected only the claim's own await before delivery, found {awaits} awaits after runners.rs:{}",
                claimed.line
            ));
        }
        Ok(())
    }

    /// A long-poll future is canceled when its timeout expires and when the
    /// client disconnects. Once `assign_job_with_mask_snapshot` commits, no fallible async work may
    /// remain between that point of no return and handing the prepared body to
    /// Axum, or the row can be stranded without any runner learning its id.
    #[test]
    fn poll_job_claim_is_the_last_await_before_delivery() {
        claim_is_the_last_await_before_delivery(include_str!("runners.rs"))
            .unwrap_or_else(|error| panic!("{error}"));
    }

    /// The shape the guard holds, written small, so each mutation below is
    /// one edit away from it.
    const FIXED: &str = r#"
pub async fn poll_job() {
    let (resp, mask) = prepare_dispatch().await?;
    if claim_dispatched_job(mask).await? {
        return Ok((StatusCode::OK, Json(resp)));
    }
}

async fn prepare_dispatch() {
    let stage = get_stage_by_id().await;
    let resp = PollJobResponse {};
}

async fn claim_dispatched_job() {
    match assign_job_with_mask_snapshot().await {}
}
"#;

    #[test]
    fn guard_accepts_the_fixed_shape() {
        claim_is_the_last_await_before_delivery(FIXED).unwrap();
    }

    /// Mutation proofs: each one breaks a different half of the contract
    /// while every call and the successful return are still present.
    #[test]
    fn guard_rejects_a_claim_moved_back_above_preparation() {
        let mutated = FIXED.replace(
            "let (resp, mask) = prepare_dispatch().await?;\n    if claim_dispatched_job(mask).await? {",
            "if claim_dispatched_job(mask).await? {\n    let (resp, mask) = prepare_dispatch().await?;",
        );
        assert_ne!(mutated, FIXED, "the mutation did not apply");
        assert!(
            claim_is_the_last_await_before_delivery(&mutated).is_err(),
            "claiming before the response is prepared must fail the guard"
        );
    }

    #[test]
    fn guard_rejects_async_work_after_the_claim_inside_the_helper() {
        let mutated = FIXED.replace(
            "match assign_job_with_mask_snapshot().await {}",
            "match assign_job_with_mask_snapshot().await {}\n    record_claim().await;",
        );
        assert_ne!(mutated, FIXED, "the mutation did not apply");
        assert!(
            claim_is_the_last_await_before_delivery(&mutated).is_err(),
            "an await after the claim inside `claim_dispatched_job` must fail the guard"
        );
    }

    #[test]
    fn guard_rejects_a_claim_made_in_poll_job_itself() {
        let mutated = FIXED.replace(
            "let (resp, mask) = prepare_dispatch().await?;",
            "match assign_job_with_mask_snapshot().await {}\n    let (resp, mask) = prepare_dispatch().await?;",
        );
        assert_ne!(mutated, FIXED, "the mutation did not apply");
        assert!(
            claim_is_the_last_await_before_delivery(&mutated).is_err(),
            "a second, direct claim in `poll_job` must fail the guard"
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
mod cache_upload_staging_tests {
    use super::*;
    use axum::body::Bytes;
    use std::convert::Infallible;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// The defect this route carried: a declared gigabyte that was also a
    /// gigabyte of heap per concurrent runner. Asserted over the source because
    /// no behavioural test can tell a spooled gigabyte from a buffered one
    /// without actually sending one — and the way back is a one-line change of
    /// the handler's parameter type.
    #[test]
    fn production_upload_path_keeps_the_archive_out_of_one_heap_buffer() {
        let source = include_str!("runners.rs");
        let production = rust_source::production_rust_code_with_doc_comments(source);
        let handler = production
            .split_once("pub async fn upload_cache(")
            .expect("cache upload handler")
            .1
            .split_once("/// The job named by `{job_id}`")
            .expect("handler end marker")
            .0;

        assert!(
            handler.contains("body: Body,"),
            "the cache upload must take a streaming body, not a buffering extractor"
        );
        assert!(
            handler.contains("stage_cache_archive("),
            "the cache upload must spool its body instead of collecting it"
        );
        assert!(
            !handler.contains("body.len()") && !handler.contains("body.as_ref()"),
            "reading the whole body's length or bytes means it is buffered again"
        );
    }

    #[tokio::test]
    async fn request_chunks_are_spooled_and_digested_in_one_pass() {
        let directory = tempfile::tempdir().unwrap();
        let body = Body::from_stream(futures::stream::iter([
            Ok::<_, Infallible>(Bytes::from_static(b"first-")),
            Ok::<_, Infallible>(Bytes::from_static(b"second")),
        ]));

        let staged = stage_cache_archive(body, directory.path(), 12)
            .await
            .unwrap();

        assert_eq!(staged.len, 12);
        // The digest the row records must be the digest of the bytes on disk —
        // the download path refuses the archive when the two disagree.
        assert_eq!(staged.sha256, cache_content_hash(b"first-second"));
        let path = staged.path.to_path_buf();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"first-second");
        drop(staged);
        assert!(!path.exists(), "TempPath must retire the cache spool");
    }

    #[tokio::test]
    async fn chunked_transport_overflow_is_413_and_leaves_no_spool() {
        let directory = tempfile::tempdir().unwrap();
        let chunks = futures::stream::iter([
            Ok::<_, Infallible>(Bytes::from_static(b"123")),
            Ok::<_, Infallible>(Bytes::from_static(b"45")),
        ]);
        let limited = Body::new(http_body_util::Limited::new(Body::from_stream(chunks), 4));

        let error = stage_cache_archive(limited, directory.path(), 10)
            .await
            .unwrap_err();

        assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            error
                .to_string()
                .contains("configured 10-byte request limit"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_dir(directory.path()).unwrap().count(),
            0,
            "a refused chunked upload left a spool behind"
        );
    }

    /// The handler's own backstop, for a chunked body the transport layer never
    /// got a `Content-Length` to refuse in advance.
    #[tokio::test]
    async fn a_body_over_the_handler_ceiling_is_413_and_leaves_no_spool() {
        let directory = tempfile::tempdir().unwrap();
        let body = Body::from_stream(futures::stream::iter([
            Ok::<_, Infallible>(Bytes::from_static(b"1234")),
            Ok::<_, Infallible>(Bytes::from_static(b"5678")),
        ]));

        let error = stage_cache_archive(body, directory.path(), 6)
            .await
            .unwrap_err();

        assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            std::fs::read_dir(directory.path()).unwrap().count(),
            0,
            "a refused oversized upload left a spool behind"
        );
    }

    /// Peak resident set size of this process so far, in bytes.
    ///
    /// The *peak*, not the current one: a body that was collected and then
    /// dropped is back off the books by the time the call returns — a large
    /// allocation goes back to the kernel on free — so a reading taken
    /// afterwards cannot tell a spool from a buffer. `VmHWM` is the high-water
    /// mark, which is exactly the number the defect moves.
    fn peak_resident_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status
            .lines()
            .find(|line| line.starts_with("VmHWM:"))?
            .strip_prefix("VmHWM:")?;
        let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
        Some(kib * 1024)
    }

    /// The measurement the fix exists for: a cache-sized upload must not become
    /// a cache-sized allocation.
    ///
    /// Asserted rather than assumed, because every other test here would pass
    /// just as well against the buffering handler — a `Bytes` body is correct,
    /// it is only expensive, and the expense is invisible to any assertion about
    /// status codes or bytes on disk. The stream hands over a quarter of a
    /// gigabyte in 1 MiB frames that all share one allocation (cloning `Bytes`
    /// is a refcount), so what the process grows by is what the *handler* kept.
    #[cfg_attr(not(target_os = "linux"), ignore = "reads /proc/self/status")]
    #[tokio::test]
    async fn a_large_upload_does_not_grow_the_process_by_its_own_size() {
        const FRAME: usize = 1024 * 1024;
        const FRAMES: usize = 256;

        let directory = tempfile::tempdir().unwrap();
        let frame = Bytes::from(vec![b'c'; FRAME]);
        let body = Body::from_stream(futures::stream::iter(
            std::iter::repeat_n(frame, FRAMES).map(Ok::<_, Infallible>),
        ));

        let before = peak_resident_bytes().expect("no /proc/self/status to measure against");
        let staged = stage_cache_archive(body, directory.path(), FRAME * FRAMES)
            .await
            .unwrap();
        let after = peak_resident_bytes().expect("no /proc/self/status to measure against");

        assert_eq!(staged.len as usize, FRAME * FRAMES);
        let grew = after.saturating_sub(before);
        let ceiling = (FRAME * FRAMES / 4) as u64;
        assert!(
            grew < ceiling,
            "a {} MiB upload grew the process by {} MiB — the body is being collected, not spooled",
            FRAME * FRAMES / (1024 * 1024),
            grew / (1024 * 1024)
        );
    }

    /// The declared ceiling and the one the router layers have to be one
    /// number: the handler's backstop is meaningless if it sits above the
    /// transport limit, and misleading if it sits below.
    #[test]
    fn the_declared_ceiling_is_the_one_the_route_table_layers() {
        let routes = include_str!("../routes.rs");
        assert!(
            rust_source::production_rust_code_only(routes).contains(
                "Wrap::runner_auth_with_body_limit(state, api::runners::CACHE_ARCHIVE_MAX_BYTES)"
            ),
            "the cache route must layer the ceiling this module declares, not a literal beside it"
        );
        assert_eq!(CACHE_ARCHIVE_MAX_BYTES, 1024 * 1024 * 1024);
    }
}
