//! OCI Manifest entity — maps to the `oci_manifest` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "oci_manifest")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub oci_repository_id: i64,
    /// Content digest (e.g. "sha256:abc123...")
    pub digest: String,
    /// Tag name (e.g. "latest"), null for untagged manifests
    pub tag: Option<String>,
    /// OCI media type (e.g. "application/vnd.docker.distribution.manifest.v2+json")
    pub media_type: String,
    /// Manifest JSON size in bytes
    pub size: i64,
    /// The raw manifest JSON content
    pub manifest_json: String,
    /// Schema version (1 or 2)
    pub schema_version: i32,
    /// User who pushed this manifest
    pub push_by: Option<i64>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
