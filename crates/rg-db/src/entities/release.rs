//! Release entity — maps to the `releases` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "releases")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    pub tag_name: String,
    pub target_commitish: String,
    pub title: String,
    pub body: Option<String>,
    pub is_draft: bool,
    pub is_prerelease: bool,
    pub author_id: i64,
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
    #[sea_orm(has_many = "super::release_asset::Entity")]
    ReleaseAsset,
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

impl Related<super::release_asset::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ReleaseAsset.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
