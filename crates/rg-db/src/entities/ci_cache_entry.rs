use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ci_cache_entries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    pub key_hash: String,
    pub file_path: String,
    pub size: i64,
    /// Hex-encoded SHA-256 of the archive *contents* (distinct from `key_hash`,
    /// which digests the cache key). Nullable: legacy entries predate digest
    /// tracking and are served without an integrity guard.
    pub sha256: Option<String>,
    pub created_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
