//! Database operations for access tokens.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::access_token::{
    self, ActiveModel, Entity as TokenEntity, Model as AccessToken,
};

/// Find a token by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<AccessToken>> {
    TokenEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find access token by id")
}

/// Find a token by its SHA-256 hash.
pub async fn find_by_hash(db: &DatabaseConnection, hash: &str) -> Result<Option<AccessToken>> {
    TokenEntity::find()
        .filter(access_token::Column::TokenHash.eq(hash))
        .one(db)
        .await
        .context("db: find access token by hash")
}

/// List all tokens for a user.
pub async fn list_by_user(db: &DatabaseConnection, user_id: i64) -> Result<Vec<AccessToken>> {
    TokenEntity::find()
        .filter(access_token::Column::UserId.eq(user_id))
        .order_by_asc(access_token::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list access tokens by user")
}

/// Create a new access token.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<AccessToken> {
    model.insert(db).await.context("db: create access token")
}

/// Delete a token by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    TokenEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete access token")?;
    Ok(())
}
