//! Runner entity — maps to the `runners` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "runners")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub name: String,
    pub token: String,
    pub status: String,
    pub labels: String,
    pub last_seen_at: DateTimeUtc,
    pub version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    // No relations defined yet
}

impl ActiveModelBehavior for ActiveModel {}
