//! The `on: schedule` tick ledger.
//!
//! Every decision here is one statement, never a read followed by a write: the
//! scheduler of every replica runs the same pass at the same minute, and a
//! restart replays the pass it was interrupted in. "Is this tick still due?"
//! and "it is mine now" have to be the same `UPDATE`, or both replicas — or the
//! process before and after its restart — run it.

use anyhow::{Context, Result};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, TryInsertResult};

use crate::entities::ci_schedule_tick::{ActiveModel, Column, Entity, Model};

/// What [`claim_tick`] decided about one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickClaim {
    /// This caller advanced the ledger to the tick and owns its run.
    Claimed,
    /// The entry was seen for the first time: the ledger now starts at this
    /// tick, and nothing runs for it. A schedule pushed at 15:00 that says
    /// `0 3 * * *` waits for tomorrow's 03:00, as it does on GitHub — it does
    /// not run at once for a tick that passed before it existed.
    Recorded,
    /// Someone already holds this tick or a later one, or the tick is closer to
    /// the last run than the instance's minimum interval allows.
    NotDue,
}

/// Claim `tick_minute` for one cron entry, exactly once across every caller.
///
/// `min_interval_minutes` is the instance's floor on how often one entry may
/// run (`[ci] schedule_min_interval_minutes`): a tick closer than that to the
/// last claimed one is not due, so `* * * * *` runs every five minutes on a
/// default instance rather than every minute.
///
/// The first sight of an entry is an insert that never runs anything, and two
/// callers racing to make it both land on [`TickClaim::Recorded`] or
/// [`TickClaim::NotDue`] — the conflict clause makes the loser's insert a
/// no-op, and neither of them claims. Every later tick is a single
/// conditional `UPDATE`, and only the caller whose statement changed the row
/// owns the tick.
pub async fn claim_tick(
    db: &impl ConnectionTrait,
    repo_id: i64,
    workflow: &str,
    cron: &str,
    tick_minute: i64,
    min_interval_minutes: i64,
) -> Result<TickClaim> {
    let inserted = Entity::insert(ActiveModel {
        repo_id: Set(repo_id),
        workflow: Set(workflow.to_string()),
        cron: Set(cron.to_string()),
        last_tick_minute: Set(tick_minute),
        ..Default::default()
    })
    .on_conflict(
        OnConflict::columns([Column::RepoId, Column::Workflow, Column::Cron])
            // MySQL has no `DO NOTHING`; a no-op update of the key is its
            // spelling, and it reports the row as not inserted all the same.
            .do_nothing_on([Column::Id])
            .to_owned(),
    )
    .do_nothing()
    .exec(db)
    .await
    .context("db: record a CI schedule entry")?;
    match inserted {
        TryInsertResult::Inserted(_) => return Ok(TickClaim::Recorded),
        TryInsertResult::Conflicted => {}
        TryInsertResult::Empty => {
            anyhow::bail!("db: a CI schedule entry insert unexpectedly carried no values")
        }
    }

    let advanced = Entity::update_many()
        .col_expr(Column::LastTickMinute, Expr::value(tick_minute))
        .filter(Column::RepoId.eq(repo_id))
        .filter(Column::Workflow.eq(workflow))
        .filter(Column::Cron.eq(cron))
        .filter(Column::LastTickMinute.lt(tick_minute))
        .filter(Column::LastTickMinute.lte(tick_minute.saturating_sub(min_interval_minutes)))
        .exec(db)
        .await
        .context("db: claim a CI schedule tick")?;
    Ok(if advanced.rows_affected == 1 {
        TickClaim::Claimed
    } else {
        TickClaim::NotDue
    })
}

/// Drop the ledger rows of a repository's entries that its default branch no
/// longer declares.
///
/// A schedule removed and later added back must start over: left in place,
/// its old row would make the first pass after the re-add run a tick at once.
pub async fn forget_undeclared(
    db: &impl ConnectionTrait,
    repo_id: i64,
    declared: &[(String, String)],
) -> Result<u64> {
    let stale = list_for_repo(db, repo_id)
        .await?
        .into_iter()
        .filter(|row| {
            !declared
                .iter()
                .any(|(workflow, cron)| *workflow == row.workflow && *cron == row.cron)
        })
        .map(|row| row.id)
        .collect::<Vec<_>>();
    if stale.is_empty() {
        return Ok(0);
    }
    let deleted = Entity::delete_many()
        .filter(Column::Id.is_in(stale))
        .exec(db)
        .await
        .context("db: forget undeclared CI schedule entries")?;
    Ok(deleted.rows_affected)
}

/// One repository's ledger rows.
pub async fn list_for_repo(db: &impl ConnectionTrait, repo_id: i64) -> Result<Vec<Model>> {
    Entity::find()
        .filter(Column::RepoId.eq(repo_id))
        .all(db)
        .await
        .context("db: list CI schedule entries")
}
