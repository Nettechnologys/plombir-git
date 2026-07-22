//! Issue entity — maps to the `issues` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "issues")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Repository this issue belongs to
    pub repo_id: i64,
    /// Auto-incremented per-repo issue number (1, 2, 3, …)
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    /// open / closed
    pub state: String,
    /// User who created the issue
    pub author_id: i64,
    /// Assigned user (nullable)
    pub assignee_id: Option<i64>,
    /// Milestone id (nullable)
    pub milestone_id: Option<i64>,
    /// Label names stored as JSON array: ["bug","help-wanted"]
    pub labels: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub closed_at: Option<DateTimeUtc>,
    pub deleted_at: Option<DateTimeUtc>,
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
    #[sea_orm(has_many = "super::issue_comment::Entity")]
    IssueComment,
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

impl Related<super::issue_comment::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::IssueComment.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
