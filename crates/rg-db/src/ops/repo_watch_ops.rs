//! Database operations for repository watches.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::repo_watch::{self, ActiveModel, Entity as RepoWatchEntity, Model};

/// Set watch state for a repo (upsert).
/// Returns the new watch_state.
///
/// Raw write: `state` is stored verbatim. The allowlist lives one layer up in
/// `rg_core::repo::service::WatchState` — go through
/// `rg_core::repo::service::set_watch` for anything carrying a client-supplied
/// value.
pub async fn set_watch_state(
    db: &DatabaseConnection,
    user_id: i64,
    repo_id: i64,
    state: &str,
) -> Result<String> {
    // Check if watch record exists
    let existing = RepoWatchEntity::find()
        .filter(repo_watch::Column::UserId.eq(user_id))
        .filter(repo_watch::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: check existing watch")?;

    let now = chrono::Utc::now();

    if let Some(existing) = existing {
        // Update existing record
        let mut model: ActiveModel = existing.into();
        model.watch_state = Set(state.to_string());
        model.updated_at = Set(now);
        model.update(db).await.context("db: update watch")?;
    } else {
        // Insert new record
        let model = ActiveModel {
            user_id: Set(user_id),
            repo_id: Set(repo_id),
            watch_state: Set(state.to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        model.insert(db).await.context("db: insert watch")?;
    }

    Ok(state.to_string())
}

/// Get watch state for a user and repo.
pub async fn get_watch_state(
    db: &DatabaseConnection,
    user_id: i64,
    repo_id: i64,
) -> Result<Option<String>> {
    let result = RepoWatchEntity::find()
        .filter(repo_watch::Column::UserId.eq(user_id))
        .filter(repo_watch::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: get watch state")?;

    Ok(result.map(|r| r.watch_state))
}

/// List the watch *rows* of a repo with pagination — subscriptions, not
/// subscribers.
///
/// Deliberately unfiltered: `DELETE .../watch` is implemented as a write of
/// `not_watching` rather than a row delete, so an unwatched and an ignoring
/// user both keep a row here. This used to be called `list_watchers`, which
/// read as "the people subscribed to this repo" and is exactly what it is not —
/// its one caller had to re-filter by `watch_state` to avoid notifying people
/// who had unsubscribed. Any consumer wanting subscribers (a watcher count, a
/// watchers endpoint) must filter by
/// `rg_core::repo::service::WatchState::Watching`.
pub async fn list_watch_rows(
    db: &DatabaseConnection,
    repo_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Model>, i64)> {
    let base = RepoWatchEntity::find()
        .filter(repo_watch::Column::RepoId.eq(repo_id))
        .order_by_desc(repo_watch::Column::UpdatedAt);

    let total = base
        .clone()
        .count(db)
        .await
        .context("db: count watch rows")? as i64;

    let rows = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list watch rows")?;

    Ok((rows, total))
}
