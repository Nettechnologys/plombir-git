//! Package entity — maps to the `packages` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "packages")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub package_registry_id: i64,
    pub owner_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub repository_url: Option<String>,
    pub is_public: bool,
    pub download_count: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::package_registry::Entity",
        from = "Column::PackageRegistryId",
        to = "super::package_registry::Column::Id"
    )]
    PackageRegistry,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::OwnerId",
        to = "super::user::Column::Id"
    )]
    Owner,
    #[sea_orm(has_many = "super::package_version::Entity")]
    PackageVersion,
}

impl Related<super::package_registry::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PackageRegistry.def()
    }
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Owner.def()
    }
}

impl Related<super::package_version::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PackageVersion.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
