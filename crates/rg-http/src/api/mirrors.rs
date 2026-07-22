//! Repository Mirror REST API.
//!
//! POST   /api/v1/repos/:owner/:name/mirror       — create mirror
//! GET    /api/v1/repos/:owner/:name/mirror       — get mirror status
//! PATCH  /api/v1/repos/:owner/:name/mirror       — update mirror settings
//! DELETE /api/v1/repos/:owner/:name/mirror       — delete mirror
//! POST   /api/v1/repos/:owner/:name/mirror/sync  — manual sync trigger

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

/// Request body for creating/updating a mirror.
#[derive(Deserialize, ToSchema)]
pub struct CreateMirrorRequest {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default = "default_interval")]
    pub sync_interval_seconds: i64,
}

fn default_interval() -> i64 {
    86400
}

/// Request body for updating a mirror.
#[derive(Deserialize, ToSchema)]
pub struct UpdateMirrorRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_interval_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// POST /api/v1/repos/{owner}/{name}/mirror
///
/// Create a mirror for the repository to periodically sync from a remote URL.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/mirror",
    tag = "Mirrors",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body = CreateMirrorRequest,
    responses(
        (status = 201, description = "Mirror created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn create_mirror(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<CreateMirrorRequest>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let user_id: i64 = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return AppError::unauthorized("invalid token subject").into_response(),
    };

    let repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name).await
    {
        Ok(Some(r)) => r,
        Ok(None) => return AppError::not_found("repository not found").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    };

    // Check write permission
    match rg_core::repo::service::can_write_repo(&state.db, &repo, Some(user_id)).await {
        Ok(true) => {}
        Ok(false) => return AppError::forbidden("no write permission").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    }

    match rg_core::mirror::service::create_mirror(
        &state.db,
        repo.id,
        body.url,
        body.username,
        body.password,
        body.sync_interval_seconds,
    )
    .await
    {
        Ok(mirror) => (StatusCode::CREATED, Json(serde_json::json!(mirror))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// GET /api/v1/repos/{owner}/{name}/mirror
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/mirror",
    tag = "Mirrors",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn get_mirror(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
) -> impl IntoResponse {
    let repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name).await
    {
        Ok(Some(r)) => r,
        Ok(None) => return AppError::not_found("repository not found").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    };

    match rg_core::mirror::service::get_mirror(&state.db, repo.id).await {
        Ok(Some(mirror)) => (StatusCode::OK, Json(serde_json::json!(mirror))).into_response(),
        Ok(None) => AppError::not_found("no mirror configured for this repository").into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

/// PATCH /api/v1/repos/{owner}/{name}/mirror
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/mirror",
    tag = "Mirrors",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body = UpdateMirrorRequest,
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_mirror(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<UpdateMirrorRequest>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let user_id: i64 = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return AppError::unauthorized("invalid token subject").into_response(),
    };

    let repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name).await
    {
        Ok(Some(r)) => r,
        Ok(None) => return AppError::not_found("repository not found").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    };

    match rg_core::repo::service::can_write_repo(&state.db, &repo, Some(user_id)).await {
        Ok(true) => {}
        Ok(false) => return AppError::forbidden("no write permission").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    }

    match rg_core::mirror::service::update_mirror(
        &state.db,
        repo.id,
        body.url,
        body.username,
        body.password,
        body.sync_interval_seconds,
        body.status,
    )
    .await
    {
        Ok(mirror) => (StatusCode::OK, Json(serde_json::json!(mirror))).into_response(),
        Err(e) => AppError::bad_request(e).into_response(),
    }
}

/// DELETE /api/v1/repos/{owner}/{name}/mirror
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/mirror",
    tag = "Mirrors",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_mirror(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let user_id: i64 = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return AppError::unauthorized("invalid token subject").into_response(),
    };

    let repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name).await
    {
        Ok(Some(r)) => r,
        Ok(None) => return AppError::not_found("repository not found").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    };

    match rg_core::repo::service::can_write_repo(&state.db, &repo, Some(user_id)).await {
        Ok(true) => {}
        Ok(false) => return AppError::forbidden("no write permission").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    }

    match rg_core::mirror::service::delete_mirror(&state.db, repo.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

/// POST /api/v1/repos/{owner}/{name}/mirror/sync
///
/// Manually trigger a mirror sync.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/mirror/sync",
    tag = "Mirrors",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Sync triggered", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn trigger_mirror_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let user_id: i64 = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return AppError::unauthorized("invalid token subject").into_response(),
    };

    let repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name).await
    {
        Ok(Some(r)) => r,
        Ok(None) => return AppError::not_found("repository not found").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    };

    match rg_core::repo::service::can_write_repo(&state.db, &repo, Some(user_id)).await {
        Ok(true) => {}
        Ok(false) => return AppError::forbidden("no write permission").into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    }

    match rg_core::mirror::service::trigger_sync(&state.db, repo.id, &state.repo_root).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "sync_triggered"})),
        )
            .into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}
