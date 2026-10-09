//! REST API handlers for notifications.

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::auth::AuthUser;
use crate::api::repo_access::RepoAuthRead;
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
    /// Why this account got it: `review_requested`, `assigned`, `mention`,
    /// `ci_failed`, `participating`; absent on a repository-watch row.
    reason: Option<String>,
    /// `issue` or `pull_request` when it is about one.
    subject_type: Option<String>,
    /// The page it opens, relative to the instance.
    link: Option<String>,
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
                    reason: n.reason,
                    subject_type: n.subject_type,
                    link: n.link,
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

// ── Mail settings (card_349c2b6a0d7c) ────────────────────────

/// Which kinds of notification are also mailed to the account.
#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct EmailNotificationSettings {
    /// A review of yours was requested — directly or through CODEOWNERS.
    pub review_requested: bool,
    /// Somebody `@mentioned` you.
    pub mention: bool,
    /// An issue was assigned to you.
    pub assigned: bool,
    /// CI failed on a pull request of yours.
    pub ci_failed: bool,
    /// Activity in an issue or pull request you take part in or subscribed to.
    pub participating: bool,
    /// A push started CI in a repository you own.
    pub ci_triggered: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct NotificationSettingsResponse {
    pub email: EmailNotificationSettings,
}

/// Every key optional: what is left out keeps its value.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct EmailNotificationSettingsPatch {
    pub review_requested: Option<bool>,
    pub mention: Option<bool>,
    pub assigned: Option<bool>,
    pub ci_failed: Option<bool>,
    pub participating: Option<bool>,
    pub ci_triggered: Option<bool>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateNotificationSettingsRequest {
    pub email: EmailNotificationSettingsPatch,
}

fn settings_response(
    settings: rg_db::entities::notification_setting::Model,
) -> NotificationSettingsResponse {
    NotificationSettingsResponse {
        email: EmailNotificationSettings {
            review_requested: settings.email_review_requested,
            mention: settings.email_mention,
            assigned: settings.email_assigned,
            ci_failed: settings.email_ci_failed,
            participating: settings.email_participating,
            ci_triggered: settings.email_ci_triggered,
        },
    }
}

/// GET /api/v1/users/me/notification-settings
#[utoipa::path(
    get,
    path = "/users/me/notification-settings",
    tag = "Notifications",
    responses(
        (status = 200, description = "The account's mail choices", body = NotificationSettingsResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_notification_settings(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> impl IntoResponse {
    match rg_db::ops::notification_setting_ops::get(&state.db, user_id).await {
        Ok(settings) => Json(settings_response(settings)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// PUT /api/v1/users/me/notification-settings
#[utoipa::path(
    put,
    path = "/users/me/notification-settings",
    tag = "Notifications",
    request_body = UpdateNotificationSettingsRequest,
    responses(
        (status = 200, description = "Stored", body = NotificationSettingsResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_notification_settings(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Json(req): Json<UpdateNotificationSettingsRequest>,
) -> impl IntoResponse {
    let mut settings = match rg_db::ops::notification_setting_ops::get(&state.db, user_id).await {
        Ok(settings) => settings,
        Err(e) => return AppError::from(e).into_response(),
    };
    let patch = req.email;
    for (field, value) in [
        (&mut settings.email_review_requested, patch.review_requested),
        (&mut settings.email_mention, patch.mention),
        (&mut settings.email_assigned, patch.assigned),
        (&mut settings.email_ci_failed, patch.ci_failed),
        (&mut settings.email_participating, patch.participating),
        (&mut settings.email_ci_triggered, patch.ci_triggered),
    ] {
        if let Some(value) = value {
            *field = value;
        }
    }
    match rg_db::ops::notification_setting_ops::put(&state.db, settings).await {
        Ok(settings) => Json(settings_response(settings)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Thread subscriptions (card_349c2b6a0d7c) ─────────────────

/// Whether the caller follows an issue or a pull request.
#[derive(Serialize, utoipa::ToSchema)]
pub struct SubscriptionResponse {
    pub subscribed: bool,
    /// Why: `author`, `commented`, `mention`, `assigned`, `review_requested`,
    /// `manual`; absent when the caller never followed it.
    pub reason: Option<String>,
}

fn subscription_response(
    row: Option<rg_db::entities::thread_subscription::Model>,
) -> SubscriptionResponse {
    SubscriptionResponse {
        subscribed: row.as_ref().is_some_and(|row| row.subscribed),
        reason: row.map(|row| row.reason),
    }
}

/// The subject `number` addresses in `repo`.
async fn thread_subject(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    kind: rg_core::notification::thread::SubjectKind,
    number: i64,
) -> Result<i64, AppError> {
    use rg_core::notification::thread::SubjectKind;
    let id = match kind {
        SubjectKind::Issue => {
            rg_db::ops::issue_ops::find_by_repo_and_number(&state.db, repo.id, number)
                .await
                .map_err(AppError::from)?
                .map(|issue| issue.id)
        }
        SubjectKind::PullRequest => {
            rg_db::ops::pull_request_ops::find_by_repo_and_number(&state.db, repo.id, number)
                .await
                .map_err(AppError::from)?
                .map(|pr| pr.id)
        }
    };
    id.ok_or_else(|| AppError::not_found(format!("no {} #{number}", kind.as_str())))
}

async fn read_subscription(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    actor_id: i64,
    kind: rg_core::notification::thread::SubjectKind,
    number: i64,
) -> axum::response::Response {
    let subject_id = match thread_subject(state, repo, kind, number).await {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    match rg_core::notification::thread::subscription(&state.db, actor_id, kind, subject_id).await {
        Ok(row) => Json(subscription_response(row)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

async fn write_subscription(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    actor_id: i64,
    kind: rg_core::notification::thread::SubjectKind,
    number: i64,
    subscribed: bool,
) -> axum::response::Response {
    let subject_id = match thread_subject(state, repo, kind, number).await {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    match rg_core::notification::thread::set_subscription(
        &state.db, actor_id, repo.id, kind, subject_id, subscribed,
    )
    .await
    {
        Ok(row) => Json(subscription_response(Some(row))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/issues/:number/subscription
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues/{number}/subscription",
    tag = "Notifications",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Whether the caller follows it", body = SubscriptionResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such issue", body = serde_json::Value),
    ),
)]
pub async fn get_issue_subscription(
    State(state): State<AppState>,
    Path((_, _, number)): Path<(String, String, i64)>,
    RepoAuthRead { repo, actor_id }: RepoAuthRead,
) -> impl IntoResponse {
    read_subscription(
        &state,
        &repo,
        actor_id,
        rg_core::notification::thread::SubjectKind::Issue,
        number,
    )
    .await
}

/// PUT /api/v1/repos/:owner/:name/issues/:number/subscription — follow the issue.
#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/issues/{number}/subscription",
    tag = "Notifications",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Subscribed", body = SubscriptionResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such issue", body = serde_json::Value),
    ),
)]
pub async fn subscribe_issue(
    State(state): State<AppState>,
    Path((_, _, number)): Path<(String, String, i64)>,
    RepoAuthRead { repo, actor_id }: RepoAuthRead,
) -> impl IntoResponse {
    write_subscription(
        &state,
        &repo,
        actor_id,
        rg_core::notification::thread::SubjectKind::Issue,
        number,
        true,
    )
    .await
}

/// DELETE /api/v1/repos/:owner/:name/issues/:number/subscription — stop following the issue; taking part again does not undo it.
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/issues/{number}/subscription",
    tag = "Notifications",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Unsubscribed", body = SubscriptionResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such issue", body = serde_json::Value),
    ),
)]
pub async fn unsubscribe_issue(
    State(state): State<AppState>,
    Path((_, _, number)): Path<(String, String, i64)>,
    RepoAuthRead { repo, actor_id }: RepoAuthRead,
) -> impl IntoResponse {
    write_subscription(
        &state,
        &repo,
        actor_id,
        rg_core::notification::thread::SubjectKind::Issue,
        number,
        false,
    )
    .await
}

/// GET /api/v1/repos/:owner/:name/pulls/:number/subscription
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls/{number}/subscription",
    tag = "Notifications",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Whether the caller follows it", body = SubscriptionResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such pull request", body = serde_json::Value),
    ),
)]
pub async fn get_pull_subscription(
    State(state): State<AppState>,
    Path((_, _, number)): Path<(String, String, i64)>,
    RepoAuthRead { repo, actor_id }: RepoAuthRead,
) -> impl IntoResponse {
    read_subscription(
        &state,
        &repo,
        actor_id,
        rg_core::notification::thread::SubjectKind::PullRequest,
        number,
    )
    .await
}

/// PUT /api/v1/repos/:owner/:name/pulls/:number/subscription — follow the pull request.
#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/pulls/{number}/subscription",
    tag = "Notifications",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Subscribed", body = SubscriptionResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such pull request", body = serde_json::Value),
    ),
)]
pub async fn subscribe_pull(
    State(state): State<AppState>,
    Path((_, _, number)): Path<(String, String, i64)>,
    RepoAuthRead { repo, actor_id }: RepoAuthRead,
) -> impl IntoResponse {
    write_subscription(
        &state,
        &repo,
        actor_id,
        rg_core::notification::thread::SubjectKind::PullRequest,
        number,
        true,
    )
    .await
}

/// DELETE /api/v1/repos/:owner/:name/pulls/:number/subscription — stop following the pull request; taking part again does not undo it.
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/pulls/{number}/subscription",
    tag = "Notifications",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Unsubscribed", body = SubscriptionResponse),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such pull request", body = serde_json::Value),
    ),
)]
pub async fn unsubscribe_pull(
    State(state): State<AppState>,
    Path((_, _, number)): Path<(String, String, i64)>,
    RepoAuthRead { repo, actor_id }: RepoAuthRead,
) -> impl IntoResponse {
    write_subscription(
        &state,
        &repo,
        actor_id,
        rg_core::notification::thread::SubjectKind::PullRequest,
        number,
        false,
    )
    .await
}
