//! Review comment entity — inline comments on specific diff lines.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "review_comments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Review this comment belongs to
    pub review_id: i64,
    /// Pull request this comment belongs to
    pub pr_id: i64,
    /// User who wrote the comment
    pub author_id: i64,
    /// File path being commented on
    pub path: String,
    /// Position in the diff (deprecated by GitHub, kept for compat)
    pub position: Option<i64>,
    /// Line number in the file
    pub line: Option<i64>,
    /// Start of a multi-line range; null means the range starts at `line`.
    pub start_line: Option<i64>,
    /// LEFT (base) or RIGHT (head)
    pub side: Option<String>,
    pub start_side: Option<String>,
    /// Comment text
    pub body: String,
    /// Replacement text for the commented head-side line.
    pub suggestion: Option<String>,
    pub suggestion_applied_at: Option<DateTimeUtc>,
    pub suggestion_applied_by_id: Option<i64>,
    pub suggestion_commit_sha: Option<String>,
    /// Commit SHA
    pub commit_id: Option<String>,
    /// ID of the comment this is replying to (nullable for top-level)
    pub reply_to_id: Option<i64>,
    /// Resolution state lives on the top-level comment of a thread.
    pub resolved_at: Option<DateTimeUtc>,
    pub resolved_by_id: Option<i64>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::pr_review::Entity",
        from = "Column::ReviewId",
        to = "super::pr_review::Column::Id"
    )]
    Review,
    #[sea_orm(
        belongs_to = "super::pull_request::Entity",
        from = "Column::PrId",
        to = "super::pull_request::Column::Id"
    )]
    PullRequest,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::AuthorId",
        to = "super::user::Column::Id"
    )]
    Author,
}

impl Related<super::pr_review::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Review.def()
    }
}

impl Related<super::pull_request::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PullRequest.def()
    }
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Author.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
