//! REST API handlers for repository collaborators.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::error::AppError;
use crate::AppState;

// ── Request / Response types ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct AddCollaboratorRequest {
    pub user_id: Option<i64>,
    pub username: Option<String>,
    pub email: Option<String>,
    /// read / write / admin
    #[serde(default = "default_permission")]
    pub permission: String,
}

fn default_permission() -> String {
    "read".to_string()
}

#[derive(Deserialize)]
pub struct UpdatePermissionRequest {
    pub permission: String,
}

// ── Handlers ──────────────────────────────────────────────────────────

/// List collaborators for a repo.
/// GET /api/v1/repos/:owner/:name/collaborators
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/collaborators",
    tag = "Collaborators",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_collaborators(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
) -> impl IntoResponse {
    match rg_core::collaborator::service::list_collaborators(&state.db, &owner, &repo).await {
        Ok(collaborators) => (StatusCode::OK, Json(collaborators)).into_response(),
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

/// Add a collaborator to a repo.
/// POST /api/v1/repos/:owner/:name/collaborators
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/collaborators",
    tag = "Collaborators",
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
pub async fn add_collaborator(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<AddCollaboratorRequest>,
) -> impl IntoResponse {
    if super::auth::extract_user_id(&headers, &state.jwt_secret).is_none() {
        return AppError::unauthorized("authentication required").into_response();
    }

    let user_id = match resolve_collaborator_user_id(&state.db, &req).await {
        Ok(user_id) => user_id,
        Err(e) => return AppError::bad_request(e.to_string()).into_response(),
    };

    match rg_core::collaborator::service::add_collaborator(
        &state.db,
        &owner,
        &repo,
        user_id,
        req.permission,
    )
    .await
    {
        Ok(collab) => (StatusCode::CREATED, Json(collab)).into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

async fn resolve_collaborator_user_id(
    db: &rg_db::DatabaseConnection,
    req: &AddCollaboratorRequest,
) -> anyhow::Result<i64> {
    if let Some(user_id) = req.user_id {
        if user_id > 0 {
            return Ok(user_id);
        }
        anyhow::bail!("user_id must be a positive integer");
    }

    if let Some(username) = req
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return rg_db::ops::user_ops::find_by_username(db, username)
            .await?
            .map(|user| user.id)
            .ok_or_else(|| anyhow::anyhow!("user '{}' not found", username));
    }

    if let Some(email) = req
        .email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return rg_db::ops::user_ops::find_by_email(db, email)
            .await?
            .map(|user| user.id)
            .ok_or_else(|| anyhow::anyhow!("user '{}' not found", email));
    }

    anyhow::bail!("user_id, username, or email is required");
}

/// Update a collaborator's permission.
/// PATCH /api/v1/repos/:owner/:name/collaborators/:id
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/collaborators/{id}",
    tag = "Collaborators",
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
pub async fn update_permission(
    State(state): State<AppState>,
    Path((_owner, _repo, id)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<UpdatePermissionRequest>,
) -> impl IntoResponse {
    if super::auth::extract_user_id(&headers, &state.jwt_secret).is_none() {
        return AppError::unauthorized("authentication required").into_response();
    }

    match rg_core::collaborator::service::update_permission(&state.db, id, req.permission).await {
        Ok(collab) => (StatusCode::OK, Json(collab)).into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

/// Remove a collaborator from a repo.
/// DELETE /api/v1/repos/:owner/:name/collaborators/:user_id
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/collaborators/{user_id}",
    tag = "Collaborators",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("user_id" = i64, Path, description = "user_id"),
    ),
    responses(
        (status = 204, description = "Removed"),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn remove_collaborator(
    State(state): State<AppState>,
    Path((owner, repo, user_id)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if super::auth::extract_user_id(&headers, &state.jwt_secret).is_none() {
        return AppError::unauthorized("authentication required").into_response();
    }

    match rg_core::collaborator::service::remove_collaborator(&state.db, &owner, &repo, user_id)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}
