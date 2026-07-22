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
    pub uploader_id: i64,
    pub created_at: DateTimeUtc,
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
