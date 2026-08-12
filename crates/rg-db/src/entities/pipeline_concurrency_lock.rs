//! CI concurrency-group lock entity — maps to `pipeline_concurrency_locks`.
//!
//! Rows are durable lock identities, not leases: an UPSERT of one row inside a
//! transaction holds the database's write lock for exactly that transaction.
//! This lets every ForgeKeep process arbitrate the same `(repo_id, group_name)`
//! before it reads active pipelines or publishes a replacement graph.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pipeline_concurrency_locks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Repository scope of the resolved `concurrency.group`.
    pub repo_id: i64,
    /// Resolved group name. Unique together with `repo_id` in the migration.
    pub group_name: String,
    /// Last acquisition time, updated so a conflicting UPSERT always performs
    /// a real row write and therefore holds the row lock until commit.
    pub touched_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id",
        on_delete = "Cascade"
    )]
    Repository,
}

impl ActiveModelBehavior for ActiveModel {}
