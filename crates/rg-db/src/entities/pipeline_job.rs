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
    /// Encrypted snapshot of the secret values sent to an external runner.
    /// Retained until the runner finishes so log retries use the same mask.
    #[serde(skip_serializing)]
    pub mask_secrets_encrypted: Option<String>,
    pub cache_key: Option<String>,
    /// JSON array of workspace-relative cache paths.
    pub cache_paths: Option<String>,
    /// The artifact this job publishes after a successful run, as the JSON
    /// object `{"name": "...", "paths": ["..."]}`. `None` when the job declares
    /// no artifact.
    pub artifacts: Option<String>,
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
    /// Unicode character count of the legacy base plus append-only chunks.
    pub log_length: i64,
    pub started_at: Option<DateTime>,
    pub finished_at: Option<DateTime>,
    pub updated_at: Option<DateTime>,
    #[sea_orm(nullable)]
    pub tags: Option<String>, // JSON array of runner tag requirements
    /// The job's steps as a JSON array of `rg_process::ci_steps::StepSpec`,
    /// for a job translated from an Actions workflow; `None` for a native
    /// job, whose `script` is the one thing it runs. A runner executes these
    /// one by one and reports each into `pipeline_job_steps`.
    pub steps: Option<String>,
    /// For a job of steps: its `needs:` (alias → job name in the pipeline) and
    /// its `outputs:` templates, as `rg_process::ci_steps::JobContext` JSON.
    pub job_context: Option<String>,
    /// The job's resolved, masked `outputs:` as a JSON object, once its last
    /// step ran; what `needs.<id>.outputs.*` reads in the jobs after it.
    pub outputs: Option<String>,
    /// What a job whose matrix reads a needed job's outputs will expand from,
    /// on the one placeholder row that stands for it until its stage is
    /// reached. Such a row is never handed to a runner.
    pub deferred_matrix: Option<String>,
    /// What decides whether an Actions job that lists `needs:` runs at all —
    /// its `if:`, which may read what the needed jobs left, and whether one
    /// of them was skipped — until its stage is reached
    /// (`m20261021_000002_pipeline_job_deferred_condition`). Such a row is
    /// never handed to a runner; the column is cleared once it may run.
    pub deferred_condition: Option<String>,
    /// The matrix job this row is a variant of — its name in the pipeline —
    /// and that job's `strategy.fail-fast` / `strategy.max-parallel`.
    pub matrix_group: Option<String>,
    pub matrix_fail_fast: Option<bool>,
    pub matrix_max_parallel: Option<i32>,
    /// An Actions job's `jobs.<id>.name`, rendered for its matrix variant —
    /// what the job is shown under beside [`Self::name`], which stays the id
    /// `needs:` and artifacts address it by. `None` without one.
    pub display_name: Option<String>,
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
