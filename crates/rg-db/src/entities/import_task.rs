//! Import task entity — maps to the `import_tasks` table.
//!
//! Tracks the progress of migrating a repository and its metadata
//! (issues, PRs, labels, milestones, releases, wiki) from
//! external platforms (GitHub, GitLab) into IronForge.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "import_tasks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// User who initiated the import
    pub user_id: i64,
    /// Target repository (null until repo is created)
    pub repo_id: Option<i64>,
    /// Source platform: "github" or "gitlab"
    pub platform: String,
    /// Source repository URL (e.g., https://github.com/user/repo)
    pub source_url: String,
    /// Target owner in IronForge
    pub target_owner: String,
    /// Target repository name
    pub target_name: String,
    /// Encrypted API access token
    pub auth_token_encrypted: Option<String>,
    /// Import status: "pending" | "cloning" | "importing" | "completed" | "failed"
    pub status: String,
    /// Progress percentage (0-100)
    pub progress: i32,
    /// Current stage description (e.g., "Importing issues (15/42)")
    pub stage: Option<String>,
    /// Error message if status is "failed"
    pub error: Option<String>,
    /// JSON mapping of external user logins to local user IDs
    pub user_mapping: Option<String>,
    pub import_repo: bool,
    pub import_issues: bool,
    pub import_pull_requests: bool,
    pub import_wiki: bool,
    pub import_releases: bool,
    pub import_labels: bool,
    pub import_milestones: bool,
    /// JSON statistics after completion
    pub stats: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id"
    )]
    Repository,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
