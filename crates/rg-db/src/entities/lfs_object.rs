//! LFS object entity — maps to the `lfs_objects` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "lfs_objects")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Repository this LFS object belongs to
    pub repo_id: i64,
    /// LFS object OID (SHA-256 hash)
    pub oid: String,
    /// Size in bytes
    pub size: i64,
    /// Whether the object has been uploaded (exists in storage)
    pub uploaded: bool,
    /// Compression algorithm used (None = uncompressed)
    pub compression: Option<String>,
    /// Compressed size in bytes (for compressed objects)
    pub compressed_size: Option<i64>,
    pub created_at: DateTimeUtc,
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
