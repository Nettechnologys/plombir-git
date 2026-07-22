//! Repository mirror entity — maps to the `mirrors` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "mirrors")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    pub url: String,
    pub username: Option<String>,
    pub password_encrypted: Option<String>,
    pub sync_interval_seconds: i64,
    pub next_sync_at: Option<DateTimeUtc>,
    pub last_sync_at: Option<DateTimeUtc>,
    pub last_sync_error: Option<String>,
    pub status: String, // "active", "inactive", "error"
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
