//! Package Version entity — maps to the `package_versions` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "package_versions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub package_id: i64,
    pub version: String,
    /// Normalized semver string (for ordering/comparison)
    pub semver: Option<String>,
    /// Package-type-specific metadata (JSON)
    pub metadata: Option<String>,
    pub size: i64,
    pub sha256: Option<String>,
    pub is_yanked: bool,
    pub download_count: i64,
    pub author_id: Option<i64>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::package::Entity",
        from = "Column::PackageId",
        to = "super::package::Column::Id"
    )]
    Package,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::AuthorId",
        to = "super::user::Column::Id"
    )]
    Author,
    #[sea_orm(has_many = "super::package_file::Entity")]
    PackageFile,
}

impl Related<super::package::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Package.def()
    }
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Author.def()
    }
}

impl Related<super::package_file::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PackageFile.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
