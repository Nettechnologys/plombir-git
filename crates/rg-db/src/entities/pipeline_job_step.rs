//! SeaORM entity for `pipeline_job_steps`: one step of a job that runs as
//! steps, and how it went.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pipeline_job_steps")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub job_id: i64,
    /// 1-based, in the order the workflow lists the steps.
    pub number: i32,
    pub name: String,
    /// `pending`, `running`, then `success` / `failure` / `skipped` /
    /// `cancelled` — the step's *outcome*.
    pub status: String,
    /// What the step counts as for the job: a `failure` under
    /// `continue-on-error` concludes as `success`. `None` until it ends.
    pub conclusion: Option<String>,
    pub exit_code: Option<i32>,
    /// The step's own log, masked like the job's.
    pub log: Option<String>,
    pub started_at: Option<DateTime>,
    pub finished_at: Option<DateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::pipeline_job::Entity",
        from = "Column::JobId",
        to = "super::pipeline_job::Column::Id"
    )]
    PipelineJob,
}

impl Related<super::pipeline_job::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PipelineJob.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
