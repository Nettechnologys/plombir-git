//! Protected branch entity — maps to the `protected_branches` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "protected_branches")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Repository this rule applies to
    pub repo_id: i64,
    /// Branch name pattern (exact match or glob)
    pub branch_name: String,
    /// Whether a PR is required to merge into this branch
    pub require_pr: bool,
    /// Whether status checks must pass before merging
    pub require_status_check: bool,
    /// JSON array of required status check names
    pub required_status_checks: Option<String>,
    /// Whether approval is required before merging
    pub require_approval: bool,
    /// Number of required approvals
    pub required_approvals: Option<i64>,
    /// Whether force push is allowed
    pub allow_force_push: bool,
    /// Require every newly introduced commit to pass `git verify-commit`.
    pub require_signed_commits: bool,
    /// JSON array of user IDs allowed to push directly
    pub allowed_push_user_ids: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
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
