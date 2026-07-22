//! SeaORM entity for `pipeline_jobs` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pipeline_jobs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub stage_id: i64,
    pub runner_id: Option<i64>,
    pub name: String,
    pub image: Option<String>, // container image (future: Docker runner)
    pub script: String,        // shell commands (newline separated)
    /// JSON object of non-secret variables captured when the pipeline is created.
    pub variables: Option<String>,
    pub cache_key: Option<String>,
    /// JSON array of workspace-relative cache paths.
    pub cache_paths: Option<String>,
    pub allow_failure: bool,
    pub timeout_seconds: Option<i64>,
    /// Execution policy captured from CI config (`on_success` or `manual`).
    pub when_condition: String,
    pub if_condition: Option<String>,
    pub environment_id: Option<i64>,
    pub environment_name: Option<String>,
    pub status: String, // pending, running, success, failed, skipped
    pub exit_code: Option<i32>,
    pub log: Option<String>,
    pub started_at: Option<DateTime>,
    pub finished_at: Option<DateTime>,
    pub updated_at: Option<DateTime>,
    #[sea_orm(nullable)]
    pub tags: Option<String>, // JSON array of runner tag requirements
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::pipeline_stage::Entity",
        from = "Column::StageId",
        to = "super::pipeline_stage::Column::Id"
    )]
    PipelineStage,
}

impl Related<super::pipeline_stage::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PipelineStage.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
