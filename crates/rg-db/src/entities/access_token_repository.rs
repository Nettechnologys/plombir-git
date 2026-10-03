//! One repository a restricted Personal Access Token may reach — maps to the
//! `access_token_repositories` table.
//!
//! Both columns are foreign keys with `ON DELETE CASCADE`: revoking the token
//! or deleting the repository removes the row. Whether the token is restricted
//! at all is `access_tokens.repo_restricted`, so losing the last row leaves the
//! token confined to nothing.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "access_token_repositories")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub token_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub repository_id: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::access_token::Entity",
        from = "Column::TokenId",
        to = "super::access_token::Column::Id"
    )]
    AccessToken,
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepositoryId",
        to = "super::repository::Column::Id"
    )]
    Repository,
}

impl ActiveModelBehavior for ActiveModel {}
