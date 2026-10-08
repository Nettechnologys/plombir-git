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

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TriggerPipelineRequest {
    /// The ref to build: `refs/heads/…` / `refs/tags/…` verbatim, a bare name
    /// read as a branch. Absent or empty → the repository's default branch.
    ///
    /// The wire name is `ref`, which is what every client and every other
    /// trigger path in this codebase calls it. It used to be `ref_name` on the
    /// struct with no rename, so `serde` filled it with `None` for every request
    /// the web client sent and the handler silently built something else
    /// (`card_64804da48693`). `ref_name` stays accepted as an alias so a caller
    /// written against the old struct name is not broken by the fix.
    #[serde(rename = "ref", alias = "ref_name")]
    ref_name: Option<String>,

    /// Values for the selected `workflow_dispatch` schemas. The workflow owns
    /// type/default/required validation; the HTTP boundary keeps the REST/CLI
    /// convention in which dispatch input values are strings.
    #[serde(default)]
    inputs: std::collections::HashMap<String, String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WorkflowDispatchSchemaParams {
    /// The ref whose committed workflow forms should be shown. Absent or empty
    /// uses the repository default branch, exactly like the trigger endpoint.
    #[serde(rename = "ref", alias = "ref_name")]
    ref_name: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkflowDispatchSchemaResponse {
    pub ref_name: String,
    pub commit_sha: String,
    pub inputs: Vec<WorkflowDispatchInputResponse>,
    pub workflows: Vec<WorkflowDispatchWorkflowResponse>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkflowDispatchWorkflowResponse {
    pub path: String,
    pub name: String,
    pub inputs: Vec<WorkflowDispatchInputResponse>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkflowDispatchInputResponse {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
    #[serde(rename = "type")]
    pub input_type: String,
    pub default: Option<String>,
    pub options: Vec<String>,
}

impl From<rg_core::ci::WorkflowDispatchInput> for WorkflowDispatchInputResponse {
    fn from(input: rg_core::ci::WorkflowDispatchInput) -> Self {
        Self {
            name: input.name,
            description: input.description,
            required: input.required,
            input_type: input.input_type,
            default: input.default,
            options: input.options,
        }
    }
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

/// GET /api/v1/repos/:owner/:name/pipelines/workflow-dispatch
/// Return the manual-run input forms committed on the selected ref.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pipelines/workflow-dispatch",
    tag = "CI/CD",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        WorkflowDispatchSchemaParams,
    ),
    responses(
        (status = 200, description = "Validated workflow_dispatch forms", body = WorkflowDispatchSchemaResponse),
        (status = 400, description = "Unknown ref or invalid committed workflow", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 500, description = "Repository storage or CI engine failure", body = serde_json::Value),
    ),
)]
pub async fn get_workflow_dispatch_schema(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    RepoRead { repo }: RepoRead,
    Query(params): Query<WorkflowDispatchSchemaParams>,
) -> impl IntoResponse {
    let owner_display = match resolve_repo_storage_owner(&state, &repo, &owner).await {
        Ok(owner) => owner,
        Err(error) => return error.into_response(),
    };
    let repo_path = state.repo_root.join(format!("{owner_display}/{name}.git"));
    if let Err(error) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(error).into_response();
    }

    let ref_name = canonical_pipeline_ref(params.ref_name.as_deref(), &repo.default_branch);
    let commit_sha = match resolve_commit_sha(&repo_path, &ref_name) {
        Ok(Some(sha)) => sha,
        Ok(None) => {
            return AppError::bad_request(format!("cannot resolve commit SHA for ref {ref_name}"))
                .into_response();
        }
        Err(error) => return AppError::from(error).into_response(),
    };
    // Every workflow at the commit is read and parsed — up to the workflow
    // byte ceiling, chosen by whoever pushed it — so the schema is built on a
    // blocking thread, not on the worker serving this request
    // (card_57ccfa9fdde3).
    let engine = state.ci_engine.clone();
    let schema_commit = commit_sha.clone();
    let schema = match tokio::task::spawn_blocking(move || {
        engine.workflow_dispatch_schema(rg_core::ci::WorkflowDispatchSchemaQuery {
            repo_path: &repo_path,
            commit_sha: &schema_commit,
        })
    })
    .await
    .context("building the manual-run form did not complete")
    {
        Ok(Ok(workflows)) => workflows,
        Ok(Err(error)) | Err(error) => return AppError::from(error).into_response(),
    };
    let workflows = schema
        .workflows
        .into_iter()
        .map(|workflow| WorkflowDispatchWorkflowResponse {
            path: workflow.path,
            name: workflow.name,
            inputs: workflow
                .inputs
                .into_iter()
                .map(WorkflowDispatchInputResponse::from)
                .collect(),
        })
        .collect();
    let inputs = schema
        .inputs
        .into_iter()
        .map(WorkflowDispatchInputResponse::from)
        .collect();

    Json(WorkflowDispatchSchemaResponse {
        ref_name,
        commit_sha,
        inputs,
        workflows,
    })
    .into_response()
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
    // Resolve the only fallible spawn prerequisite before publishing
    // `pending`. The production CI engine's post-commit resume is an
    // infallible task spawn; external-runner mode does not require a local bare
    // repository at all, so existence is deliberately not asserted here.
    let owner_display = match resolve_repo_storage_owner(&state, &repo, &owner).await {
        Ok(owner) => owner,
        Err(error) => return error.into_response(),
    };
    let repo_path = state
        .repo_root
        .join(format!("{}/{}.git", owner_display, name));
    let released = match rg_db::ops::pipeline_ops::play_manual_job_and_resume_pipeline_chain(
        &state.db,
        pipeline_id,
        job.stage_id,
        job.id,
    )
    .await
    {
        Ok(released) => released,
        Err(error) => return AppError::from(error).into_response(),
    };
    if !released {
        // Losing this race is the textbook `Conflict`: a double click, or a
        // retry after a timeout whose first attempt actually landed.
        return AppError::conflict("manual job was already released").into_response();
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
    request_body = TriggerPipelineRequest,
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "The selected ref disappeared while the pipeline was being created", body = serde_json::Value),
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

    // The ref to build. A literal `refs/heads/main` used to stand in for
    // "nothing was asked for", so a repository whose default branch is
    // `develop` answered `400` about a branch its caller never named — while
    // the branch it should have used was sitting in the row the gate had
    // already read.
    let ref_name = canonical_pipeline_ref(body.ref_name.as_deref(), &repo.default_branch);
    let commit_sha = match resolve_commit_sha(&repo_path, &ref_name) {
        Ok(Some(sha)) => sha,
        Ok(None) => {
            // Naming the ref is the whole point: the caller has to be able to
            // tell "the branch I asked for is gone" from "the default branch
            // this repository records does not exist".
            return AppError::bad_request(format!("cannot resolve commit SHA for ref {ref_name}"))
                .into_response();
        }
        Err(e) => return AppError::from(e).into_response(),
    };

    // Check if CI config exists. The wording lives in `rg_core::ci` next to the
    // gate itself: this used to say `no .plombir-git-ci.yml found`, which lied to
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
            // The event a workflow can actually declare. See
            // `rg_core::ci::WORKFLOW_DISPATCH_EVENT`.
            trigger_type: rg_core::ci::WORKFLOW_DISPATCH_EVENT,
            base_branch: None,
            // A manual run has no previous revision of its own; a `paths:`
            // filter falls back to the diff of the commit it is asked to build.
            previous_sha: None,
            inputs: Some(&body.inputs),
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
        Ok(pipeline_id) => {
            if let Err(error) =
                reconcile_published_pipeline_ref(&state.db, &repo_path, &ref_name, pipeline_id)
                    .await
            {
                return error.into_response();
            }

            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": pipeline_id,
                    "status": "pending",
                    "commit_sha": commit_sha,
                    "ref_name": ref_name,
                })),
            )
                .into_response()
        }
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
        (status = 409, description = "The pipeline ref no longer exists", body = serde_json::Value),
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

    match resolve_commit_sha(&repo_path, &pipeline.ref_name) {
        // Retry deliberately keeps the original commit even when a live branch
        // has advanced. Existence is the lifecycle question here; equality
        // would silently change reruns into builds of the branch's new head.
        Ok(Some(_)) => {}
        Ok(None) => return missing_pipeline_ref(&pipeline.ref_name).into_response(),
        Err(error) => return AppError::from(error).into_response(),
    }

    // The same run again means the same inputs again. They are read back from
    // the pipeline row rather than reconstructed from the jobs it produced: a
    // job carries normalized `INPUT_*` environment names, and a reusable child
    // job may have overwritten any of them with its own `workflow_call` input
    // of the same name (card_24f475c09a17).
    let dispatch_inputs = match stored_dispatch_inputs(&pipeline) {
        Ok(inputs) => inputs,
        Err(error) => return error.into_response(),
    };

    match state
        .ci_engine
        .trigger_pipeline(rg_core::ci::TriggerPipelineParams {
            db: &state.db,
            repo_path: &repo_path,
            repo_id: pipeline.repo_id,
            commit_sha: &pipeline.commit_sha,
            ref_name: &pipeline.ref_name,
            // A retry runs the pipeline again, so it runs under the event that
            // produced it — `"retry"` was a name no `on:` clause can carry, and
            // it reached the matcher, which answered "nothing is triggered by
            // this" for every repository on `.gitea/workflows/`. The column
            // therefore no longer distinguishes a rerun from the original; if
            // that distinction is wanted it needs a field of its own rather
            // than the event name (card_e87a1b6f9633).
            trigger_type: &pipeline.trigger_type,
            // The rest of the run's provenance, read back off the same row for
            // the same reason (card_74d58ec3ac1e). Both used to be `None` here,
            // which is not "this run had none" but "ask the repository as it
            // stands today": the matcher then filters a retried pull request
            // against the *default* branch — so a PR into `develop` selects no
            // workflow and the retry falls through to `.plombir-git-ci.yml`, a
            // different graph under the same `201` — and a `paths:` filter
            // diffs against the commit's first parent instead of the range the
            // push actually covered. A row written before the columns existed
            // reads `None` and keeps exactly that older behaviour, rather than
            // being replayed against a branch nobody recorded.
            previous_sha: pipeline.previous_sha.as_deref(),
            base_branch: pipeline.base_branch.as_deref(),
            inputs: dispatch_inputs.as_ref(),
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
        Ok(new_id) => {
            if let Err(error) =
                reconcile_published_pipeline_ref(&state.db, &repo_path, &pipeline.ref_name, new_id)
                    .await
            {
                return error.into_response();
            }

            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": new_id,
                    "status": "pending",
                    "original_pipeline_id": id,
                })),
            )
                .into_response()
        }
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

fn missing_pipeline_ref(ref_name: &str) -> AppError {
    AppError::conflict(format!("pipeline ref no longer exists: {ref_name}"))
}

/// The `workflow_dispatch` inputs a retry has to replay, read off the pipeline
/// row that recorded them.
///
/// `None` means the run carried none — every automatic producer, and every
/// manual run started with an empty map. A row written before the column
/// existed reads the same way: the values were never recorded anywhere, so the
/// retry re-resolves the workflow's own declarations rather than inventing
/// values, and a `required:` input the original caller supplied is refused by
/// name instead of being silently replaced.
///
/// Unreadable JSON is *ours* — this crate wrote it — and it is a 500 rather
/// than a retry that quietly runs with different inputs than the run it claims
/// to repeat.
fn stored_dispatch_inputs(
    pipeline: &rg_db::entities::pipeline::Model,
) -> Result<Option<std::collections::HashMap<String, String>>, AppError> {
    pipeline
        .dispatch_inputs
        .as_deref()
        .map(|stored| {
            serde_json::from_str(stored).map_err(|error| {
                AppError::internal(format!(
                    "pipeline {} recorded workflow_dispatch inputs that cannot be read back: \
                     {error}",
                    pipeline.id
                ))
            })
        })
        .transpose()
}

/// Close the producer side of deleted-ref cancellation for HTTP-created runs.
///
/// The deletion hook cancels every graph visible during its query. Manual and
/// retry requests can already have resolved a ref but still be awaiting config
/// discovery and graph insertion at that point. Once this exact graph exists,
/// re-read the ref; if deletion won, cancel only the graph this request made so
/// a later ref recreation or independent live run cannot be collateral damage.
async fn reconcile_published_pipeline_ref(
    db: &rg_db::DatabaseConnection,
    repo_path: &std::path::Path,
    ref_name: &str,
    pipeline_id: i64,
) -> Result<(), AppError> {
    match resolve_commit_sha(repo_path, ref_name) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => {
            rg_db::ops::pipeline_ops::cancel_pipeline_chain(db, pipeline_id)
                .await
                .map_err(AppError::from)?;
            Err(missing_pipeline_ref(ref_name))
        }
        // An unreadable repository is not proof that its ref disappeared. The
        // resolver keeps the storage failure diagnostic and we avoid a false,
        // destructive cancellation, matching the post-push producer contract.
        Err(error) => Err(AppError::from(error)),
    }
}

fn resolve_commit_sha(
    repo_path: &std::path::Path,
    ref_name: &str,
) -> anyhow::Result<Option<String>> {
    // Same class as the CI gate in `rg_core::ci::has_ci_config`: `.ok()?` turned
    // "the server cannot open this repository" into "your ref is wrong" (the
    // caller answers 400 `cannot resolve commit SHA for ref`), with nothing in
    // the log to tell the two apart.
    let repo = match rg_git::repository::open(repo_path) {
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

    // A ref Git's grammar rejects — `main^`, `@{-1}`, `refs/heads/-x`, `a..b` —
    // is a deterministically wrong request, and every caller here takes the ref
    // straight from a client: the dispatch-schema query, the manual trigger
    // body, and the ref a retry/reconcile replays. gix refuses the same
    // spellings, but only as an anonymous validation error that `AppError::from`
    // reports as a 500, telling the caller to retry what can never work.
    rg_git::refname::validate_refname(&ref_name_normalized)
        .map_err(|_| rg_core::error::invalid_request("invalid ref name"))?;

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

/// Canonical ref spelling shared by the form probe and the trigger itself.
///
/// If these endpoints resolve a bare/default ref differently, the form can be
/// valid for one commit while the POST immediately executes another.
fn canonical_pipeline_ref(requested: Option<&str>, default_branch: &str) -> String {
    let requested = requested.map(str::trim).unwrap_or_default();
    let requested = if requested.is_empty() {
        default_branch
    } else {
        requested
    };
    if requested.starts_with("refs/") {
        requested.to_owned()
    } else {
        format!("refs/heads/{requested}")
    }
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
