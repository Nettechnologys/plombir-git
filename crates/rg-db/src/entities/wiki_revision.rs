//! Wiki revision entity — snapshot saved on every page update.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "wiki_revisions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub wiki_page_id: i64,
    pub content: String,
    pub message: Option<String>,
    pub author_id: Option<i64>,
    pub version: i32,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::wiki_page::Entity",
        from = "Column::WikiPageId",
        to = "super::wiki_page::Column::Id"
    )]
    WikiPage,
}

impl Related<super::wiki_page::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::WikiPage.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
