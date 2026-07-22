//! Notification service — create and manage user notifications.

use anyhow::Result;
use sea_orm::DatabaseConnection;

use rg_db::ops::notification_ops;

/// Create a notification for a user.
pub async fn notify(
    db: &DatabaseConnection,
    user_id: i64,
    event_type: &str,
    title: &str,
    body: Option<&str>,
    repo_id: Option<i64>,
) -> Result<rg_db::entities::notification::Model> {
    notification_ops::create_notification(db, user_id, event_type, title, body, repo_id).await
}

/// List notifications for a user.
pub async fn list_notifications(
    db: &DatabaseConnection,
    user_id: i64,
    unread_only: bool,
) -> Result<Vec<rg_db::entities::notification::Model>> {
    notification_ops::list_notifications(db, user_id, unread_only).await
}

/// Paginated list of notifications. Returns (data, total).
pub async fn list_notifications_paginated(
    db: &DatabaseConnection,
    user_id: i64,
    unread_only: bool,
    offset: u64,
    limit: u64,
) -> Result<(Vec<rg_db::entities::notification::Model>, i64)> {
    notification_ops::list_notifications_paginated(db, user_id, unread_only, offset, limit).await
}

/// Mark a notification as read.
pub async fn mark_read(db: &DatabaseConnection, id: i64) -> Result<()> {
    notification_ops::mark_notification_read(db, id).await
}

/// Mark a notification as read for its owning user.
pub async fn mark_read_for_user(db: &DatabaseConnection, id: i64, user_id: i64) -> Result<()> {
    notification_ops::mark_notification_read_for_user(db, id, user_id).await
}

/// Mark all notifications as read for a user.
pub async fn mark_all_read(db: &DatabaseConnection, user_id: i64) -> Result<u64> {
    notification_ops::mark_all_read(db, user_id).await
}

/// Get unread notification count.
pub async fn unread_count(db: &DatabaseConnection, user_id: i64) -> Result<u64> {
    notification_ops::unread_count(db, user_id).await
}

/// Delete a notification.
pub async fn delete_notification(db: &DatabaseConnection, id: i64) -> Result<()> {
    notification_ops::delete_notification(db, id).await
}

/// Delete a notification for its owning user.
pub async fn delete_notification_for_user(
    db: &DatabaseConnection,
    id: i64,
    user_id: i64,
) -> Result<()> {
    notification_ops::delete_notification_for_user(db, id, user_id).await
}

// ── Watch notification helpers ─────────────────────────────────────────

/// Notify all watchers of a repository about an event.
pub async fn notify_watchers(
    db: &DatabaseConnection,
    repo_id: i64,
    author_name: &str,
    title: &str,
    notification_type: &str,
    body: Option<String>,
) -> Result<()> {
    let watchers = rg_db::ops::repo_watch_ops::list_watchers(db, repo_id, 0, 1000)
        .await?
        .0;
    // Resolve author once outside the loop to avoid N+1 queries
    let author_opt = if author_name.is_empty() {
        None
    } else {
        rg_db::ops::user_ops::find_by_username(db, author_name)
            .await
            .ok()
            .flatten()
    };
    for watcher in watchers {
        // Don't notify the author themselves
        if let Some(ref author) = author_opt {
            if author.id == watcher.user_id {
                continue;
            }
        }
        if let Err(e) = notification_ops::create_notification(
            db,
            watcher.user_id,
            notification_type,
            title,
            body.as_deref(),
            Some(repo_id),
        )
        .await
        {
            tracing::warn!(
                "Failed to notify watcher {} about {notification_type}: {e}",
                watcher.user_id
            );
        }
    }
    Ok(())
}
