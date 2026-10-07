//! Database operations for LFS locks.

use anyhow::{Context, Result};
use sea_orm::prelude::DateTimeUtc;
use sea_orm::*;

use crate::entities::lfs_lock::{self, ActiveModel, Entity as LfsLockEntity, Model as LfsLock};

/// What an attempt to lock a path found.
#[derive(Clone, Debug, PartialEq)]
pub enum LockAttempt {
    /// The path was free and is now locked by the caller.
    Created(LfsLock),
    /// Somebody — possibly the caller — already holds the lock on the path.
    Held(LfsLock),
}

/// Lock `path` in `repo_id` for `owner_id`, or report the lock already on it.
///
/// The unique index on `(repo_id, path)` decides between two people locking the
/// same file at once: exactly one insert wins, and the other reads the winner
/// back. A winner unlocked again before that read leaves nothing to report, so
/// the attempt is made once more rather than answered with a lock that is gone.
pub async fn create(
    db: &DatabaseConnection,
    repo_id: i64,
    path: &str,
    ref_name: Option<&str>,
    owner_id: i64,
    locked_at: DateTimeUtc,
) -> Result<LockAttempt> {
    for _ in 0..3 {
        if let Some(held) = find_by_path(db, repo_id, path).await? {
            return Ok(LockAttempt::Held(held));
        }
        let model = ActiveModel {
            repo_id: Set(repo_id),
            path: Set(path.to_string()),
            ref_name: Set(ref_name.map(str::to_string)),
            owner_id: Set(owner_id),
            locked_at: Set(locked_at),
            ..Default::default()
        };
        match model.insert(db).await {
            Ok(lock) => return Ok(LockAttempt::Created(lock)),
            Err(error) if crate::is_unique_violation(&error) => continue,
            Err(error) => return Err(anyhow::Error::from(error).context("db: create LFS lock")),
        }
    }
    anyhow::bail!("db: the lock on an LFS path kept changing hands while it was being taken")
}

/// The lock on `path` in `repo_id`, if there is one.
pub async fn find_by_path(
    db: &DatabaseConnection,
    repo_id: i64,
    path: &str,
) -> Result<Option<LfsLock>> {
    LfsLockEntity::find()
        .filter(lfs_lock::Column::RepoId.eq(repo_id))
        .filter(lfs_lock::Column::Path.eq(path))
        .one(db)
        .await
        .context("db: find LFS lock by path")
}

/// The lock `id`, if it exists and belongs to `repo_id`.
pub async fn find_by_id(db: &DatabaseConnection, repo_id: i64, id: i64) -> Result<Option<LfsLock>> {
    LfsLockEntity::find_by_id(id)
        .filter(lfs_lock::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: find LFS lock by id")
}

/// Which locks a listing asks for.
#[derive(Clone, Debug, Default)]
pub struct LockQuery<'a> {
    pub path: Option<&'a str>,
    pub id: Option<i64>,
    /// Only locks with an id greater than this — the page cursor.
    pub after_id: Option<i64>,
}

/// Up to `limit` locks of `repo_id` matching `query`, oldest first, plus
/// whether another page follows.
pub async fn list(
    db: &DatabaseConnection,
    repo_id: i64,
    query: &LockQuery<'_>,
    limit: u64,
) -> Result<(Vec<LfsLock>, bool)> {
    let mut select = LfsLockEntity::find().filter(lfs_lock::Column::RepoId.eq(repo_id));
    if let Some(path) = query.path {
        select = select.filter(lfs_lock::Column::Path.eq(path));
    }
    if let Some(id) = query.id {
        select = select.filter(lfs_lock::Column::Id.eq(id));
    }
    if let Some(after) = query.after_id {
        select = select.filter(lfs_lock::Column::Id.gt(after));
    }
    let mut locks = select
        .order_by_asc(lfs_lock::Column::Id)
        .limit(limit + 1)
        .all(db)
        .await
        .context("db: list LFS locks")?;
    let more = locks.len() as u64 > limit;
    locks.truncate(limit as usize);
    Ok((locks, more))
}

/// Every lock in `repo_id` held by anyone but `holder` — all of them when there
/// is no holder (a deploy key cannot hold a lock).
///
/// Unpaged, for the push check: a lock it did not see would be a lock that
/// does not hold. A repository's locks are files people are editing by hand,
/// a set on the scale of a team's working copy rather than of its history.
pub async fn list_held_by_others(
    db: &DatabaseConnection,
    repo_id: i64,
    holder: Option<i64>,
) -> Result<Vec<LfsLock>> {
    let mut select = LfsLockEntity::find().filter(lfs_lock::Column::RepoId.eq(repo_id));
    if let Some(holder) = holder {
        select = select.filter(lfs_lock::Column::OwnerId.ne(holder));
    }
    select
        .order_by_asc(lfs_lock::Column::Id)
        .all(db)
        .await
        .context("db: list the LFS locks other people hold")
}

/// Remove lock `id` of `repo_id` — when `owner_id` is given, only while that
/// user still holds it. Returns whether a row went.
///
/// The ownership condition sits in the `DELETE` itself, so a lock that changed
/// hands between the caller's read and this statement is left alone rather than
/// removed on the strength of a stale answer.
pub async fn delete(
    db: &DatabaseConnection,
    repo_id: i64,
    id: i64,
    owner_id: Option<i64>,
) -> Result<bool> {
    let mut delete = LfsLockEntity::delete_many()
        .filter(lfs_lock::Column::Id.eq(id))
        .filter(lfs_lock::Column::RepoId.eq(repo_id));
    if let Some(owner) = owner_id {
        delete = delete.filter(lfs_lock::Column::OwnerId.eq(owner));
    }
    let result = delete.exec(db).await.context("db: delete LFS lock")?;
    Ok(result.rows_affected > 0)
}
