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

/// Spend a reset link, reporting whether this call is the one that spent it.
///
/// A compare-and-swap, not a write: the `WHERE` names the exact state the
/// caller believed it was acting on — a live, unspent link — so the database
/// picks the winner in one statement. The predecessor, `mark_used`, filtered on
/// the id alone and dropped `rows_affected`, which left the "is it still
/// unspent?" question answered in application memory a full Argon2 pass before
/// the answer was acted on. Two requests carrying the same link both read
/// `used = false`, both wrote a password, and both were answered a session.
///
/// The expiry is re-asserted here for the same reason `assign_job` restates its
/// candidate query: a caller that checked it earlier checked it against a
/// different instant, and a gate that only holds when every caller remembers to
/// check first is a convention, not a gate.
///
/// `false` is an ordinary outcome — the link is spent, expired or gone — and
/// belongs to whoever holds a dead link, not to a failure.
pub async fn consume(db: &DatabaseConnection, token_id: i64) -> Result<bool, sea_orm::DbErr> {
    let result = password_reset_token::Entity::update_many()
        .col_expr(password_reset_token::Column::Used, Expr::value(true))
        .filter(password_reset_token::Column::Id.eq(token_id))
        .filter(password_reset_token::Column::Used.eq(false))
        .filter(password_reset_token::Column::ExpiresAt.gt(Utc::now()))
        .exec(db)
        .await?;
    Ok(result.rows_affected == 1)
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
