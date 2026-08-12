//! Database operations for repository merge queues.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sea_orm::*;

use crate::entities::merge_queue_entry::{self, Entity as QueueEntity, Model as QueueEntry};

pub async fn find_by_pr(db: &DatabaseConnection, pr_id: i64) -> Result<Option<QueueEntry>> {
    QueueEntity::find()
        .filter(merge_queue_entry::Column::PrId.eq(pr_id))
        .one(db)
        .await
        .context("db: find merge-queue entry by PR")
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
async fn adopt_existing(
    db: &DatabaseConnection,
    existing: QueueEntry,
    enqueued_by_id: i64,
    strategy: &str,
    now: DateTime<Utc>,
) -> Result<QueueEntry> {
    if matches!(existing.status.as_str(), "queued" | "running") {
        return Ok(existing);
    }
    let next_attempt = existing
        .attempt_number
        .checked_add(1)
        .context("merge-queue attempt number exhausted")?;
    let mut active: merge_queue_entry::ActiveModel = existing.into();
    active.enqueued_by_id = Set(enqueued_by_id);
    active.strategy = Set(strategy.to_string());
    active.attempt_number = Set(next_attempt);
    active.status = Set("queued".to_string());
    active.failure_reason = Set(None);
    active.created_at = Set(now);
    active.updated_at = Set(now);
    active.started_at = Set(None);
    active.finished_at = Set(None);
    active.merge_group_sha = Set(None);
    active.merge_group_base_sha = Set(None);
    active.merge_group_head_sha = Set(None);
    active.merge_group_pipeline_id = Set(None);
    active.update(db).await.context("db: re-enqueue PR")
}

/// Put a PR on its repository's merge queue, or return the entry it already has.
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
pub async fn enqueue(
    db: &DatabaseConnection,
    repo_id: i64,
    pr_id: i64,
    enqueued_by_id: i64,
    strategy: &str,
) -> Result<QueueEntry> {
    let now = Utc::now();
    if let Some(existing) = find_by_pr(db, pr_id).await? {
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
        Ok(entry) => Ok(entry),
        Err(error) if crate::is_unique_violation(&error) => {
            // Lost the race for the first row. Whoever won holds this PR's
            // entry, so adopt it the way the existing-row branch would.
            match find_by_pr(db, pr_id).await? {
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
pub async fn cancel(db: &DatabaseConnection, pr_id: i64) -> Result<Option<QueueEntry>> {
    let txn = db.begin().await.context("db: begin merge-queue cancel")?;
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
        .exec(&txn)
        .await
        .context("db: cancel merge-queue entry")?;
    if result.rows_affected == 0 {
        txn.commit()
            .await
            .context("db: commit refused merge-queue cancel")?;
        return Ok(None);
    }
    let canceled = QueueEntity::find()
        .filter(merge_queue_entry::Column::PrId.eq(pr_id))
        .one(&txn)
        .await
        .context("db: read canceled merge-queue attempt")?
        .context("canceled merge-queue entry vanished inside its transaction")?;
    txn.commit()
        .await
        .context("db: commit merge-queue cancel")?;
    Ok(Some(canceled))
}
