//! One attempt at executing an agent run — maps to the `agent_run_attempts`
//! table (ADR-0001 D12, card_621be18a01e5).
//!
//! A runner lost mid-run is a new attempt, never an overwrite: the history of
//! which pipeline carried which token stays. The attempt's token points here
//! (`access_tokens.agent_run_attempt_id`), so deleting the run takes its
//! attempts and their tokens with it.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "agent_run_attempts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub run_id: i64,
    /// 1, 2, … within the run; `(run_id, attempt)` is unique.
    pub attempt: i64,
    /// The pipeline that carries the attempt; `None` once it is deleted.
    pub pipeline_id: Option<i64>,
    pub created_at: DateTimeUtc,
    pub finished_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::agent_run::Entity",
        from = "Column::RunId",
        to = "super::agent_run::Column::Id"
    )]
    Run,
}

impl Related<super::agent_run::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Run.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
