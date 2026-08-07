//! Spent WebAuthn ceremony challenges — the state that makes a passkey
//! challenge single-use.
use sea_orm::*;

use crate::entities::webauthn_ceremony_spend;
pub use crate::entities::webauthn_ceremony_spend::Entity;

/// Spend a ceremony's challenge, reporting whether this call is the one that
/// spent it.
///
/// One conditional statement whose row is the answer — the shape
/// [`crate::ops::mfa_backup_code_ops::verify_and_consume`],
/// [`crate::ops::password_reset_token_ops::consume`] and
/// [`crate::ops::user_ops::consume_totp_step`] all take, for the same reason.
/// Here the conditional lives in a UNIQUE index rather than a `WHERE`: there is
/// no prior row to name, so the statement that creates one is the statement
/// that arbitrates. Two concurrent replays of one assertion both reach the
/// insert; the database lets exactly one through, and the loser is refused
/// without either handler having read anything.
///
/// The predecessor was nothing at all. `unseal_state` is a pure function of the
/// signing key and the clock, so an intercepted ceremony cookie plus its
/// assertion body passed `POST /users/passkeys/login/finish` as many times as
/// they were presented, for the whole 300 seconds the cookie stayed young
/// (`card_7c7ada2d6a72`).
///
/// `expires_at` is when the record may be dropped, and it must sit *after* the
/// last instant the ceremony's cookie can still be unsealed — including
/// `jsonwebtoken`'s default 60-second expiry leeway. A record dropped early is
/// a replay window reopened, so the caller passes the ceremony TTL plus that
/// margin (see `CEREMONY_SPEND_RETENTION_SECS` in `rg-http`).
///
/// Expired records are dropped on the way past. That keeps the table bounded by
/// the ceremonies of the last few minutes without a background sweep — and a
/// sweep nobody calls is exactly how [`delete_expired`]'s neighbour in
/// `password_reset_token_ops` became a comment.
///
/// `false` is an ordinary outcome — this challenge has been answered already —
/// and the caller must give it the same refusal a bad signature gets. Telling
/// the two apart would tell whoever is replaying that the material they
/// intercepted was genuine.
///
/// Call this on a plain connection, not inside an open transaction: the refusal
/// arrives as a failed statement, and PostgreSQL aborts the whole transaction a
/// failed statement belongs to. A caller that wrapped the ceremony in one would
/// find the ordinary "already answered" answer had taken its other writes with
/// it.
pub async fn spend(
    db: &DatabaseConnection,
    ceremony_id: &str,
    expires_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool, DbErr> {
    delete_expired(db).await?;

    let now = chrono::Utc::now();
    let am = webauthn_ceremony_spend::ActiveModel {
        id: NotSet,
        ceremony_id: Set(ceremony_id.to_string()),
        spent_at: Set(now),
        expires_at: Set(expires_at),
    };
    match Entity::insert(am).exec(db).await {
        Ok(_) => Ok(true),
        Err(error) => {
            // The insert failed. Either the UNIQUE index refused a second
            // spend of this ceremony — the whole point of the table — or the
            // database is unwell, and those must not be answered alike: the
            // first is a refusal the holder of a dead challenge earns, the
            // second is ours and retryable. Re-reading tells them apart on
            // every backend, which parsing driver error text does not.
            match Entity::find()
                .filter(webauthn_ceremony_spend::Column::CeremonyId.eq(ceremony_id))
                .one(db)
                .await
            {
                Ok(Some(_)) => Ok(false),
                Ok(None) => Err(error),
                // The re-read failed too, so nothing is known about the row.
                // Report the failure that is actually in hand rather than
                // guessing which of the two it was.
                Err(reread) => Err(reread),
            }
        }
    }
}

/// Drop the records of ceremonies whose cookie can no longer be unsealed.
///
/// Called by [`spend`] itself, so the table stays bounded on any instance where
/// passkeys are used at all, and an instance where they are not keeps at most
/// the handful of rows its last few ceremonies left.
pub async fn delete_expired(db: &DatabaseConnection) -> Result<u64, DbErr> {
    let result = Entity::delete_many()
        .filter(webauthn_ceremony_spend::Column::ExpiresAt.lt(chrono::Utc::now()))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}
