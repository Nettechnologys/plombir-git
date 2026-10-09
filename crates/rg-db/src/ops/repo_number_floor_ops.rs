//! The retired-number floor of a repository's issue and pull-request numbers.
//!
//! A deletion raises the floor *before* it removes the row, so a number is
//! spent even if the delete then fails; an allocator reads the floor next to
//! `max(number)` and takes the larger. See the migration
//! `m20261009_000004_repo_number_floors`.

use anyhow::{Context, Result};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::*;

use crate::entities::repo_number_floor::{self, Entity as FloorEntity};

/// One repository-local number sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberSpace {
    Issue,
    PullRequest,
}

impl NumberSpace {
    fn key(self) -> &'static str {
        match self {
            NumberSpace::Issue => "issue",
            NumberSpace::PullRequest => "pull_request",
        }
    }
}

/// The highest number a deletion has retired in `space` — `0` if none has.
pub async fn retired_up_to<C: ConnectionTrait>(
    db: &C,
    repo_id: i64,
    space: NumberSpace,
) -> Result<i64> {
    let row = FloorEntity::find_by_id((repo_id, space.key().to_string()))
        .one(db)
        .await
        .context("db: read retired number floor")?;
    Ok(row.map_or(0, |row| row.retired_up_to))
}

/// Record that `number` has been retired in `space`, so it is never handed out
/// again. Only ever raises the floor; concurrent retirements of different
/// numbers settle on the larger without a backend-specific `MAX` expression:
/// a conditional raise, an insert that yields to a row created meanwhile, and
/// the conditional raise once more against that row.
pub async fn retire<C: ConnectionTrait>(
    db: &C,
    repo_id: i64,
    space: NumberSpace,
    number: i64,
) -> Result<()> {
    if raise(db, repo_id, space, number).await? {
        return Ok(());
    }
    FloorEntity::insert(repo_number_floor::ActiveModel {
        repo_id: Set(repo_id),
        space: Set(space.key().to_string()),
        retired_up_to: Set(number),
    })
    .on_conflict(
        OnConflict::columns([
            repo_number_floor::Column::RepoId,
            repo_number_floor::Column::Space,
        ])
        // MySQL needs a harmless assignment for its DO NOTHING polyfill.
        .do_nothing_on([repo_number_floor::Column::RepoId])
        .to_owned(),
    )
    .do_nothing()
    .exec(db)
    .await
    .context("db: create retired number floor")?;
    raise(db, repo_id, space, number).await?;
    Ok(())
}

/// Raise an existing floor to `number` if it is lower. Returns whether the
/// floor now stands at or above `number`; `false` means there is no row yet.
async fn raise<C: ConnectionTrait>(
    db: &C,
    repo_id: i64,
    space: NumberSpace,
    number: i64,
) -> Result<bool> {
    let updated = FloorEntity::update_many()
        .col_expr(repo_number_floor::Column::RetiredUpTo, Expr::value(number))
        .filter(repo_number_floor::Column::RepoId.eq(repo_id))
        .filter(repo_number_floor::Column::Space.eq(space.key()))
        .filter(repo_number_floor::Column::RetiredUpTo.lt(number))
        .exec(db)
        .await
        .context("db: raise retired number floor")?;
    if updated.rows_affected > 0 {
        return Ok(true);
    }
    // Nothing raised: either there is no row yet, or it already stands at or
    // above `number` — in which case there is nothing left to do either.
    Ok(retired_up_to(db, repo_id, space).await? >= number)
}
