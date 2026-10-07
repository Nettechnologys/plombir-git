//! Database operations for LFS objects.

use anyhow::{Context, Result};
use sea_orm::prelude::DateTimeUtc;
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::lfs_object::{self, ActiveModel, Entity as LfsEntity, Model as LfsObject};

/// Find an LFS object by (repo_id, oid).
pub async fn find_by_repo_and_oid(
    db: &DatabaseConnection,
    repo_id: i64,
    oid: &str,
) -> Result<Option<LfsObject>> {
    LfsEntity::find()
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .filter(lfs_object::Column::Oid.eq(oid))
        .one(db)
        .await
        .context("db: find LFS object by repo and oid")
}

/// Create a new LFS object record.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<LfsObject> {
    model.insert(db).await.context("db: create LFS object")
}

/// Every object of `repo_id` whose bytes have been stored, oldest first.
pub async fn list_uploaded_by_repo(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<Vec<LfsObject>> {
    LfsEntity::find()
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .filter(lfs_object::Column::Uploaded.eq(true))
        .order_by_asc(lfs_object::Column::Id)
        .all(db)
        .await
        .context("db: list uploaded LFS objects of a repository")
}

/// Rows per `INSERT` in [`create_many`]: seven bound columns each stays well
/// under SQLite's and PostgreSQL's limits on parameters per statement.
const CREATE_MANY_CHUNK: usize = 500;

/// Record `models` in one transaction, so a repository ends up with all of
/// them or none.
pub async fn create_many(db: &DatabaseConnection, models: Vec<ActiveModel>) -> Result<()> {
    if models.is_empty() {
        return Ok(());
    }
    let txn = db.begin().await.context("db: begin LFS object batch")?;
    let mut models = models.into_iter().peekable();
    while models.peek().is_some() {
        let chunk: Vec<ActiveModel> = models.by_ref().take(CREATE_MANY_CHUNK).collect();
        LfsEntity::insert_many(chunk)
            .exec(&txn)
            .await
            .context("db: insert LFS object batch")?;
    }
    txn.commit().await.context("db: commit LFS object batch")
}

/// Outcome of a bid for an object's publication lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationLeaseBid {
    /// Nobody held the lease; it is now held by the bidding token.
    Granted,
    /// The previous holder's lease had expired and was taken over. Expiry is a
    /// guess about a dead process, not a fact — the old holder may still be
    /// running, so a taker cannot claim exclusivity over the stored bytes.
    TakenOver,
    /// Another request holds a live lease.
    Busy,
}

/// Bid for the exclusive right to publish one LFS object's blob.
///
/// The blob key is content-addressed and stable, so the storage layer cannot
/// tell two concurrent first publications apart. This row is the arbiter
/// instead: exactly one bidder wins each round, and it wins across processes
/// because the winner is decided by a single conditional `UPDATE`.
pub async fn bid_for_publication_lease(
    db: &DatabaseConnection,
    id: i64,
    token: &str,
    stale_before: DateTimeUtc,
) -> Result<PublicationLeaseBid> {
    let now = chrono::Utc::now();

    let granted = LfsEntity::update_many()
        .col_expr(lfs_object::Column::PublisherToken, Expr::value(token))
        .col_expr(lfs_object::Column::PublisherSince, Expr::value(now))
        .filter(lfs_object::Column::Id.eq(id))
        .filter(lfs_object::Column::PublisherToken.is_null())
        .exec(db)
        .await
        .context("db: take LFS publication lease")?;
    if granted.rows_affected > 0 {
        return Ok(PublicationLeaseBid::Granted);
    }

    let taken_over = LfsEntity::update_many()
        .col_expr(lfs_object::Column::PublisherToken, Expr::value(token))
        .col_expr(lfs_object::Column::PublisherSince, Expr::value(now))
        .filter(lfs_object::Column::Id.eq(id))
        .filter(lfs_object::Column::PublisherToken.is_not_null())
        .filter(lfs_object::Column::PublisherSince.lt(stale_before))
        .exec(db)
        .await
        .context("db: take over expired LFS publication lease")?;
    if taken_over.rows_affected > 0 {
        return Ok(PublicationLeaseBid::TakenOver);
    }

    Ok(PublicationLeaseBid::Busy)
}

/// Release a publication lease. Returns whether this token still held it —
/// `false` means the lease had already been taken over, which is exactly the
/// case where the holder must not assume its stored bytes are still its own.
pub async fn release_publication_lease(
    db: &DatabaseConnection,
    id: i64,
    token: &str,
) -> Result<bool> {
    let released = LfsEntity::update_many()
        .col_expr(
            lfs_object::Column::PublisherToken,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            lfs_object::Column::PublisherSince,
            Expr::value(Option::<DateTimeUtc>::None),
        )
        .filter(lfs_object::Column::Id.eq(id))
        .filter(lfs_object::Column::PublisherToken.eq(token))
        .exec(db)
        .await
        .context("db: release LFS publication lease")?;
    Ok(released.rows_affected > 0)
}

/// How many objects of `repo_id` are stored, and how many bytes they declare.
pub async fn usage(db: &DatabaseConnection, repo_id: i64) -> Result<(u64, i64)> {
    #[derive(Debug, FromQueryResult)]
    struct Usage {
        count: i64,
        bytes: Option<i64>,
    }
    let usage = LfsEntity::find()
        .select_only()
        .column_as(lfs_object::Column::Id.count(), "count")
        .column_as(lfs_object::Column::Size.sum(), "bytes")
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .filter(lfs_object::Column::Uploaded.eq(true))
        .into_model::<Usage>()
        .one(db)
        .await
        .context("db: sum the LFS objects of a repository")?;
    Ok(usage.map_or((0, 0), |usage| {
        (usage.count.max(0) as u64, usage.bytes.unwrap_or(0))
    }))
}

/// Up to `limit` objects of `repo_id` with an id above `after_id`, oldest
/// first, and whether another page follows.
pub async fn list_page(
    db: &DatabaseConnection,
    repo_id: i64,
    after_id: Option<i64>,
    limit: u64,
) -> Result<(Vec<LfsObject>, bool)> {
    let mut select = LfsEntity::find().filter(lfs_object::Column::RepoId.eq(repo_id));
    if let Some(after) = after_id {
        select = select.filter(lfs_object::Column::Id.gt(after));
    }
    let mut rows = select
        .order_by_asc(lfs_object::Column::Id)
        .limit(limit + 1)
        .all(db)
        .await
        .context("db: list a page of LFS objects")?;
    let more = rows.len() as u64 > limit;
    rows.truncate(limit as usize);
    Ok((rows, more))
}

/// Every object row of `repo_id`, uploaded or only announced, oldest first.
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<LfsObject>> {
    LfsEntity::find()
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .order_by_asc(lfs_object::Column::Id)
        .all(db)
        .await
        .context("db: list the LFS objects of a repository")
}

/// Remove object row `id` of `repo_id` — only while it is older than
/// `created_before` and no publication holds it. Returns whether it went.
///
/// Both conditions sit in the `DELETE`, so an upload that took the row's
/// publication lease after the caller decided the object was unused keeps it.
pub async fn delete_unused(
    db: &DatabaseConnection,
    repo_id: i64,
    id: i64,
    created_before: DateTimeUtc,
) -> Result<bool> {
    let deleted = LfsEntity::delete_many()
        .filter(lfs_object::Column::Id.eq(id))
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .filter(lfs_object::Column::CreatedAt.lt(created_before))
        .filter(lfs_object::Column::PublisherToken.is_null())
        .exec(db)
        .await
        .context("db: delete an unused LFS object")?;
    Ok(deleted.rows_affected > 0)
}
