//! Artifact entity — maps to the `artifacts` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "artifacts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub job_id: i64,
    pub name: String,
    pub file_path: String,
    pub size: i64,
    pub created_at: DateTimeUtc,
    pub expires_at: Option<DateTimeUtc>,
    /// Hex-encoded SHA-256 of the artifact bytes, recorded at upload. `None` for
    /// legacy artifacts uploaded before digest tracking existed.
    pub sha256: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
