//! Retired-number high-water mark — maps to the `repo_number_floors` table.
//!
//! See `migrations::m20261009_000004_repo_number_floors` for why it exists.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "repo_number_floors")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub repo_id: i64,
    /// The number space: `issue` or `pull_request`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub space: String,
    /// The highest number a deletion has retired in this space.
    pub retired_up_to: i64,
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
