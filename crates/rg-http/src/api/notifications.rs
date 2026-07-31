//! REST API handlers for notifications.

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::auth::AuthUser;
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::AppState;

// ── Response types ───────────────────────────────────────────

#[derive(Serialize)]
struct NotificationResponse {
    id: i64,
    user_id: i64,
    event_type: String,
    title: String,
    body: Option<String>,
    repo_id: Option<i64>,
    is_read: bool,
    created_at: String,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListNotificationsQuery {
    unread_only: Option<bool>,
    #[serde(flatten)]
    #[param(ignore)]
    pagination: PaginationParams,
}

// ── Handlers ─────────────────────────────────────────────────

/// GET /api/v1/notifications
/// List notifications for the authenticated user.
#[utoipa::path(
    get,
    path = "/notifications",
    tag = "Notifications",
    params(ListNotificationsQuery, PaginationParams),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_notifications(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Query(params): Query<ListNotificationsQuery>,
) -> impl IntoResponse {
    let unread_only = params.unread_only.unwrap_or(false);
    let pagination = params.pagination.clamp();
    let offset = pagination.offset();
    let limit = pagination.limit();

    match rg_core::notification::list_notifications_paginated(
        &state.db,
        user_id,
        unread_only,
        offset,
        limit,
    )
    .await
    {
        Ok((notifications, total)) => {
            let resp: Vec<NotificationResponse> = notifications
                .into_iter()
                .map(|n| NotificationResponse {
                    id: n.id,
                    user_id: n.user_id,
                    event_type: n.event_type,
                    title: n.title,
                    body: n.body,
                    repo_id: n.repo_id,
                    is_read: n.is_read,
                    created_at: n.created_at.to_string(),
                })
                .collect();
            Json(PaginatedResponse::new(resp, &pagination, total as u64)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/notifications/unread-count
#[utoipa::path(
    get,
    path = "/notifications/unread-count",
    tag = "Notifications",
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn unread_count(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> impl IntoResponse {
    match rg_core::notification::unread_count(&state.db, user_id).await {
        Ok(count) => Json(serde_json::json!({"unread_count": count})).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// POST /api/v1/notifications/:id/read
#[utoipa::path(
    post,
    path = "/notifications/{id}/read",
    tag = "Notifications",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn mark_read(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match rg_core::notification::mark_read_for_user(&state.db, id, user_id).await {
        Ok(()) => Json(serde_json::json!({"id": id, "is_read": true})).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// POST /api/v1/notifications/mark-all-read
#[utoipa::path(
    post,
    path = "/notifications/mark-all-read",
    tag = "Notifications",
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn mark_all_read(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> impl IntoResponse {
    match rg_core::notification::mark_all_read(&state.db, user_id).await {
        Ok(count) => Json(serde_json::json!({"marked_read": count})).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/notifications/:id
#[utoipa::path(
    delete,
    path = "/notifications/{id}",
    tag = "Notifications",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_notification(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match rg_core::notification::delete_notification_for_user(&state.db, id, user_id).await {
        Ok(()) => Json(serde_json::json!({"deleted": true})).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}
