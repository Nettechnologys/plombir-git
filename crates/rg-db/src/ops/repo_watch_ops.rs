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

/// One page of a repository's watch rows in `state`, keyed on `id`.
///
/// Both halves of that sentence are deliberate, and both exist because the one
/// consumer is the notification fan-out:
///
/// - **Filtered in SQL.** `DELETE .../watch` is implemented as a write of
///   `not_watching` rather than a row delete, so an unwatched and an ignoring
///   user both keep a row. Paging over *every* row and discarding them in the
///   caller makes the page size a function of how many people once unwatched —
///   a repository whose subscribers have all left is a page of tombstones with
///   the actual recipients behind them.
/// - **Keyset, not offset.** `updated_at` moves whenever somebody toggles a
///   subscription, so an offset walk ordered by it silently skips or repeats
///   recipients mid-fan-out. `id` never moves.
///
/// `state` is the caller's policy rather than this layer's: the allowlist lives
/// in `rg_core::repo::service::WatchState`, and the delivery point applies it
/// (see `rg_core::notification`).
///
/// Pass `after_id = 0` for the first page, then the `id` of the last row
/// returned. A short page is the last one.
pub async fn list_in_state_after(
    db: &DatabaseConnection,
    repo_id: i64,
    state: &str,
    after_id: i64,
    limit: u64,
) -> Result<Vec<Model>> {
    RepoWatchEntity::find()
        .filter(repo_watch::Column::RepoId.eq(repo_id))
        .filter(repo_watch::Column::WatchState.eq(state))
        .filter(repo_watch::Column::Id.gt(after_id))
        .order_by_asc(repo_watch::Column::Id)
        .limit(limit)
        .all(db)
        .await
        .context("db: list watch rows in state")
}
