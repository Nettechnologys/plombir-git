//! Pull request entity — maps to the `pull_requests` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pull_requests")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Repository this PR targets
    pub repo_id: i64,
    /// Auto-incremented per-repo PR number
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    /// open / merging (transient) / closed / merged
    pub state: String,
    /// Whether the PR is still a draft and cannot be merged.
    pub is_draft: bool,
    /// Merge automatically once branch protection requirements are satisfied.
    pub auto_merge_enabled: bool,
    pub auto_merge_strategy: Option<String>,
    pub auto_merge_enabled_by_id: Option<i64>,
    pub auto_merge_enabled_at: Option<DateTimeUtc>,
    /// User who opened the PR
    pub author_id: i64,
    /// Assigned reviewer (nullable)
    pub reviewer_id: Option<i64>,
    /// The branch containing changes (source)
    pub head_branch: String,
    /// The target branch to merge into
    pub base_branch: String,
    /// Git commit SHA of the head branch at PR creation / latest push
    pub head_sha: Option<String>,
    /// Merge strategy: merge / squash / rebase (null = not yet merged)
    pub merge_strategy: Option<String>,
    /// SHA of the merge commit (null = not yet merged)
    pub merge_commit_sha: Option<String>,
    /// For fork PRs: the repository where head_branch lives (null = same as repo_id)
    pub head_repo_id: Option<i64>,
    /// Milestone id (nullable)
    pub milestone_id: Option<i64>,
    /// Label names stored as JSON array: ["bug","feature"]
    pub labels: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub closed_at: Option<DateTimeUtc>,
    pub merged_at: Option<DateTimeUtc>,
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
        from = "Column::AuthorId",
        to = "super::user::Column::Id"
    )]
    Author,
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Author.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
