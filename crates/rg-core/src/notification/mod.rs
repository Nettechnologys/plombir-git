//! Notification service — create and manage user notifications.

use anyhow::Result;
use sea_orm::DatabaseConnection;

use rg_db::ops::notification_ops;

use crate::repo::service::WatchState;

pub mod mail;
pub mod thread;

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

/// Resolve a user needed only to enrich or route a notification.
///
/// A missing row is a legitimate absence: accounts can be deleted after an
/// event was committed. A failed query is different. The primary operation is
/// already complete, so it still must not be unwound, but the failure must be
/// visible with enough identity to explain the degraded notification.
pub(crate) async fn best_effort_user_by_id(
    db: &DatabaseConnection,
    user_id: i64,
    repo_id: i64,
    notification_type: &str,
    lookup_role: &str,
) -> Option<rg_db::entities::user::Model> {
    match rg_db::ops::user_ops::find_by_id(db, user_id).await {
        Ok(user) => user,
        Err(error) => {
            tracing::warn!(
                repo_id,
                user_id,
                notification_type,
                lookup_role,
                error = %format!("{error:#}"),
                "notification user lookup failed"
            );
            None
        }
    }
}

async fn best_effort_user_by_username(
    db: &DatabaseConnection,
    username: &str,
    repo_id: i64,
    notification_type: &str,
) -> Option<rg_db::entities::user::Model> {
    match rg_db::ops::user_ops::find_by_username(db, username).await {
        Ok(user) => user,
        Err(error) => {
            tracing::warn!(
                repo_id,
                username,
                notification_type,
                lookup_role = "author",
                error = %format!("{error:#}"),
                "notification user lookup failed"
            );
            None
        }
    }
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
    db: &impl crate::db::DbPools,
    tracker: &crate::task_tracker::TaskTracker,
    event: WatchEvent,
) {
    let db = db.owned();
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
/// Every watcher is re-checked against the rules of
/// [`crate::repo::service::can_read_repo`] — page by page, through
/// [`crate::repo::service::readers_among`] — before delivery. A watch row outlives the access that created it — the repo
/// can be flipped to private, a collaborator removed, or the row predate the
/// read gate on the subscribe endpoint — and the notification body carries the
/// repository's content (PR titles, branch and milestone names). Gating only
/// the subscribe endpoint would keep serving all three cases, so the check
/// lives here, at the single point every watch notification passes through.
/// A failed check drops the recipients rather than delivering to them.
///
/// The subscription state is filtered for the same reason, one layer down in
/// the query this pages through: see [`WATCH_STATE_SUBSCRIBED`].
///
/// Every subscriber is reached, however many there are. Each page of
/// [`WATCH_FANOUT_PAGE`] is one read check and one multi-row insert, so a
/// failed check or insert costs that page — logged with its size — and the
/// walk continues; only a failed *page query* aborts it, and the count reached
/// by then is logged rather than lost.
pub async fn notify_watchers(db: &impl crate::db::DbPools, event: &WatchEvent) -> Result<()> {
    let write = db.write();
    let db = db.read();
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
        best_effort_user_by_username(db, &event.author_name, repo_id, &event.notification_type)
            .await
    };

    let notification_type = event.notification_type.as_str();
    let mut considered = 0usize;
    let mut delivered = 0usize;
    loop {
        let page_len = page.len() as u64;
        let last_id = page.last().map_or(0, |watcher| watcher.id);

        // One read check and one insert per page, not per subscriber: the walk
        // used to autocommit an INSERT for every watcher, taking the write lock
        // once per recipient while every other writer on the instance waited.
        let candidates: Vec<i64> = page
            .iter()
            .map(|watcher| watcher.user_id)
            // Don't notify the author themselves
            .filter(|user_id| {
                author_opt
                    .as_ref()
                    .is_none_or(|author| author.id != *user_id)
            })
            .collect();
        considered += page.len();
        let readers = match crate::repo::service::readers_among(db, &repo, &candidates).await {
            Ok(readers) => readers,
            Err(e) => {
                // Fail closed: an unreadable permission answer must not become
                // a delivered notification. The page is skipped, not the walk.
                tracing::warn!(
                    repo_id,
                    notification_type,
                    skipped = candidates.len(),
                    "Skipping a page of watchers: read check failed: {e:#}"
                );
                Default::default()
            }
        };
        let recipients: Vec<i64> = candidates
            .into_iter()
            .filter(|user_id| {
                let readable = readers.contains(user_id);
                if !readable {
                    tracing::debug!(
                        "Skipping watcher {user_id} for {notification_type}: no read access to repo {repo_id}"
                    );
                }
                readable
            })
            .collect();
        match notification_ops::create_notifications(
            write,
            &recipients,
            notification_type,
            &event.title,
            event.body.as_deref(),
            Some(repo_id),
        )
        .await
        {
            Ok(written) => delivered += written as usize,
            Err(e) => tracing::warn!(
                repo_id,
                notification_type,
                undelivered = recipients.len(),
                "Failed to notify a page of watchers: {e:#}"
            ),
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

#[cfg(test)]
mod tests {
    use super::{best_effort_user_by_id, best_effort_user_by_username};
    use crate::test_support::{migrated_memory_database, CapturedLogs};

    #[tokio::test(flavor = "current_thread")]
    async fn failed_notification_user_lookups_warn_with_context_and_stay_best_effort() {
        let db = migrated_memory_database().await;
        db.clone().close().await.expect("close test database");
        let (logs, _guard) = CapturedLogs::capture();

        assert!(
            best_effort_user_by_id(&db, 41, 7, "pull_request", "actor")
                .await
                .is_none(),
            "a failed enrichment lookup must preserve best-effort None semantics"
        );
        assert!(
            best_effort_user_by_id(&db, 42, 8, "push", "actor")
                .await
                .is_none(),
            "a failed pusher lookup must preserve best-effort None semantics"
        );
        assert!(
            best_effort_user_by_id(&db, 43, 9, "ci_triggered", "recipient")
                .await
                .is_none(),
            "a failed recipient lookup must preserve best-effort None semantics"
        );
        assert!(
            best_effort_user_by_username(&db, "push-author", 10, "push")
                .await
                .is_none(),
            "a failed self-exclusion lookup must preserve best-effort None semantics"
        );

        let rendered = logs.rendered();
        for expected in [
            "notification user lookup failed",
            "repo_id=7",
            "repo_id=8",
            "repo_id=9",
            "repo_id=10",
            "user_id=41",
            "user_id=42",
            "user_id=43",
            "username=\"push-author\"",
            "notification_type=\"pull_request\"",
            "notification_type=\"push\"",
            "notification_type=\"ci_triggered\"",
            "lookup_role=\"actor\"",
            "lookup_role=\"recipient\"",
            "lookup_role=\"author\"",
            "db: find user by id",
            "db: find user by username",
        ] {
            assert!(
                rendered.contains(expected),
                "missing `{expected}` in {rendered}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn absent_notification_users_are_not_logged_as_backend_failures() {
        let db = migrated_memory_database().await;
        let (logs, _guard) = CapturedLogs::capture();

        assert!(
            best_effort_user_by_id(&db, i64::MAX, 7, "ci_triggered", "recipient")
                .await
                .is_none()
        );
        assert!(
            best_effort_user_by_username(&db, "missing-author", 7, "push")
                .await
                .is_none()
        );
        assert!(
            logs.rendered().is_empty(),
            "a genuine missing row must not be reported as a database failure"
        );
    }

    /// Statements one watch fan-out sends for a private repository whose
    /// `watchers` subscribers are all collaborators, and how many of them it
    /// notified.
    async fn fanout_cost(watchers: usize) -> (usize, usize) {
        use sea_orm::{ActiveValue::Set, EntityTrait, PaginatorTrait};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let mut db = migrated_memory_database().await;
        let owner = rg_db::ops::user_ops::create_user(&db, "fan-owner", "fan-owner@x.test", "", "")
            .await
            .expect("create owner");
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                owner_id: Set(owner.id),
                name: Set("fanned".to_string()),
                is_private: Set(true),
                default_branch: Set("main".to_string()),
                stars_count: Set(0),
                forks_count: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .expect("create repo");
        for index in 0..watchers {
            let user = rg_db::ops::user_ops::create_user(
                &db,
                &format!("fan-{index}"),
                &format!("fan-{index}@x.test"),
                "",
                "",
            )
            .await
            .expect("create watcher");
            rg_db::ops::repo_collaborator_ops::create(
                &db,
                rg_db::entities::repo_collaborator::ActiveModel {
                    repo_id: Set(repo.id),
                    user_id: Set(user.id),
                    permission: Set("read".to_string()),
                    created_at: Set(now),
                    ..Default::default()
                },
            )
            .await
            .expect("add collaborator");
            rg_db::ops::repo_watch_ops::set_watch_state(&db, user.id, repo.id, "watching")
                .await
                .expect("watch");
        }
        // One subscriber who may not read the repository any more.
        let outsider = rg_db::ops::user_ops::create_user(&db, "fan-out", "fan-out@x.test", "", "")
            .await
            .expect("create outsider");
        rg_db::ops::repo_watch_ops::set_watch_state(&db, outsider.id, repo.id, "watching")
            .await
            .expect("watch");

        let statements = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&statements);
        db.set_metric_callback(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        super::notify_watchers(
            &db,
            &super::WatchEvent {
                repo_id: repo.id,
                author_name: "fan-owner".to_string(),
                title: "pushed".to_string(),
                notification_type: "push".to_string(),
                body: None,
            },
        )
        .await
        .expect("fan out");
        let sent = statements.load(Ordering::SeqCst);
        let delivered = rg_db::entities::notification::Entity::find()
            .count(&db)
            .await
            .expect("count notifications") as usize;
        (sent, delivered)
    }

    /// card_f25c98fdf3ee: the fan-out used to check and insert one subscriber
    /// at a time — up to two reads and an autocommitted INSERT each, the write
    /// lock taken once per recipient. A page is now one read check and one
    /// insert, so the statement count does not grow with the audience, and the
    /// outsider who lost access is still left out.
    #[tokio::test]
    async fn a_fan_out_costs_the_same_statements_for_three_watchers_or_forty() {
        let (few, few_delivered) = fanout_cost(3).await;
        let (many, many_delivered) = fanout_cost(40).await;
        assert_eq!(few_delivered, 3);
        assert_eq!(many_delivered, 40);
        assert_eq!(
            few, many,
            "a fan-out to 3 watchers sent {few} statements, to 40 sent {many}"
        );
    }
}
