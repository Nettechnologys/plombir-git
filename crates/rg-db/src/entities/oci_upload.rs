//! OCI Upload entity — maps to the `oci_upload` table.
//!
//! Tracks in-progress chunked blob uploads (session-based).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "oci_upload")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub oci_repository_id: i64,
    /// Upload session UUID
    pub uuid: String,
    /// Expected digest (known after final chunk)
    pub digest: Option<String>,
    /// Bytes uploaded so far
    pub bytes_uploaded: i64,
    /// Temp file path for accumulating chunks
    pub upload_path: String,
    pub created_at: DateTimeUtc,
    /// Sessions expire after 24h
    pub expires_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
