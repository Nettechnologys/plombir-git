//! Import REST API — repository migration endpoints.
//!
//! POST   /api/v1/imports       — start a new import
//! GET    /api/v1/imports/{id}   — check import status
//! GET    /api/v1/imports        — list user's imports
//! DELETE /api/v1/imports/{id}   — cancel/delete an import

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::auth::extract_bearer_claims;
use crate::error::AppError;
use crate::AppState;

/// Request body for starting a new import.
#[derive(Deserialize, ToSchema)]
pub struct StartImportRequest {
    /// Source platform: "github", "gitlab", "gitea", or "git"
    pub platform: String,
    /// Source repository URL (e.g., https://github.com/user/repo)
    pub source_url: String,
    /// Target owner in IronForge
    pub target_owner: String,
    /// Target repository name (defaults to source repo name)
    #[serde(default)]
    pub target_name: Option<String>,
    /// API access token for the source platform
    pub auth_token: Option<String>,
    /// Whether to import the repository itself
    #[serde(default = "default_true")]
    pub import_repo: bool,
    /// Whether to import issues
    #[serde(default = "default_true")]
    pub import_issues: bool,
    /// Whether to import pull/merge requests
    #[serde(default = "default_true")]
    pub import_pull_requests: bool,
    /// Whether to import wiki pages
    #[serde(default)]
    pub import_wiki: bool,
    /// Whether to import releases
    #[serde(default = "default_true")]
    pub import_releases: bool,
    /// Whether to import labels
    #[serde(default = "default_true")]
    pub import_labels: bool,
    /// Whether to import milestones
    #[serde(default = "default_true")]
    pub import_milestones: bool,
}

fn default_true() -> bool {
    true
}

/// POST /api/v1/imports
///
/// Start a new import from GitHub, GitLab, Gitea, or a generic Git remote.
#[utoipa::path(
    post,
    path = "/imports",
    tag = "Imports",
    request_body = StartImportRequest,
    responses(
        (status = 201, description = "Import started", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn start_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<StartImportRequest>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let user_id: i64 = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return AppError::unauthorized("invalid token subject").into_response(),
    };

    // Validate platform
    if !matches!(
        body.platform.as_str(),
        "github" | "gitlab" | "gitea" | "git"
    ) {
        return AppError::bad_request("platform must be 'github', 'gitlab', 'gitea', or 'git'")
            .into_response();
    }

    // Resolve target name
    let target_name = match body.target_name {
        Some(ref n) if !n.is_empty() => n.clone(),
        _ => {
            let url = body
                .source_url
                .trim_end_matches('/')
                .trim_end_matches(".git");
            url.split('/')
                .next_back()
                .unwrap_or("imported-repo")
                .to_string()
        }
    };

    match rg_core::import::service::start_import(
        &state.db,
        user_id,
        body.platform,
        body.source_url,
        body.target_owner,
        target_name,
        body.auth_token,
        body.import_repo,
        body.import_issues,
        body.import_pull_requests,
        body.import_wiki,
        body.import_releases,
        body.import_labels,
        body.import_milestones,
        &state.repo_root,
    )
    .await
    {
        Ok(task) => (StatusCode::CREATED, Json(serde_json::json!(task))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// GET /api/v1/imports/{id}
///
/// Check the status of an import task.
#[utoipa::path(
    get,
    path = "/imports/{id}",
    tag = "Imports",
    params(
        ("id" = i64, Path, description = "Import task ID"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn get_import_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let user_id: i64 = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return AppError::unauthorized("invalid token subject").into_response(),
    };

    match rg_db::ops::import_task_ops::find_by_id(&state.db, id).await {
        Ok(Some(task)) if task.user_id == user_id => {
            (StatusCode::OK, Json(serde_json::json!(task))).into_response()
        }
        Ok(Some(_)) => AppError::not_found("import task not found").into_response(),
        Ok(None) => AppError::not_found("import task not found").into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

/// GET /api/v1/imports
///
/// List the current user's import tasks.
#[utoipa::path(
    get,
    path = "/imports",
    tag = "Imports",
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_imports(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let user_id: i64 = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return AppError::unauthorized("invalid token subject").into_response(),
    };

    match rg_db::ops::import_task_ops::find_by_user(&state.db, user_id, 20).await {
        Ok(tasks) => (StatusCode::OK, Json(serde_json::json!(tasks))).into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

/// DELETE /api/v1/imports/{id}
///
/// Cancel and delete an import task.
#[utoipa::path(
    delete,
    path = "/imports/{id}",
    tag = "Imports",
    params(
        ("id" = i64, Path, description = "Import task ID"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn delete_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let user_id: i64 = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return AppError::unauthorized("invalid token subject").into_response(),
    };

    match rg_db::ops::import_task_ops::find_by_id(&state.db, id).await {
        Ok(Some(task)) if task.user_id == user_id => {
            match rg_db::ops::import_task_ops::delete_by_id(&state.db, id).await {
                Ok(()) => StatusCode::NO_CONTENT.into_response(),
                Err(e) => AppError::internal(e).into_response(),
            }
        }
        Ok(Some(_)) => AppError::not_found("import task not found").into_response(),
        Ok(None) => AppError::not_found("import task not found").into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}
