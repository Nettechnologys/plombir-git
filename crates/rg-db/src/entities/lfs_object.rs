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
    pub created_at: DateTimeUtc,
    /// Token of the request currently publishing this object's blob, if any.
    ///
    /// Publication writes a stable content-addressed key, so two concurrent
    /// first uploads of one `oid` are indistinguishable at the storage layer.
    /// Holding this token is what lets a failed publication prove the bytes
    /// under that key are its own before rolling them back.
    pub publisher_token: Option<String>,
    /// When the current publisher took the lease — the basis for taking over
    /// a lease left behind by a process that died mid-publication.
    pub publisher_since: Option<DateTimeUtc>,
    /// When a request last answered "already stored" for this object — a batch
    /// upload the client then skips, or an import or merge that found it
    /// present. Whoever got that answer is about to point a ref at the object,
    /// so removing unused objects treats this like a fresh upload.
    pub last_claimed_at: Option<DateTimeUtc>,
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
