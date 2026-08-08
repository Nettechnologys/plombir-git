//! Import task entity — maps to the `import_tasks` table.
//!
//! Tracks the progress of migrating a repository and its metadata
//! (issues, PRs, labels, milestones, releases, wiki) from
//! external platforms (GitHub, GitLab) into ForgeKeep.
//!
//! ## The source platform's access token is deliberately absent
//!
//! The table still carries an `auth_token_encrypted` column (always NULL from
//! `m20260727_000002` on) and this model deliberately has no field for it: the
//! import worker is started in-process by `rg_core::import::service::start_import`
//! and is handed the token in memory, so nothing ever reads it back from the
//! row. Storing it bought nothing and cost plenty — the name promised
//! encryption that did not exist, and because handlers serialize this model
//! wholesale (`GET /imports/{id}` is polled in a loop by the progress page) the
//! user's GitHub/GitLab PAT was echoed back on every poll.
//!
//! Keep it that way: a token that must survive a restart needs real encryption
//! (`rg_core::auth::encryption`, as `mirrors.password_encrypted` does) plus a
//! response DTO — not a field re-added here.

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
    /// Target owner in ForgeKeep
    pub target_owner: String,
    /// Target repository name
    pub target_name: String,
    /// Import status: "pending" | "cloning" | "importing" | "completed" | "failed"
    pub status: String,
    /// Progress percentage (0-100)
    pub progress: i32,
    /// Current stage description (e.g., "Importing issues (15/42)")
    pub stage: Option<String>,
    /// Error message if status is "failed"
    pub error: Option<String>,
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
