//! Durable Issue, pull-request and comment attachment metadata.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "attachments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub uuid: String,
    pub repo_id: i64,
    pub uploader_id: i64,
    pub issue_id: Option<i64>,
    pub pull_request_id: Option<i64>,
    pub issue_comment_id: Option<i64>,
    pub review_comment_id: Option<i64>,
    pub filename: String,
    pub blob_key: String,
    pub content_type: String,
    pub size: i64,
    pub download_count: i64,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id"
    )]
    Repository,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UploaderId",
        to = "super::user::Column::Id"
    )]
    Uploader,
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Uploader.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
