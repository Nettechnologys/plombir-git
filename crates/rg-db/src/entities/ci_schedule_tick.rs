//! SeaORM entity for `ci_schedule_ticks`: the last claimed tick of one
//! `on: schedule` cron entry.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ci_schedule_ticks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    /// The workflow file name under `.gitea/workflows/`.
    pub workflow: String,
    /// The `- cron:` value as the file spells it.
    pub cron: String,
    /// The last tick claimed for this entry, in whole minutes since the Unix
    /// epoch. Claiming the next one is a conditional `UPDATE` on this column.
    pub last_tick_minute: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id"
    )]
    Repository,
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
