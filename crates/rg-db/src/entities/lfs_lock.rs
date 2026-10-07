//! LFS lock entity — maps to the `lfs_locks` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "lfs_locks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Repository the locked path is in.
    pub repo_id: i64,
    /// The locked path, as the client spelled it. Unique per repository.
    pub path: String,
    /// The ref the client was on when it locked, when it said. Recorded, not
    /// enforced: a lock covers the path on every branch.
    pub ref_name: Option<String>,
    /// The user who holds the lock.
    pub owner_id: i64,
    pub locked_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id"
    )]
    Repository,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::OwnerId",
        to = "super::user::Column::Id"
    )]
    Owner,
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Owner.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
