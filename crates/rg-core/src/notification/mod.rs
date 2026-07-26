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
///
/// A notification belonging to somebody else is reported as absent on purpose —
/// confirming that id #4711 exists would leak the shape of another user's
/// inbox. What must *not* be reported as absent is a failed query, which is why
/// the ops layer answers with a bool and the marker is built here.
pub async fn mark_read_for_user(db: &DatabaseConnection, id: i64, user_id: i64) -> Result<()> {
    notification_ops::mark_notification_read_for_user(db, id, user_id)
        .await?
        .then_some(())
        .ok_or_else(|| crate::error::not_found("notification"))
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
///
/// Same masking as [`mark_read_for_user`]: another user's notification is
/// "not found", a broken query is not.
pub async fn delete_notification_for_user(
    db: &DatabaseConnection,
    id: i64,
    user_id: i64,
) -> Result<()> {
    notification_ops::delete_notification_for_user(db, id, user_id)
        .await?
        .then_some(())
        .ok_or_else(|| crate::error::not_found("notification"))
}

// ── Watch notification helpers ─────────────────────────────────────────

/// Notify all watchers of a repository about an event.
///
/// Every watcher is re-checked against [`crate::repo::service::can_read_repo`]
/// before delivery. A watch row outlives the access that created it — the repo
/// can be flipped to private, a collaborator removed, or the row predate the
/// read gate on the subscribe endpoint — and the notification body carries the
/// repository's content (PR titles, branch and milestone names). Gating only
/// the subscribe endpoint would keep serving all three cases, so the check
/// lives here, at the single point every watch notification passes through.
/// A failed check drops the recipient rather than delivering to them.
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
    if watchers.is_empty() {
        return Ok(());
    }
    // A repo that is gone (or soft-deleted) has nobody left to notify. Loading
    // it here also gives `can_read_repo` the model it needs, which short-circuits
    // to `true` for a public repo without touching the DB again.
    let Some(repo) = rg_db::ops::repo_ops::find_by_id(db, repo_id).await? else {
        return Ok(());
    };
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
        match crate::repo::service::can_read_repo(db, &repo, Some(watcher.user_id)).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(
                    "Skipping watcher {} for {notification_type}: no read access to repo {repo_id}",
                    watcher.user_id
                );
                continue;
            }
            Err(e) => {
                // Fail closed: an unreadable permission answer must not become
                // a delivered notification.
                tracing::warn!(
                    "Skipping watcher {} for {notification_type}: read check failed: {e}",
                    watcher.user_id
                );
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
