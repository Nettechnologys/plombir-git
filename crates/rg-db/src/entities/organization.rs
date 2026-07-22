//! SeaORM entity for `organizations` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "organizations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Unique organization slug (e.g. "acme-corp")
    pub name: String,
    /// Display name (e.g. "Acme Corporation")
    pub display_name: Option<String>,
    pub description: Option<String>,
    /// User who created / owns the organization
    pub owner_id: i64,
    /// Organization visibility: "public" or "private"
    pub visibility: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub deleted_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::OwnerId",
        to = "super::user::Column::Id"
    )]
    Owner,
    #[sea_orm(has_many = "super::team::Entity")]
    Teams,
    #[sea_orm(has_many = "super::organization_member::Entity")]
    Members,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Owner.def()
    }
}

impl Related<super::team::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Teams.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
