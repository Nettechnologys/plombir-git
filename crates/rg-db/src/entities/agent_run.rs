//! One run of an agent a person asked a bot for — maps to the `agent_runs`
//! table (ADR-0001 D12, card_621be18a01e5).
//!
//! The row outlives the run's token and the bot itself: it is the durable
//! record that a pull request was written by an agent, which the CI gate asks
//! for every new head commit. `bot_id` and `triggered_by` therefore carry no
//! foreign key; a missing account reads as a ghost.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "agent_runs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub bot_id: i64,
    pub repository_id: i64,
    /// The issue the run works on; `None` once that issue is deleted.
    pub issue_id: Option<i64>,
    /// The pull request an iteration works on; `None` for the first run of an
    /// issue.
    pub pull_request_id: Option<i64>,
    /// 1 for the first run of an issue, then one more per iteration.
    pub iteration: i64,
    /// The person who asked for the run.
    pub triggered_by: i64,
    /// What asked for the run, unique: a second delivery of one event inserts
    /// nothing.
    #[sea_orm(unique)]
    pub trigger_key: String,
    /// `queued`, `running`, `succeeded`, `failed` or `cancelled`.
    pub status: String,
    /// `Some(true)` while the run is not finished, `None` after. Part of the
    /// unique `(repository_id, bot_id, active)` index: one active run per bot
    /// in a repository.
    pub active: Option<bool>,
    /// The pull request the run opened or updated.
    pub result_pull_request_id: Option<i64>,
    pub failure_reason: Option<String>,
    /// The refs the run's tokens may write: `refs/heads/agent/<bot>/` for the
    /// first run of an issue, the pull request's branch for an iteration. Kept
    /// after the token is gone, so the CI gate still knows which branches hold
    /// the agent's code (ADR-0001 D4). `None` for a run that never had one.
    pub write_ref_prefix: Option<String>,
    pub created_at: DateTimeUtc,
    pub finished_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepositoryId",
        to = "super::repository::Column::Id"
    )]
    Repository,
    #[sea_orm(has_many = "super::agent_run_attempt::Entity")]
    Attempts,
}

impl Related<super::agent_run_attempt::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Attempts.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
