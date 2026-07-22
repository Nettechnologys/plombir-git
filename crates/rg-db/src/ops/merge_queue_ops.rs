//! Database operations for repository merge queues.

use anyhow::{Context, Result};
use chrono::Utc;
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

pub async fn enqueue(
    db: &DatabaseConnection,
    repo_id: i64,
    pr_id: i64,
    enqueued_by_id: i64,
    strategy: &str,
) -> Result<QueueEntry> {
    let now = Utc::now();
    if let Some(existing) = find_by_pr(db, pr_id).await? {
        if matches!(existing.status.as_str(), "queued" | "running") {
            return Ok(existing);
        }
        let mut active: merge_queue_entry::ActiveModel = existing.into();
        active.enqueued_by_id = Set(enqueued_by_id);
        active.strategy = Set(strategy.to_string());
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
        return active.update(db).await.context("db: re-enqueue PR");
    }

    let insert = merge_queue_entry::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        pr_id: Set(pr_id),
        enqueued_by_id: Set(enqueued_by_id),
        strategy: Set(strategy.to_string()),
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
        Err(error) => {
            // A concurrent enqueue may win the unique(pr_id) race.
            if let Some(entry) = find_by_pr(db, pr_id).await? {
                Ok(entry)
            } else {
                Err(error).context("db: enqueue PR")
            }
        }
    }
}

pub async fn set_merge_group(
    db: &DatabaseConnection,
    entry_id: i64,
    group_sha: &str,
    base_sha: &str,
    head_sha: &str,
    pipeline_id: i64,
) -> Result<QueueEntry> {
    let entry = QueueEntity::find_by_id(entry_id)
        .one(db)
        .await?
        .context("merge-queue entry not found")?;
    let mut active: merge_queue_entry::ActiveModel = entry.into();
    active.merge_group_sha = Set(Some(group_sha.to_string()));
    active.merge_group_base_sha = Set(Some(base_sha.to_string()));
    active.merge_group_head_sha = Set(Some(head_sha.to_string()));
    active.merge_group_pipeline_id = Set(Some(pipeline_id));
    active.updated_at = Set(Utc::now());
    active
        .update(db)
        .await
        .context("db: set merge-group pipeline")
}

pub async fn clear_merge_group(db: &DatabaseConnection, entry_id: i64) -> Result<QueueEntry> {
    let entry = QueueEntity::find_by_id(entry_id)
        .one(db)
        .await?
        .context("merge-queue entry not found")?;
    let mut active: merge_queue_entry::ActiveModel = entry.into();
    active.merge_group_sha = Set(None);
    active.merge_group_base_sha = Set(None);
    active.merge_group_head_sha = Set(None);
    active.merge_group_pipeline_id = Set(None);
    active.updated_at = Set(Utc::now());
    active.update(db).await.context("db: clear merge group")
}

pub async fn claim(db: &DatabaseConnection, entry_id: i64) -> Result<bool> {
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
        .filter(merge_queue_entry::Column::Status.eq("queued"))
        .exec(db)
        .await
        .context("db: claim merge-queue entry")?;
    Ok(result.rows_affected == 1)
}

pub async fn finish(
    db: &DatabaseConnection,
    entry_id: i64,
    status: &str,
    failure_reason: Option<String>,
) -> Result<QueueEntry> {
    let entry = QueueEntity::find_by_id(entry_id)
        .one(db)
        .await?
        .context("merge-queue entry not found")?;
    let now = Utc::now();
    let mut active: merge_queue_entry::ActiveModel = entry.into();
    active.status = Set(status.to_string());
    active.failure_reason = Set(failure_reason);
    active.updated_at = Set(now);
    active.finished_at = Set(Some(now));
    active
        .update(db)
        .await
        .context("db: finish merge-queue entry")
}

pub async fn cancel(db: &DatabaseConnection, pr_id: i64) -> Result<bool> {
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
    Ok(result.rows_affected == 1)
}
