//! Release asset entity — maps to the `release_assets` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "release_assets")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub release_id: i64,
    pub filename: String,
    pub size: i64,
    pub content_type: String,
    pub download_count: i64,
    /// The account that uploaded the asset, or `None` once that account has
    /// been deleted — see [`super::attachment::Model::uploader_id`].
    pub uploader_id: Option<i64>,
    pub created_at: DateTimeUtc,
    /// Hex-encoded SHA-256 of the asset bytes, recorded at upload time.
    /// `None` for assets uploaded before digest tracking existed.
    pub sha256: Option<String>,
    /// Detached DSSE attestation envelope (JSON) binding this asset's SHA-256 to
    /// a signed provenance statement. `None` until the asset is explicitly
    /// signed (opt-in). See `rg_core::attestation`.
    pub attestation: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::release::Entity",
        from = "Column::ReleaseId",
        to = "super::release::Column::Id"
    )]
    Release,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UploaderId",
        to = "super::user::Column::Id"
    )]
    Uploader,
}

impl Related<super::release::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Release.def()
    }
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Uploader.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
