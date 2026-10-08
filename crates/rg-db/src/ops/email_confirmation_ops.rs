//! Links mailed out to prove an address: a registration waiting for its
//! address, an account moving to a new one. See
//! [`crate::entities::email_confirmation`].
//!
//! A link works once and only while it is fresh: [`take_live`] deletes the row
//! it returns in the same statement that proves it is still there, so two
//! clicks race for one row and exactly one of them wins it.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sea_orm::*;

use crate::entities::email_confirmation::{
    self, Entity, Model, PURPOSE_EMAIL_CHANGE, PURPOSE_NOTICE, PURPOSE_REGISTRATION,
};

/// Whether any mail — a link or a notice — went to `email` after `since`.
///
/// The per-address cooldown: an anonymous endpoint that mails whatever
/// address it is given is a way to flood that address, and the limiter in
/// front of it counts clients, not recipients.
pub async fn mailed_since(
    db: &DatabaseConnection,
    email: &str,
    since: DateTime<Utc>,
) -> Result<bool> {
    Ok(Entity::find()
        .filter(email_confirmation::Column::Email.eq(email))
        .filter(email_confirmation::Column::CreatedAt.gt(since))
        .one(db)
        .await
        .context("db: look up recent mail to an address")?
        .is_some())
}

/// Record a registration waiting for `email`, replacing any earlier one for
/// the same address: the newest link is the one that works.
pub async fn replace_pending_registration(
    db: &DatabaseConnection,
    email: &str,
    username: &str,
    password_hash: &str,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    let transaction = db.begin().await.context("db: begin pending registration")?;
    Entity::delete_many()
        .filter(email_confirmation::Column::Purpose.eq(PURPOSE_REGISTRATION))
        .filter(email_confirmation::Column::Email.eq(email))
        .exec(&transaction)
        .await
        .context("db: drop the earlier pending registration")?;
    insert(
        &transaction,
        PURPOSE_REGISTRATION,
        email,
        None,
        Some(username),
        Some(password_hash),
        token_hash,
        expires_at,
    )
    .await?;
    transaction
        .commit()
        .await
        .context("db: commit pending registration")
}

/// Record that `user_id` asked to move to `email`, replacing any earlier
/// request of the same account.
pub async fn replace_pending_email_change(
    db: &DatabaseConnection,
    user_id: i64,
    email: &str,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    let transaction = db.begin().await.context("db: begin pending email change")?;
    Entity::delete_many()
        .filter(email_confirmation::Column::Purpose.eq(PURPOSE_EMAIL_CHANGE))
        .filter(email_confirmation::Column::UserId.eq(user_id))
        .exec(&transaction)
        .await
        .context("db: drop the earlier pending email change")?;
    insert(
        &transaction,
        PURPOSE_EMAIL_CHANGE,
        email,
        Some(user_id),
        None,
        None,
        token_hash,
        expires_at,
    )
    .await?;
    transaction
        .commit()
        .await
        .context("db: commit pending email change")
}

/// Record that a notice without a link went to `email`, so the cooldown sees
/// it. `marker_hash` only fills the unique column — nothing can redeem it, as
/// the row is born expired.
pub async fn record_notice(db: &DatabaseConnection, email: &str, marker_hash: &str) -> Result<()> {
    insert(
        db,
        PURPOSE_NOTICE,
        email,
        None,
        None,
        None,
        marker_hash,
        Utc::now(),
    )
    .await
}

/// Take the confirmation `token_hash` names, if it is still live.
///
/// The row is deleted by the same call that returns it, and the delete is
/// conditional on the row still being there and fresh, so a link is spent
/// exactly once however many times it is clicked at once. A notice row never
/// qualifies.
pub async fn take_live(db: &DatabaseConnection, token_hash: &str) -> Result<Option<Model>> {
    let now = Utc::now();
    let Some(row) = Entity::find()
        .filter(email_confirmation::Column::TokenHash.eq(token_hash))
        .filter(email_confirmation::Column::Purpose.ne(PURPOSE_NOTICE))
        .filter(email_confirmation::Column::ExpiresAt.gt(now))
        .one(db)
        .await
        .context("db: look up an email confirmation")?
    else {
        return Ok(None);
    };
    let spent = Entity::delete_many()
        .filter(email_confirmation::Column::Id.eq(row.id))
        .filter(email_confirmation::Column::ExpiresAt.gt(now))
        .exec(db)
        .await
        .context("db: spend an email confirmation")?;
    Ok((spent.rows_affected == 1).then_some(row))
}

/// Drop every row that can no longer be redeemed and is older than `before`.
pub async fn purge_expired(db: &DatabaseConnection, before: DateTime<Utc>) -> Result<u64> {
    Ok(Entity::delete_many()
        .filter(email_confirmation::Column::ExpiresAt.lte(Utc::now()))
        .filter(email_confirmation::Column::CreatedAt.lt(before))
        .exec(db)
        .await
        .context("db: purge expired email confirmations")?
        .rows_affected)
}

#[allow(clippy::too_many_arguments)]
async fn insert<C: ConnectionTrait>(
    db: &C,
    purpose: &str,
    email: &str,
    user_id: Option<i64>,
    username: Option<&str>,
    password_hash: Option<&str>,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    email_confirmation::ActiveModel {
        id: NotSet,
        purpose: Set(purpose.to_string()),
        token_hash: Set(token_hash.to_string()),
        email: Set(email.to_string()),
        user_id: Set(user_id),
        username: Set(username.map(str::to_string)),
        password_hash: Set(password_hash.map(str::to_string)),
        expires_at: Set(expires_at),
        created_at: Set(Utc::now()),
    }
    .insert(db)
    .await
    .context("db: record an email confirmation")?;
    Ok(())
}
