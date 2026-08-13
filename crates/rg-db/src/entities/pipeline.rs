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
    /// The resolved `concurrency.group` this pipeline belongs to, or `None` when
    /// its workflow declared no `concurrency:` block.
    ///
    /// This is what "what does this pipeline have to wait for" is answered
    /// from. `None` neither waits nor is waited for: a workflow that asked for
    /// no serialization must not be cancelled by one that did (card_4c5214698ae9).
    pub concurrency_group: Option<String>,
    /// The named `workflow_dispatch` inputs this run was started with, as a
    /// JSON object of the caller's own map, or `None` for every producer that
    /// has none.
    ///
    /// This is the provenance a retry replays (card_24f475c09a17). The values
    /// are stored unresolved on purpose: a retry re-reads the same commit, so
    /// re-resolving them against the same declarations reproduces the original
    /// run — while the resolved form would bake defaults into a row that never
    /// asked for them. Nothing else can stand in for it: the job environment
    /// carries normalized `INPUT_*` names, which a reusable child job may have
    /// overwritten with its own `workflow_call` input of the same name.
    pub dispatch_inputs: Option<String>,
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
