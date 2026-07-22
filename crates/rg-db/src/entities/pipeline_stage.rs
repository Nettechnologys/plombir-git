//! SeaORM entity for `pipeline_stages` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pipeline_stages")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub pipeline_id: i64,
    pub name: String,
    pub stage_order: i32,
    pub status: String, // pending, running, success, failed, skipped
    pub started_at: Option<DateTime>,
    pub finished_at: Option<DateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::pipeline_job::Entity")]
    Job,
    #[sea_orm(
        belongs_to = "super::pipeline::Entity",
        from = "Column::PipelineId",
        to = "super::pipeline::Column::Id"
    )]
    Pipeline,
}

impl Related<super::pipeline_job::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Job.def()
    }
}

impl Related<super::pipeline::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Pipeline.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
