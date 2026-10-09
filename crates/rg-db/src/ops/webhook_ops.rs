//! Database operations for webhooks and webhook deliveries.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::webhook::{
    self, ActiveModel as WebhookActiveModel, Entity as WebhookEntity, Model as Webhook,
};
use crate::entities::webhook_delivery::{
    ActiveModel as DeliveryActiveModel, Entity as DeliveryEntity, Model as WebhookDelivery,
};

// ── Webhook CRUD ──────────────────────────────────────────────────────────

/// Find a webhook by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Webhook>> {
    WebhookEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find webhook by id")
}

/// List webhooks for a repo.
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Webhook>> {
    WebhookEntity::find()
        .filter(webhook::Column::RepoId.eq(repo_id))
        .order_by_desc(webhook::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list webhooks by repo")
}

/// Active webhooks of `repo_id` whose subscription list *mentions* `event`.
///
/// A narrowing filter, not a verdict: `events` is one comma-joined string, and
/// `LIKE '%<event>%'` cannot tell a list entry from a substring of one. Deciding
/// membership is `rg_core::webhook::service::subscription_covers`'s job, and the
/// one caller applies it to every row this returns — do not use these rows as a
/// delivery list on their own (card_55c9cddfe9d6).
pub async fn list_active_by_repo_and_event(
    db: &DatabaseConnection,
    repo_id: i64,
    event: &str,
) -> Result<Vec<Webhook>> {
    WebhookEntity::find()
        .filter(webhook::Column::RepoId.eq(repo_id))
        .filter(webhook::Column::Active.eq(true))
        .filter(webhook::Column::Events.contains(event))
        .all(db)
        .await
        .context("db: list active webhooks by repo and event")
}

/// Create a new webhook.
pub async fn create_webhook(db: &DatabaseConnection, model: WebhookActiveModel) -> Result<Webhook> {
    model.insert(db).await.context("db: create webhook")
}

/// Update a webhook in one conditional statement.
///
/// The repository-scoped read happens before this call. `None` therefore means
/// a concurrent delete won, while database failures remain errors.
#[allow(clippy::too_many_arguments)]
pub async fn update_webhook(
    db: &DatabaseConnection,
    id: i64,
    url: String,
    content_type: String,
    secret_encrypted: Option<String>,
    active: bool,
    events: String,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Webhook>> {
    let result = WebhookEntity::update_many()
        .col_expr(webhook::Column::Url, Expr::value(url))
        .col_expr(webhook::Column::ContentType, Expr::value(content_type))
        .col_expr(
            webhook::Column::SecretEncrypted,
            Expr::value(secret_encrypted),
        )
        .col_expr(webhook::Column::Active, Expr::value(active))
        .col_expr(webhook::Column::Events, Expr::value(events))
        .col_expr(webhook::Column::UpdatedAt, Expr::value(updated_at))
        .filter(webhook::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: update webhook")?;
    match result.rows_affected {
        0 | 1 => find_by_id(db, id).await,
        rows => anyhow::bail!("db: webhook update affected {rows} rows for id {id}"),
    }
}

/// Delete a webhook by id, reporting whether this call removed it.
///
/// `false` means the row was already gone when the DELETE ran — the caller's
/// own lookup happened in a separate statement, so a concurrent delete can win
/// in between. Only the request that actually removed the row may report a
/// deletion.
pub async fn delete_webhook_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = WebhookEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete webhook")?;
    Ok(result.rows_affected > 0)
}

// ── Webhook Delivery ──────────────────────────────────────────────────────

/// Create a new webhook delivery record.
pub async fn create_delivery(
    db: &DatabaseConnection,
    model: DeliveryActiveModel,
) -> Result<WebhookDelivery> {
    model
        .insert(db)
        .await
        .context("db: create webhook delivery")
}

/// List deliveries for a webhook.
pub async fn list_deliveries_by_webhook(
    db: &DatabaseConnection,
    webhook_id: i64,
) -> Result<Vec<WebhookDelivery>> {
    deliveries_query(webhook_id)
        .all(db)
        .await
        .context("db: list webhook deliveries")
}

/// The newest 50 deliveries of one webhook, newest first.
///
/// `id` breaks ties between deliveries recorded in the same instant — a burst
/// of pushes fans out faster than the timestamp's resolution — and lets the
/// `(webhook_id, created_at, id)` index hand the rows over in order, so the
/// engine reads 50 payloads instead of every payload the webhook ever sent.
pub(crate) fn deliveries_query(webhook_id: i64) -> Select<DeliveryEntity> {
    DeliveryEntity::find()
        .filter(crate::entities::webhook_delivery::Column::WebhookId.eq(webhook_id))
        .order_by_desc(crate::entities::webhook_delivery::Column::CreatedAt)
        .order_by_desc(crate::entities::webhook_delivery::Column::Id)
        .limit(50)
}

/// Find a delivery by id.
pub async fn find_delivery_by_id(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<WebhookDelivery>> {
    DeliveryEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find webhook delivery by id")
}

/// Delete up to `limit` deliveries recorded before `cutoff`, oldest first, and
/// return how many went.
///
/// The retention sweep's batch. A delivery keeps its full request payload and
/// the receiver's response body, the hook's page shows the latest fifty, and
/// nothing else reads the rest — so without a bound the table grows by a full
/// payload per event per hook for as long as the instance lives. Two short
/// statements per batch rather than one unbounded `DELETE`: the ids are read
/// first and deleted by key, so no single statement holds the write lock for
/// the whole backlog (and MySQL refuses `LIMIT` inside an `IN` subquery). The
/// caller bounds `limit`.
pub async fn delete_deliveries_before(
    db: &DatabaseConnection,
    cutoff: chrono::DateTime<chrono::Utc>,
    limit: u64,
) -> Result<u64> {
    use crate::entities::webhook_delivery::Column;
    let ids: Vec<i64> = DeliveryEntity::find()
        .select_only()
        .column(Column::Id)
        .filter(Column::CreatedAt.lt(cutoff))
        .order_by_asc(Column::CreatedAt)
        .order_by_asc(Column::Id)
        .limit(limit)
        .into_tuple()
        .all(db)
        .await
        .context("db: list expired webhook deliveries")?;
    if ids.is_empty() {
        return Ok(0);
    }
    let result = DeliveryEntity::delete_many()
        .filter(Column::Id.is_in(ids))
        .exec(db)
        .await
        .context("db: delete expired webhook deliveries")?;
    Ok(result.rows_affected)
}
