//! Operations for password_reset_tokens

use anyhow::Context;
use chrono::Utc;
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection,
    EntityTrait, QueryFilter, TransactionTrait,
};

use crate::entities::{password_reset_token, user};

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
pub async fn consume<C>(db: &C, token_id: i64) -> Result<bool, sea_orm::DbErr>
where
    C: sea_orm::ConnectionTrait,
{
    let result = password_reset_token::Entity::update_many()
        .col_expr(password_reset_token::Column::Used, Expr::value(true))
        .filter(password_reset_token::Column::Id.eq(token_id))
        .filter(password_reset_token::Column::Used.eq(false))
        .filter(password_reset_token::Column::ExpiresAt.gt(Utc::now()))
        .exec(db)
        .await?;
    Ok(result.rows_affected == 1)
}

/// Complete a password reset without spending the link ahead of a doomed write.
///
/// The token claim, password write, session-generation bump and sibling-token
/// invalidation are one decision. In particular, an account retirement that
/// wins after the caller's validation read makes the conditional user update
/// affect no row; that ordinary refusal rolls the token claim back instead of
/// turning a usable one-time link into collateral damage from the race.
///
/// The first statement in the transaction is a write. That matters on SQLite:
/// taking a read snapshot and then trying to upgrade it after another writer
/// commits produces `SQLITE_BUSY_SNAPSHOT`, while a write-first transaction can
/// be retried safely because none of this function's effects escape before the
/// commit.
pub async fn complete_password_reset(
    db: &DatabaseConnection,
    token_id: i64,
    user_id: i64,
    password_hash: &str,
) -> anyhow::Result<Option<user::Model>> {
    crate::contention::retry_transaction("complete password reset", || async move {
        let transaction = db
            .begin()
            .await
            .context("db: begin password reset completion")?;
        let result: anyhow::Result<Option<user::Model>> = async {
            if !consume(&transaction, token_id)
                .await
                .context("db: consume password reset token")?
            {
                return Ok(None);
            }

            let now = Utc::now();
            let updated = user::Entity::update_many()
                .col_expr(
                    user::Column::PasswordHash,
                    Expr::value(password_hash.to_string()),
                )
                .col_expr(
                    user::Column::SessionVersion,
                    Expr::col(user::Column::SessionVersion).add(1),
                )
                .col_expr(user::Column::UpdatedAt, Expr::value(now))
                .filter(user::Column::Id.eq(user_id))
                .filter(user::Column::IsActive.eq(true))
                .filter(user::Column::DeletedAt.is_null())
                .exec(&transaction)
                .await
                .context("db: write password reset")?;
            match updated.rows_affected {
                0 => return Ok(None),
                1 => {}
                rows => anyhow::bail!("db: password reset affected {rows} rows for user {user_id}"),
            }

            invalidate_user_tokens(&transaction, user_id)
                .await
                .context("db: invalidate password reset tokens")?;

            user::Entity::find()
                .filter(user::Column::Id.eq(user_id))
                .filter(user::Column::IsActive.eq(true))
                .filter(user::Column::DeletedAt.is_null())
                .one(&transaction)
                .await
                .context("db: reload user after password reset")
        }
        .await;

        match result {
            Ok(Some(user)) => {
                transaction
                    .commit()
                    .await
                    .context("db: commit password reset completion")?;
                Ok(Some(user))
            }
            Ok(None) => {
                transaction
                    .rollback()
                    .await
                    .context("db: roll back refused password reset")?;
                Ok(None)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error).context(format!(
                        "db: roll back password reset completion: {rollback_error}"
                    ));
                }
                Err(error)
            }
        }
    })
    .await
}

/// Whether a reset link was issued to `user_id` at or after `since` and is
/// still on file — the per-account cooldown of `forgot_password`.
pub async fn issued_since(
    db: &DatabaseConnection,
    user_id: i64,
    since: chrono::DateTime<chrono::Utc>,
) -> Result<bool, sea_orm::DbErr> {
    use sea_orm::PaginatorTrait;
    let count = password_reset_token::Entity::find()
        .filter(password_reset_token::Column::UserId.eq(user_id))
        .filter(password_reset_token::Column::CreatedAt.gte(since))
        .count(db)
        .await?;
    Ok(count > 0)
}

/// Invalidate all unused tokens for a user (e.g., after successful reset).
pub async fn invalidate_user_tokens<C>(db: &C, user_id: i64) -> Result<(), sea_orm::DbErr>
where
    C: sea_orm::ConnectionTrait,
{
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
