//! OCI tag entity — maps to the `oci_tag` table.
//!
//! A tag is a mutable name pointing at a content-addressed manifest, and the
//! mapping is many-to-one: `app:$SHA` and `app:latest` routinely name the same
//! image. The name used to live as a column on `oci_manifest` under a unique
//! key, which made that ordinary pair impossible (card_56f118bbe845).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "oci_tag")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub oci_repository_id: i64,
    /// Tag name (e.g. "latest"), unique within its repository
    pub tag: String,
    /// The `oci_manifest` row this tag currently names
    pub oci_manifest_id: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
