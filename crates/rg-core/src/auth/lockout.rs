//! Brute-force lockout policy for the doors that take a password.
//!
//! `login_attempts` / `locked_until` on the user row are the forge's only brake
//! on password guessing, and until this module existed that brake lived inline
//! in `POST /users/login`. The two other doors that accept a password — SSH on
//! port 22 and `docker login` against the registry — verified the Argon2 hash
//! and nothing else: no lock was honoured, no strike was recorded, and no row
//! reached `login_log`. A threshold of five on one door is worth nothing while
//! two others beside it count to infinity in silence, and the silence is the
//! worse half: an attacker walking the SSH port left no trace in the audit log
//! and none on the admin's view of the account.
//!
//! Both call sites now finish their attempt through [`settle_password_attempt`],
//! *after* the Argon2 verification. That order is load-bearing. Reading
//! `locked_until` before the hash would answer "is this account locked?" — and
//! therefore "does it exist?" — through the response time, which is exactly the
//! oracle `card_b2fa2c6311a7` closed with [`crate::auth::password::verify_password_or_dummy`].
//!
//! Only *failures* are written to `login_log` from here. `POST /users/login`
//! also logs its successes, but that is one row per human sign-in; the registry
//! token endpoint is hit with Basic credentials on every `docker pull`, so
//! logging its successes would bury the brute-force signal these rows exist to
//! carry under a per-request access log.

use chrono::Utc;
use rg_db::entities::user::Model as User;
use rg_db::ops::{login_log_ops, user_ops};
use sea_orm::DatabaseConnection;

/// Failed password attempts tolerated before an account locks for 15 minutes.
///
/// Shared by every password door on purpose: a threshold that is written out
/// per call site is the same hole this module closes, one refactor later.
pub const MAX_FAILED_PASSWORD_ATTEMPTS: i32 = 5;

/// Verdict of one password attempt, once the lockout policy has been applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordAttempt {
    /// The password matched an account that is allowed to authenticate.
    Accepted,
    /// Refused. `locked` says the brute-force lock was the reason (or has just
    /// become one); it is for the server's log only — a client that could tell
    /// the two rejections apart would be told which usernames are real.
    Rejected { locked: bool },
}

/// Where an attempt came from, for the login log.
#[derive(Debug, Clone, Copy)]
pub struct AttemptOrigin<'a> {
    /// The login exactly as the client presented it — recorded even, and
    /// especially, when it matches no account.
    pub login: &'a str,
    /// The door the password arrived at: `"ssh"`, `"registry"`. Stored in
    /// `login_log.auth_provider` (truncated at 20 chars by the op), so that a
    /// run of failures can be attributed to a port instead of appearing as an
    /// unexplained series of web logins.
    pub channel: &'a str,
    pub ip_address: Option<&'a str>,
    pub user_agent: Option<&'a str>,
}

/// Settle one password attempt: apply the lockout, advance the brute-force
/// counter, and file the attempt in the login log.
///
/// Call it *after* the Argon2 verification, with `user` set to the row the
/// login resolved to (`None` when there is no such account) and `password_ok`
/// carrying the verifier's verdict.
///
/// Never fails. A database that cannot record a strike must not turn a rejected
/// password into an accepted one, so a write error is logged and the verdict
/// stands — the same permissive default `POST /users/login` uses, for the same
/// reason.
pub async fn settle_password_attempt(
    db: &DatabaseConnection,
    user: Option<&User>,
    password_ok: bool,
    origin: AttemptOrigin<'_>,
) -> PasswordAttempt {
    let now = Utc::now();
    let mut locked = user.is_some_and(|user| {
        user.locked_until
            .is_some_and(|locked_until| locked_until > now)
    });

    // A locked account is refused even when the password is right — that is the
    // entire point of the lock — and a deactivated one is refused for the
    // reason `is_usable` exists.
    if let Some(user) = user {
        if password_ok && !locked && user.is_usable() {
            // Clear the strikes the way a successful web login does, or an
            // honest user who mistyped twice this morning carries those two
            // forever: nothing decays `login_attempts`, so three more over the
            // following months would lock them out. Skipped when there is
            // nothing to clear, which keeps the common path — every registry
            // token request — free of a write.
            if user.login_attempts > 0 || user.locked_until.is_some() {
                if let Err(error) = user_ops::reset_login_failures(db, user.id).await {
                    tracing::warn!(
                        user_id = user.id,
                        error = %format!("{error:#}"),
                        "failed to clear the brute-force counter after a successful password login"
                    );
                }
            }
            return PasswordAttempt::Accepted;
        }

        // Only a wrong password advances the counter, exactly as on
        // `POST /users/login`: refusing a deactivated or already-locked account
        // says nothing about whether anybody is guessing, and counting those
        // would keep an account locked forever on its owner's own retries.
        if !password_ok && !locked {
            locked =
                match user_ops::record_failed_login(db, user.id, MAX_FAILED_PASSWORD_ATTEMPTS).await
                {
                    Ok(locked) => locked,
                    Err(error) => {
                        // `false` is also what a successful write reports for
                        // "not locked yet", so without this line a broken write
                        // degrades the lockout into a no-op that looks fine.
                        tracing::warn!(
                            user_id = user.id,
                            error = %format!("{error:#}"),
                            "failed to record a failed password attempt, brute-force counter did not advance"
                        );
                        false
                    }
                };
        }
    }

    if let Err(error) = login_log_ops::log_attempt(
        db,
        user.map(|user| user.id),
        origin.login,
        origin.channel,
        origin.ip_address,
        origin.user_agent,
        false,
        Some(if locked {
            "account_locked"
        } else {
            "invalid_credentials"
        }),
    )
    .await
    {
        tracing::warn!(
            login = origin.login,
            channel = origin.channel,
            error = %format!("{error:#}"),
            "failed to record a rejected password attempt in the login log"
        );
    }

    PasswordAttempt::Rejected { locked }
}
