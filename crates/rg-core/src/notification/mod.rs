//! Notification service — create and manage user notifications.

use anyhow::Result;
use sea_orm::DatabaseConnection;

use rg_db::ops::notification_ops;

use crate::repo::service::WatchState;

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

/// Mark a notification as read for its owning user.
///
/// A notification belonging to somebody else is reported as absent on purpose —
/// confirming that id #4711 exists would leak the shape of another user's
/// inbox. What must *not* be reported as absent is a failed query, which is why
/// the ops layer answers with a bool and the marker is built here.
///
/// There is deliberately no `mark_read(db, id)` next to this one. There was:
/// unscoped, `pub`, and — after the handler was moved onto the scoped variant —
/// called by nobody. A notification id is an instance-wide primary key, so the
/// unscoped twin was one forgotten suffix away from marking another user's row,
/// and its being dead made that hazard free rather than harmless. The same
/// applies to [`delete_notification_for_user`] below.
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

/// The one [`WatchState`] that means "send me things".
///
/// The subscribe endpoint accepts three states and `DELETE .../watch` is
/// implemented as `set_watch(.., "not_watching")` rather than a row delete, so
/// the row survives an unwatch. The `repo_watches` table therefore holds
/// subscriptions, not subscribers, and the fan-out selects on this state rather
/// than on `!= "not_watching"`.
const WATCH_STATE_SUBSCRIBED: WatchState = WatchState::Watching;

/// How many subscriptions one fan-out query pulls at a time.
///
/// A page size, not a ceiling: [`notify_watchers`] keeps paging until the table
/// is exhausted. It used to be a bare `limit = 1000` with no loop, which capped
/// delivery silently — watcher #1001 was subscribed by every observable measure
/// and simply never heard anything.
const WATCH_FANOUT_PAGE: u64 = 500;

/// One repository event, in the owned form a detached fan-out task takes.
pub struct WatchEvent {
    pub repo_id: i64,
    /// Username of the account that caused the event, empty when there isn't
    /// one (an unauthenticated push, an auto-merge). Used to keep them off
    /// their own recipient list, so an empty name excludes nobody.
    pub author_name: String,
    pub title: String,
    /// `notification.event_type` — `push`, `pull_request`, `milestone`.
    pub notification_type: String,
    pub body: Option<String>,
}

/// Fan `event` out to the repository's watchers on `tracker`, returning as soon
/// as the task is queued.
///
/// This is what a request path calls. The fan-out is `O(subscribers)` database
/// round-trips — a read check and an insert each — so awaiting it inline made
/// opening a pull request on a popular repository cost the client the whole
/// walk. Tracked rather than a bare `tokio::spawn` so a SIGTERM in the next few
/// seconds drains the deliveries instead of severing them (card_3b4275a366ab).
///
/// A caller already off the request path (the post-push hooks, which are
/// themselves detached) can just await [`notify_watchers`] instead.
pub fn spawn_notify_watchers(
    db: &DatabaseConnection,
    tracker: &crate::task_tracker::TaskTracker,
    event: WatchEvent,
) {
    let db = db.clone();
    tracker.spawn(async move {
        if let Err(e) = notify_watchers(&db, &event).await {
            tracing::warn!(
                repo_id = event.repo_id,
                notification_type = %event.notification_type,
                error = %format!("{e:#}"),
                "watch fan-out failed"
            );
        }
    });
}

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
///
/// The subscription state is filtered for the same reason, one layer down in
/// the query this pages through: see [`WATCH_STATE_SUBSCRIBED`].
///
/// Every subscriber is reached, however many there are. A per-recipient failure
/// is logged and the walk continues; only a failed *query* aborts it, and the
/// count reached by then is logged rather than lost.
pub async fn notify_watchers(db: &DatabaseConnection, event: &WatchEvent) -> Result<()> {
    let repo_id = event.repo_id;
    let mut page = watch_page(db, repo_id, 0).await?;
    if page.is_empty() {
        return Ok(());
    }
    // A repo that is gone (or soft-deleted) has nobody left to notify. Loading
    // it here also gives `can_read_repo` the model it needs, which short-circuits
    // to `true` for a public repo without touching the DB again.
    let Some(repo) = rg_db::ops::repo_ops::find_by_id(db, repo_id).await? else {
        return Ok(());
    };
    // Resolve author once outside the loop to avoid N+1 queries
    let author_opt = if event.author_name.is_empty() {
        None
    } else {
        rg_db::ops::user_ops::find_by_username(db, &event.author_name)
            .await
            .ok()
            .flatten()
    };

    let notification_type = event.notification_type.as_str();
    let mut considered = 0usize;
    let mut delivered = 0usize;
    loop {
        let page_len = page.len() as u64;
        let last_id = page.last().map_or(0, |watcher| watcher.id);

        for watcher in page {
            considered += 1;
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
            match notification_ops::create_notification(
                db,
                watcher.user_id,
                notification_type,
                &event.title,
                event.body.as_deref(),
                Some(repo_id),
            )
            .await
            {
                Ok(_) => delivered += 1,
                Err(e) => tracing::warn!(
                    "Failed to notify watcher {} about {notification_type}: {e}",
                    watcher.user_id
                ),
            }
        }

        // A short page is the last one — no extra query to discover the end.
        if page_len < WATCH_FANOUT_PAGE {
            break;
        }
        page = match watch_page(db, repo_id, last_id).await {
            Ok(page) => page,
            Err(e) => {
                // Say how far the walk got: the subscribers past this point are
                // the ones who will wonder why they heard nothing.
                tracing::warn!(
                    repo_id,
                    notification_type,
                    considered,
                    delivered,
                    "watch fan-out aborted mid-walk"
                );
                return Err(e);
            }
        };
        if page.is_empty() {
            break;
        }
    }

    tracing::debug!(
        repo_id,
        notification_type,
        considered,
        delivered,
        "watch fan-out complete"
    );
    Ok(())
}

/// One page of subscribed watch rows after `after_id` (0 for the first page).
async fn watch_page(
    db: &DatabaseConnection,
    repo_id: i64,
    after_id: i64,
) -> Result<Vec<rg_db::entities::repo_watch::Model>> {
    rg_db::ops::repo_watch_ops::list_in_state_after(
        db,
        repo_id,
        WATCH_STATE_SUBSCRIBED.as_str(),
        after_id,
        WATCH_FANOUT_PAGE,
    )
    .await
}
