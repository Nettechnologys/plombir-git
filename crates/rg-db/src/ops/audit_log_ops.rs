//! Database operations for `audit_log`.
//!
//! Audit logs are append-only — no update or delete.

use sea_orm::*;

use crate::entities::audit_log::{ActiveModel, Column, Entity, Model};

/// Insert a new audit log entry.
pub async fn insert(db: &DatabaseConnection, entry: ActiveModel) -> Result<Model, DbErr> {
    Entity::insert(entry).exec_with_returning(db).await
}

/// List audit logs with pagination, optionally filtered.
#[allow(clippy::too_many_arguments)]
pub async fn list_paginated(
    db: &DatabaseConnection,
    page: u64,
    page_size: u64,
    user_id: Option<i64>,
    action: Option<&str>,
    resource_type: Option<&str>,
    start_time: Option<chrono::DateTime<chrono::Utc>>,
    end_time: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(Vec<Model>, u64), DbErr> {
    let mut query = Entity::find();

    if let Some(uid) = user_id {
        query = query.filter(Column::UserId.eq(uid));
    }
    if let Some(act) = action {
        query = query.filter(Column::Action.eq(act));
    }
    if let Some(rt) = resource_type {
        query = query.filter(Column::ResourceType.eq(rt));
    }
    if let Some(st) = start_time {
        query = query.filter(Column::CreatedAt.gte(st));
    }
    if let Some(et) = end_time {
        query = query.filter(Column::CreatedAt.lte(et));
    }

    let total = query.clone().count(db).await?;
    let logs = query
        .order_by_desc(Column::CreatedAt)
        .paginate(db, page_size)
        .fetch_page(page)
        .await?;

    Ok((logs, total))
}

/// Fetch a single audit log by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Model>, DbErr> {
    Entity::find_by_id(id).one(db).await
}

/// Count audit logs for a specific user (for rate-limiting or dashboard).
pub async fn count_for_user(db: &DatabaseConnection, user_id: i64) -> Result<u64, DbErr> {
    Entity::find()
        .filter(Column::UserId.eq(user_id))
        .count(db)
        .await
}

/// List audit log entries before a given date (for archival).
pub async fn list_before(
    db: &DatabaseConnection,
    cutoff: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Model>, DbErr> {
    Entity::find()
        .filter(Column::CreatedAt.lt(cutoff))
        .all(db)
        .await
}

/// List a bounded oldest-first batch of audit entries before a cutoff.
pub async fn list_before_limit(
    db: &DatabaseConnection,
    cutoff: chrono::DateTime<chrono::Utc>,
    limit: u64,
) -> Result<Vec<Model>, DbErr> {
    Entity::find()
        .filter(Column::CreatedAt.lt(cutoff))
        .order_by_asc(Column::CreatedAt)
        .order_by_asc(Column::Id)
        .limit(limit)
        .all(db)
        .await
}

/// Delete audit log entries by their IDs (after archival).
pub async fn delete_by_ids(db: &DatabaseConnection, ids: &[i64]) -> Result<(), DbErr> {
    Entity::delete_many()
        .filter(Column::Id.is_in(ids.iter().copied()))
        .exec(db)
        .await?;
    Ok(())
}
