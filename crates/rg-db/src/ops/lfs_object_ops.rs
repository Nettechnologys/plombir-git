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
    /// The row is gone — removed as unused while this bid waited. Waiting on
    /// would wait for a lease nobody can release; the caller registers the
    /// object again instead.
    Gone,
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

    let exists = LfsEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: check an LFS object a publication lease was refused for")?
        .is_some();
    Ok(if exists {
        PublicationLeaseBid::Busy
    } else {
        PublicationLeaseBid::Gone
    })
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
        .column_as(
            super::aggregate::sum_i64(db.get_database_backend(), lfs_object::Column::Size),
            "bytes",
        )
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

/// Mark `repo_id`'s object `oid` claimed at `now`, if it is stored and no
/// lease holds it. Returns whether it is — `false` is the answer to give a
/// client as "send the bytes".
///
/// One conditional `UPDATE` rather than a read and a write, so it is ordered
/// against [`lease_unused_for_removal`]: either the claim lands first and the
/// removal leaves the row alone, or the removal's lease lands first and this
/// claims nothing.
pub async fn claim_uploaded(
    db: &DatabaseConnection,
    repo_id: i64,
    oid: &str,
    now: DateTimeUtc,
) -> Result<bool> {
    let claimed = LfsEntity::update_many()
        .col_expr(lfs_object::Column::LastClaimedAt, Expr::value(now))
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .filter(lfs_object::Column::Oid.eq(oid))
        .filter(lfs_object::Column::Uploaded.eq(true))
        // A leased row is being published or removed; either way its bytes
        // are not settled, and "send the bytes" is the safe answer — the
        // upload then waits for the lease (card_10864c45e31e).
        .filter(lfs_object::Column::PublisherToken.is_null())
        .exec(db)
        .await
        .context("db: claim a stored LFS object")?;
    Ok(claimed.rows_affected > 0)
}

/// Take the publication lease of object row `id` of `repo_id` for its removal
/// — only while it was created and last claimed before `created_before` and no
/// publication holds it. Returns whether the lease is now `token`'s.
///
/// Every condition sits in the `UPDATE`, so an upload that took the row's
/// publication lease, or a push that was told the object is stored, after the
/// caller decided the object was unused keeps it.
///
/// The lease, not a `DELETE`, is what a removal starts with (card_10864c45e31e).
/// Deleting the row first left a window before the bytes went in which a new
/// upload of the same object registered a fresh row, found the old bytes still
/// under the content-addressed key, reused them — and lost them a moment later.
/// While the row is leased, a publication waits for it ([`bid_for_publication_lease`]
/// answers `Busy`), and [`claim_uploaded`] does not answer "stored" for it.
pub async fn lease_unused_for_removal(
    db: &DatabaseConnection,
    repo_id: i64,
    id: i64,
    token: &str,
    created_before: DateTimeUtc,
) -> Result<bool> {
    let leased = LfsEntity::update_many()
        .col_expr(lfs_object::Column::PublisherToken, Expr::value(token))
        .col_expr(
            lfs_object::Column::PublisherSince,
            Expr::value(chrono::Utc::now()),
        )
        .filter(lfs_object::Column::Id.eq(id))
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .filter(lfs_object::Column::CreatedAt.lt(created_before))
        .filter(
            Condition::any()
                .add(lfs_object::Column::LastClaimedAt.is_null())
                .add(lfs_object::Column::LastClaimedAt.lt(created_before)),
        )
        .filter(lfs_object::Column::PublisherToken.is_null())
        .exec(db)
        .await
        .context("db: lease an unused LFS object for removal")?;
    Ok(leased.rows_affected > 0)
}

/// Remove object row `id` of `repo_id` once its stored bytes are gone — only
/// while `token`, from [`lease_unused_for_removal`], still holds its lease.
/// Returns whether the row went.
pub async fn delete_leased(
    db: &DatabaseConnection,
    repo_id: i64,
    id: i64,
    token: &str,
) -> Result<bool> {
    let deleted = LfsEntity::delete_many()
        .filter(lfs_object::Column::Id.eq(id))
        .filter(lfs_object::Column::RepoId.eq(repo_id))
        .filter(lfs_object::Column::PublisherToken.eq(token))
        .exec(db)
        .await
        .context("db: delete a removed LFS object")?;
    Ok(deleted.rows_affected > 0)
}

#[cfg(test)]
mod unused_object_tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, Statement};

    async fn object(created_at: &str) -> (DatabaseConnection, LfsObject) {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::run_migrations(&db).await.unwrap();
        for sql in [
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(1, 'owner', 'owner@example.invalid', 'x', 0, 1, '2024-01-01', '2024-01-01')",
            "INSERT INTO repositories(id, owner_id, name, is_private, default_branch, stars_count, forks_count, created_at, updated_at) \
             VALUES(1, 1, 'assets', 0, 'main', 0, 0, '2024-01-01', '2024-01-01')",
        ] {
            db.execute(Statement::from_string(DbBackend::Sqlite, sql.to_string()))
                .await
                .unwrap();
        }
        let row = create(
            &db,
            ActiveModel {
                id: NotSet,
                repo_id: Set(1),
                oid: Set("a".repeat(64)),
                size: Set(1),
                uploaded: Set(true),
                created_at: Set(created_at.parse().unwrap()),
                publisher_token: Set(None),
                publisher_since: Set(None),
                last_claimed_at: Set(None),
            },
        )
        .await
        .unwrap();
        (db, row)
    }

    /// The lease itself carries the claim, so a claim that lands after the
    /// caller read the row as unused still keeps it.
    #[tokio::test]
    async fn a_claim_after_the_unused_read_keeps_the_object() {
        let (db, row) = object("2020-01-01T00:00:00Z").await;
        let now = chrono::Utc::now();
        let cutoff = now - chrono::Duration::hours(24);

        assert!(claim_uploaded(&db, 1, &row.oid, now).await.unwrap());
        assert!(!lease_unused_for_removal(&db, 1, row.id, "prune", cutoff)
            .await
            .unwrap());
        assert!(find_by_repo_and_oid(&db, 1, &row.oid)
            .await
            .unwrap()
            .is_some());

        // An old claim protects nothing.
        let old = "2020-01-02T00:00:00Z".parse().unwrap();
        assert!(claim_uploaded(&db, 1, &row.oid, old).await.unwrap());
        assert!(lease_unused_for_removal(&db, 1, row.id, "prune", cutoff)
            .await
            .unwrap());
        assert!(delete_leased(&db, 1, row.id, "prune").await.unwrap());
        // And once the row is gone, a claim says so.
        assert!(!claim_uploaded(&db, 1, &row.oid, now).await.unwrap());
    }

    /// card_10864c45e31e: between the lease and the row's removal the bytes
    /// are being deleted, so nobody may be told the object is stored, nor
    /// start publishing it — and only the lease holder removes the row.
    #[tokio::test]
    async fn a_row_leased_for_removal_is_neither_stored_nor_publishable() {
        let (db, row) = object("2020-01-01T00:00:00Z").await;
        let now = chrono::Utc::now();
        let cutoff = now - chrono::Duration::hours(24);
        assert!(lease_unused_for_removal(&db, 1, row.id, "prune", cutoff)
            .await
            .unwrap());

        assert!(
            !claim_uploaded(&db, 1, &row.oid, now).await.unwrap(),
            "a push was told an object being removed is stored"
        );
        assert_eq!(
            bid_for_publication_lease(&db, row.id, "upload", cutoff)
                .await
                .unwrap(),
            PublicationLeaseBid::Busy,
            "an upload started publishing an object being removed"
        );
        assert!(!delete_leased(&db, 1, row.id, "upload").await.unwrap());
        assert!(delete_leased(&db, 1, row.id, "prune").await.unwrap());
        assert_eq!(
            bid_for_publication_lease(&db, row.id, "upload", cutoff)
                .await
                .unwrap(),
            PublicationLeaseBid::Gone,
            "a removed row must not read as a lease someone else holds"
        );
    }
}
