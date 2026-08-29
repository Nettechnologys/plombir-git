//! Database operations for repository watches.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::repo_watch::{self, ActiveModel, Entity as RepoWatchEntity, Model};

/// Set watch state for a repo (upsert).
/// Returns the new watch state, or `None` when a repository/user cascade wins
/// after this call observed an existing watch row.
///
/// Raw write: `state` is stored verbatim. The allowlist lives one layer up in
/// `rg_core::repo::service::WatchState` — go through
/// `rg_core::repo::service::set_watch` for anything carrying a client-supplied
/// value.
///
/// `(user_id, repo_id)` is UNIQUE (`idx_repo_watches_user_repo_unique`), and
/// the read below is a separate statement from the insert that follows it. A
/// double-clicked watch button, or two tabs of the same account, both read "no
/// row" and both insert; one meets the constraint. That loss says the watch row
/// this call wanted now exists, so it is resolved by re-reading it and writing
/// this call's state onto it — the last click wins, which is what the user who
/// clicked it expects.
///
/// Only a UNIQUE violation is treated this way. A foreign key failure (the user
/// or repository is gone) or a broken connection stays an error: the watch was
/// not recorded, and returning the requested state anyway would leave the UI
/// showing a subscription the database does not have.
///
/// An existing row that disappears is not retried as an insert: its repository
/// or user may have been deleted, and state must not be recreated after that
/// authoritative parent transition.
pub async fn set_watch_state(
    db: &DatabaseConnection,
    user_id: i64,
    repo_id: i64,
    state: &str,
) -> Result<Option<String>> {
    let now = chrono::Utc::now();

    if let Some(existing) = find_watch(db, user_id, repo_id).await? {
        return Ok(apply_state(db, existing, state, now)
            .await?
            .map(|_| state.to_string()));
    }

    let insert = ActiveModel {
        user_id: Set(user_id),
        repo_id: Set(repo_id),
        watch_state: Set(state.to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await;

    match insert {
        Ok(_) => Ok(Some(state.to_string())),
        Err(error) if crate::is_unique_violation(&error) => {
            match find_watch(db, user_id, repo_id).await? {
                Some(existing) => Ok(apply_state(db, existing, state, now)
                    .await?
                    .map(|_| state.to_string())),
                // Not there after all, so the collision was on some other
                // constraint. Report the original failure.
                None => Err(error).context("db: insert watch"),
            }
        }
        Err(error) => Err(error).context("db: insert watch"),
    }
}

/// Read the one watch row identified by the unique key.
async fn find_watch(db: &DatabaseConnection, user_id: i64, repo_id: i64) -> Result<Option<Model>> {
    RepoWatchEntity::find()
        .filter(repo_watch::Column::UserId.eq(user_id))
        .filter(repo_watch::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: check existing watch")
}

/// Write this call's watch state onto an existing row.
///
/// The stable row identity keeps a concurrent replacement distinct. A parent
/// cascade after the read returns `None`, never `RecordNotUpdated`.
pub async fn apply_state(
    db: &DatabaseConnection,
    existing: Model,
    state: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Model>> {
    let id = existing.id;
    let user_id = existing.user_id;
    let repo_id = existing.repo_id;
    let updated = RepoWatchEntity::update_many()
        .col_expr(
            repo_watch::Column::WatchState,
            Expr::value(state.to_string()),
        )
        .col_expr(repo_watch::Column::UpdatedAt, Expr::value(now))
        .filter(repo_watch::Column::Id.eq(id))
        .filter(repo_watch::Column::UserId.eq(user_id))
        .filter(repo_watch::Column::RepoId.eq(repo_id))
        .exec(db)
        .await
        .context("db: update watch")?;
    match updated.rows_affected {
        0 | 1 => {}
        rows => anyhow::bail!(
            "db: watch update affected {rows} rows for id {id}, user {user_id}, repo {repo_id}"
        ),
    }

    // MySQL reports zero changed rows for an unchanged state. Re-read the
    // exact observed identity so a no-op remains success while a repository or
    // user cascade is authoritative absence rather than RecordNotUpdated.
    RepoWatchEntity::find()
        .filter(repo_watch::Column::Id.eq(id))
        .filter(repo_watch::Column::UserId.eq(user_id))
        .filter(repo_watch::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: find updated watch")
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
