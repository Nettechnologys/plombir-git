//! One repository a bot's owner allowed the bot to work in as an agent — maps
//! to the `agent_profile_repositories` table (ADR-0001 D6).
//!
//! Both columns are foreign keys with `ON DELETE CASCADE`: deleting the
//! profile or the repository removes the row.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "agent_profile_repositories")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub profile_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub repository_id: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::agent_profile::Entity",
        from = "Column::ProfileId",
        to = "super::agent_profile::Column::Id"
    )]
    AgentProfile,
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepositoryId",
        to = "super::repository::Column::Id"
    )]
    Repository,
}

impl ActiveModelBehavior for ActiveModel {}
