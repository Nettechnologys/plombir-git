//! Database operations for notifications.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::notification;

/// Create a notification.
pub async fn create_notification(
    db: &DatabaseConnection,
    user_id: i64,
    event_type: &str,
    title: &str,
    body: Option<&str>,
    repo_id: Option<i64>,
) -> Result<notification::Model> {
    let model = notification::ActiveModel {
        user_id: Set(user_id),
        event_type: Set(event_type.to_string()),
        title: Set(title.to_string()),
        body: Set(body.map(|s| s.to_string())),
        repo_id: Set(repo_id),
        is_read: Set(false),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    };
    model.insert(db).await.context("db: create notification")
}

/// Paginated list of notifications for a user. Returns (data, total).
///
/// Ordered by `created_at` **and** `id`: the timestamp alone leaves ties for
/// the engine to resolve however it scans, and a `LIMIT/OFFSET` walk over an
/// order that may change between two requests hands the same notification out
/// twice while the one beside it is never delivered.
pub async fn list_notifications_paginated(
    db: &DatabaseConnection,
    user_id: i64,
    unread_only: bool,
    offset: u64,
    limit: u64,
) -> Result<(Vec<notification::Model>, i64)> {
    let query = page_query(user_id, unread_only);

    let total = query
        .clone()
        .count(db)
        .await
        .context("db: count notifications")? as i64;
    let notifications = query
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list notifications (paginated)")?;

    Ok((notifications, total))
}

/// The ordered selection [`list_notifications_paginated`] cuts a page from,
/// kept apart so `query_plan_tests` explains the statement the server sends.
pub(crate) fn page_query(user_id: i64, unread_only: bool) -> Select<notification::Entity> {
    let mut base = notification::Entity::find().filter(notification::Column::UserId.eq(user_id));
    if unread_only {
        base = base.filter(notification::Column::IsRead.eq(false));
    }
    base.order_by_desc(notification::Column::CreatedAt)
        .order_by_desc(notification::Column::Id)
}

/// Mark a notification as read for its owning user.
///
/// Returns `false` when this user has no notification with that id — the row
/// belongs to somebody else, or never existed. That distinction is the caller's
/// to name: `rg-db` sits below `rg-core`, so it cannot build the
/// `rg_core::error::NotFound` marker the HTTP layer classifies on, and an
/// `anyhow!("… not found")` here would be indistinguishable from a failed query
/// by the time it reached a handler.
pub async fn mark_notification_read_for_user(
    db: &DatabaseConnection,
    id: i64,
    user_id: i64,
) -> Result<bool> {
    let result = notification::Entity::update_many()
        .col_expr(notification::Column::IsRead, Expr::value(true))
        .filter(notification::Column::Id.eq(id))
        .filter(notification::Column::UserId.eq(user_id))
        .exec(db)
        .await
        .context("db: mark notification read")?;
    match result.rows_affected {
        1 => Ok(true),
        // MySQL reports zero changed rows when the notification was already
        // read. Re-read the same scoped identity so that no-op and a winning
        // DELETE remain distinct without relying on backend settings.
        0 => Ok(notification::Entity::find()
            .filter(notification::Column::Id.eq(id))
            .filter(notification::Column::UserId.eq(user_id))
            .one(db)
            .await
            .context("db: find notification after no-op read update")?
            .is_some()),
        rows => anyhow::bail!(
            "db: notification read update affected {rows} rows for id {id} and user {user_id}"
        ),
    }
}

/// Mark all notifications as read for a user.
pub async fn mark_all_read(db: &DatabaseConnection, user_id: i64) -> Result<u64> {
    let result = notification::Entity::update_many()
        .col_expr(notification::Column::IsRead, Expr::value(true))
        .filter(notification::Column::UserId.eq(user_id))
        .filter(notification::Column::IsRead.eq(false))
        .exec(db)
        .await
        .context("db: mark all notifications read")?;

    Ok(result.rows_affected)
}

/// Get unread notification count for a user.
pub async fn unread_count(db: &DatabaseConnection, user_id: i64) -> Result<u64> {
    let count = notification::Entity::find()
        .filter(notification::Column::UserId.eq(user_id))
        .filter(notification::Column::IsRead.eq(false))
        .count(db)
        .await
        .context("db: unread notification count")?;

    Ok(count)
}

/// Delete a notification for its owning user.
/// Returns `false` when this user has no notification with that id — see
/// [`mark_notification_read_for_user`] for why the absence is reported as a
/// value rather than as an error.
pub async fn delete_notification_for_user(
    db: &DatabaseConnection,
    id: i64,
    user_id: i64,
) -> Result<bool> {
    let Some(model) = notification::Entity::find()
        .filter(notification::Column::Id.eq(id))
        .filter(notification::Column::UserId.eq(user_id))
        .one(db)
        .await
        .context("db: find notification for user delete")?
    else {
        return Ok(false);
    };

    model.delete(db).await.context("db: delete notification")?;
    Ok(true)
}

// ── Thread notifications (card_349c2b6a0d7c) ─────────────────────────────

/// What one recipient is told about an issue or a pull request.
pub struct ThreadNotification<'a> {
    pub user_id: i64,
    pub repo_id: i64,
    pub subject_type: &'a str,
    pub subject_id: i64,
    pub reason: &'a str,
    pub title: &'a str,
    pub body: Option<&'a str>,
    pub link: &'a str,
    /// Owe the recipient a mail for this row.
    pub email: bool,
}

/// The recipient's unread notification about this subject, if there is one.
pub async fn find_unread_for_subject(
    db: &DatabaseConnection,
    user_id: i64,
    subject_type: &str,
    subject_id: i64,
) -> Result<Option<notification::Model>> {
    notification::Entity::find()
        .filter(notification::Column::UserId.eq(user_id))
        .filter(notification::Column::SubjectType.eq(subject_type))
        .filter(notification::Column::SubjectId.eq(subject_id))
        .filter(notification::Column::IsRead.eq(false))
        .order_by_desc(notification::Column::Id)
        .one(db)
        .await
        .context("db: find unread notification for subject")
}

/// Insert a thread notification.
pub async fn create_for_subject(
    db: &DatabaseConnection,
    note: &ThreadNotification<'_>,
) -> Result<notification::Model> {
    let now = chrono::Utc::now();
    notification::ActiveModel {
        user_id: Set(note.user_id),
        event_type: Set(note.subject_type.to_string()),
        title: Set(note.title.to_string()),
        body: Set(note.body.map(str::to_string)),
        repo_id: Set(Some(note.repo_id)),
        is_read: Set(false),
        created_at: Set(now),
        reason: Set(Some(note.reason.to_string())),
        subject_type: Set(Some(note.subject_type.to_string())),
        subject_id: Set(Some(note.subject_id)),
        link: Set(Some(note.link.to_string())),
        updated_at: Set(Some(now)),
        email_pending: Set(note.email),
        ..Default::default()
    }
    .insert(db)
    .await
    .context("db: create thread notification")
}

/// Fold a later event into an unread row: it moves to the top of the inbox
/// with the newest title and body. `reason` replaces the stored one only when
/// given — the caller keeps the stronger of the two. A pending mail stays
/// pending; one more is owed only when `email` says so.
pub async fn fold_into(
    db: &DatabaseConnection,
    id: i64,
    note: &ThreadNotification<'_>,
    reason: Option<&str>,
) -> Result<bool> {
    let now = chrono::Utc::now();
    let mut update = notification::Entity::update_many()
        .col_expr(notification::Column::Title, Expr::value(note.title))
        .col_expr(
            notification::Column::Body,
            Expr::value(note.body.map(str::to_string)),
        )
        .col_expr(notification::Column::Link, Expr::value(note.link))
        .col_expr(notification::Column::CreatedAt, Expr::value(now))
        .col_expr(notification::Column::UpdatedAt, Expr::value(now));
    if let Some(reason) = reason {
        update = update.col_expr(notification::Column::Reason, Expr::value(reason));
    }
    if note.email {
        update = update.col_expr(notification::Column::EmailPending, Expr::value(true));
    }
    let result = update
        .filter(notification::Column::Id.eq(id))
        .filter(notification::Column::IsRead.eq(false))
        .exec(db)
        .await
        .context("db: fold event into notification")?;
    Ok(result.rows_affected > 0)
}

/// Rows the mail dispatcher still owes a message for, grouped by recipient.
pub async fn list_email_pending(
    db: &DatabaseConnection,
    limit: u64,
) -> Result<Vec<notification::Model>> {
    notification::Entity::find()
        .filter(notification::Column::EmailPending.eq(true))
        .order_by_asc(notification::Column::UserId)
        .order_by_asc(notification::Column::Id)
        .limit(limit)
        .all(db)
        .await
        .context("db: list notifications owed a mail")
}

/// The dispatcher has dealt with these rows.
pub async fn clear_email_pending(db: &DatabaseConnection, ids: &[i64]) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let result = notification::Entity::update_many()
        .col_expr(notification::Column::EmailPending, Expr::value(false))
        .filter(notification::Column::Id.is_in(ids.iter().copied()))
        .exec(db)
        .await
        .context("db: clear pending notification mail")?;
    Ok(result.rows_affected)
}

/// Remove every notification about a subject — the issue is gone.
pub async fn delete_for_subject(
    db: &DatabaseConnection,
    subject_type: &str,
    subject_id: i64,
) -> Result<u64> {
    let result = notification::Entity::delete_many()
        .filter(notification::Column::SubjectType.eq(subject_type))
        .filter(notification::Column::SubjectId.eq(subject_id))
        .exec(db)
        .await
        .context("db: delete notifications for subject")?;
    Ok(result.rows_affected)
}
