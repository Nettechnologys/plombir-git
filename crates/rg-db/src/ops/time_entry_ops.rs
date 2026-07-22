//! Database operations for time entries.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::time_entry::{self, ActiveModel, Entity as TimeEntryEntity, Model};

/// Create a time entry.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.insert(db).await.context("db: create time entry")
}

/// Find a time entry by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Model>> {
    TimeEntryEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find time entry")
}

/// List time entries for an issue (paginated).
pub async fn list_by_issue(
    db: &DatabaseConnection,
    issue_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Model>, i64)> {
    let base = TimeEntryEntity::find()
        .filter(time_entry::Column::IssueId.eq(issue_id))
        .order_by_desc(time_entry::Column::CreatedAt);

    let total = base
        .clone()
        .count(db)
        .await
        .context("db: count time entries")? as i64;
    let entries = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list time entries")?;

    Ok((entries, total))
}

/// Get total tracked minutes for an issue.
pub async fn total_minutes_by_issue(db: &DatabaseConnection, issue_id: i64) -> Result<i64> {
    use sea_orm::prelude::*;

    let result: Option<(Option<i64>,)> = TimeEntryEntity::find()
        .filter(time_entry::Column::IssueId.eq(issue_id))
        .select_only()
        .column_as(time_entry::Column::DurationMinutes.sum(), "total")
        .into_tuple()
        .one(db)
        .await
        .context("db: sum time entries")?;

    let total = result
        .map(|(total_or,)| total_or)
        .and_then(|v| v)
        .unwrap_or(0);
    Ok(total)
}

/// Delete a time entry by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    TimeEntryEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete time entry")?;
    Ok(())
}
