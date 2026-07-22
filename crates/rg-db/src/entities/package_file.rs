//! Package File entity — maps to the `package_files` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "package_files")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub version_id: i64,
    pub filename: String,
    pub size: i64,
    pub sha256: Option<String>,
    pub storage_path: String,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::package_version::Entity",
        from = "Column::VersionId",
        to = "super::package_version::Column::Id"
    )]
    PackageVersion,
}

impl Related<super::package_version::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PackageVersion.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
