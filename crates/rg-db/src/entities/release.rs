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
    /// The account that published the release, or `None` once that account has
    /// been deleted. A release belongs to its repository, not to its author, so
    /// the column is `ON DELETE SET NULL` — otherwise deleting the author took
    /// the whole release, and through `release_assets.release_id`, every asset
    /// anybody had uploaded to it
    /// (`m20260805_000002_uploads_outlive_their_uploader`).
    pub author_id: Option<i64>,
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
