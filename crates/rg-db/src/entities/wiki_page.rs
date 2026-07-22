//! Wiki page entity — maps to the `wiki_pages` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "wiki_pages")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Repository this wiki belongs to
    pub repo_id: i64,
    /// Page title (used as slug, e.g. "Home" → "Home.md")
    pub title: String,
    /// Page content (Markdown)
    pub content: String,
    /// Commit message for this revision
    pub message: Option<String>,
    /// User who last edited the page
    pub author_id: Option<i64>,
    /// Git blob SHA of this revision
    pub sha: Option<String>,
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
