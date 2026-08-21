//! PR Review entity — maps to the `pr_reviews` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pr_reviews")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Pull request this review belongs to
    pub pr_id: i64,
    /// Repository ID (denormalized for efficient queries)
    pub repo_id: i64,
    /// User who submitted the review
    pub reviewer_id: i64,
    /// comment / approve / request_changes. `dismiss` also occurs, but only on
    /// rows imported from a foreign forge that reported a withdrawn review
    /// without saying which verdict it had been.
    pub action: String,
    /// Review body text (nullable for approve)
    pub body: Option<String>,
    /// Commit SHA being reviewed
    pub commit_id: Option<String>,
    pub created_at: DateTimeUtc,
    /// When this review was withdrawn, if it was. A dismissal is a property of
    /// the review it dismisses, not a separate opinion by whoever dismissed it
    /// — `count_current_approvals` reads this column and nothing else to decide
    /// whether the verdict still stands (card_dc0f5d58e5f4).
    pub dismissed_at: Option<DateTimeUtc>,
    /// Who withdrew the review. Durable history: it survives the dismissor's
    /// account, exactly as `reviewer_id` survives the reviewer's.
    pub dismissed_by: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::pull_request::Entity",
        from = "Column::PrId",
        to = "super::pull_request::Column::Id"
    )]
    PullRequest,
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id"
    )]
    Repository,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::ReviewerId",
        to = "super::user::Column::Id"
    )]
    Reviewer,
    #[sea_orm(has_many = "super::review_comment::Entity")]
    Comments,
}

impl Related<super::pull_request::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PullRequest.def()
    }
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Reviewer.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
