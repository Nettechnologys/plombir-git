//! Database operations for webhooks and webhook deliveries.

use anyhow::{Context, Result};
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

/// List active webhooks for a repo that listen to a given event.
pub async fn list_active_by_repo_and_event(
    db: &DatabaseConnection,
    repo_id: i64,
    event: &str,
) -> Result<Vec<Webhook>> {
    // Events are stored comma-separated; use LIKE for matching
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

/// Update a webhook.
pub async fn update_webhook(db: &DatabaseConnection, model: WebhookActiveModel) -> Result<Webhook> {
    model.update(db).await.context("db: update webhook")
}

/// Delete a webhook by id.
pub async fn delete_webhook_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    WebhookEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete webhook")?;
    Ok(())
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
    DeliveryEntity::find()
        .filter(crate::entities::webhook_delivery::Column::WebhookId.eq(webhook_id))
        .order_by_desc(crate::entities::webhook_delivery::Column::CreatedAt)
        .limit(50)
        .all(db)
        .await
        .context("db: list webhook deliveries")
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
