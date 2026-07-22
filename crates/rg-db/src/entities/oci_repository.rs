//! OCI Repository entity — maps to the `oci_repository` table.
//!
//! Links an IronForge repository to an OCI container registry namespace.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "oci_repository")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    /// OCI namespace (e.g. "library/ubuntu")
    pub namespace: String,
    pub owner_id: i64,
    pub is_public: bool,
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
