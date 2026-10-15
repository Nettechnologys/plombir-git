//! Push mirror database operations.
//!
//! The rows are written from three places that do not know about each other —
//! the settings API, the post-push hooks and the background pass — so every
//! write that decides something is a single conditional `UPDATE` rather than a
//! read followed by a write.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::sea_query::{Expr, Func};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect,
};

use crate::entities::push_mirror::{ActiveModel, Column, Entity, Model};
use crate::entities::repository;

/// How long a pass may hold a mirror before another may take it over. A push
/// runs under the git gateway's own timeout, far below this; a lease this old
/// belongs to a process that died mid-pass.
pub const SYNC_LEASE_STALE_AFTER: chrono::Duration = chrono::Duration::hours(1);

/// The `next_sync_at` that makes a mirror due right away.
///
/// A second in the past, not `now`: MySQL keeps these columns to the second
/// and *rounds* the fraction, so `now` can be stored up to half a second in
/// the future — and the wake-up that follows a push then lists nothing and
/// the push waits for the next tick.
pub fn due_now() -> chrono::DateTime<Utc> {
    Utc::now() - chrono::Duration::seconds(1)
}

/// The most of a failure's text `last_sync_error` keeps. MySQL's `TEXT` holds
/// 64 KiB, and a push refused ref by ref can say more than that; an `UPDATE`
/// that does not fit fails, and the mirror would then show its previous state
/// instead of the failure. The head of the message is where git names the cause.
pub const MAX_SYNC_ERROR_BYTES: usize = 16 * 1024;

fn bounded_error(error: &str) -> String {
    if error.len() <= MAX_SYNC_ERROR_BYTES {
        return error.to_string();
    }
    let mut end = MAX_SYNC_ERROR_BYTES;
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated)", &error[..end])
}

pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.insert(db).await.context("db: create push mirror")
}

/// A repository's push mirrors, oldest first.
pub async fn list_for_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Model>> {
    Entity::find()
        .filter(Column::RepoId.eq(repo_id))
        .order_by_asc(Column::Id)
        .all(db)
        .await
        .context("db: list push mirrors")
}

pub async fn count_for_repo(db: &DatabaseConnection, repo_id: i64) -> Result<u64> {
    Entity::find()
        .filter(Column::RepoId.eq(repo_id))
        .count(db)
        .await
        .context("db: count push mirrors")
}

/// One push mirror, only if it belongs to `repo_id` — the id comes from a URL
/// under that repository, and another repository's row is not there.
pub async fn find_for_repo(
    db: &DatabaseConnection,
    repo_id: i64,
    id: i64,
) -> Result<Option<Model>> {
    Entity::find_by_id(id)
        .filter(Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: find push mirror")
}

pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.update(db).await.context("db: update push mirror")
}

/// Delete one push mirror of `repo_id`. `false` when there was none.
pub async fn delete_for_repo(db: &DatabaseConnection, repo_id: i64, id: i64) -> Result<bool> {
    let deleted = Entity::delete_many()
        .filter(Column::Id.eq(id))
        .filter(Column::RepoId.eq(repo_id))
        .exec(db)
        .await
        .context("db: delete push mirror")?;
    Ok(deleted.rows_affected > 0)
}

/// Record that `repo_id`'s refs moved: every enabled push mirror now has
/// something to send. Mirrors that sync on commit become due at once; the
/// others keep their interval. Returns how many mirrors were marked.
pub async fn mark_ref_moved(db: &DatabaseConnection, repo_id: i64) -> Result<u64> {
    let now = Utc::now();
    let marked = Entity::update_many()
        .col_expr(
            Column::PendingSince,
            Func::coalesce([Expr::col(Column::PendingSince).into(), Expr::value(now)]).into(),
        )
        .col_expr(
            Column::PendingRevision,
            Expr::col(Column::PendingRevision).add(1),
        )
        .col_expr(
            Column::NextSyncAt,
            Expr::case(
                Expr::col(Column::SyncOnCommit).eq(true),
                Expr::value(due_now()),
            )
            .finally(Expr::col(Column::NextSyncAt))
            .into(),
        )
        .filter(Column::RepoId.eq(repo_id))
        .filter(Column::Enabled.eq(true))
        .exec(db)
        .await
        .context("db: mark push mirrors behind")?;
    Ok(marked.rows_affected)
}

/// Make one push mirror due now ("Sync now"). `false` when it is not there or
/// switched off.
pub async fn request_sync(db: &DatabaseConnection, repo_id: i64, id: i64) -> Result<bool> {
    let requested = Entity::update_many()
        .col_expr(Column::NextSyncAt, Expr::value(due_now()))
        .filter(Column::Id.eq(id))
        .filter(Column::RepoId.eq(repo_id))
        .filter(Column::Enabled.eq(true))
        .exec(db)
        .await
        .context("db: request a push mirror sync")?;
    Ok(requested.rows_affected > 0)
}

/// Enabled push mirrors whose time has come, earliest first, of repositories
/// that are not deleted.
pub async fn list_due(db: &DatabaseConnection, limit: u64) -> Result<Vec<Model>> {
    Entity::find()
        .inner_join(repository::Entity)
        .filter(repository::Column::DeletedAt.is_null())
        .filter(Column::Enabled.eq(true))
        .filter(Column::NextSyncAt.lte(Utc::now()))
        .order_by_asc(Column::NextSyncAt)
        .limit(limit)
        .all(db)
        .await
        .context("db: list due push mirrors")
}

/// Take the lease for one pass. `false` when another pass holds a fresh one or
/// the mirror was switched off or deleted since it was listed.
pub async fn acquire_lease(db: &DatabaseConnection, id: i64, token: &str) -> Result<bool> {
    let now = Utc::now();
    let taken = Entity::update_many()
        .col_expr(Column::LeaseToken, Expr::value(token))
        .col_expr(Column::LeaseAcquiredAt, Expr::value(now))
        .filter(Column::Id.eq(id))
        .filter(Column::Enabled.eq(true))
        .filter(
            Column::LeaseToken
                .is_null()
                .or(Column::LeaseAcquiredAt.lt(now - SYNC_LEASE_STALE_AFTER)),
        )
        .exec(db)
        .await
        .context("db: take a push mirror lease")?;
    Ok(taken.rows_affected > 0)
}

/// A pass reached the remote. Clears `pending_since` only if no ref moved
/// since the pass read `started_revision`; a move during the pass keeps the
/// mirror behind and — when it syncs on commit — due again right away.
/// `next_sync_at` is the interval's next turn, `None` for a mirror without
/// an interval.
pub async fn record_success(
    db: &DatabaseConnection,
    id: i64,
    token: &str,
    started_revision: i64,
    next_sync_at: Option<chrono::DateTime<Utc>>,
) -> Result<bool> {
    let now = Utc::now();
    let caught_up = Expr::col(Column::PendingRevision).eq(started_revision);
    let recorded = Entity::update_many()
        .col_expr(Column::LastSyncAt, Expr::value(now))
        .col_expr(Column::LastSuccessAt, Expr::value(now))
        .col_expr(Column::LastSyncError, Expr::value(Option::<String>::None))
        .col_expr(
            Column::PendingSince,
            Expr::case(
                caught_up.clone(),
                Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .finally(Expr::col(Column::PendingSince))
            .into(),
        )
        .col_expr(
            Column::NextSyncAt,
            Expr::case(
                caught_up.or(Expr::col(Column::SyncOnCommit).eq(false)),
                Expr::value(next_sync_at),
            )
            .finally(Expr::value(due_now()))
            .into(),
        )
        .col_expr(Column::LeaseToken, Expr::value(Option::<String>::None))
        .col_expr(
            Column::LeaseAcquiredAt,
            Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .filter(Column::Id.eq(id))
        .filter(Column::LeaseToken.eq(token))
        .exec(db)
        .await
        .context("db: record a push mirror pass")?;
    Ok(recorded.rows_affected > 0)
}

/// A pass failed. The mirror stays behind and is retried at `retry_at`.
pub async fn record_failure(
    db: &DatabaseConnection,
    id: i64,
    token: &str,
    error: &str,
    retry_at: chrono::DateTime<Utc>,
) -> Result<bool> {
    let recorded = Entity::update_many()
        .col_expr(Column::LastSyncAt, Expr::value(Utc::now()))
        .col_expr(Column::LastSyncError, Expr::value(bounded_error(error)))
        .col_expr(Column::NextSyncAt, Expr::value(retry_at))
        .col_expr(Column::LeaseToken, Expr::value(Option::<String>::None))
        .col_expr(
            Column::LeaseAcquiredAt,
            Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .filter(Column::Id.eq(id))
        .filter(Column::LeaseToken.eq(token))
        .exec(db)
        .await
        .context("db: record a failed push mirror pass")?;
    Ok(recorded.rows_affected > 0)
}

/// What the metrics need: how many enabled mirrors failed their last pass, and
/// the oldest ref move an enabled mirror has not received yet.
pub async fn health(db: &DatabaseConnection) -> Result<(u64, Option<chrono::DateTime<Utc>>)> {
    let failing = Entity::find()
        .filter(Column::Enabled.eq(true))
        .filter(Column::LastSyncError.is_not_null())
        .count(db)
        .await
        .context("db: count failing push mirrors")?;
    let oldest = Entity::find()
        .filter(Column::Enabled.eq(true))
        .filter(Column::PendingSince.is_not_null())
        .order_by_asc(Column::PendingSince)
        .one(db)
        .await
        .context("db: oldest pending push mirror")?
        .and_then(|mirror| mirror.pending_since);
    Ok((failing, oldest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_error_is_cut_on_a_character_boundary() {
        let short = "refused";
        assert_eq!(bounded_error(short), short);
        let long = "ё".repeat(MAX_SYNC_ERROR_BYTES);
        let bounded = bounded_error(&long);
        assert!(bounded.len() <= MAX_SYNC_ERROR_BYTES + "… (truncated)".len());
        assert!(bounded.ends_with("… (truncated)"));
    }
}
