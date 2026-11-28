//! How many agent runs one bot started on one UTC day — maps to the
//! `agent_run_quotas` table (ADR-0001 D9, card_b391e92474a0).
//!
//! The daily limit is reserved by a conditional update of `used` in the
//! transaction that records the run, never by counting runs first; see
//! [`crate::ops::agent_run_ops::reserve_daily_run`].

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "agent_run_quotas")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// No foreign key, as on `agent_runs`: the count outlives the bot with the
    /// runs it counts.
    pub bot_id: i64,
    /// `YYYY-MM-DD`, UTC; `(bot_id, day)` is unique.
    pub day: String,
    pub used: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
