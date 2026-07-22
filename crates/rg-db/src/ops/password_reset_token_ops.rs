//! Operations for password_reset_tokens

use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection,
    EntityTrait, QueryFilter,
};

use crate::entities::password_reset_token;

/// Create a new password reset token record.
pub async fn create(
    db: &DatabaseConnection,
    user_id: i64,
    token_hash: &str,
    expires_at: chrono::DateTime<chrono::Utc>,
) -> Result<password_reset_token::Model, sea_orm::DbErr> {
    let model = password_reset_token::ActiveModel {
        user_id: Set(user_id),
        token_hash: Set(token_hash.to_string()),
        expires_at: Set(expires_at),
        used: Set(false),
        created_at: Set(Utc::now()),
        ..Default::default()
    };
    model.insert(db).await
}

/// Find a token record by its hash (for validation).
pub async fn find_by_hash(
    db: &DatabaseConnection,
    token_hash: &str,
) -> Result<Option<password_reset_token::Model>, sea_orm::DbErr> {
    password_reset_token::Entity::find()
        .filter(password_reset_token::Column::TokenHash.eq(token_hash))
        .one(db)
        .await
}

/// Mark a token as used.
pub async fn mark_used(db: &DatabaseConnection, token_id: i64) -> Result<(), sea_orm::DbErr> {
    password_reset_token::Entity::update_many()
        .col_expr(password_reset_token::Column::Used, Expr::value(true))
        .filter(password_reset_token::Column::Id.eq(token_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Invalidate all unused tokens for a user (e.g., after successful reset).
pub async fn invalidate_user_tokens(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<(), sea_orm::DbErr> {
    password_reset_token::Entity::delete_many()
        .filter(password_reset_token::Column::UserId.eq(user_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Clean up expired tokens (can be called periodically).
pub async fn delete_expired(db: &DatabaseConnection) -> Result<u64, sea_orm::DbErr> {
    let result = password_reset_token::Entity::delete_many()
        .filter(password_reset_token::Column::ExpiresAt.lt(Utc::now()))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}
