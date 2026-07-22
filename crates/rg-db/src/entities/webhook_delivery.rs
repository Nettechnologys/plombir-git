//! Webhook delivery entity — maps to the `webhook_deliveries` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "webhook_deliveries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The webhook this delivery belongs to
    pub webhook_id: i64,
    /// Event type (push, issues, pull_request, etc.)
    pub event: String,
    /// Unique delivery UUID
    pub delivery_id: String,
    /// HTTP status code of the response (null = pending / failed to connect)
    pub response_status: Option<i32>,
    /// Request payload (JSON)
    pub request_payload: Option<String>,
    /// Response body
    pub response_body: Option<String>,
    /// Duration in milliseconds
    pub duration_ms: Option<i64>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::webhook::Entity",
        from = "Column::WebhookId",
        to = "super::webhook::Column::Id"
    )]
    Webhook,
}

impl Related<super::webhook::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Webhook.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
