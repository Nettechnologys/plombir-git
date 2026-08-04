//! Webhook REST API endpoints.
//!
//! Every endpoint here is repository *administration*: a webhook carries the
//! delivery target and the HMAC key ForgeKeep signs deliveries with, exactly
//! like a deploy key or a CI secret. So all seven verbs — reads included — sit
//! behind the [`RepoAdmin`] extractor, the same door `api::deploy_keys` and
//! `api::ci_secrets` use. They previously stopped at
//! `extract_user_id`, which is authentication, not authorization: any account
//! with a valid token could read — and rewrite — the webhooks of any
//! repository, private ones included, and the reply handed over the raw
//! `secret` with them.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::repo_access::RepoAdmin;
use crate::error::AppError;
use crate::AppState;

// ── Wire types ────────────────────────────────────────────────────────────

/// A webhook as the API reports it.
///
/// The entity behind it carries `secret` — the key every delivery's
/// `X-Hub-Signature-256` is computed with — and serializing the row wholesale
/// handed that key to every caller of every webhook endpoint. A reader of it
/// can forge deliveries at will, so the wire shape is spelled out by hand: what
/// the settings form needs is *whether* a secret is stored, not what it is.
/// (Same treatment as `MirrorResponse` gives the mirror password.)
#[derive(Serialize, ToSchema)]
pub struct WebhookResponse {
    pub id: i64,
    pub repo_id: i64,
    pub url: String,
    pub content_type: String,
    /// Whether an HMAC secret is configured. The value itself never leaves the
    /// server — write a new one to replace it.
    pub has_secret: bool,
    pub active: bool,
    pub events: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<rg_db::entities::webhook::Model> for WebhookResponse {
    fn from(hook: rg_db::entities::webhook::Model) -> Self {
        Self {
            id: hook.id,
            repo_id: hook.repo_id,
            url: hook.url,
            content_type: hook.content_type,
            has_secret: hook.secret_encrypted.is_some_and(|s| !s.is_empty()),
            active: hook.active,
            events: hook.events,
            created_at: hook.created_at,
            updated_at: hook.updated_at,
        }
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────

/// List webhooks for a repo.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/hooks",
    tag = "Webhooks",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = [WebhookResponse]),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository admin access required", body = serde_json::Value),
        (status = 404, description = "Repository not found", body = serde_json::Value),
    ),
)]
pub async fn list_webhooks(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    match rg_core::webhook::service::list_webhooks(&state.db, repo.id).await {
        Ok(hooks) => {
            let body: Vec<WebhookResponse> = hooks.into_iter().map(WebhookResponse::from).collect();
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Create a webhook.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/hooks",
    tag = "Webhooks",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = WebhookResponse),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository admin access required", body = serde_json::Value),
        (status = 404, description = "Repository not found", body = serde_json::Value),
    ),
)]
pub async fn create_webhook(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
    Json(body): Json<rg_core::webhook::service::CreateWebhookRequest>,
) -> impl IntoResponse {
    match rg_core::webhook::service::create_webhook(
        &state.db,
        repo.id,
        &body,
        &state.encryption_key,
    )
    .await
    {
        Ok(hook) => (StatusCode::CREATED, Json(WebhookResponse::from(hook))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Get a webhook by id.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/hooks/{id}",
    tag = "Webhooks",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = WebhookResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository admin access required", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn get_webhook(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    match webhook_in_repo(&state.db, repo.id, id).await {
        Ok(hook) => (StatusCode::OK, Json(WebhookResponse::from(hook))).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Update a webhook.
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/hooks/{id}",
    tag = "Webhooks",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = WebhookResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository admin access required", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn update_webhook(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, .. }: RepoAdmin,
    Json(body): Json<rg_core::webhook::service::UpdateWebhookRequest>,
) -> impl IntoResponse {
    let existing = match webhook_in_repo(&state.db, repo.id, id).await {
        Ok(hook) => hook,
        Err(e) => return e.into_response(),
    };

    match rg_core::webhook::service::update_webhook(
        &state.db,
        &existing,
        &body,
        &state.encryption_key,
    )
    .await
    {
        Ok(hook) => (StatusCode::OK, Json(WebhookResponse::from(hook))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Delete a webhook.
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/hooks/{id}",
    tag = "Webhooks",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository admin access required", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn delete_webhook(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    if let Err(e) = webhook_in_repo(&state.db, repo.id, id).await {
        return e.into_response();
    }

    match rg_core::webhook::service::delete_webhook(&state.db, id).await {
        Ok(true) => (
            StatusCode::OK,
            Json(serde_json::json!({"message": "webhook deleted"})),
        )
            .into_response(),
        // "webhook deleted" is a statement about this request. The scoping
        // lookup above ran in a statement of its own, so a concurrent delete
        // can have taken the row in between — that request deleted it, this
        // one did not.
        Ok(false) => AppError::not_found("webhook not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// List recent deliveries for a webhook.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/hooks/{id}/deliveries",
    tag = "Webhooks",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository admin access required", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn list_deliveries(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    if let Err(e) = webhook_in_repo(&state.db, repo.id, id).await {
        return e.into_response();
    }

    match rg_core::webhook::service::list_deliveries(&state.db, id).await {
        Ok(deliveries) => (StatusCode::OK, Json(serde_json::json!(deliveries))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Redeliver a webhook.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver",
    tag = "Webhooks",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
        ("delivery_id" = i64, Path, description = "delivery_id"),
    ),
    responses(
        (status = 200, description = "Redelivery triggered", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Repository admin access required", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
    ),
)]
pub async fn redeliver(
    State(state): State<AppState>,
    Path((_, _, id, delivery_id)): Path<(String, String, i64, i64)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    let hook = match webhook_in_repo(&state.db, repo.id, id).await {
        Ok(hook) => hook,
        Err(e) => return e.into_response(),
    };

    if let Err(e) = delivery_in_webhook(&state.db, hook.id, delivery_id).await {
        return e.into_response();
    }

    match rg_core::webhook::service::redeliver(&state.db, delivery_id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"message": "redelivery triggered"})),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Fetch a webhook and re-anchor it to the repository the caller was authorized
/// for.
///
/// `{id}` is a global `webhooks` primary key, so being an admin of one
/// repository must not reach another one's rows. A mismatch answers 404, not
/// 403: a 403 would still confirm the id exists, which is most of what an
/// id-walking caller wants to learn.
async fn webhook_in_repo(
    db: &DatabaseConnection,
    repo_id: i64,
    webhook_id: i64,
) -> Result<rg_db::entities::webhook::Model, AppError> {
    match rg_core::webhook::service::get_webhook(db, webhook_id).await {
        Ok(Some(hook)) if hook.repo_id == repo_id => Ok(hook),
        Ok(Some(_)) | Ok(None) => Err(AppError::not_found("webhook not found")),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Fetch a delivery and re-anchor it to the webhook the caller was authorized
/// for.
///
/// `{delivery_id}` is a global `webhook_deliveries` primary key, so being an
/// admin of one repository must not replay another one's deliveries. The anchor
/// is the webhook rather than the repository because that is the row the id
/// hangs off — [`webhook_in_repo`] has already tied that webhook to the
/// repository, so the two checks chain into the same guarantee.
///
/// A mismatch answers 404 for the same reason [`webhook_in_repo`] does.
///
/// Spelled as a named helper rather than inline in `redeliver`: an inline
/// `delivery.webhook_id == hook.id` is invisible to `global_id_anchor_guard`,
/// so the next delivery route could drop it and still ship green.
async fn delivery_in_webhook(
    db: &DatabaseConnection,
    webhook_id: i64,
    delivery_id: i64,
) -> Result<rg_db::entities::webhook_delivery::Model, AppError> {
    match rg_core::webhook::service::get_delivery(db, delivery_id).await {
        Ok(Some(delivery)) if delivery.webhook_id == webhook_id => Ok(delivery),
        Ok(Some(_)) | Ok(None) => Err(AppError::not_found("delivery not found")),
        Err(e) => Err(AppError::from(e)),
    }
}
