//! Repository Mirror REST API.
//!
//! POST   /api/v1/repos/:owner/:name/mirror       — create mirror
//! GET    /api/v1/repos/:owner/:name/mirror       — get mirror status
//! PATCH  /api/v1/repos/:owner/:name/mirror       — update mirror settings
//! DELETE /api/v1/repos/:owner/:name/mirror       — delete mirror
//! POST   /api/v1/repos/:owner/:name/mirror/sync  — manual sync trigger

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::repo_access::RepoWrite;
use crate::error::AppError;
use crate::AppState;

/// A mirror as the API reports it.
///
/// The entity behind it carries `password_encrypted` — the credential for the
/// remote — and serializing the row wholesale handed that to every caller of
/// every mirror endpoint. Nothing outside the sync worker has any business
/// seeing it, so the wire shape is spelled out by hand: what the settings form
/// needs is *whether* a password is stored, not what it is.
#[derive(Serialize, ToSchema)]
pub struct MirrorResponse {
    pub id: i64,
    pub repo_id: i64,
    pub url: String,
    /// Username for the remote, if the mirror authenticates.
    pub username: Option<String>,
    /// Whether a password is stored for the remote. The value itself never
    /// leaves the server.
    pub has_credentials: bool,
    pub sync_interval_seconds: i64,
    pub next_sync_at: Option<DateTime<Utc>>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_sync_error: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<rg_db::entities::mirror::Model> for MirrorResponse {
    fn from(m: rg_db::entities::mirror::Model) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
            url: m.url,
            username: m.username,
            has_credentials: m.password_encrypted.is_some(),
            sync_interval_seconds: m.sync_interval_seconds,
            next_sync_at: m.next_sync_at,
            last_sync_at: m.last_sync_at,
            last_sync_error: m.last_sync_error,
            status: m.status,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// Request body for creating/updating a mirror.
#[derive(Deserialize, ToSchema)]
pub struct CreateMirrorRequest {
    /// The remote to mirror. A credential written into the URL
    /// (`https://user:token@host/repo.git`) is moved into `username` +
    /// the encrypted password column and does not stay in the stored URL, so
    /// `url` here and the `url` in the response need not be byte-identical.
    /// A URL with a `user@` and no password is refused: nothing in it says
    /// whether that is a login name or a token.
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Password or access token for the remote. Encrypted before storage and
    /// handed to the `git` subprocess only for the duration of a sync.
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
    /// Replacement remote. Same credential handling as on create.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Replacement password or access token. An empty string clears the stored
    /// credential; omitting the field leaves it as it is.
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
        (status = 201, description = "Mirror created", body = MirrorResponse),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "This repository already has a mirror", body = serde_json::Value),
    ),
)]
pub async fn create_mirror(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<CreateMirrorRequest>,
) -> impl IntoResponse {
    match rg_core::mirror::service::create_mirror(
        &state.db,
        repo.id,
        body.url,
        body.username,
        body.password,
        body.sync_interval_seconds,
        &state.encryption_key,
    )
    .await
    {
        Ok(mirror) => (StatusCode::CREATED, Json(MirrorResponse::from(mirror))).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 200, description = "Success", body = MirrorResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Write access denied", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn get_mirror(
    State(state): State<AppState>,
    // Write access, not read access: a mirror is repository *administration*,
    // not repository content. The reply names the remote and its username and
    // says whether a credential is stored — under `RepoRead` a public repo
    // handed all of that to anonymous callers. The other four verbs on this
    // resource already require write, and the settings UI that consumes this
    // endpoint lives behind the same door, so read is the odd one out. (GitHub
    // likewise shows mirror configuration only with push access.)
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    match rg_core::mirror::service::get_mirror(&state.db, repo.id).await {
        Ok(Some(mirror)) => (StatusCode::OK, Json(MirrorResponse::from(mirror))).into_response(),
        Ok(None) => AppError::not_found("no mirror configured for this repository").into_response(),
        Err(e) => AppError::from(e).into_response(),
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
        (status = 200, description = "Updated", body = MirrorResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_mirror(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<UpdateMirrorRequest>,
) -> impl IntoResponse {
    match rg_core::mirror::service::update_mirror(
        &state.db,
        repo.id,
        body.url,
        body.username,
        body.password,
        body.sync_interval_seconds,
        body.status,
        &state.encryption_key,
    )
    .await
    {
        Ok(mirror) => (StatusCode::OK, Json(MirrorResponse::from(mirror))).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
    Path((_, _)): Path<(String, String)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    match rg_core::mirror::service::delete_mirror(&state.db, repo.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::from(e).into_response(),
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
    Path((_, _)): Path<(String, String)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    match rg_core::mirror::service::trigger_sync(
        &state.db,
        repo.id,
        &state.repo_root,
        &state.encryption_key,
    )
    .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "sync_triggered"})),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}
