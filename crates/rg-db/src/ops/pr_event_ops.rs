//! Append-only pull-request event operations.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, NotSet, QueryFilter, QueryOrder,
    Set,
};

use crate::entities::pr_event::{self, Entity as PrEventEntity, Model as PrEvent};

pub async fn record<C: ConnectionTrait>(
    db: &C,
    repo_id: i64,
    pr_id: i64,
    actor_id: Option<i64>,
    event_type: &str,
    body: Option<String>,
    metadata: serde_json::Value,
) -> Result<PrEvent> {
    pr_event::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        pr_id: Set(pr_id),
        actor_id: Set(actor_id),
        event_type: Set(event_type.to_string()),
        body: Set(body),
        metadata: Set(metadata.to_string()),
        created_at: Set(Utc::now()),
    }
    .insert(db)
    .await
    .context("db: append pull-request event")
}

pub async fn list_by_pr<C: ConnectionTrait>(db: &C, pr_id: i64) -> Result<Vec<PrEvent>> {
    PrEventEntity::find()
        .filter(pr_event::Column::PrId.eq(pr_id))
        .order_by_asc(pr_event::Column::CreatedAt)
        .order_by_asc(pr_event::Column::Id)
        .all(db)
        .await
        .context("db: list pull-request events")
}
