//! What an account can do about itself, and what an administrator can do to
//! hand one over.
//!
//! Before this module a signed-in user could change nothing about their own
//! account: not the password (so a compromised one stayed compromised, and on
//! an instance without SMTP a forgotten one took a database edit), not the
//! profile, not the address, not the picture, and an account could only be
//! removed by an administrator (card_ca894e30ac80). And an instance with
//! registration closed and no identity provider had no way at all to add a
//! colleague: administrators could edit accounts but not create one
//! (card_9f18b657580b).
//!
//! Every password written here goes through the same compare-and-swap: it
//! revokes the account's sessions and reset links in the commit that replaces
//! it, and a password an administrator chose carries
//! `password_change_required`, which every password door refuses until its
//! holder has picked their own (see [`crate::auth::lockout::PasswordAttempt`]).

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use sea_orm::{DatabaseConnection, Set};

use rg_db::entities::user::Model as User;
use rg_db::ops::user_ops;

use super::service::{ldap_directory_holds, valid_email, validate_username, LdapDirectories};
use crate::auth::password;

/// Longest `display_name` an account may set for itself.
pub const MAX_DISPLAY_NAME_CHARS: usize = 255;
/// Longest `bio` an account may set for itself.
pub const MAX_BIO_CHARS: usize = 2000;

/// How long a link proving an address stays usable.
pub const EMAIL_CONFIRMATION_LIFETIME: Duration = Duration::hours(24);

/// At most one mail per address in this window, whatever triggers it.
///
/// Registration and the address change are endpoints that mail an address the
/// requester typed; the rate limiter in front of them counts *clients*, so
/// without this a handful of clients could fill one stranger's inbox.
pub const EMAIL_COOLDOWN: Duration = Duration::minutes(5);

// ── Passwords ────────────────────────────────────────────────────────────

/// Check `new_password` against the rules every password here follows.
pub fn validate_new_password(new_password: &str, username: &str) -> Result<()> {
    password::PasswordValidator::standard()
        .validate_with_username(new_password, username)
        .map_err(|error| crate::error::invalid_request(error.to_string()))
}

/// Replace `user`'s password with `new_password`, after the caller has proved
/// the current one against `user.password_hash`.
///
/// Returns the account as committed — with the `session_version` the caller's
/// fresh session has to carry, since every older one was just revoked.
pub async fn change_password(
    db: &DatabaseConnection,
    user: &User,
    new_password: &str,
) -> Result<User> {
    if user.auth_provider != "local" {
        return Err(crate::error::invalid_request(format!(
            "this account signs in through {}, which holds its password",
            user.auth_provider
        )));
    }
    validate_new_password(new_password, &user.username)?;
    if password::verify_password(new_password, &user.password_hash)
        .await
        .with_context(|| format!("cannot verify the password of user {}", user.id))?
    {
        return Err(crate::error::invalid_request(
            "the new password must differ from the current one",
        ));
    }
    let new_hash = password::hash_password(new_password)
        .await
        .context("failed to hash the new password")?;
    user_ops::change_password(db, user.id, &user.password_hash, &new_hash)
        .await?
        .ok_or_else(|| {
            // The stored hash is no longer the one the current password was
            // proved against — a reset or an administrator got there first — or
            // the account closed meanwhile. Either way this request proved a
            // password that is gone.
            crate::error::conflict(
                "the password changed while this request was in flight; sign in again",
            )
        })
}

/// A password an administrator hands over, shown once.
///
/// 20 symbols from 58 (`a-z`, `A-Z` and `2-9` without the look-alikes
/// `l`/`I`/`O`/`0`/`1`) is about 117 bits — read off a screen and typed once,
/// then replaced. A `-` every five symbols keeps it legible and satisfies the
/// special-character rule the validator applies to every password.
pub fn generate_temporary_password() -> String {
    use rand::RngCore;
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::rngs::OsRng;
    loop {
        let mut raw = [0u8; 20];
        rng.fill_bytes(&mut raw);
        let symbols: Vec<char> = raw
            .iter()
            // Rejection would be exact; 256 % 58 = 24 leaves a bias of under
            // one part in ten on a few symbols, which costs ~0.1 bit in 117.
            .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
            .collect();
        let candidate = symbols
            .chunks(5)
            .map(|chunk| chunk.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("-");
        // Every class the validator demands, by construction of the loop.
        if password::PasswordValidator::standard()
            .validate(&candidate)
            .is_ok()
        {
            return candidate;
        }
    }
}

/// A new local account with a password its holder must replace on first use.
pub struct ProvisionedAccount {
    pub user: User,
    pub temporary_password: String,
}

/// Create a local account for someone, as an instance administrator.
///
/// The same rules as self-registration: a name no account, organization or
/// page holds, an address no account holds, nothing an enabled directory
/// already owns. The password is generated, shown once to the administrator,
/// and opens nothing until its holder has chosen their own.
pub async fn create_account(
    db: &DatabaseConnection,
    directories: LdapDirectories<'_>,
    username: &str,
    email: &str,
    display_name: Option<&str>,
    is_admin: bool,
) -> Result<ProvisionedAccount> {
    validate_username(username)?;
    if !valid_email(email) {
        return Err(crate::error::invalid_request(
            "email must contain '@' with a non-empty local and domain part",
        ));
    }
    let display_name = normalize_text("display_name", display_name, MAX_DISPLAY_NAME_CHARS)?;
    if crate::namespace::owner_name_is_taken(db, username).await? {
        return Err(crate::error::conflict(format!(
            "username '{username}' is already taken"
        )));
    }
    if user_ops::find_by_email(db, email).await?.is_some() {
        return Err(crate::error::conflict(format!(
            "email '{email}' is already registered"
        )));
    }
    if ldap_directory_holds(db, directories, username, Some(email)).await? {
        return Err(crate::error::conflict(format!(
            "username '{username}' or email '{email}' belongs to a directory account; \
             it is created when that person first signs in"
        )));
    }

    let temporary_password = generate_temporary_password();
    let password_hash = password::hash_password(&temporary_password)
        .await
        .context("failed to hash the temporary password")?;
    let now = Utc::now();
    let user = user_ops::create(
        db,
        rg_db::entities::user::ActiveModel {
            username: Set(username.to_string()),
            email: Set(email.to_string()),
            password_hash: Set(password_hash),
            display_name: Set(display_name),
            is_admin: Set(is_admin),
            is_active: Set(true),
            password_change_required: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            ..Default::default()
        },
    )
    .await
    .map_err(|error| {
        if rg_db::is_unique_violation_anyhow(&error) {
            crate::error::conflict("username or email is already registered")
        } else {
            error
        }
    })?;
    Ok(ProvisionedAccount {
        user,
        temporary_password,
    })
}

/// Give `target_user_id` a new password chosen by an administrator, which its
/// holder must replace on first use. Every session and reset link the account
/// had is revoked in the same commit.
pub async fn reset_password(
    db: &DatabaseConnection,
    target_user_id: i64,
) -> Result<ProvisionedAccount> {
    let target = user_ops::find_by_id(db, target_user_id)
        .await?
        .ok_or_else(|| crate::error::not_found("user"))?;
    if target.auth_provider != "local" {
        return Err(crate::error::invalid_request(format!(
            "this account signs in through {}, which holds its password",
            target.auth_provider
        )));
    }
    let temporary_password = generate_temporary_password();
    let password_hash = password::hash_password(&temporary_password)
        .await
        .context("failed to hash the temporary password")?;
    let user = user_ops::set_password_by_administrator(db, target.id, &password_hash)
        .await?
        .ok_or_else(|| crate::error::not_found("user"))?;
    Ok(ProvisionedAccount {
        user,
        temporary_password,
    })
}

/// Take the second factor off `target_user_id`'s account, as an operator.
///
/// The way back in for an owner whose authenticator is gone — and, since a
/// password reset of an MFA account ends at the second factor, the only way
/// back in once a stolen session has enrolled an authenticator the owner never
/// held. Shared by `POST /admin/users/{id}/mfa/reset` and
/// `plombir-git reset-mfa` so the two doors cannot drift: both revoke every
/// session first (the one that enrolled the attacker's authenticator is the
/// one that must not survive this), then drop the factor, the enrolment in
/// flight and every unused backup code in one commit. The caller journals.
///
/// An account with no second factor is refused rather than quietly "reset":
/// the operator was told MFA locks this person out, and the honest answer is
/// that it does not — their problem is elsewhere, and their sessions need not
/// be revoked for it.
pub async fn reset_mfa(db: &DatabaseConnection, target_user_id: i64) -> Result<User> {
    let target = user_ops::find_by_id(db, target_user_id)
        .await?
        .ok_or_else(|| crate::error::not_found("user"))?;
    if !target.mfa_enabled {
        return Err(crate::error::conflict(
            "MFA is not enabled for this account",
        ));
    }
    user_ops::invalidate_sessions(db, target.id)
        .await
        .context("revoke the sessions of an account whose MFA is being reset")?;
    user_ops::disable_mfa(db, target.id)
        .await?
        .ok_or_else(|| crate::error::not_found("user"))
}

// ── Profile ──────────────────────────────────────────────────────────────

/// Trim a free-text profile field; blank is "unset", over `max` is refused.
fn normalize_text(field: &str, value: Option<&str>, max: usize) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > max {
        return Err(crate::error::invalid_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(Some(value.to_string()))
}

/// Update the profile fields an account owns. `None` leaves a field alone,
/// `Some(None)` — or a blank string — clears it.
pub async fn update_profile(
    db: &DatabaseConnection,
    user_id: i64,
    display_name: Option<Option<String>>,
    bio: Option<Option<String>>,
) -> Result<super::service::UserInfo> {
    let display_name = display_name
        .map(|value| normalize_text("display_name", value.as_deref(), MAX_DISPLAY_NAME_CHARS))
        .transpose()?;
    let bio = bio
        .map(|value| normalize_text("bio", value.as_deref(), MAX_BIO_CHARS))
        .transpose()?;
    let updated = user_ops::update_by_id(db, user_id, display_name, bio, None, None)
        .await?
        .ok_or_else(|| crate::error::not_found("user"))?;
    Ok(updated.into())
}

// ── Avatar ───────────────────────────────────────────────────────────────

/// Largest picture an account may upload.
pub const MAX_AVATAR_BYTES: usize = 512 * 1024;

/// The image type `bytes` actually are, judged by their signature alone.
///
/// The client's `Content-Type` is not consulted: it is a claim, and the bytes
/// are served back to every visitor under the type decided here. SVG is not
/// on the list — it is a document that can carry script, not a picture.
pub fn sniff_avatar(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The URL `users.avatar_url` carries for an uploaded picture. The digest is
/// the cache key: a new picture is a new URL, so the old one can be cached
/// for as long as anybody likes.
pub fn avatar_url(username: &str, sha256: &str) -> String {
    format!(
        "/api/v1/avatars/{username}?v={}",
        &sha256[..sha256.len().min(16)]
    )
}

/// Store `bytes` as `user`'s picture.
pub async fn set_avatar(db: &DatabaseConnection, user: &User, bytes: Vec<u8>) -> Result<String> {
    use sha2::Digest;

    if bytes.len() > MAX_AVATAR_BYTES {
        return Err(crate::error::invalid_request(format!(
            "an avatar is at most {} KiB",
            MAX_AVATAR_BYTES / 1024
        )));
    }
    let Some(content_type) = sniff_avatar(&bytes) else {
        return Err(crate::error::invalid_request(
            "an avatar must be a PNG, JPEG, GIF or WebP image",
        ));
    };
    let sha256 = hex::encode(sha2::Sha256::digest(&bytes));
    let url = avatar_url(&user.username, &sha256);
    if !rg_db::ops::user_avatar_ops::replace(db, user.id, content_type, &sha256, bytes, &url)
        .await?
    {
        return Err(crate::error::not_found("user"));
    }
    Ok(url)
}

/// Remove `user_id`'s uploaded picture.
pub async fn remove_avatar(db: &DatabaseConnection, user_id: i64) -> Result<()> {
    if !rg_db::ops::user_avatar_ops::remove(db, user_id).await? {
        return Err(crate::error::not_found("user"));
    }
    Ok(())
}

// ── Proving an address ───────────────────────────────────────────────────

/// Where a confirmation link points, and how it leaves.
#[derive(Clone, Copy)]
pub struct Mailer<'a> {
    pub smtp: &'a crate::email::SmtpConfig,
    /// The configured public URL — never one derived from the request, or the
    /// requester would choose where the token is sent.
    pub base_url: &'a str,
}

/// A fresh link token and the hash that is stored for it.
fn new_link_token() -> (String, String) {
    use rand::RngCore;
    use sha2::Digest;
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let token = hex::encode(raw);
    let hash = hex::encode(sha2::Sha256::digest(token.as_bytes()));
    (token, hash)
}

fn token_hash(token: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(token.trim().as_bytes()))
}

/// Send one mail off the request path, through the tracker graceful shutdown
/// drains. The address is never logged: two of the three callers are
/// anonymous, and a log line per request would be a list of addresses tried.
fn send_detached(
    mailer: Mailer<'_>,
    to: &str,
    subject: &'static str,
    message: String,
    link: Option<String>,
) {
    let smtp = mailer.smtp.clone();
    let to = to.to_string();
    crate::task_tracker::delivery_tracker().spawn(async move {
        if let Err(error) =
            crate::email::send_html_notification(&smtp, &to, subject, &message, link.as_deref())
                .await
        {
            tracing::warn!(
                subject,
                error = %format!("{error:#}"),
                "an address-confirmation mail was not delivered"
            );
        }
    });
}

/// Whether `email` was mailed recently, so the next mail is skipped; and, when
/// it was not, the marker for this one is recorded by the caller's write.
async fn in_cooldown(db: &DatabaseConnection, email: &str) -> Result<bool> {
    rg_db::ops::email_confirmation_ops::mailed_since(db, email, Utc::now() - EMAIL_COOLDOWN).await
}

/// Self-registration on an instance whose mode is
/// [`super::registration::RegistrationMode::VerifyEmail`].
///
/// Nothing is created yet. The name, the address and the password's hash wait
/// in `email_confirmations` until the link mailed to the address is followed,
/// and only then does [`confirm`] create the account — so an address nobody
/// proved holds no account and no session (card_45f98ab2fe1a).
///
/// The name is checked here and refused out loud, as on every other door: a
/// username is public. The address is not. A taken address and a free one get
/// the same answer from the caller — `Ok(())` — after the same work: the
/// password is hashed either way, and the mail that goes out is the link to a
/// free address or, to a taken one, a note to its owner that somebody tried.
pub async fn register_pending(
    db: &DatabaseConnection,
    directories: LdapDirectories<'_>,
    mailer: Mailer<'_>,
    username: &str,
    email: &str,
    plaintext_password: &str,
) -> Result<()> {
    validate_username(username)?;
    if !valid_email(email) {
        return Err(crate::error::invalid_request(
            "email must contain '@' with a non-empty local and domain part",
        ));
    }
    validate_new_password(plaintext_password, username)?;
    if crate::namespace::owner_name_is_taken(db, username).await?
        || ldap_directory_holds(db, directories, username, None).await?
    {
        return Err(crate::error::conflict(format!(
            "username '{username}' is already taken"
        )));
    }

    // From here on, both answers cost the same and say the same.
    let password_hash = password::hash_password(plaintext_password)
        .await
        .context("failed to hash password")?;
    let address_taken = user_ops::find_by_email(db, email).await?.is_some()
        || ldap_directory_holds(db, directories, username, Some(email)).await?;

    // Old rows are swept whenever one is written, so the table stays as large
    // as the last day's traffic.
    rg_db::ops::email_confirmation_ops::purge_expired(db, Utc::now() - Duration::days(1)).await?;
    // Inside the cooldown nothing is written either: replacing the pending
    // row would kill the link already in the inbox and send no new one. A
    // second attempt within the window therefore confirms what the first
    // one asked for.
    if in_cooldown(db, email).await? {
        return Ok(());
    }

    if address_taken {
        let (_, marker) = new_link_token();
        rg_db::ops::email_confirmation_ops::record_notice(db, email, &marker).await?;
        send_detached(
            mailer,
            email,
            "Someone tried to register with your address",
            "Someone just tried to create a new account with this email address, which \
             already belongs to an account here. If it was you, sign in instead — or reset \
             your password if you no longer know it. If it was not you, nothing has changed \
             and you can ignore this message."
                .to_string(),
            Some(format!("{}/login", mailer.base_url.trim_end_matches('/'))),
        );
        return Ok(());
    }

    let (token, token_hash) = new_link_token();
    rg_db::ops::email_confirmation_ops::replace_pending_registration(
        db,
        email,
        username,
        &password_hash,
        &token_hash,
        Utc::now() + EMAIL_CONFIRMATION_LIFETIME,
    )
    .await?;
    send_detached(
        mailer,
        email,
        "Confirm your email address",
        format!(
            "Follow the link below to confirm this address and create the account \
             {username}. The link works once, for 24 hours. If you did not register, ignore \
             this message and no account will be created."
        ),
        Some(confirmation_link(mailer, &token)),
    );
    Ok(())
}

fn confirmation_link(mailer: Mailer<'_>, token: &str) -> String {
    format!(
        "{}/verify-email?token={token}",
        mailer.base_url.trim_end_matches('/')
    )
}

/// Ask to move `user` to `new_email`. The address changes only when the link
/// mailed to it is followed (see [`confirm`]).
///
/// An address another account holds gets the same answer as a free one; its
/// owner is told somebody tried. Only local accounts carry an address of their
/// own — a directory's or an identity provider's comes from there.
pub async fn request_email_change(
    db: &DatabaseConnection,
    mailer: Mailer<'_>,
    user: &User,
    new_email: &str,
) -> Result<()> {
    let new_email = new_email.trim();
    if user.auth_provider != "local" {
        return Err(crate::error::invalid_request(format!(
            "this account's address comes from {}",
            user.auth_provider
        )));
    }
    if !valid_email(new_email) {
        return Err(crate::error::invalid_request(
            "email must contain '@' with a non-empty local and domain part",
        ));
    }
    if new_email == user.email {
        return Err(crate::error::invalid_request(
            "this is already the account's address",
        ));
    }
    if in_cooldown(db, new_email).await? {
        return Ok(());
    }
    if user_ops::find_by_email(db, new_email).await?.is_some() {
        let (_, marker) = new_link_token();
        rg_db::ops::email_confirmation_ops::record_notice(db, new_email, &marker).await?;
        send_detached(
            mailer,
            new_email,
            "Someone tried to use your address",
            "Another account just asked to move to this email address, which already belongs \
             to an account here. Nothing has changed, and you can ignore this message."
                .to_string(),
            None,
        );
        return Ok(());
    }
    let (token, token_hash) = new_link_token();
    rg_db::ops::email_confirmation_ops::replace_pending_email_change(
        db,
        user.id,
        new_email,
        &token_hash,
        Utc::now() + EMAIL_CONFIRMATION_LIFETIME,
    )
    .await?;
    send_detached(
        mailer,
        new_email,
        "Confirm your new email address",
        format!(
            "The account {} asked to use this address from now on. Follow the link below to \
             confirm it; it works once, for 24 hours. If you did not ask for this, ignore this \
             message and nothing will change.",
            user.username
        ),
        Some(confirmation_link(mailer, &token)),
    );
    Ok(())
}

/// What following a confirmation link did.
pub enum Confirmed {
    /// A pending registration became an account — `user` is the new row.
    Registered(User),
    /// An account moved to a new address; `previous_email` is the one it left.
    EmailChanged { user: User, previous_email: String },
}

/// Follow the link `token` names: create the account a registration was
/// waiting for, or move an account to the address it asked for.
///
/// The link is spent by this call whatever happens next. What was checked when
/// it was issued is checked again, because a day may have passed: the name and
/// the address may have been taken meanwhile, and registration may have been
/// closed — `permit` is decided by the caller, now, not when the mail went out
/// (`None` when registration is closed; an address change does not use it).
pub async fn confirm(
    db: &DatabaseConnection,
    directories: LdapDirectories<'_>,
    permit: Option<super::registration::RegistrationPermit>,
    token: &str,
) -> Result<Confirmed> {
    use rg_db::entities::email_confirmation::{PURPOSE_EMAIL_CHANGE, PURPOSE_REGISTRATION};

    let Some(row) = rg_db::ops::email_confirmation_ops::take_live(db, &token_hash(token)).await?
    else {
        return Err(crate::error::invalid_request(
            "this confirmation link is invalid, already used, or expired",
        ));
    };
    match row.purpose.as_str() {
        PURPOSE_REGISTRATION => {
            let (Some(username), Some(password_hash)) = (row.username, row.password_hash) else {
                anyhow::bail!(
                    "pending registration {} has no username or password",
                    row.id
                );
            };
            let Some(permit) = permit else {
                return Err(crate::error::forbidden(
                    "self-service registration was closed after this link was sent",
                ));
            };
            if crate::namespace::owner_name_is_taken(db, &username).await?
                || ldap_directory_holds(db, directories, &username, None).await?
            {
                return Err(crate::error::conflict(format!(
                    "username '{username}' was taken before this address was confirmed; \
                     register again with another name"
                )));
            }
            if user_ops::find_by_email(db, &row.email).await?.is_some() {
                return Err(crate::error::conflict(
                    "this address belongs to an account now; sign in instead",
                ));
            }
            let user = super::service::create_registered_account(
                db,
                permit,
                &username,
                &row.email,
                password_hash,
            )
            .await?;
            Ok(Confirmed::Registered(user))
        }
        PURPOSE_EMAIL_CHANGE => {
            let user_id = row
                .user_id
                .with_context(|| format!("email change {} names no account", row.id))?;
            let before = user_ops::find_by_id(db, user_id)
                .await?
                .ok_or_else(|| crate::error::not_found("user"))?;
            let user = user_ops::update_email(db, user_id, &row.email)
                .await
                .map_err(|error| {
                    if rg_db::is_unique_violation_anyhow(&error) {
                        crate::error::conflict(
                            "this address belongs to another account now; nothing was changed",
                        )
                    } else {
                        error
                    }
                })?
                .ok_or_else(|| crate::error::not_found("user"))?;
            Ok(Confirmed::EmailChanged {
                user,
                previous_email: before.email,
            })
        }
        other => anyhow::bail!(
            "email confirmation {} has unknown purpose {other:?}",
            row.id
        ),
    }
}

/// Tell `previous_email` that its account moved away from it — the one signal
/// the owner gets if somebody else with a session made the change.
pub fn notify_previous_address(mailer: Mailer<'_>, previous_email: &str, username: &str) {
    send_detached(
        mailer,
        previous_email,
        "Your account's email address was changed",
        format!(
            "The account {username} no longer uses this email address. If you made this \
             change, there is nothing to do. If you did not, sign in, change your password, \
             and contact an administrator."
        ),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local account with a password, as registration creates one.
    async fn local_account(db: &DatabaseConnection, name: &str) -> User {
        let now = Utc::now();
        user_ops::create(
            db,
            rg_db::entities::user::ActiveModel {
                username: Set(name.to_string()),
                email: Set(format!("{name}@example.invalid")),
                password_hash: Set(password::hash_password("Old$pass1").await.unwrap()),
                is_active: Set(true),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    /// A generated password has to clear the rule every password here follows,
    /// or an administrator would hand over one its holder cannot even log in
    /// with to replace.
    #[test]
    fn a_temporary_password_passes_the_password_rules() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let generated = generate_temporary_password();
            assert_eq!(generated.len(), 23, "{generated}");
            password::PasswordValidator::standard()
                .validate(&generated)
                .unwrap_or_else(|error| panic!("{generated}: {error}"));
            assert!(seen.insert(generated), "a temporary password repeated");
        }
    }

    #[test]
    fn an_avatar_is_judged_by_its_bytes() {
        assert_eq!(sniff_avatar(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_avatar(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_avatar(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff_avatar(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        for refused in [
            b"<svg xmlns='http://www.w3.org/2000/svg'/>".as_slice(),
            b"<html><script>",
            b"RIFF\0\0\0\0WAVE",
            b"",
        ] {
            assert_eq!(sniff_avatar(refused), None, "{refused:?}");
        }
    }

    /// card_9f18b657580b: the right password on an account that owes a change
    /// opens nothing at any password door — it is reported as owed, ahead of
    /// a second factor, and the row is left as it was.
    #[tokio::test]
    async fn the_right_administrator_password_is_owed_a_change_at_every_door() {
        use crate::auth::lockout::{settle_password_attempt, AttemptOrigin, PasswordAttempt};

        let db = crate::test_support::migrated_memory_database().await;
        let user = local_account(&db, "carol").await;
        let reset = reset_password(&db, user.id).await.unwrap();
        assert!(reset.user.password_change_required);

        let attempt = settle_password_attempt(
            &db,
            Some(&reset.user),
            true,
            AttemptOrigin {
                login: "carol",
                channel: "ssh",
                ip_address: None,
                user_agent: None,
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(attempt, PasswordAttempt::PasswordChangeRequired),
            "{attempt:?}"
        );

        // The holder's own change clears it, and the next attempt is accepted.
        let changed = change_password(&db, &reset.user, "Car0l$own")
            .await
            .unwrap();
        assert!(!changed.password_change_required);
        let attempt = settle_password_attempt(
            &db,
            Some(&changed),
            true,
            AttemptOrigin {
                login: "carol",
                channel: "ssh",
                ip_address: None,
                user_agent: None,
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(attempt, PasswordAttempt::Accepted(_)),
            "{attempt:?}"
        );
    }

    /// A change proved against a password that a reset has since replaced
    /// must not overwrite the reset.
    #[tokio::test]
    async fn a_change_loses_to_a_reset_that_landed_first() {
        let db = crate::test_support::migrated_memory_database().await;
        let user = local_account(&db, "dora").await;
        let stale = reset_password(&db, user.id).await.unwrap().user;
        let reset_again = reset_password(&db, user.id).await.unwrap();
        let error = change_password(&db, &stale, "D0ra$own").await.unwrap_err();
        assert!(format!("{error:#}").contains("changed while"), "{error:#}");
        let now = user_ops::find_by_id(&db, user.id).await.unwrap().unwrap();
        assert_eq!(now.password_hash, reset_again.user.password_hash);
    }
}
