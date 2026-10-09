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

/// The ids of `pr_id`'s events that are about review comment `comment_id` —
/// the ones whose metadata names it.
pub async fn ids_for_comment<C: ConnectionTrait>(
    db: &C,
    pr_id: i64,
    comment_id: i64,
) -> Result<Vec<i64>> {
    Ok(list_by_pr(db, pr_id)
        .await?
        .into_iter()
        .filter(|event| {
            serde_json::from_str::<serde_json::Value>(&event.metadata)
                .ok()
                .and_then(|metadata| metadata.get("comment_id")?.as_i64())
                == Some(comment_id)
        })
        .map(|event| event.id)
        .collect())
}

/// Carry a moderated comment's new text into the events that copied it.
///
/// The one exception to append-only, together with [`delete_by_ids`]: an event
/// keeps a copy of the comment it records, and a comment edited or deleted to
/// take a pasted secret down must not survive in the timeline that copied it
/// (card_60961272e1ba).
pub async fn replace_bodies<C: ConnectionTrait>(db: &C, ids: &[i64], body: &str) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let result = PrEventEntity::update_many()
        .col_expr(
            pr_event::Column::Body,
            sea_orm::sea_query::Expr::value(body.to_string()),
        )
        .filter(pr_event::Column::Id.is_in(ids.iter().copied()))
        .exec(db)
        .await
        .context("db: replace moderated pull-request event bodies")?;
    Ok(result.rows_affected)
}

/// Remove the events of a deleted comment — see [`replace_bodies`].
pub async fn delete_by_ids<C: ConnectionTrait>(db: &C, ids: &[i64]) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let result = PrEventEntity::delete_many()
        .filter(pr_event::Column::Id.is_in(ids.iter().copied()))
        .exec(db)
        .await
        .context("db: delete moderated pull-request events")?;
    Ok(result.rows_affected)
}
