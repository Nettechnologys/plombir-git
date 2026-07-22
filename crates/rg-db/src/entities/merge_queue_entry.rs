//! Repository merge-queue entry.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "merge_queue_entries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    pub pr_id: i64,
    pub enqueued_by_id: i64,
    pub strategy: String,
    /// queued / running / merged / failed / canceled
    pub status: String,
    pub failure_reason: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub started_at: Option<DateTimeUtc>,
    pub finished_at: Option<DateTimeUtc>,
    pub merge_group_sha: Option<String>,
    pub merge_group_base_sha: Option<String>,
    pub merge_group_head_sha: Option<String>,
    pub merge_group_pipeline_id: Option<i64>,
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
        belongs_to = "super::pull_request::Entity",
        from = "Column::PrId",
        to = "super::pull_request::Column::Id"
    )]
    PullRequest,
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl Related<super::pull_request::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PullRequest.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
