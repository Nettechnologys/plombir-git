//! Database operations for repository merge queues.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sea_orm::*;

use crate::entities::merge_queue_entry::{self, Entity as QueueEntity, Model as QueueEntry};

async fn find_by_pr_with<C>(db: &C, pr_id: i64) -> Result<Option<QueueEntry>>
where
    C: ConnectionTrait,
{
    QueueEntity::find()
        .filter(merge_queue_entry::Column::PrId.eq(pr_id))
        .one(db)
        .await
        .context("db: find merge-queue entry by PR")
}

pub async fn find_by_pr(db: &DatabaseConnection, pr_id: i64) -> Result<Option<QueueEntry>> {
    find_by_pr_with(db, pr_id).await
}

/// Read a PR's queue entry inside a wider ownership transaction.
pub async fn find_by_pr_in_transaction(
    transaction: &DatabaseTransaction,
    pr_id: i64,
) -> Result<Option<QueueEntry>> {
    find_by_pr_with(transaction, pr_id).await
}

pub async fn find_by_merge_group_sha(
    db: &DatabaseConnection,
    repo_id: i64,
    sha: &str,
) -> Result<Option<QueueEntry>> {
    QueueEntity::find()
        .filter(merge_queue_entry::Column::RepoId.eq(repo_id))
        .filter(merge_queue_entry::Column::MergeGroupSha.eq(sha))
        .filter(merge_queue_entry::Column::Status.is_in(["queued", "running"]))
        .one(db)
        .await
        .context("db: find merge-queue entry by merge-group SHA")
}

pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<QueueEntry>> {
    QueueEntity::find()
        .filter(merge_queue_entry::Column::RepoId.eq(repo_id))
        .filter(merge_queue_entry::Column::Status.is_in(["queued", "running"]))
        .order_by_asc(merge_queue_entry::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list repository merge queue")
}

/// Bring the PR's existing queue entry into the state this `enqueue` call asked
/// for.
///
/// An entry that is already `queued` or `running` *is* the requested outcome, so
/// it is returned untouched: asking twice must not send a PR that is waiting its
/// turn to the back of the queue, nor swap the strategy of a merge already in
/// flight. A finished entry (`merged` / `failed` / `canceled`) is recycled
/// instead — this call's actor and strategy take over and the merge-group
/// columns are cleared, so the row describes this attempt rather than the last
/// one.
///
/// Shared by the ordinary path and the raced one so the two cannot drift apart.
/// `None` means the observed row disappeared under a PR/repository cascade; the
/// stale snapshot is never inserted or reported as a successful enqueue.
pub async fn adopt_existing<C>(
    db: &C,
    existing: QueueEntry,
    enqueued_by_id: i64,
    strategy: &str,
    now: DateTime<Utc>,
) -> Result<Option<QueueEntry>>
where
    C: ConnectionTrait,
{
    let id = existing.id;
    let repo_id = existing.repo_id;
    let pr_id = existing.pr_id;
    let mut observed = existing;

    // A worker can move `queued -> running -> terminal` while this call is
    // validating the row it read. Follow that finite state change rather than
    // returning a terminal snapshot as a successful enqueue. Four passes cover
    // every ordinary transition plus one concurrent re-enqueue; sustained
    // churn is an honest server-side conflict, not permission to spin forever.
    for _ in 0..4 {
        let attempt_number = observed.attempt_number;
        let status = observed.status.clone();
        let updated = if matches!(status.as_str(), "queued" | "running") {
            // This deliberately writes the column to itself. It preserves queue
            // order and timestamps, while making the existence check a guarded
            // writer statement: a parent cascade that wins after the first
            // SELECT makes this affect nothing. MySQL may also report zero for
            // a genuine no-op, so rows_affected is never the final verdict.
            QueueEntity::update_many()
                .col_expr(
                    merge_queue_entry::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::col(merge_queue_entry::Column::UpdatedAt).into(),
                )
                .filter(merge_queue_entry::Column::Id.eq(id))
                .filter(merge_queue_entry::Column::RepoId.eq(repo_id))
                .filter(merge_queue_entry::Column::PrId.eq(pr_id))
                .filter(merge_queue_entry::Column::AttemptNumber.eq(attempt_number))
                .filter(merge_queue_entry::Column::Status.eq(status))
                .exec(db)
                .await
                .context("db: validate live merge-queue entry")?
        } else {
            let next_attempt = attempt_number
                .checked_add(1)
                .context("merge-queue attempt number exhausted")?;
            QueueEntity::update_many()
                .col_expr(
                    merge_queue_entry::Column::EnqueuedById,
                    sea_orm::sea_query::Expr::value(enqueued_by_id),
                )
                .col_expr(
                    merge_queue_entry::Column::Strategy,
                    sea_orm::sea_query::Expr::value(strategy.to_string()),
                )
                .col_expr(
                    merge_queue_entry::Column::AttemptNumber,
                    sea_orm::sea_query::Expr::value(next_attempt),
                )
                .col_expr(
                    merge_queue_entry::Column::Status,
                    sea_orm::sea_query::Expr::value("queued"),
                )
                .col_expr(
                    merge_queue_entry::Column::FailureReason,
                    sea_orm::sea_query::Expr::value(Option::<String>::None),
                )
                .col_expr(
                    merge_queue_entry::Column::CreatedAt,
                    sea_orm::sea_query::Expr::value(now),
                )
                .col_expr(
                    merge_queue_entry::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(now),
                )
                .col_expr(
                    merge_queue_entry::Column::StartedAt,
                    sea_orm::sea_query::Expr::value(Option::<DateTime<Utc>>::None),
                )
                .col_expr(
                    merge_queue_entry::Column::FinishedAt,
                    sea_orm::sea_query::Expr::value(Option::<DateTime<Utc>>::None),
                )
                .col_expr(
                    merge_queue_entry::Column::MergeGroupSha,
                    sea_orm::sea_query::Expr::value(Option::<String>::None),
                )
                .col_expr(
                    merge_queue_entry::Column::MergeGroupBaseSha,
                    sea_orm::sea_query::Expr::value(Option::<String>::None),
                )
                .col_expr(
                    merge_queue_entry::Column::MergeGroupHeadSha,
                    sea_orm::sea_query::Expr::value(Option::<String>::None),
                )
                .col_expr(
                    merge_queue_entry::Column::MergeGroupPipelineId,
                    sea_orm::sea_query::Expr::value(Option::<i64>::None),
                )
                .filter(merge_queue_entry::Column::Id.eq(id))
                .filter(merge_queue_entry::Column::RepoId.eq(repo_id))
                .filter(merge_queue_entry::Column::PrId.eq(pr_id))
                .filter(merge_queue_entry::Column::AttemptNumber.eq(attempt_number))
                .filter(merge_queue_entry::Column::Status.eq(status))
                .exec(db)
                .await
                .context("db: re-enqueue PR")?
        };
        match updated.rows_affected {
            0 | 1 => {}
            rows => anyhow::bail!(
                "db: merge-queue adoption affected {rows} rows for entry {id}, repo {repo_id}, PR {pr_id}"
            ),
        }

        // Always re-read the exact row identity. This distinguishes MySQL's
        // zero-change no-op from a winning PR/repository cascade and prevents a
        // recycled attempt from being mistaken for the stale snapshot above.
        let Some(current) = QueueEntity::find()
            .filter(merge_queue_entry::Column::Id.eq(id))
            .filter(merge_queue_entry::Column::RepoId.eq(repo_id))
            .filter(merge_queue_entry::Column::PrId.eq(pr_id))
            .one(db)
            .await
            .context("db: find adopted merge-queue entry")?
        else {
            return Ok(None);
        };
        if matches!(current.status.as_str(), "queued" | "running") {
            return Ok(Some(current));
        }
        observed = current;
    }

    anyhow::bail!(
        "db: merge-queue entry {id} for repo {repo_id}, PR {pr_id} kept changing while being enqueued"
    )
}

/// Put a PR on its repository's merge queue, or return the entry it already has.
/// `None` means an existing entry disappeared under a parent cascade after it
/// was read; callers must resolve which parent is gone and publish no event.
///
/// `pr_id` is UNIQUE (`idx_merge_queue_pr_unique`), and the lookup above is a
/// separate statement from the insert below it. Two clicks of "merge when ready"
/// on the same PR both read `None` and both insert; one of them meets the
/// constraint. That loss says the entry this call wanted now exists — the
/// outcome the caller asked for — so it is resolved by re-reading the winner's
/// row and treating it exactly as the existing-row branch would have.
///
/// Only a UNIQUE violation is resolved that way. Previously *any* insert failure
/// re-read `pr_id` and reported success if a row was found, so a foreign-key
/// failure or a broken connection became a successful enqueue as soon as the PR
/// happened to have an old entry — a write that never happened, reported as
/// done.
pub async fn enqueue<C>(
    db: &C,
    repo_id: i64,
    pr_id: i64,
    enqueued_by_id: i64,
    strategy: &str,
) -> Result<Option<QueueEntry>>
where
    C: ConnectionTrait,
{
    let now = Utc::now();
    if let Some(existing) = find_by_pr_with(db, pr_id).await? {
        return adopt_existing(db, existing, enqueued_by_id, strategy, now).await;
    }

    let insert = merge_queue_entry::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        pr_id: Set(pr_id),
        enqueued_by_id: Set(enqueued_by_id),
        strategy: Set(strategy.to_string()),
        attempt_number: Set(1),
        status: Set("queued".to_string()),
        failure_reason: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        started_at: Set(None),
        finished_at: Set(None),
        merge_group_sha: Set(None),
        merge_group_base_sha: Set(None),
        merge_group_head_sha: Set(None),
        merge_group_pipeline_id: Set(None),
    }
    .insert(db)
    .await;
    match insert {
        Ok(entry) => Ok(Some(entry)),
        Err(error) if crate::is_unique_violation(&error) => {
            // Lost the race for the first row. Whoever won holds this PR's
            // entry, so adopt it the way the existing-row branch would.
            match find_by_pr_with(db, pr_id).await? {
                Some(existing) => adopt_existing(db, existing, enqueued_by_id, strategy, now).await,
                // Not there after all, so the collision was on some other
                // constraint. Report the original failure rather than inventing
                // a reason for it.
                None => Err(error).context("db: enqueue PR"),
            }
        }
        Err(error) => Err(error).context("db: enqueue PR"),
    }
}

/// Attach a published merge-group pipeline only to the queue attempt that
/// produced it and only while that attempt has no different pipeline owner.
/// A terminal row can be recycled under the same primary key, so `entry_id`
/// alone is not an ownership token. Concurrent passes of the same attempt also
/// need the pipeline-id compare-and-set: without it both updates match and the
/// last writer silently strands the first graph. Re-attaching the same pipeline
/// remains idempotent for deterministic adoption after a transient write error.
pub async fn set_merge_group(
    db: &DatabaseConnection,
    entry_id: i64,
    attempt_number: i64,
    group_sha: &str,
    base_sha: &str,
    head_sha: &str,
    pipeline_id: i64,
) -> Result<bool> {
    let result = QueueEntity::update_many()
        .col_expr(
            merge_queue_entry::Column::MergeGroupSha,
            sea_orm::sea_query::Expr::value(Some(group_sha.to_string())),
        )
        .col_expr(
            merge_queue_entry::Column::MergeGroupBaseSha,
            sea_orm::sea_query::Expr::value(Some(base_sha.to_string())),
        )
        .col_expr(
            merge_queue_entry::Column::MergeGroupHeadSha,
            sea_orm::sea_query::Expr::value(Some(head_sha.to_string())),
        )
        .col_expr(
            merge_queue_entry::Column::MergeGroupPipelineId,
            sea_orm::sea_query::Expr::value(Some(pipeline_id)),
        )
        .col_expr(
            merge_queue_entry::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(Utc::now()),
        )
        .filter(merge_queue_entry::Column::Id.eq(entry_id))
        .filter(merge_queue_entry::Column::AttemptNumber.eq(attempt_number))
        .filter(merge_queue_entry::Column::Status.eq("queued"))
        .filter(
            Condition::any()
                .add(merge_queue_entry::Column::MergeGroupPipelineId.is_null())
                .add(merge_queue_entry::Column::MergeGroupPipelineId.eq(pipeline_id)),
        )
        .exec(db)
        .await
        .context("db: set merge-group pipeline")?;
    Ok(result.rows_affected == 1)
}

pub async fn clear_merge_group(
    db: &DatabaseConnection,
    entry_id: i64,
    attempt_number: i64,
) -> Result<bool> {
    let result = QueueEntity::update_many()
        .col_expr(
            merge_queue_entry::Column::MergeGroupSha,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            merge_queue_entry::Column::MergeGroupBaseSha,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            merge_queue_entry::Column::MergeGroupHeadSha,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            merge_queue_entry::Column::MergeGroupPipelineId,
            sea_orm::sea_query::Expr::value(Option::<i64>::None),
        )
        .col_expr(
            merge_queue_entry::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(Utc::now()),
        )
        .filter(merge_queue_entry::Column::Id.eq(entry_id))
        .filter(merge_queue_entry::Column::AttemptNumber.eq(attempt_number))
        .filter(merge_queue_entry::Column::Status.eq("queued"))
        .exec(db)
        .await
        .context("db: clear merge group")?;
    Ok(result.rows_affected == 1)
}

pub async fn claim(db: &DatabaseConnection, entry_id: i64, attempt_number: i64) -> Result<bool> {
    let now = Utc::now();
    let result = QueueEntity::update_many()
        .col_expr(
            merge_queue_entry::Column::Status,
            sea_orm::sea_query::Expr::value("running"),
        )
        .col_expr(
            merge_queue_entry::Column::StartedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            merge_queue_entry::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(merge_queue_entry::Column::Id.eq(entry_id))
        .filter(merge_queue_entry::Column::AttemptNumber.eq(attempt_number))
        .filter(merge_queue_entry::Column::Status.eq("queued"))
        .exec(db)
        .await
        .context("db: claim merge-queue entry")?;
    Ok(result.rows_affected == 1)
}

pub async fn finish(
    db: &DatabaseConnection,
    entry_id: i64,
    attempt_number: i64,
    status: &str,
    failure_reason: Option<String>,
) -> Result<bool> {
    let now = Utc::now();
    let result = QueueEntity::update_many()
        .col_expr(
            merge_queue_entry::Column::Status,
            sea_orm::sea_query::Expr::value(status.to_string()),
        )
        .col_expr(
            merge_queue_entry::Column::FailureReason,
            sea_orm::sea_query::Expr::value(failure_reason),
        )
        .col_expr(
            merge_queue_entry::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            merge_queue_entry::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(merge_queue_entry::Column::Id.eq(entry_id))
        .filter(merge_queue_entry::Column::AttemptNumber.eq(attempt_number))
        .filter(merge_queue_entry::Column::Status.is_in(["queued", "running"]))
        .exec(db)
        .await
        .context("db: finish merge-queue entry")?;
    Ok(result.rows_affected == 1)
}

/// Cancel and return the exact queue attempt that won the conditional write.
/// The read stays in the writer transaction so an immediate re-enqueue cannot
/// clear the old pipeline id before the caller has a chance to retire it.
async fn cancel_with<C>(db: &C, pr_id: i64) -> Result<Option<QueueEntry>>
where
    C: ConnectionTrait,
{
    let now = Utc::now();
    let result = QueueEntity::update_many()
        .col_expr(
            merge_queue_entry::Column::Status,
            sea_orm::sea_query::Expr::value("canceled"),
        )
        .col_expr(
            merge_queue_entry::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            merge_queue_entry::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(merge_queue_entry::Column::PrId.eq(pr_id))
        .filter(merge_queue_entry::Column::Status.eq("queued"))
        .exec(db)
        .await
        .context("db: cancel merge-queue entry")?;
    if result.rows_affected == 0 {
        return Ok(None);
    }
    let canceled = QueueEntity::find()
        .filter(merge_queue_entry::Column::PrId.eq(pr_id))
        .one(db)
        .await
        .context("db: read canceled merge-queue attempt")?
        .context("canceled merge-queue entry vanished inside its transaction")?;
    Ok(Some(canceled))
}

/// Cancel a queued attempt inside a wider ownership transaction.
pub async fn cancel_in_transaction(
    transaction: &DatabaseTransaction,
    pr_id: i64,
) -> Result<Option<QueueEntry>> {
    cancel_with(transaction, pr_id).await
}

pub async fn cancel(db: &DatabaseConnection, pr_id: i64) -> Result<Option<QueueEntry>> {
    let txn = db.begin().await.context("db: begin merge-queue cancel")?;
    let canceled = cancel_with(&txn, pr_id).await?;
    txn.commit()
        .await
        .context("db: commit merge-queue cancel")?;
    Ok(canceled)
}
