//! Database operations for repository mirrors.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{
    prelude::DateTimeUtc,
    sea_query::{Expr, OnConflict},
    *,
};

use crate::entities::mirror::{self, ActiveModel, Entity as MirrorEntity, Model};
use crate::entities::{mirror_sync_lease, repository};

/// Create a mirror record.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.insert(db).await.context("db: create mirror")
}

/// Find a mirror by its ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Model>> {
    MirrorEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find mirror by id")
}

/// Find a mirror by repository ID.
pub async fn find_by_repo_id(db: &DatabaseConnection, repo_id: i64) -> Result<Option<Model>> {
    MirrorEntity::find()
        .filter(mirror::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: find mirror by repo_id")
}

/// Update a mirror record.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.update(db).await.context("db: update mirror")
}

/// Delete a mirror by ID. `Ok(false)` means no such row.
///
/// The caller's lookup and this `DELETE` are two statements: reporting
/// `rows_affected` is what stops a route from confirming a deletion that a
/// concurrent request had already performed.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = MirrorEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete mirror")?;
    Ok(result.rows_affected > 0)
}

/// How a mirror deletion ended, from the row's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorRetirement {
    /// The row is gone and its clone directory is the caller's to retire.
    Deleted,
    /// There was no such row — a concurrent delete got there first.
    NotFound,
    /// A pass holds this mirror's sync lease; nothing was deleted.
    MirrorSyncInFlight,
}

/// Delete a mirror row unless a pass is writing its clone directory.
///
/// The mirror's own deletion faces the window
/// [`crate::ops::repo_ops::soft_delete_unless_mirror_syncing`] closes for the
/// repository's, and for the same reason: `git clone --mirror` writes to the
/// absolute path it was handed for minutes after the check that admitted it, so
/// a gate read before the delete leaves a gap in which a pass takes the lease
/// and re-creates `<repo_root>/<repo_id>.mirror` after the caller has retired it
/// — bytes nothing owns, under a name no later namespace collides with and no
/// sweep walks.
///
/// The DELETE comes *before* the lease read deliberately, exactly as the
/// repository's soft delete does. It takes the mirror row's exclusive lock — the
/// same row [`bid_for_sync_lease`] reads under `lock_exclusive` before it admits
/// a pass — so the two transactions are ordered by the database rather than by
/// luck, whichever arrives first:
///
/// * this one first ⇒ the bid waits on the row, then reads a deleted mirror and
///   declines;
/// * the bid first ⇒ its lease is committed and visible here, and this
///   transaction rolls back;
/// * the bid first but still open ⇒ its lease is invisible here and this
///   commits, after which the bid's locked read finally returns no mirror and
///   declines anyway.
///
/// Writing first is also what makes it work on SQLite, where `lock_exclusive` is
/// a no-op: a transaction whose first statement is a write takes the single
/// writer slot outright instead of leaving a lock-upgrade window.
pub async fn delete_by_id_unless_syncing(
    db: &DatabaseConnection,
    id: i64,
    repo_id: i64,
    stale_before: DateTimeUtc,
) -> Result<MirrorRetirement> {
    let transaction = db
        .begin()
        .await
        .context("db: begin guarded mirror delete")?;

    let deleted = MirrorEntity::delete_by_id(id)
        .exec(&transaction)
        .await
        .context("db: delete mirror")?;
    if deleted.rows_affected == 0 {
        transaction
            .rollback()
            .await
            .context("db: roll back a delete of a mirror that is not there")?;
        return Ok(MirrorRetirement::NotFound);
    }

    let syncing = sync_lease_in_flight(&transaction, repo_id, stale_before)
        .await
        .context("db: check for a mirror sync in flight while deleting a mirror")?;
    if syncing.is_some() {
        transaction
            .rollback()
            .await
            .context("db: roll back a mirror delete a sync pass is holding")?;
        return Ok(MirrorRetirement::MirrorSyncInFlight);
    }

    transaction
        .commit()
        .await
        .context("db: commit guarded mirror delete")?;
    Ok(MirrorRetirement::Deleted)
}

/// List mirrors that are due for sync.
///
/// "Not switched off", not "healthy": the sweep that consumes this list is also
/// what writes [`mirror::STATUS_ERROR`] into `status` when a pass fails, so
/// selecting `status = "active"` here would mean every mirror leaves its own
/// retry queue the first time the network, the credential or the SSRF guard
/// says no — and nothing else ever picks it back up (card_770723efaa96). The
/// operator's off switch is the one thing this filter is allowed to read.
///
/// The repository, on the other hand, is not this row's own state: repository
/// deletion is a *soft* delete, so `mirrors.repo_id ON DELETE CASCADE` never
/// fires and the row stays due forever. The sweep that consumed it then cloned
/// the upstream back into `<repo_root>/<repo_id>.mirror` — a directory the
/// deletion had just retired — so an operator who cleared it by hand got it
/// back one interval later, and the server kept reaching out to a third-party
/// remote on behalf of a repository that no longer exists (card_374998ffebc1).
/// The join is what keeps those rows out; `sync_mirror` re-checks for the gap
/// between this selection and its own `git` subprocess.
///
/// Ordered by how long each mirror has been waiting, because the `LIMIT` makes
/// this a queue and an unordered `LIMIT` is whatever the backend's plan happens
/// to hand back. A mirror that keeps landing in the batch and a mirror that
/// never does is the difference between a schedule and a lottery — and a
/// handful of rows that are due on every single tick would otherwise be able to
/// fill the batch forever, starving every correctly configured mirror behind
/// them (card_3d4c7b8b27c8). A row that has never synced sorts first: `NULL` is
/// "due since forever", and it is the only state in which the mirror has not
/// run at all.
pub async fn list_due_sync(db: &DatabaseConnection, limit: u64) -> Result<Vec<Model>> {
    let now = Utc::now();
    MirrorEntity::find()
        .inner_join(repository::Entity)
        .filter(repository::Column::DeletedAt.is_null())
        .filter(mirror::Column::Status.ne(mirror::STATUS_INACTIVE))
        .filter(
            mirror::Column::NextSyncAt
                .is_null()
                .or(mirror::Column::NextSyncAt.lte(now)),
        )
        // `NULL` sorts first on SQLite and MySQL but last on PostgreSQL, so the
        // "never synced" case is lifted into the ordering itself rather than
        // left to the backend's null placement.
        .order_by_asc(Expr::expr(mirror::Column::NextSyncAt.is_null()).eq(false))
        .order_by_asc(mirror::Column::NextSyncAt)
        .order_by_asc(mirror::Column::Id)
        .limit(limit)
        .all(db)
        .await
        .context("db: list due sync mirrors")
}

/// List all mirrors (admin).
pub async fn list_all(db: &DatabaseConnection) -> Result<Vec<Model>> {
    MirrorEntity::find()
        .all(db)
        .await
        .context("db: list all mirrors")
}

// ── Sync lease ──────────────────────────────────────────────────────────────

/// How long a mirror sync lease may go unreleased before another pass is
/// allowed to take the mirror over.
///
/// A pass that is still cloning has not abandoned anything, so this is not a
/// deadline for the clone — it is the point past which a *dead* holder must stop
/// blocking the mirror, and above all must stop making the repository it belongs
/// to impossible to delete. Generous enough that a large upstream on a slow
/// remote is never timed out, short enough that a crashed process is not a
/// permanent obstruction. Deliberately the same horizon as
/// [`crate::ops::repo_ops::TRANSFER_LEASE_STALE_AFTER`]: both cover one
/// long-running byte move made by a process that can die without releasing.
pub const SYNC_LEASE_STALE_AFTER: chrono::Duration = chrono::Duration::hours(6);

/// The answer to "may this pass start writing this repository's mirror clone?".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncLeaseBid {
    /// The caller's token holds the lease and owns the clone directory.
    Granted,
    /// Taken over from a holder whose lease had gone stale.
    TakenOver,
    /// Another pass is writing this mirror right now.
    Busy,
    /// The repository row is gone or already soft-deleted — its mirror clone is
    /// storage the deletion has retired, and re-creating it is the whole defect.
    RepositoryGone,
    /// The mirror row itself is gone. `DELETE /mirror` retires the same clone
    /// directory the repository's deletion does, so a pass admitted after it
    /// would re-create exactly the bytes that deletion removed.
    MirrorGone,
}

/// Take the lease that makes a mirror pass visible to the deletion of the
/// repository that owns it.
///
/// The window this closes is described in
/// `m20260807_000001_create_mirror_sync_lease`: `git clone --mirror` writes to
/// the absolute path it was handed for minutes after the lifecycle check that
/// admitted it, and the deletion retires that path in between.
///
/// The repository lifecycle is verified *inside* this transaction and under
/// `lock_exclusive`, which is what makes the two orders decided rather than
/// raced. The deletion's own commit
/// ([`crate::ops::repo_ops::soft_delete_unless_mirror_syncing`]) writes the
/// repository row before it reads this table, so exactly one of the two wins:
/// either the deletion holds the row and this bid waits and then sees a
/// soft-deleted repository, or this bid holds the row and the deletion waits and
/// then sees the lease.
///
/// The insert comes first deliberately. SQLite has no row-level `FOR UPDATE`, so
/// `lock_exclusive` is a no-op there and the transaction's first *write* is what
/// acquires its single writer slot — the same ordering
/// [`crate::ops::repo_ops::bid_for_transfer_lease`] relies on, and for the same
/// reason: a read followed by a write leaves a lock-upgrade window in which the
/// deletion can become the writer and then wait on this transaction's snapshot.
pub async fn bid_for_sync_lease(
    db: &DatabaseConnection,
    repo_id: i64,
    token: &str,
    stale_before: DateTimeUtc,
) -> Result<SyncLeaseBid> {
    use mirror_sync_lease::Entity as Lease;
    let now = Utc::now();

    let transaction = db
        .begin()
        .await
        .context("db: begin mirror sync lease bid")?;

    Lease::insert(mirror_sync_lease::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        token: Set(token.to_string()),
        since: Set(now),
    })
    .on_conflict(
        OnConflict::column(mirror_sync_lease::Column::RepoId)
            // MySQL has no conflict target and needs a harmless assignment as
            // its DO NOTHING polyfill, exactly as `bid_for_transfer_lease` does.
            // PostgreSQL and SQLite emit DO NOTHING for the column above.
            .do_nothing_on([mirror_sync_lease::Column::Id])
            .to_owned(),
    )
    .exec_without_returning(&transaction)
    .await
    .context("db: bid for mirror sync lease")?;

    // Whether that insert landed is not something every backend will say, so ask
    // the row who holds it.
    let held = Lease::find()
        .filter(mirror_sync_lease::Column::RepoId.eq(repo_id))
        .one(&transaction)
        .await
        .context("db: read mirror sync lease holder")?;

    let mut outcome = match held {
        // The holder released between the insert and this read, taking the row
        // with it. The mirror is free but this token does not hold it, and
        // saying otherwise would hand out a lease nothing records.
        None => SyncLeaseBid::Busy,
        Some(held) if held.token == token => SyncLeaseBid::Granted,
        Some(held) => {
            // Someone else holds it. Only a stale hold may be taken over, and
            // only from the exact holder this read saw: filtering on the old
            // token is what keeps two waiters from both believing they took over
            // the same lease.
            let taken_over = Lease::update_many()
                .col_expr(mirror_sync_lease::Column::Token, Expr::value(token))
                .col_expr(mirror_sync_lease::Column::Since, Expr::value(now))
                .filter(mirror_sync_lease::Column::RepoId.eq(repo_id))
                .filter(mirror_sync_lease::Column::Token.eq(held.token))
                .filter(mirror_sync_lease::Column::Since.lt(stale_before))
                .exec(&transaction)
                .await
                .context("db: take over a stale mirror sync lease")?;
            if taken_over.rows_affected > 0 {
                SyncLeaseBid::TakenOver
            } else {
                SyncLeaseBid::Busy
            }
        }
    };

    // Holding the lease is only worth anything if the repository whose storage
    // this pass is about to write still exists.
    if matches!(outcome, SyncLeaseBid::Granted | SyncLeaseBid::TakenOver)
        && repository::Entity::find_by_id(repo_id)
            .filter(repository::Column::DeletedAt.is_null())
            .lock_exclusive()
            .one(&transaction)
            .await
            .context("db: read the repository a mirror sync lease was taken for")?
            .is_none()
    {
        outcome = SyncLeaseBid::RepositoryGone;
    }

    // …and neither is holding it worth anything once the mirror itself is gone.
    // `DELETE /mirror` retires the same clone directory, so this locked read is
    // the half that orders this bid against
    // [`delete_by_id_unless_syncing`] — without it the two transactions touch
    // disjoint rows and both can commit, leaving a pass cloning an upstream back
    // into a directory the deletion had just removed.
    if matches!(outcome, SyncLeaseBid::Granted | SyncLeaseBid::TakenOver)
        && MirrorEntity::find()
            .filter(mirror::Column::RepoId.eq(repo_id))
            .lock_exclusive()
            .one(&transaction)
            .await
            .context("db: read the mirror a sync lease was taken for")?
            .is_none()
    {
        outcome = SyncLeaseBid::MirrorGone;
    }

    match outcome {
        // A refusal must not leave the lease this transaction inserted behind:
        // it would block the mirror until it went stale, and block the deletion
        // that refused it for just as long.
        SyncLeaseBid::Granted | SyncLeaseBid::TakenOver => transaction
            .commit()
            .await
            .context("db: commit mirror sync lease bid")?,
        _ => transaction
            .rollback()
            .await
            .context("db: roll back a refused mirror sync lease bid")?,
    }
    Ok(outcome)
}

/// Release a sync lease. Returns whether this token still held it — `false`
/// means it had already been taken over, which is exactly the case where the
/// holder must not assume the clone directory is still its own.
pub async fn release_sync_lease(
    db: &DatabaseConnection,
    repo_id: i64,
    token: &str,
) -> Result<bool> {
    let released = mirror_sync_lease::Entity::delete_many()
        .filter(mirror_sync_lease::Column::RepoId.eq(repo_id))
        .filter(mirror_sync_lease::Column::Token.eq(token))
        .exec(db)
        .await
        .context("db: release mirror sync lease")?;
    Ok(released.rows_affected > 0)
}

/// Whether a repository's mirror clone is being written right now.
///
/// Read by the deletion path, which must not retire a directory a `git`
/// subprocess is still writing into by absolute path. A lease past
/// `stale_before` is not an answer — its holder is gone, and a dead pass must
/// never make a repository undeletable.
///
/// Generic over the connection because the deletion reads it twice: once up
/// front for an early refusal that costs no staging, and once inside the
/// transaction that soft-deletes the row, where the answer has to be the one
/// that write is serialized against.
pub async fn sync_lease_in_flight<C: ConnectionTrait>(
    db: &C,
    repo_id: i64,
    stale_before: DateTimeUtc,
) -> Result<Option<mirror_sync_lease::Model>> {
    mirror_sync_lease::Entity::find()
        .filter(mirror_sync_lease::Column::RepoId.eq(repo_id))
        .filter(mirror_sync_lease::Column::Since.gte(stale_before))
        .one(db)
        .await
        .context("db: read mirror sync lease")
}
