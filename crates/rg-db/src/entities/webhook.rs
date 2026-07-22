//! Webhook entity — maps to the `webhooks` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "webhooks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Repository this webhook belongs to
    pub repo_id: i64,
    /// Webhook URL (target to POST to)
    pub url: String,
    /// Content type: json / form
    pub content_type: String,
    /// Secret for HMAC-SHA256 signature
    pub secret: Option<String>,
    /// Whether the webhook is active
    pub active: bool,
    /// Events that trigger this webhook (comma-separated: push,issues,pull_request,etc.)
    pub events: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id"
    )]
    Repository,
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
