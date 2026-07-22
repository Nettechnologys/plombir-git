//! REST API handlers for branch protection rules.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::error::AppError;
use crate::AppState;

// ── Request / Response types ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateProtectionRequest {
    pub branch_name: String,
    #[serde(default)]
    pub require_pr: bool,
    #[serde(default)]
    pub require_status_check: bool,
    #[serde(default)]
    pub required_status_checks: Option<Vec<String>>,
    #[serde(default)]
    pub require_approval: bool,
    #[serde(default)]
    pub required_approvals: Option<i64>,
    #[serde(default)]
    pub allow_force_push: bool,
    #[serde(default)]
    pub require_signed_commits: bool,
    #[serde(default)]
    pub allowed_push_user_ids: Option<Vec<i64>>,
}

#[derive(Deserialize)]
pub struct UpdateProtectionRequest {
    #[serde(default)]
    pub require_pr: Option<bool>,
    #[serde(default)]
    pub require_status_check: Option<bool>,
    #[serde(default)]
    pub required_status_checks: Option<Vec<String>>,
    #[serde(default)]
    pub require_approval: Option<bool>,
    #[serde(default)]
    pub required_approvals: Option<i64>,
    #[serde(default)]
    pub allow_force_push: Option<bool>,
    #[serde(default)]
    pub require_signed_commits: Option<bool>,
    #[serde(default)]
    pub allowed_push_user_ids: Option<Vec<i64>>,
}

// ── Handlers ──────────────────────────────────────────────────────────

/// List branch protection rules for a repo.
/// GET /api/v1/repos/:owner/:name/branches/protection
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/branches/protection",
    tag = "Branch Protection",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_protections(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = super::repo_access::require_read(&state, &headers, &owner, &repo).await {
        return error.into_response();
    }
    match rg_core::branch_protection::service::list_protections(&state.db, &owner, &repo).await {
        Ok(protections) => (StatusCode::OK, Json(protections)).into_response(),
        Err(e) => {
            tracing::error!(%e, "list_protections failed");
            AppError::internal(e).into_response()
        }
    }
}

/// Create a branch protection rule.
/// POST /api/v1/repos/:owner/:name/branches/protection
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/branches/protection",
    tag = "Branch Protection",
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
pub async fn create_protection(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateProtectionRequest>,
) -> impl IntoResponse {
    if let Err(error) = super::repo_access::require_admin(&state, &headers, &owner, &repo).await {
        return error.into_response();
    }

    match rg_core::branch_protection::service::create_protection(
        &state.db,
        &owner,
        &repo,
        req.branch_name,
        req.require_pr,
        req.require_status_check,
        req.required_status_checks,
        req.require_approval,
        req.required_approvals,
        req.allow_force_push,
        req.require_signed_commits,
        req.allowed_push_user_ids,
    )
    .await
    {
        Ok(protection) => (StatusCode::CREATED, Json(protection)).into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

/// Get a branch protection rule by ID.
/// GET /api/v1/repos/:owner/:name/branches/protection/:id
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/branches/protection/{id}",
    tag = "Branch Protection",
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
pub async fn get_protection(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = super::repo_access::require_read(&state, &headers, &owner, &repo).await {
        return error.into_response();
    }
    match rg_core::branch_protection::service::get_protection_for_repo(&state.db, &owner, &repo, id)
        .await
    {
        Ok(protection) => (StatusCode::OK, Json(protection)).into_response(),
        Err(e) => AppError::not_found(e.to_string()).into_response(),
    }
}

/// Update a branch protection rule.
/// PATCH /api/v1/repos/:owner/:name/branches/protection/:id
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/branches/protection/{id}",
    tag = "Branch Protection",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_protection(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<UpdateProtectionRequest>,
) -> impl IntoResponse {
    if let Err(error) = super::repo_access::require_admin(&state, &headers, &owner, &repo).await {
        return error.into_response();
    }

    match rg_core::branch_protection::service::update_protection_for_repo(
        &state.db,
        &owner,
        &repo,
        id,
        req.require_pr,
        req.require_status_check,
        req.required_status_checks,
        req.require_approval,
        req.required_approvals,
        req.allow_force_push,
        req.require_signed_commits,
        req.allowed_push_user_ids,
    )
    .await
    {
        Ok(protection) => (StatusCode::OK, Json(protection)).into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

/// Delete a branch protection rule.
/// DELETE /api/v1/repos/:owner/:name/branches/protection/:id
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/branches/protection/{id}",
    tag = "Branch Protection",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_protection(
    State(state): State<AppState>,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = super::repo_access::require_admin(&state, &headers, &owner, &repo).await {
        return error.into_response();
    }

    match rg_core::branch_protection::service::delete_protection_for_repo(
        &state.db, &owner, &repo, id,
    )
    .await
    {
        Ok(()) => (StatusCode::NO_CONTENT, Json(serde_json::json!({}))).into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}
