//! Operations for password_reset_tokens

use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection,
    EntityTrait, QueryFilter,
};

use crate::entities::password_reset_token;

/// Create a new password reset token record.
///
/// Expired rows are dropped on the way past, before the insert. This is the
/// only statement that grows the table, so sweeping here bounds it by the links
/// still inside their fifteen-minute window — on every instance, without a
/// background loop that has to be started, configured and shut down.
///
/// That is deliberate rather than convenient. [`delete_expired`] carried the
/// doc comment "can be called periodically" and had no caller at all
/// (`card_dc5ea612d97a`), so hashes of password-reset material sat in the table
/// forever for every user who never opened the mail. The neighbour that hit the
/// same wall first — [`crate::ops::webauthn_ceremony_ops::spend`] — already
/// records this shape as the answer, and names *this* function as the
/// cautionary tale it was avoiding.
///
/// A failing sweep is propagated rather than swallowed: the delete and the
/// insert go to the same table on the same connection, so a database too unwell
/// for one is too unwell for the other, and a caller told "your reset link was
/// created" must not be told it about a row that was never written.
///
/// One consequence worth knowing when writing tests: planting an already-expired
/// row and then creating another link removes the first. Plant the expired one
/// last, or assert on it before the next `create`.
pub async fn create(
    db: &DatabaseConnection,
    user_id: i64,
    token_hash: &str,
    expires_at: chrono::DateTime<chrono::Utc>,
) -> Result<password_reset_token::Model, sea_orm::DbErr> {
    delete_expired(db).await?;

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

/// Drop the rows of reset links that can no longer be spent.
///
/// Called by [`create`] itself — see the note there for why it is not a
/// background sweep. `used` is not part of the filter on purpose: a spent link
/// inside its window is already removed by [`invalidate_user_tokens`], and
/// widening this one to "spent or expired" would make it delete rows the
/// caller's own transaction may still be looking at.
pub async fn delete_expired(db: &DatabaseConnection) -> Result<u64, sea_orm::DbErr> {
    let result = password_reset_token::Entity::delete_many()
        .filter(password_reset_token::Column::ExpiresAt.lt(Utc::now()))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}
