//! Links mailed out to prove an address: a registration waiting for its
//! address, an account moving to a new one, an account proving the one it has. See
//! [`crate::entities::email_confirmation`].
//!
//! A link works once and only while fresh. Registration and address verification
//! use [`take_live`]; address changes are spent inside
//! [`super::user_ops::complete_email_change`] with the account mutation.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sea_orm::*;

use crate::entities::email_confirmation::{
    self, Entity, Model, PURPOSE_EMAIL_CHANGE, PURPOSE_EMAIL_VERIFY, PURPOSE_NOTICE,
    PURPOSE_REGISTRATION,
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
    // Delete-then-insert on an indexed key: two of these for neighbouring
    // addresses can deadlock on MySQL's next-key locks (1213), which is
    // contention, not a failure of the registration (card_a673a3823085).
    crate::contention::retry_transaction("record a pending registration", || async {
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
    })
    .await
}

/// Record an email change only while the account snapshot whose password and
/// second factor were proved remains current. The user row is locked before
/// child rows, shared with password writes, reset and link redemption.
/// `false` is stale proof; a MySQL no-change lock update is not a refusal.
pub async fn replace_pending_email_change(
    db: &DatabaseConnection,
    proved: &crate::entities::user::Model,
    email: &str,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<bool> {
    use crate::entities::user;
    crate::contention::retry_transaction("record a pending email change", || async {
        let transaction = db.begin().await.context("db: begin pending email change")?;
        user::Entity::update_many()
            .col_expr(
                user::Column::SessionVersion,
                sea_query::Expr::col(user::Column::SessionVersion).into(),
            )
            .filter(user::Column::Id.eq(proved.id))
            .exec(&transaction)
            .await?;
        let current = user::Entity::find_by_id(proved.id)
            .one(&transaction)
            .await?;
        if !current.as_ref().is_some_and(|current| {
            current.is_usable()
                && current.auth_provider == "local"
                && current.locked_until.is_none_or(|until| until <= Utc::now())
                && current.session_version == proved.session_version
                && current.password_hash == proved.password_hash
                && current.mfa_enabled == proved.mfa_enabled
                && current.totp_secret == proved.totp_secret
                && current.email == proved.email
        }) {
            transaction.rollback().await?;
            return Ok(false);
        }
        invalidate_email_changes(&transaction, proved.id).await?;
        insert(
            &transaction,
            PURPOSE_EMAIL_CHANGE,
            email,
            Some(proved.id),
            None,
            None,
            token_hash,
            expires_at,
        )
        .await?;
        transaction
            .commit()
            .await
            .context("db: commit pending email change")?;
        Ok(true)
    })
    .await
}

/// Revoke pending address changes inside the credential mutation transaction.
pub async fn invalidate_email_changes<C: ConnectionTrait>(db: &C, user_id: i64) -> Result<()> {
    Entity::delete_many()
        .filter(email_confirmation::Column::UserId.eq(user_id))
        .filter(email_confirmation::Column::Purpose.eq(PURPOSE_EMAIL_CHANGE))
        .exec(db)
        .await
        .context("db: invalidate pending email changes")?;
    Ok(())
}

/// Record the link that proves `user_id` receives mail at the address it
/// already has, replacing an earlier one that was not followed.
pub async fn replace_pending_email_verification(
    db: &DatabaseConnection,
    user_id: i64,
    email: &str,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    replace_pending_for_account(
        db,
        PURPOSE_EMAIL_VERIFY,
        user_id,
        email,
        token_hash,
        expires_at,
    )
    .await
}

/// One live link per account and purpose: the newest replaces the rest.
async fn replace_pending_for_account(
    db: &DatabaseConnection,
    purpose: &'static str,
    user_id: i64,
    email: &str,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    // The same delete-then-insert as a registration, and the same retry.
    crate::contention::retry_transaction("record a pending account link", || async {
        let transaction = db.begin().await.context("db: begin pending account link")?;
        Entity::delete_many()
            .filter(email_confirmation::Column::Purpose.eq(purpose))
            .filter(email_confirmation::Column::UserId.eq(user_id))
            .exec(&transaction)
            .await
            .context("db: drop the earlier pending account link")?;
        insert(
            &transaction,
            purpose,
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
            .context("db: commit pending account link")
    })
    .await
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
    let Some(row) = find_live(db, token_hash).await? else {
        return Ok(None);
    };
    // Address changes must be spent atomically with the account write, under
    // its lock. A generic take must never detach that proof from the mutation.
    if row.purpose == PURPOSE_EMAIL_CHANGE {
        return Ok(None);
    }
    let spent = Entity::delete_many()
        .filter(email_confirmation::Column::Id.eq(row.id))
        .filter(email_confirmation::Column::TokenHash.eq(token_hash))
        .filter(email_confirmation::Column::Purpose.ne(PURPOSE_EMAIL_CHANGE))
        .filter(email_confirmation::Column::ExpiresAt.gt(Utc::now()))
        .exec(db)
        .await
        .context("db: spend an email confirmation")?;
    Ok((spent.rows_affected == 1).then_some(row))
}

/// A preliminary lookup, never authorization to mutate an account. The
/// conditional spend under the user lock is the source of truth for changes.
pub async fn find_live(db: &DatabaseConnection, token_hash: &str) -> Result<Option<Model>> {
    let now = Utc::now();
    Entity::find()
        .filter(email_confirmation::Column::TokenHash.eq(token_hash))
        .filter(email_confirmation::Column::Purpose.ne(PURPOSE_NOTICE))
        .filter(email_confirmation::Column::ExpiresAt.gt(now))
        .one(db)
        .await
        .context("db: look up an email confirmation")
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
