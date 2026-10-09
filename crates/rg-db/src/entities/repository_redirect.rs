//! Old repository address — maps to the `repository_redirects` table.
//!
//! See `migrations::m20261009_000920_repository_redirects`.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "repository_redirects")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// `user` or `org`: the table `namespace_id` is a row of.
    pub namespace_kind: String,
    pub namespace_id: i64,
    /// The name the repository left.
    pub name: String,
    /// The repository it went to; its current name is on its own row.
    pub repo_id: i64,
    pub created_at: DateTimeUtc,
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
