use super::repo_access::{require_admin, require_authenticated_read, require_read};
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use sea_orm::{NotSet, Set};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct EnvironmentRequest {
    pub name: String,
    #[serde(default)]
    pub protected: bool,
    #[serde(default = "default_required_approvals")]
    pub required_approvals: i32,
    #[serde(default)]
    pub allowed_approver_ids: Vec<i64>,
}
fn default_required_approvals() -> i32 {
    1
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnvironmentResponse {
    pub id: i64,
    pub name: String,
    pub protected: bool,
    pub required_approvals: i32,
    pub allowed_approver_ids: Vec<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
fn response(model: rg_db::entities::ci_environment::Model) -> EnvironmentResponse {
    EnvironmentResponse {
        id: model.id,
        name: model.name,
        protected: model.protected,
        required_approvals: model.required_approvals,
        allowed_approver_ids: model
            .allowed_approver_ids
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default(),
        created_at: model.created_at,
        updated_at: model.updated_at,
    }
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && !name.chars().any(char::is_control)
}
async fn validate_request(state: &AppState, body: &EnvironmentRequest) -> Result<(), AppError> {
    if !valid_name(body.name.trim()) {
        return Err(AppError::bad_request(
            "environment name must contain 1-255 printable characters",
        ));
    }
    if !(1..=10).contains(&body.required_approvals) {
        return Err(AppError::bad_request(
            "required_approvals must be between 1 and 10",
        ));
    }
    let mut unique = body.allowed_approver_ids.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != body.allowed_approver_ids.len() {
        return Err(AppError::bad_request(
            "allowed_approver_ids must not contain duplicates",
        ));
    }
    if body.protected && !unique.is_empty() && body.required_approvals as usize > unique.len() {
        return Err(AppError::bad_request(
            "required approvals exceed the approver list",
        ));
    }
    for user_id in unique {
        if rg_db::ops::user_ops::find_by_id(&state.db, user_id)
            .await
            .map_err(AppError::internal)?
            .is_none()
        {
            return Err(AppError::bad_request(format!(
                "approver user {user_id} does not exist"
            )));
        }
    }
    Ok(())
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/actions/environments", tag = "CI/CD", responses((status = 200, body = [EnvironmentResponse])))]
pub async fn list(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let repo = match require_read(&state, &headers, &owner, &name).await {
        Ok(repo) => repo,
        Err(error) => return error.into_response(),
    };
    match rg_db::ops::ci_environment_ops::list(&state.db, repo.id).await {
        Ok(items) => Json(items.into_iter().map(response).collect::<Vec<_>>()).into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/actions/environments", tag = "CI/CD", request_body = EnvironmentRequest, responses((status = 201, body = EnvironmentResponse)))]
pub async fn create(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<EnvironmentRequest>,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = validate_request(&state, &body).await {
        return error.into_response();
    }
    let now = chrono::Utc::now();
    let model = rg_db::entities::ci_environment::ActiveModel {
        id: NotSet,
        repo_id: Set(repo.id),
        name: Set(body.name.trim().to_string()),
        protected: Set(body.protected),
        required_approvals: Set(body.required_approvals),
        allowed_approver_ids: Set(Some(
            serde_json::to_string(&body.allowed_approver_ids).unwrap_or_default(),
        )),
        created_at: Set(now),
        updated_at: Set(now),
    };
    match rg_db::ops::ci_environment_ops::create(&state.db, model).await {
        Ok(model) => (StatusCode::CREATED, Json(response(model))).into_response(),
        Err(error) if error.to_string().to_ascii_lowercase().contains("unique") => {
            AppError::conflict("environment already exists").into_response()
        }
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[utoipa::path(put, path = "/repos/{owner}/{name}/actions/environments/{id}", tag = "CI/CD", request_body = EnvironmentRequest, responses((status = 200, body = EnvironmentResponse)))]
pub async fn update(
    State(state): State<AppState>,
    Path((owner, name, id)): Path<(String, String, i64)>,
    headers: HeaderMap,
    Json(body): Json<EnvironmentRequest>,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = validate_request(&state, &body).await {
        return error.into_response();
    }
    let model = match rg_db::ops::ci_environment_ops::find_by_id(&state.db, id).await {
        Ok(Some(model)) if model.repo_id == repo.id => model,
        Ok(_) => return AppError::not_found("environment not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let mut active: rg_db::entities::ci_environment::ActiveModel = model.into();
    active.name = Set(body.name.trim().to_string());
    active.protected = Set(body.protected);
    active.required_approvals = Set(body.required_approvals);
    active.allowed_approver_ids = Set(Some(
        serde_json::to_string(&body.allowed_approver_ids).unwrap_or_default(),
    ));
    active.updated_at = Set(chrono::Utc::now());
    match rg_db::ops::ci_environment_ops::update(&state.db, active).await {
        Ok(model) => Json(response(model)).into_response(),
        Err(error) if error.to_string().to_ascii_lowercase().contains("unique") => {
            AppError::conflict("environment already exists").into_response()
        }
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/actions/environments/{id}", tag = "CI/CD", responses((status = 204)))]
pub async fn delete(
    State(state): State<AppState>,
    Path((owner, name, id)): Path<(String, String, i64)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (repo, _) = match require_admin(&state, &headers, &owner, &name).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match rg_db::ops::ci_environment_ops::find_by_id(&state.db, id).await {
        Ok(Some(model)) if model.repo_id == repo.id => {}
        Ok(_) => return AppError::not_found("environment not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    }
    match rg_db::ops::ci_environment_ops::has_jobs(&state.db, id).await {
        Ok(true) => {
            return AppError::conflict("environment is referenced by pipeline history")
                .into_response()
        }
        Ok(false) => {}
        Err(error) => return AppError::internal(error).into_response(),
    }
    match rg_db::ops::ci_environment_ops::delete(&state.db, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApprovalResponse {
    pub job_id: i64,
    pub approvals: u64,
    pub required_approvals: i32,
    pub released: bool,
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/pipelines/{pipeline_id}/jobs/{job_id}/approve", tag = "CI/CD", responses((status = 200, body = ApprovalResponse)))]
pub async fn approve(
    State(state): State<AppState>,
    Path((owner, name, pipeline_id, job_id)): Path<(String, String, i64, i64)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (repo, actor_id) = match require_authenticated_read(&state, &headers, &owner, &name).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, pipeline_id).await {
        Ok(Some(pipeline)) if pipeline.repo_id == repo.id => pipeline,
        Ok(_) => return AppError::not_found("pipeline not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(job)) => job,
        Ok(None) => return AppError::not_found("job not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    let stage = match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await {
        Ok(Some(stage)) if stage.pipeline_id == pipeline_id => stage,
        Ok(_) => return AppError::not_found("job not found").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    };
    if job.status != "waiting_approval" || pipeline.status != "waiting_approval" {
        return AppError::bad_request("job is not awaiting environment approval").into_response();
    }
    let environment_id = match job.environment_id {
        Some(id) => id,
        None => return AppError::bad_request("job has no protected environment").into_response(),
    };
    let environment = match rg_db::ops::ci_environment_ops::find_by_id(&state.db, environment_id)
        .await
    {
        Ok(Some(environment)) if environment.repo_id == repo.id && environment.protected => {
            environment
        }
        Ok(_) => {
            return AppError::bad_request("protected environment no longer exists").into_response()
        }
        Err(error) => return AppError::internal(error).into_response(),
    };
    let allowed: Vec<i64> = environment
        .allowed_approver_ids
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    let is_admin =
        match rg_core::repo::service::can_admin_repo(&state.db, &repo, Some(actor_id)).await {
            Ok(value) => value,
            Err(error) => return AppError::internal(error).into_response(),
        };
    if !is_admin && !allowed.contains(&actor_id) {
        return AppError::forbidden("user is not an allowed environment approver").into_response();
    }
    match rg_db::ops::ci_environment_ops::add_approval(&state.db, job_id, environment.id, actor_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return AppError::conflict("user already approved this job").into_response(),
        Err(error) => return AppError::internal(error).into_response(),
    }
    let approvals = match rg_db::ops::ci_environment_ops::count_approvals(&state.db, job_id).await {
        Ok(count) => count,
        Err(error) => return AppError::internal(error).into_response(),
    };
    let mut released = false;
    if approvals >= environment.required_approvals as u64 {
        released =
            match rg_db::ops::ci_environment_ops::release_approved_job(&state.db, job_id).await {
                Ok(value) => value,
                Err(error) => return AppError::internal(error).into_response(),
            };
        let stage_ready = match rg_db::ops::pipeline_ops::stage_has_job_status(
            &state.db,
            stage.id,
            "waiting_approval",
        )
        .await
        {
            Ok(has_waiting) => !has_waiting,
            Err(error) => return AppError::internal(error).into_response(),
        };
        if released && stage_ready {
            if let Err(error) =
                rg_db::ops::pipeline_ops::resume_approval_chain(&state.db, pipeline_id, stage.id)
                    .await
            {
                return AppError::internal(error).into_response();
            }
            let storage_owner = if repo.org_id.is_some() {
                owner.clone()
            } else {
                match rg_db::ops::user_ops::find_by_id(&state.db, repo.owner_id).await {
                    Ok(Some(user)) => user.username,
                    Ok(None) => {
                        return AppError::internal("repository owner not found").into_response()
                    }
                    Err(error) => return AppError::internal(error).into_response(),
                }
            };
            let repo_path = state
                .repo_root
                .join(format!("{storage_owner}/{}.git", repo.name));
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
        }
    }
    Json(ApprovalResponse {
        job_id,
        approvals,
        required_approvals: environment.required_approvals,
        released,
    })
    .into_response()
}
