//! OCI Blob entity — maps to the `oci_blob` table.
//!
//! Content-addressed blob storage for OCI container images.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "oci_blob")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub oci_repository_id: i64,
    /// Content digest (e.g. "sha256:abc123...")
    pub digest: String,
    /// OCI media type (e.g. "application/vnd.docker.image.rootfs.diff.tar.gzip")
    pub media_type: String,
    /// Blob size in bytes
    pub size: i64,
    /// File-system path to the stored blob
    pub storage_path: String,
    /// Reference count — blob is garbage-collected when this reaches 0
    pub ref_count: i32,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
