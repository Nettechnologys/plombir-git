//! SeaORM entity for `pipelines` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pipelines")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    pub commit_sha: String,
    pub ref_name: String,
    pub status: String,       // pending, running, success, failed, canceled
    pub trigger_type: String, // push, manual, webhook
    pub triggered_by: Option<i64>,
    pub started_at: Option<DateTime>,
    pub finished_at: Option<DateTime>,
    pub created_at: DateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::pipeline_stage::Entity")]
    Stage,
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id"
    )]
    Repository,
}

impl Related<super::pipeline_stage::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Stage.def()
    }
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
