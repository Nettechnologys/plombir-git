//! SeaORM entity for `notifications` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "notifications")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// User who receives this notification
    pub user_id: i64,
    /// Event type: "push", "issue", "pr", "review", "pipeline", etc.
    pub event_type: String,
    /// Short title
    pub title: String,
    /// Detailed body (optional)
    pub body: Option<String>,
    /// Associated repository (optional)
    pub repo_id: Option<i64>,
    /// Whether the user has read this notification
    pub is_read: bool,
    pub created_at: DateTimeUtc,
    /// Why this user got it: `review_requested`, `assigned`, `mention`,
    /// `ci_failed`, `participating`; absent on a repository-watch row.
    pub reason: Option<String>,
    /// What it is about — `issue` or `pull_request` — with [`Self::subject_id`].
    /// One unread row per subject and user: a later event in the same thread
    /// updates it instead of stacking another.
    pub subject_type: Option<String>,
    pub subject_id: Option<i64>,
    /// The page it opens, relative to the instance (`/owner/repo/issues/7`).
    pub link: Option<String>,
    /// When a later event last folded into this row.
    pub updated_at: Option<DateTimeUtc>,
    /// The mail dispatcher still owes the recipient a message for this row.
    #[serde(skip)]
    pub email_pending: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
