//! OCI Manifest entity — maps to the `oci_manifest` table.
//!
//! Purely content-addressed: one row per `(repository, digest)`. The names an
//! image answers to live in [`super::oci_tag`], because a tag column here could
//! only ever hold one of them (card_56f118bbe845).
//!
//! Who published an image lives in `audit_log` for the same reason: the row
//! belongs to the bytes, so a `push_by` column here could only ever name the
//! *first* publisher of those bytes and left every later re-tag unattributed
//! (card_b70de2169bd6, `m20260823_000001_oci_manifest_push_audit`).

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
    /// OCI media type (e.g. "application/vnd.docker.distribution.manifest.v2+json")
    pub media_type: String,
    /// Manifest JSON size in bytes
    pub size: i64,
    /// The raw manifest JSON content
    pub manifest_json: String,
    /// Schema version (1 or 2)
    pub schema_version: i32,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
