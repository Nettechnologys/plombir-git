//! Password-door policy: the brute-force lockout, and the second factor.
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
//!
//! # The second factor lives here for the same reason
//!
//! `mfa_enabled` used to be read by the web login and by the SSO callback, and
//! by nobody else. The two doors that end their attempt here read it nowhere:
//! a correct password was a full session over SSH and a full-scope registry
//! token, on an account whose owner had switched MFA on and whose browser was
//! being asked for a TOTP code for the same password. The second factor was a
//! convention of one door rather than a property of the credential.
//!
//! So the decision is taken in the same call that already applies the lock, and
//! it is reported as [`PasswordAttempt::SecondFactorRequired`] — a variant, not
//! a bool, so a future password door cannot compile without answering it. The
//! policy is the one GitHub and Gitea settle on: a password is not enough on a
//! non-interactive channel that has nowhere to prompt for a code, and the
//! account authenticates there with a credential that is already a standing
//! second factor — an SSH key, or a personal access token.
//!
//! Exemptions exist and must stay *explicit*, the way the passkey door
//! (`rg-http/src/api/passkeys.rs::login_finish`) is exempt: a passkey assertion
//! is itself phishing-resistant strong authentication, and its doc comment says
//! so. What this module rules out is the silent exemption — the door that never
//! asked.

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
#[derive(Debug, Clone, PartialEq)]
pub enum PasswordAttempt {
    /// The password matched an account that is allowed to authenticate.
    ///
    /// Carries the fresh row from the lifecycle finalizer. A password reset may
    /// advance `session_version` after the credential lookup; an SSH session
    /// must not be born with the stale generation merely because the password
    /// itself was correct.
    Accepted(Box<User>),
    /// The password was right, and it is not enough: the account carries a
    /// second factor this door has no way to ask for.
    ///
    /// Kept apart from `Rejected` on purpose. It is only ever reached by a
    /// caller who already presented the correct password, so naming the reason
    /// out loud discloses nothing a guesser could use — the same reason
    /// `POST /users/login` may answer "account is temporarily locked" in words
    /// — and the account's owner otherwise has no way to learn why the password
    /// that works in the browser stopped working here.
    SecondFactorRequired,
    /// The password was right, and it is one an administrator chose: a new
    /// account, or a reset handed over by hand. It opens nothing until its
    /// holder has replaced it with one only they know — through the web login,
    /// which is the one door that can ask for the new one (card_9f18b657580b).
    ///
    /// Named out loud for the same reason as [`Self::SecondFactorRequired`]:
    /// only the holder of the right password ever reaches it.
    PasswordChangeRequired,
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

/// Settle one password attempt: apply the lockout, require the second factor,
/// advance the brute-force counter, and file the attempt in the login log.
///
/// Call it *after* the Argon2 verification, with `user` set to the row the
/// login resolved to (`None` when there is no such account) and `password_ok`
/// carrying the verifier's verdict.
///
/// A rejected password always produces a verdict: a database that cannot record
/// its strike must not turn it into an accepted one, so that best-effort write
/// is logged and the rejection stands. A *correct* password is different. It
/// is accepted only after a conditional lifecycle finalizer confirms the
/// account is still open; an unavailable finalizer is an error, and a vanished
/// account is a rejection.
pub async fn settle_password_attempt(
    db: &DatabaseConnection,
    user: Option<&User>,
    password_ok: bool,
    origin: AttemptOrigin<'_>,
) -> anyhow::Result<PasswordAttempt> {
    let now = Utc::now();
    let mut locked = user.is_some_and(|user| {
        user.locked_until
            .is_some_and(|locked_until| locked_until > now)
    });
    let mut second_factor_required = false;
    let mut password_change_required = false;

    // A locked account is refused even when the password is right — that is the
    // entire point of the lock — and a deactivated one is refused for the
    // reason `is_usable` exists.
    if let Some(user) = user {
        if password_ok && !locked && user.is_usable() {
            // This write is the linearization point between credential proof
            // and authentication publication. It runs even when the counters
            // are already zero: skipping the no-op case would leave the common
            // SSH/registry path with no check after password verification, so a
            // retirement which won in that gap could still be answered Accept.
            match user_ops::reset_login_failures_if_open(db, user.id).await? {
                // Ahead of the second factor: a password nobody but an
                // administrator knows yet must not get as far as a session.
                Some(finalized) if finalized.password_change_required => {
                    password_change_required = true;
                }
                Some(finalized) if !finalized.mfa_enabled => {
                    return Ok(PasswordAttempt::Accepted(Box::new(finalized)));
                }
                Some(_) => {
                    second_factor_required = true;
                }
                None => {
                    tracing::warn!(
                        user_id = user.id,
                        channel = origin.channel,
                        "password verified after the account had begun retirement; refusing authentication"
                    );
                }
            }
            // The strikes are cleared above before the MFA branch is taken, and
            // deliberately so: they count *wrong* passwords, and this one was
            // right. Leaving them would make the second factor a slow lockout
            // of its own — the owner's git remote retries the password it has
            // always used, and five of those would close the web login too.
        }

        // Only a wrong password advances the counter, exactly as on
        // `POST /users/login`: refusing a deactivated or already-locked account
        // says nothing about whether anybody is guessing, and counting those
        // would keep an account locked forever on its owner's own retries.
        if !password_ok && !locked {
            locked = match user_ops::record_failed_login(db, user.id, MAX_FAILED_PASSWORD_ATTEMPTS)
                .await
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
        Some(if password_change_required {
            "password_change_required"
        } else if second_factor_required {
            "mfa_required"
        } else if locked {
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

    if password_change_required {
        Ok(PasswordAttempt::PasswordChangeRequired)
    } else if second_factor_required {
        Ok(PasswordAttempt::SecondFactorRequired)
    } else {
        Ok(PasswordAttempt::Rejected { locked })
    }
}
