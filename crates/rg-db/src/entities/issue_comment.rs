//! Issue comment entity — maps to the `issue_comments` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "issue_comments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The issue this comment belongs to
    pub issue_id: i64,
    /// User who wrote the comment
    pub author_id: i64,
    /// Who wrote this on the platform it was imported from — a byline beside
    /// the local author, never in place of it (card_04088376105b). `None` for
    /// everything written here.
    pub original_author: Option<String>,
    /// The source platform's numeric id for [`Self::original_author`], which a
    /// rename cannot reassign.
    pub original_author_id: Option<i64>,
    /// The platform instance that issued [`Self::original_author_id`]: `github`
    /// (github.com), `gitlab` (gitlab.com), or `github:<host>` / `gitlab:<host>`
    /// for any other server, whose ids are never matched to a sign-in.
    pub original_service: Option<String>,
    pub body: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::issue::Entity",
        from = "Column::IssueId",
        to = "super::issue::Column::Id"
    )]
    Issue,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::AuthorId",
        to = "super::user::Column::Id"
    )]
    Author,
}

impl Related<super::issue::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Issue.def()
    }
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Author.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
