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
    /// The branch this run's `on:` filters were matched against — a pull
    /// request's base branch, the branch a merge group is merging into — or
    /// `None` for the events that target no branch other than the ref they
    /// carry (a push, a manual run).
    ///
    /// Recorded because a retry has to filter the same way (card_74d58ec3ac1e).
    /// Absent, the matcher falls back to the repository's *current* default
    /// branch, so the retry of a PR into `develop` is judged as a PR into
    /// `main`: its workflow matches nothing and the run silently falls through
    /// to `.forgekeep-ci.yml` — a different graph under the same `201`.
    pub base_branch: Option<String>,
    /// Where the ref stood before the event that produced this run, for the
    /// `paths:` / `paths-ignore:` filters, or `None` for a producer that had no
    /// previous revision to hand over.
    ///
    /// The other half of the same provenance (card_74d58ec3ac1e): those filters
    /// ask which files changed, which is a diff. Absent, the fallback is the
    /// commit's first parent — the whole push for a single commit, and a
    /// too-narrow range for a fast-forward of several, so a job the push ran
    /// would be skipped on its retry.
    pub previous_sha: Option<String>,
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
