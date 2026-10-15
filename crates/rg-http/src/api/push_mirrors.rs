//! Push mirror REST API.
//!
//! GET    /api/v1/repos/:owner/:name/push_mirrors           — list
//! POST   /api/v1/repos/:owner/:name/push_mirrors           — add
//! PATCH  /api/v1/repos/:owner/:name/push_mirrors/:id       — change settings
//! DELETE /api/v1/repos/:owner/:name/push_mirrors/:id       — remove
//! POST   /api/v1/repos/:owner/:name/push_mirrors/:id/sync  — push now
//!
//! Repository administration, so `RepoAdmin` on every verb: a push mirror
//! hands this repository's whole history and a credential for another forge
//! to a remote the caller chooses. The pull mirror is administration too: its
//! pass rewrites every branch past branch protection (card_b42563679dd3).

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::access_audit::{grant_actor, record_grant};
use crate::api::repo_access::RepoAdmin;
use crate::error::AppError;
use crate::AppState;

/// A push mirror as the API reports it. The credential never leaves the
/// server; `has_credentials` says whether one is stored.
#[derive(Serialize, ToSchema)]
pub struct PushMirrorResponse {
    pub id: i64,
    /// The remote, without a credential in it.
    pub url: String,
    pub username: Option<String>,
    pub has_credentials: bool,
    /// Pushed after every ref move, not only on the interval.
    pub sync_on_commit: bool,
    /// `0`: no interval — pushed on commit and on request only.
    pub sync_interval_seconds: i64,
    /// The remote is kept an exact copy: rewritten branches are overwritten
    /// and deleted ones removed there.
    pub force: bool,
    pub enabled: bool,
    pub next_sync_at: Option<DateTime<Utc>>,
    /// The first ref move the remote has not received yet; `null` when it is
    /// up to date.
    pub pending_since: Option<DateTime<Utc>>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    /// Why the last pass failed; `null` after a success.
    pub last_sync_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<rg_db::entities::push_mirror::Model> for PushMirrorResponse {
    fn from(m: rg_db::entities::push_mirror::Model) -> Self {
        Self {
            id: m.id,
            url: m.url,
            username: m.username,
            has_credentials: m.password_encrypted.is_some(),
            sync_on_commit: m.sync_on_commit,
            sync_interval_seconds: m.sync_interval_seconds,
            force: m.force,
            enabled: m.enabled,
            next_sync_at: m.next_sync_at,
            pending_since: m.pending_since,
            last_sync_at: m.last_sync_at,
            last_success_at: m.last_success_at,
            last_sync_error: m.last_sync_error,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize, ToSchema)]
pub struct CreatePushMirrorRequest {
    /// The remote: `https://host/owner/repo.git`. A credential written into
    /// the URL is moved into `username` and the encrypted password and does
    /// not stay in the stored address.
    pub url: String,
    pub username: Option<String>,
    /// Password or access token with push rights on the remote. Encrypted
    /// before storage and handed to `git` only for the duration of a push.
    pub password: Option<String>,
    #[serde(default = "default_true")]
    pub sync_on_commit: bool,
    #[serde(default = "default_interval")]
    pub sync_interval_seconds: i64,
    /// Overwrite the remote's history to match this repository. Off by default.
    #[serde(default)]
    pub force: bool,
}

fn default_true() -> bool {
    true
}

fn default_interval() -> i64 {
    rg_core::mirror::push::DEFAULT_SYNC_INTERVAL_SECONDS
}

#[derive(Deserialize, ToSchema)]
pub struct UpdatePushMirrorRequest {
    pub url: Option<String>,
    /// An empty string clears the stored username.
    pub username: Option<String>,
    /// An empty string clears the stored credential.
    pub password: Option<String>,
    pub sync_on_commit: Option<bool>,
    pub sync_interval_seconds: Option<i64>,
    pub force: Option<bool>,
    pub enabled: Option<bool>,
}

fn credential_change(supplied: Option<&String>) -> &'static str {
    match supplied {
        None => "unchanged",
        Some(value) if value.is_empty() => "cleared",
        Some(_) => "replaced",
    }
}

fn audit_details(
    mirror: &rg_db::entities::push_mirror::Model,
    credential: Option<&str>,
) -> serde_json::Value {
    let mut details = serde_json::json!({
        "push_mirror_id": mirror.id,
        "target_url": mirror.url,
        "username": mirror.username,
        "has_credential": mirror.password_encrypted.is_some(),
        "force": mirror.force,
        "enabled": mirror.enabled,
    });
    if let Some(credential) = credential {
        details["credential"] = credential.into();
    }
    details
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/push_mirrors",
    tag = "Mirrors",
    params(("owner" = String, Path), ("name" = String, Path)),
    responses(
        (status = 200, description = "Push mirrors, oldest first", body = [PushMirrorResponse]),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository administration denied", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn list_push_mirrors(
    State(state): State<AppState>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    match rg_core::mirror::push::list_push_mirrors(&state.db, repo.id).await {
        Ok(mirrors) => Json(
            mirrors
                .into_iter()
                .map(PushMirrorResponse::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/push_mirrors",
    tag = "Mirrors",
    params(("owner" = String, Path), ("name" = String, Path)),
    request_body = CreatePushMirrorRequest,
    responses(
        (status = 201, description = "Push mirror added", body = PushMirrorResponse),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository administration denied", body = serde_json::Value),
        (status = 409, description = "Too many push mirrors", body = serde_json::Value),
    ),
)]
pub async fn create_push_mirror(
    State(state): State<AppState>,
    Path((owner, _)): Path<(String, String)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
    Json(body): Json<CreatePushMirrorRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    match rg_core::mirror::push::create_push_mirror(
        &state.db,
        repo.id,
        rg_core::mirror::push::NewPushMirror {
            url: body.url,
            username: body.username,
            password: body.password,
            sync_on_commit: body.sync_on_commit,
            sync_interval_seconds: body.sync_interval_seconds,
            force: body.force,
        },
        state.mirror_transport_policy,
        &state.encryption_key,
    )
    .await
    {
        Ok(mirror) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.push_mirror_create",
                &owner,
                &repo,
                &headers,
                audit_details(&mirror, None),
            )
            .await;
            (StatusCode::CREATED, Json(PushMirrorResponse::from(mirror))).into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/push_mirrors/{id}",
    tag = "Mirrors",
    params(("owner" = String, Path), ("name" = String, Path), ("id" = i64, Path)),
    request_body = UpdatePushMirrorRequest,
    responses(
        (status = 200, description = "Updated", body = PushMirrorResponse),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository administration denied", body = serde_json::Value),
        (status = 404, description = "No such push mirror", body = serde_json::Value),
    ),
)]
pub async fn update_push_mirror(
    State(state): State<AppState>,
    Path((owner, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
    Json(body): Json<UpdatePushMirrorRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let credential = credential_change(body.password.as_ref());
    match rg_core::mirror::push::update_push_mirror(
        &state.db,
        repo.id,
        id,
        rg_core::mirror::push::PushMirrorChanges {
            url: body.url,
            username: body.username,
            password: body.password,
            sync_on_commit: body.sync_on_commit,
            sync_interval_seconds: body.sync_interval_seconds,
            force: body.force,
            enabled: body.enabled,
        },
        state.mirror_transport_policy,
        &state.encryption_key,
    )
    .await
    {
        Ok(mirror) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.push_mirror_update",
                &owner,
                &repo,
                &headers,
                audit_details(&mirror, Some(credential)),
            )
            .await;
            Json(PushMirrorResponse::from(mirror)).into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/push_mirrors/{id}",
    tag = "Mirrors",
    params(("owner" = String, Path), ("name" = String, Path), ("id" = i64, Path)),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository administration denied", body = serde_json::Value),
        (status = 404, description = "No such push mirror", body = serde_json::Value),
    ),
)]
pub async fn delete_push_mirror(
    State(state): State<AppState>,
    Path((owner, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    // Read first: after the delete nothing says which remote stopped receiving.
    let existing = match rg_db::ops::push_mirror_ops::find_for_repo(&state.db, repo.id, id).await {
        Ok(Some(mirror)) => mirror,
        Ok(None) => return AppError::not_found("push mirror not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_core::mirror::push::delete_push_mirror(&state.db, repo.id, id).await {
        Ok(()) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.push_mirror_delete",
                &owner,
                &repo,
                &headers,
                audit_details(&existing, None),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/push_mirrors/{id}/sync",
    tag = "Mirrors",
    params(("owner" = String, Path), ("name" = String, Path), ("id" = i64, Path)),
    responses(
        (status = 202, description = "Queued; the push runs in the background", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository administration denied", body = serde_json::Value),
        (status = 404, description = "No such push mirror", body = serde_json::Value),
        (status = 409, description = "The push mirror is switched off", body = serde_json::Value),
    ),
)]
pub async fn sync_push_mirror(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    match rg_core::mirror::push::request_push_mirror_sync(&state.db, repo.id, id).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({"status": "sync_queued"})),
        )
            .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}
