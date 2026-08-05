//! Time tracking service — track time spent on issues.

use anyhow::Result;
use chrono::Utc;
use rg_db::entities::time_entry::{ActiveModel, Model as TimeEntry};
use sea_orm::{ActiveValue::Set, DatabaseConnection};

/// Add a time entry to an issue.
pub async fn add_time(
    db: &DatabaseConnection,
    issue_id: i64,
    user_id: i64,
    duration_minutes: i64,
    description: Option<String>,
) -> Result<TimeEntry> {
    if duration_minutes <= 0 {
        return Err(crate::error::invalid_request("duration must be positive"));
    }

    let now = Utc::now();
    let model = ActiveModel {
        issue_id: Set(issue_id),
        user_id: Set(Some(user_id)),
        duration_minutes: Set(duration_minutes),
        description: Set(description),
        created_at: Set(now),
        ..Default::default()
    };

    rg_db::ops::time_entry_ops::create(db, model).await
}

/// List time entries for an issue (paginated).
pub async fn list_time_entries(
    db: &DatabaseConnection,
    issue_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<TimeEntry>, i64)> {
    rg_db::ops::time_entry_ops::list_by_issue(db, issue_id, offset, limit).await
}

/// Get total tracked time for an issue (in minutes).
pub async fn total_time_minutes(db: &DatabaseConnection, issue_id: i64) -> Result<i64> {
    rg_db::ops::time_entry_ops::total_minutes_by_issue(db, issue_id).await
}

/// Delete a time entry that belongs to `issue_id`.
///
/// The issue is part of the signature on purpose: `id` is a global
/// `time_entries` primary key, so a caller authorized for one issue must not be
/// able to reach another one's rows through it. Taking the entry id alone made
/// that impossible to enforce at the call site — the handler had nothing to
/// compare against.
///
/// An entry that lives under a different issue reports `not_found`, the same
/// answer as an entry that does not exist at all: a distinct "exists, but not
/// yours" would still confirm the id is real, which is most of what an
/// id-walking caller wants to learn.
/// A `DELETE` that removed no row reports `not_found` too: the scope check
/// above is a separate statement, so a concurrent delete can empty the row out
/// from under it, and `Ok(())` there would confirm a deletion this call did not
/// perform.
pub async fn delete_time_entry(db: &DatabaseConnection, issue_id: i64, id: i64) -> Result<()> {
    match rg_db::ops::time_entry_ops::find_by_id(db, id).await? {
        Some(entry) if entry.issue_id == issue_id => {
            if rg_db::ops::time_entry_ops::delete_by_id(db, id).await? {
                Ok(())
            } else {
                Err(crate::error::not_found("time entry"))
            }
        }
        _ => Err(crate::error::not_found("time entry")),
    }
}

/// Format minutes into a human-readable string (e.g. "2h 15m").
pub fn format_duration(minutes: i64) -> String {
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    let mins = minutes % 60;
    if mins == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {mins}m")
    }
}
