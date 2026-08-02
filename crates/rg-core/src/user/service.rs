//! User service — business logic for user registration, login, profile, admin management,
//! and password reset.

use anyhow::{bail, Context, Result};
use chrono::{Duration, Utc};
use sea_orm::{ActiveValue::Set, DatabaseConnection};

use rg_db::{entities::user::ActiveModel as UserActiveModel, ops::user_ops};

use crate::auth::{jwt, password};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    Password,
    Ldap,
}

pub struct LoginOutcome {
    pub response: AuthResponse,
    pub method: LoginMethod,
}

const LDAP_IDENTITY_CONFLICT: &str = "LDAP identity conflicts with an existing account";

/// A paginated list of users with total count.
pub struct PaginatedUsers {
    pub users: Vec<UserInfo>,
    pub total: i64,
}

/// Public user information (safe to return to clients).
#[derive(Debug, serde::Serialize)]
pub struct UserInfo {
    pub id: i64,
    pub username: String,
    pub email: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    pub is_admin: bool,
    pub is_active: bool,
    pub auth_provider: String,
    pub last_login_at: Option<chrono::DateTime<Utc>>,
    pub login_attempts: i32,
    pub locked_until: Option<chrono::DateTime<Utc>>,
    pub created_at: chrono::DateTime<Utc>,
}

/// Response after a successful login or registration.
#[derive(Debug, serde::Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub user_id: i64,
    pub username: String,
}

/// What a completed password reset entitles its holder to.
///
/// An enum, and not an [`AuthResponse`] with a flag next to it, for the reason
/// [`PasswordAttempt::SecondFactorRequired`](crate::auth::lockout::PasswordAttempt)
/// is one: whether the account still owes a second factor is a *policy*, and a
/// caller that cannot compile without answering it is a gate. A caller that
/// merely ought to remember to ask is a convention — which is exactly what let
/// this door mint a seven-day session for an `mfa_enabled` account while every
/// other door was busy refusing one.
#[derive(Debug)]
pub enum PasswordResetOutcome {
    /// No second factor is enrolled, so the new password is the whole
    /// credential and the reset ends logged in, as it always has.
    Session(AuthResponse),
    /// The password was changed and does not open a session on its own.
    ///
    /// MFA is bought for precisely this situation — a mailbox in someone
    /// else's hands — so the holder of the reset link is left where
    /// `POST /users/login` would leave them: first factor proved, second one
    /// still owed.
    SecondFactorRequired { user_id: i64, username: String },
}

/// Validate a username according to ForgeKeep rules.
///
/// Rules:
/// - Length: 3–30 characters
/// - Must start with an alphanumeric character
/// - May only contain alphanumeric characters, hyphens, and underscores
/// - Must not contain path traversal sequences (`..` or `/`)
///
/// Returns `Ok(())` if valid, `Err` with a descriptive message otherwise.
/// Every rejection here is a rule the *request* broke, so each one carries
/// `InvalidRequest` and is allowed to reach the client verbatim as a 400. This
/// function performs no I/O, so it has no other kind of failure to confuse it
/// with — but its callers do, and they used to answer 400 to those too.
pub fn validate_username(username: &str) -> Result<()> {
    if username.len() < 3 || username.len() > 30 {
        return Err(crate::error::invalid_request(
            "username must be between 3 and 30 characters",
        ));
    }

    let first_char = username.chars().next().unwrap(); // len >= 3, safe to unwrap
    if !first_char.is_ascii_alphanumeric() {
        return Err(crate::error::invalid_request(
            "username must start with an alphanumeric character",
        ));
    }

    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(crate::error::invalid_request(
            "username must only contain alphanumeric characters, hyphens, and underscores",
        ));
    }

    // Path traversal prevention
    if username.contains("..") || username.contains('/') {
        return Err(crate::error::invalid_request(
            "username contains invalid characters",
        ));
    }

    Ok(())
}

/// Register a new user.
///
/// Returns an `AuthResponse` with a JWT token.
pub async fn register(
    db: &DatabaseConnection,
    username: &str,
    email: &str,
    plaintext_password: &str,
    jwt_secret: &str,
) -> Result<AuthResponse> {
    // Validate inputs. The taken-name / taken-email checks are the caller's to
    // fix and carry `InvalidRequest`; the lookups performing them are ours, and
    // a failed one now stays a 5xx instead of telling the client its own
    // registration was malformed.
    if rg_db::ops::user_ops::find_by_username(db, username)
        .await?
        .is_some()
    {
        return Err(crate::error::invalid_request(format!(
            "username '{username}' is already taken"
        )));
    }

    if user_ops::find_by_email(db, email).await?.is_some() {
        return Err(crate::error::invalid_request(format!(
            "email '{email}' is already registered"
        )));
    }

    // ── Username validation ──────────────────────────────────────
    validate_username(username)?;

    // ── Email validation ─────────────────────────────────────────
    if !valid_email(email) {
        return Err(crate::error::invalid_request(
            "email must contain '@' with a non-empty local and domain part",
        ));
    }

    // ── Password validation ──────────────────────────────────────
    let password_validator = password::PasswordValidator::standard();
    password_validator
        .validate_with_username(plaintext_password, username)
        .map_err(|e| crate::error::invalid_request(e.to_string()))?;

    let password_hash =
        password::hash_password(plaintext_password).context("failed to hash password")?;

    let now = Utc::now();
    let model = UserActiveModel {
        username: Set(username.to_string()),
        email: Set(email.to_string()),
        password_hash: Set(password_hash),
        is_admin: Set(false),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        ..Default::default()
    };

    let user = user_ops::create(db, model).await.map_err(|error| {
        // The two lookups above keep their precise sequential messages.  A
        // concurrent registration can still lose the gap before this insert;
        // that is the same client-correctable outcome, not an outage.
        if rg_db::is_unique_violation_anyhow(&error) {
            crate::error::invalid_request("username or email is already registered")
        } else {
            error
        }
    })?;
    let token = jwt::generate_token(user.id, &user.username, user.session_version, jwt_secret, 7)?;

    Ok(AuthResponse {
        token,
        user_id: user.id,
        username: user.username,
    })
}

/// Authenticate a user by username/password. Returns a JWT on success.
pub async fn login(
    db: &DatabaseConnection,
    username_or_email: &str,
    plaintext_password: &str,
    jwt_secret: &str,
) -> Result<AuthResponse> {
    // Try username first, then email
    let user = if username_or_email.contains('@') {
        user_ops::find_by_email(db, username_or_email).await?
    } else {
        user_ops::find_by_username(db, username_or_email).await?
    };

    // Always pay for one Argon2 verification, including when there is nothing
    // to verify against: bailing out early on an unknown account would answer
    // "does this account exist?" through the response time.
    //
    // The `?` is the point: a hash the verifier cannot use is *our* breakage,
    // and it leaves here as an error carrying `UnusablePasswordHash` rather
    // than as the "invalid credentials" below. The account name rides along in
    // the context so the operator log says which row to go and look at.
    let password_ok = password::verify_password_or_dummy(
        plaintext_password,
        user.as_ref().map(|u| u.password_hash.as_str()),
    )
    .with_context(|| format!("cannot verify the password of '{username_or_email}'"))?;

    let Some(user) = user else {
        bail!("invalid credentials");
    };
    if !password_ok {
        bail!("invalid credentials");
    }

    // Checked after the password so that "account is disabled" is only ever
    // disclosed to whoever already knows the credentials.
    if !user.is_usable() {
        bail!("account is disabled");
    }

    let token = jwt::generate_token(user.id, &user.username, user.session_version, jwt_secret, 7)?;

    Ok(AuthResponse {
        token,
        user_id: user.id,
        username: user.username,
    })
}

/// Authenticate through the account's configured provider. Unknown users may
/// be provisioned only after a successful bind against an enabled LDAP source.
pub async fn login_with_configured_auth(
    db: &DatabaseConnection,
    username_or_email: &str,
    plaintext_password: &str,
    jwt_secret: &str,
    encryption_key: &str,
) -> Result<LoginOutcome> {
    let existing = find_login_user(db, username_or_email).await?;
    if existing.as_ref().is_some_and(|user| {
        user.locked_until
            .is_some_and(|locked_until| locked_until > Utc::now())
    }) {
        bail!("account is temporarily locked");
    }
    match existing.as_ref().map(|user| user.auth_provider.as_str()) {
        Some("local") => Ok(LoginOutcome {
            response: login(db, username_or_email, plaintext_password, jwt_secret).await?,
            method: LoginMethod::Password,
        }),
        Some("ldap") | None => {
            login_via_ldap(
                db,
                existing,
                username_or_email,
                plaintext_password,
                jwt_secret,
                encryption_key,
            )
            .await
        }
        Some(_) => {
            // Account exists but authenticates through a provider no password
            // reaches — burn the same Argon2 work the local branch would.
            password::burn_dummy_verification(plaintext_password);
            bail!("invalid credentials")
        }
    }
}

async fn find_login_user(
    db: &DatabaseConnection,
    username_or_email: &str,
) -> Result<Option<rg_db::entities::user::Model>> {
    if username_or_email.contains('@') {
        user_ops::find_by_email(db, username_or_email).await
    } else {
        user_ops::find_by_username(db, username_or_email).await
    }
}

/// Authenticate against the enabled LDAP sources.
///
/// Unknown usernames are routed here, so a rejection that never reached an
/// actual bind must still cost what the local password branch costs — with no
/// LDAP provider configured this function would otherwise return instantly for
/// an unknown account while a real one pays for its Argon2 hash, and the
/// difference answers "does this account exist?".
async fn login_via_ldap(
    db: &DatabaseConnection,
    existing: Option<rg_db::entities::user::Model>,
    username_or_email: &str,
    plaintext_password: &str,
    jwt_secret: &str,
    encryption_key: &str,
) -> Result<LoginOutcome> {
    let mut attempted_bind = false;
    let outcome = login_via_ldap_inner(
        db,
        existing,
        username_or_email,
        plaintext_password,
        jwt_secret,
        encryption_key,
        &mut attempted_bind,
    )
    .await;
    if outcome.is_err() && !attempted_bind {
        password::burn_dummy_verification(plaintext_password);
    }
    outcome
}

/// Sets `attempted_bind` as soon as a real LDAP round-trip is under way, so the
/// caller knows whether the elapsed time already came from the network.
async fn login_via_ldap_inner(
    db: &DatabaseConnection,
    existing: Option<rg_db::entities::user::Model>,
    username_or_email: &str,
    plaintext_password: &str,
    jwt_secret: &str,
    encryption_key: &str,
    attempted_bind: &mut bool,
) -> Result<LoginOutcome> {
    if plaintext_password.is_empty() {
        bail!("invalid credentials");
    }
    if existing.as_ref().is_some_and(|user| !user.is_usable()) {
        bail!("account is disabled");
    }

    let lookup = existing
        .as_ref()
        .and_then(|user| user.ldap_uid.as_deref())
        .unwrap_or(username_or_email);
    let mut providers: Vec<_> = rg_db::ops::sso_provider_ops::list_enabled(db)
        .await?
        .into_iter()
        .filter(|provider| provider.provider_type == "ldap")
        .collect();
    if let Some(provider_id) = existing.as_ref().and_then(|user| user.ldap_provider_id) {
        providers.retain(|provider| provider.id == provider_id);
    } else if existing.is_some() && providers.len() != 1 {
        bail!("invalid credentials");
    }
    for provider in providers {
        let config = match ldap_config_from_provider(&provider, encryption_key) {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!(
                    provider_id = provider.id,
                    error = %format!("{error:#}"),
                    "ignoring invalid LDAP provider configuration"
                );
                continue;
            }
        };
        *attempted_bind = true;
        let ldap_user = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            crate::auth::ldap::authenticate(&config, lookup, plaintext_password),
        )
        .await
        {
            Ok(Ok(user)) => user,
            Ok(Err(error)) => {
                tracing::warn!(
                    provider_id = provider.id,
                    error = %format!("{error:#}"),
                    "LDAP authentication attempt failed"
                );
                continue;
            }
            Err(_) => {
                tracing::warn!(
                    provider_id = provider.id,
                    "LDAP authentication attempt timed out"
                );
                continue;
            }
        };

        let user = match resolve_ldap_identity(db, existing.as_ref(), provider.id, ldap_user).await
        {
            Ok(user) => user,
            Err(error) if error.to_string() == LDAP_IDENTITY_CONFLICT => {
                tracing::warn!(
                    provider_id = provider.id,
                    error = %format!("{error:#}"),
                    "LDAP identity could not be linked"
                );
                bail!("invalid credentials");
            }
            Err(error) => {
                tracing::error!(
                    provider_id = provider.id,
                    error = %format!("{error:#}"),
                    "LDAP identity provisioning failed after a successful bind"
                );
                return Err(error);
            }
        };
        let token =
            jwt::generate_token(user.id, &user.username, user.session_version, jwt_secret, 7)?;
        return Ok(LoginOutcome {
            response: AuthResponse {
                token,
                user_id: user.id,
                username: user.username,
            },
            method: LoginMethod::Ldap,
        });
    }
    bail!("invalid credentials")
}

/// Build a bindable LDAP config from a stored provider row.
///
/// Takes the *encryption* secret, not the JWT one: the bind password is
/// AES-GCM at rest, and conflating the two is what made a rotated signing
/// secret break every LDAP login with "bind password could not be decrypted"
/// (card_d740512de0a8).
fn ldap_config_from_provider(
    provider: &rg_db::entities::sso_provider::Model,
    encryption_key: &str,
) -> Result<crate::auth::ldap::LdapConfig> {
    let raw_host = provider
        .ldap_host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .context("LDAP host is missing")?;
    let (host, explicit_tls) = raw_host
        .strip_prefix("ldaps://")
        .map(|host| (host, Some(true)))
        .or_else(|| {
            raw_host
                .strip_prefix("ldap://")
                .map(|host| (host, Some(false)))
        })
        .unwrap_or((raw_host, None));
    if host.is_empty() || host.contains('/') {
        bail!("LDAP host is invalid");
    }
    let use_tls = explicit_tls.unwrap_or(match provider.ldap_port {
        Some(port) => port == 636,
        None => true,
    });
    let port = provider
        .ldap_port
        .unwrap_or(if use_tls { 636 } else { 389 });
    let port = u16::try_from(port).context("LDAP port is invalid")?;
    if port == 0 {
        bail!("LDAP port is invalid");
    }
    let bind_password_enc = provider
        .ldap_bind_password_enc
        .as_deref()
        .context("LDAP bind password is missing")?;
    let key = crate::auth::encryption::derive_key(encryption_key);
    let bind_password = crate::auth::encryption::decrypt(bind_password_enc, &key)
        .context("LDAP bind password could not be decrypted")?;
    let required = |value: Option<&str>, name: &str| -> Result<String> {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .with_context(|| format!("LDAP {name} is missing"))
    };
    let user_filter = provider
        .ldap_user_filter
        .as_deref()
        .unwrap_or("(uid={username})")
        .trim()
        .to_string();
    if !user_filter.contains("{username}") {
        bail!("LDAP user filter must contain '{{username}}'");
    }
    Ok(crate::auth::ldap::LdapConfig {
        host: host.to_string(),
        port,
        use_tls,
        insecure_skip_tls_verify: false,
        bind_dn: required(provider.ldap_bind_dn.as_deref(), "bind DN")?,
        bind_password,
        base_dn: required(provider.ldap_base_dn.as_deref(), "base DN")?,
        user_filter,
    })
}

pub async fn test_ldap_provider_connection(
    provider: &rg_db::entities::sso_provider::Model,
    encryption_key: &str,
) -> Result<()> {
    if provider.provider_type != "ldap" {
        bail!("provider is not LDAP");
    }
    let config = ldap_config_from_provider(provider, encryption_key)?;
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        crate::auth::ldap::test_connection(&config),
    )
    .await
    .context("LDAP connection test timed out")??;
    Ok(())
}

async fn resolve_ldap_identity(
    db: &DatabaseConnection,
    existing: Option<&rg_db::entities::user::Model>,
    ldap_provider_id: i64,
    ldap_user: crate::auth::ldap::LdapUser,
) -> Result<rg_db::entities::user::Model> {
    let username = ldap_user
        .uid
        .as_deref()
        .unwrap_or(&ldap_user.username)
        .trim();
    validate_username(username).context("LDAP username is not valid for ForgeKeep")?;

    if let Some(user) = existing {
        if user.auth_provider != "ldap"
            || user.username != username
            || user
                .ldap_provider_id
                .is_some_and(|provider_id| provider_id != ldap_provider_id)
        {
            bail!(LDAP_IDENTITY_CONFLICT);
        }
        return user_ops::sync_ldap_identity(
            db,
            user.id,
            ldap_provider_id,
            ldap_user.display_name.as_deref(),
            &ldap_user.dn,
            ldap_user.uid.as_deref(),
        )
        .await;
    }

    if user_ops::find_by_username(db, username).await?.is_some() {
        bail!(LDAP_IDENTITY_CONFLICT);
    }
    let email = ldap_user
        .email
        .as_deref()
        .map(str::trim)
        .filter(|email| valid_email(email))
        .context("LDAP account does not have a valid email address")?;
    if user_ops::find_by_email(db, email).await?.is_some() {
        bail!(LDAP_IDENTITY_CONFLICT);
    }

    create_or_resolve_ldap_identity(
        db,
        ldap_provider_id,
        username,
        email,
        ldap_user.display_name.as_deref(),
        &ldap_user.dn,
        ldap_user.uid.as_deref(),
    )
    .await
}

/// Insert a post-bind LDAP identity or recover the winner of the same first-login race.
///
/// The preflight above is deliberately kept for clear, early conflict errors, but it
/// cannot serialize two requests. A UNIQUE failure is only recoverable when the
/// record that won carries this request's directory identity; a username alone is
/// not an identity and must never be adopted.
#[allow(clippy::too_many_arguments)]
async fn create_or_resolve_ldap_identity(
    db: &DatabaseConnection,
    ldap_provider_id: i64,
    username: &str,
    email: &str,
    display_name: Option<&str>,
    ldap_dn: &str,
    ldap_uid: Option<&str>,
) -> Result<rg_db::entities::user::Model> {
    let error = match user_ops::create_ldap_user(
        db,
        ldap_provider_id,
        username,
        email,
        display_name,
        ldap_dn,
        ldap_uid,
    )
    .await
    {
        Ok(user) => {
            // First-login LDAP auto-provision is a new account: count it in the
            // `users_registered_total` funnel with `ldap` provenance so directory-only
            // deployments don't silently undercount registrations.
            crate::metrics_hook::record_user_provisioned("ldap");
            return Ok(user);
        }
        Err(error) => error,
    };

    // Only the database's typed UNIQUE classification proves that a concurrent
    // insert could have won. Connection, foreign-key and check failures remain
    // real server errors; pretending that they created an identity would both
    // hide the outage and authenticate the wrong principal.
    if !rg_db::is_unique_violation_anyhow(&error) {
        return Err(error);
    }

    if let Some(user) =
        resolve_raced_ldap_identity(db, ldap_provider_id, username, email, ldap_uid).await?
    {
        return Ok(user);
    }

    // A UNIQUE violation with no matching directory identity was on an
    // unrelated constraint. Preserve the original database error instead of
    // fabricating a successful login.
    Err(error)
}

/// Find the winner of a first-login LDAP race without adopting another account.
async fn resolve_raced_ldap_identity(
    db: &DatabaseConnection,
    ldap_provider_id: i64,
    username: &str,
    email: &str,
    ldap_uid: Option<&str>,
) -> Result<Option<rg_db::entities::user::Model>> {
    if let Some(ldap_uid) = ldap_uid {
        if let Some(user) =
            user_ops::find_by_ldap_provider_and_uid(db, ldap_provider_id, ldap_uid).await?
        {
            return Ok(Some(user));
        }
    }

    if let Some(user) = user_ops::find_by_email(db, email).await? {
        if user.auth_provider == "ldap"
            && user.ldap_provider_id == Some(ldap_provider_id)
            && user.ldap_uid.as_deref() == ldap_uid
        {
            return Ok(Some(user));
        }
        bail!(LDAP_IDENTITY_CONFLICT);
    }

    // A username collision belongs to another account unless the stable LDAP
    // identity lookup above already proved otherwise. It remains an explicit
    // conflict rather than an account takeover.
    if user_ops::find_by_username(db, username).await?.is_some() {
        bail!(LDAP_IDENTITY_CONFLICT);
    }

    Ok(None)
}

/// The shape every stored address must have, wherever it enters ForgeKeep:
/// self-registration, an LDAP directory entry, or an SSO provider's profile.
/// One rule in one place — the three call sites used to spell it out
/// separately, and an address is an account lookup key in all three.
pub(crate) fn valid_email(email: &str) -> bool {
    matches!(email.split_once('@'), Some((local, domain)) if !local.is_empty() && !domain.is_empty())
}

// ── Admin user management ───────────────────────────────────────

impl From<rg_db::entities::user::Model> for UserInfo {
    fn from(u: rg_db::entities::user::Model) -> Self {
        Self {
            id: u.id,
            username: u.username,
            email: u.email,
            display_name: u.display_name,
            avatar_url: u.avatar_url,
            bio: u.bio,
            is_admin: u.is_admin,
            is_active: u.is_active,
            auth_provider: u.auth_provider,
            last_login_at: u.last_login_at,
            login_attempts: u.login_attempts,
            locked_until: u.locked_until,
            created_at: u.created_at,
        }
    }
}

/// List all users with pagination (admin only).
pub async fn list_users_admin(
    db: &DatabaseConnection,
    page: u64,
    per_page: u64,
) -> Result<PaginatedUsers> {
    let (users, total) = user_ops::list_users(db, page, per_page).await?;
    Ok(PaginatedUsers {
        users: users.into_iter().map(Into::into).collect(),
        total,
    })
}

/// Update any user's profile fields (admin only).
pub async fn update_user_admin(
    db: &DatabaseConnection,
    target_user_id: i64,
    display_name: Option<Option<String>>,
    bio: Option<Option<String>>,
    is_admin: Option<bool>,
    is_active: Option<bool>,
) -> Result<UserInfo> {
    // Absence crosses the rg-db boundary as a value; this layer can give it the
    // typed domain meaning that the HTTP error classifier maps to a safe 404.
    let Some(updated) =
        user_ops::update_by_id(db, target_user_id, display_name, bio, is_admin, is_active).await?
    else {
        return Err(crate::error::not_found("user"));
    };
    Ok(updated.into())
}

/// Delete a user (admin only).
pub async fn delete_user(db: &DatabaseConnection, user_id: i64) -> Result<()> {
    // `rg-db` cannot depend on this crate's domain error types, so absence
    // crosses that boundary as a value and becomes a typed 404 here.
    if !user_ops::delete_by_id(db, user_id).await? {
        return Err(crate::error::not_found("user"));
    }
    Ok(())
}

/// Get a single user by ID (admin view).
pub async fn get_user_by_id(db: &DatabaseConnection, user_id: i64) -> Result<Option<UserInfo>> {
    let user = user_ops::find_by_id(db, user_id).await?;
    Ok(user.map(Into::into))
}

// ── Password reset ──────────────────────────────────────────────

/// Wall-clock budget every [`forgot_password`] call is padded out to.
///
/// H-5: the property we owe is "the reply takes the same time whether or not
/// the address belongs to an account". A per-branch `sleep(100ms)` cannot give
/// that — it pads only the two cheap branches, while the real one pays for two
/// row writes on top of the same 100 ms, so the pad *inverts* the signal
/// instead of erasing it. Padding to a common deadline measured from entry does
/// give it, as long as the work stays under budget — which is why the SMTP
/// round-trip (network-bound, unbounded) is detached rather than awaited.
const FORGOT_PASSWORD_BUDGET: std::time::Duration = std::time::Duration::from_millis(100);

/// Initiate a password reset. Generates a token and sends an email.
/// Silently succeeds even if the email is not found (to prevent user enumeration).
/// H-5: every code path returns at the same deadline ([`FORGOT_PASSWORD_BUDGET`]
/// after entry) and no path awaits the SMTP send, so the response time carries
/// no signal about whether the address exists.
pub async fn forgot_password(
    db: &DatabaseConnection,
    email: &str,
    smtp_config: Option<&crate::email::SmtpConfig>,
    base_url: &str,
) -> Result<()> {
    let start = tokio::time::Instant::now();
    let result = forgot_password_inner(db, email, smtp_config, base_url).await;

    // H-5: pad to the shared deadline. Applies to the error branch too — a DB
    // fault on the "user exists" path must not answer faster or slower than one
    // on the lookup itself.
    let elapsed = start.elapsed();
    if elapsed > FORGOT_PASSWORD_BUDGET {
        // Not fatal, but the pad has stopped hiding which branch ran: something
        // on the request path got slow and needs detaching (or the budget
        // raising) before enumeration becomes observable again.
        tracing::warn!(
            elapsed_ms = elapsed.as_millis() as u64,
            budget_ms = FORGOT_PASSWORD_BUDGET.as_millis() as u64,
            "forgot-password outran its anti-enumeration timing budget"
        );
    }
    tokio::time::sleep_until(start + FORGOT_PASSWORD_BUDGET).await;

    result
}

/// The actual reset work. Kept separate so [`forgot_password`] can pad every
/// exit — early return, success, and error alike — to one deadline.
async fn forgot_password_inner(
    db: &DatabaseConnection,
    email: &str,
    smtp_config: Option<&crate::email::SmtpConfig>,
    base_url: &str,
) -> Result<()> {
    let Some(user) = user_ops::find_by_email(db, email).await? else {
        return Ok(());
    };

    // Only local users can reset via email (LDAP/OAuth users use their provider)
    if user.auth_provider != "local" {
        return Ok(());
    }

    // A disabled account gets the same silent no-op an unknown address gets.
    // Otherwise the reset flow is a way back in that needs no administrator:
    // the mail still lands in the mailbox the offboarded user controls, and
    // `reset_password` hands out a working session at the end of it.
    if !user.is_usable() {
        return Ok(());
    }

    // Invalidate old unused tokens
    rg_db::ops::password_reset_token_ops::invalidate_user_tokens(db, user.id).await?;

    // Generate a random token
    let raw_token = uuid::Uuid::new_v4().to_string();
    use sha2::Digest;
    let token_hash = hex::encode(sha2::Sha256::digest(raw_token.as_bytes()));

    // Token valid for 15 minutes
    let expires_at = Utc::now() + Duration::minutes(15);

    rg_db::ops::password_reset_token_ops::create(db, user.id, &token_hash, expires_at).await?;

    // Build reset link
    let reset_url = format!(
        "{}/reset-password?token={}",
        base_url.trim_end_matches('/'),
        raw_token
    );

    // Send the mail off the request path. The SMTP round-trip is network-bound
    // with no ceiling, so awaiting it here would put this branch — the one that
    // only runs for addresses that exist — arbitrarily far outside the timing
    // budget its caller pads to. Routed through the shared delivery tracker
    // rather than a bare `tokio::spawn` so graceful shutdown drains the send
    // instead of severing it on SIGTERM.
    if let Some(smtp) = smtp_config {
        let smtp = smtp.clone();
        let recipient = user.email.clone();
        let user_id = user.id;
        let message = format!(
            "We received a request to reset the password for your ForgeKeep account ({}). \
             Click the button below to set a new password. This link expires in 15 minutes.",
            user.username
        );
        crate::task_tracker::delivery_tracker().spawn(async move {
            let subject = "Reset your ForgeKeep password";
            if let Err(e) = crate::email::send_html_notification(
                &smtp,
                &recipient,
                subject,
                &message,
                Some(&reset_url),
            )
            .await
            {
                // Log by id, never by address: this endpoint is unauthenticated.
                tracing::warn!(
                    user_id,
                    error = %format!("{e:#}"),
                    "password reset email not delivered — the token was issued and will expire unused, and the request answered success, so the user is waiting for a link that never arrives"
                );
            }
        });
    }

    tracing::info!(user_id = user.id, "password reset requested");

    Ok(())
}

/// Reset a password using a valid reset token.
///
/// Ends the reset, not necessarily the login: see [`PasswordResetOutcome`].
pub async fn reset_password(
    db: &DatabaseConnection,
    raw_token: &str,
    new_password: &str,
    jwt_secret: &str,
) -> Result<PasswordResetOutcome> {
    use sha2::Digest;
    let token_hash = hex::encode(sha2::Sha256::digest(raw_token.as_bytes()));

    let token_record = rg_db::ops::password_reset_token_ops::find_by_hash(db, &token_hash).await?;
    // A token that is absent, spent or expired is the caller's problem and
    // answers 400; the lookup that decides it is ours and answers 5xx.
    let token = match token_record {
        Some(t) if !t.used && t.expires_at > Utc::now() => t,
        _ => {
            return Err(crate::error::invalid_request(
                "invalid or expired reset token",
            ))
        }
    };

    // Validate new password
    let user = user_ops::find_by_id(db, token.user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user not found"))?;

    // Re-checked here and not only in `forgot_password`: the account may have
    // been disabled in the fifteen minutes the token is alive for, and this
    // function ends by minting a JWT. Reported as a bad token rather than as
    // "disabled" — the holder of the link has proved nothing yet.
    if !user.is_usable() {
        return Err(crate::error::invalid_request(
            "invalid or expired reset token",
        ));
    }

    let password_validator = password::PasswordValidator::standard();
    password_validator
        .validate_with_username(new_password, &user.username)
        .map_err(|e| crate::error::invalid_request(e.to_string()))?;

    let new_hash = password::hash_password(new_password).context("failed to hash new password")?;

    // Update password
    use sea_orm::ActiveModelTrait;
    let mut active: UserActiveModel = user.clone().into();
    active.password_hash = Set(new_hash);
    active.updated_at = Set(Utc::now());
    active.update(db).await?;

    // This follows the password write, so a token minted after the reset uses
    // the new generation while every session from before it is rejected.
    let user = user_ops::invalidate_sessions(db, user.id).await?;

    // Mark token as used
    rg_db::ops::password_reset_token_ops::mark_used(db, token.id).await?;

    // Invalidate any other unused tokens for this user
    rg_db::ops::password_reset_token_ops::invalidate_user_tokens(db, user.id).await?;

    // The password is written either way — a refused session must not become a
    // refused reset, or an MFA account could never recover a lost password at
    // all. Only what the reset *hands back* is at stake below.
    if user.mfa_enabled {
        tracing::info!(
            user_id = user.id,
            "password reset completed; the account owes its second factor before a session exists"
        );
        return Ok(PasswordResetOutcome::SecondFactorRequired {
            user_id: user.id,
            username: user.username,
        });
    }

    // Generate new JWT
    let jwt_token =
        jwt::generate_token(user.id, &user.username, user.session_version, jwt_secret, 7)?;

    Ok(PasswordResetOutcome::Session(AuthResponse {
        token: jwt_token,
        user_id: user.id,
        username: user.username,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ldap_provider(secret: &str) -> rg_db::entities::sso_provider::Model {
        let key = crate::auth::encryption::derive_key(secret);
        let now = chrono::Utc::now();
        rg_db::entities::sso_provider::Model {
            id: 1,
            name: "Directory".into(),
            slug: "directory".into(),
            provider_type: "ldap".into(),
            client_id: None,
            client_secret_enc: None,
            discovery_url: None,
            scopes: None,
            ldap_host: Some("ldaps://ldap.example.com".into()),
            ldap_port: None,
            ldap_bind_dn: Some("cn=service,dc=example,dc=com".into()),
            ldap_bind_password_enc: Some(
                crate::auth::encryption::encrypt("bind-secret", &key).unwrap(),
            ),
            ldap_base_dn: Some("dc=example,dc=com".into()),
            ldap_user_filter: Some("(uid={username})".into()),
            enabled: true,
            icon_url: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn builds_fail_closed_tls_ldap_config_from_encrypted_provider() {
        let config = ldap_config_from_provider(&ldap_provider("jwt-secret"), "jwt-secret").unwrap();
        assert_eq!(config.host, "ldap.example.com");
        assert_eq!(config.port, 636);
        assert!(config.use_tls);
        assert!(!config.insecure_skip_tls_verify);
        assert_eq!(config.bind_password, "bind-secret");

        let mut implicit_tls = ldap_provider("jwt-secret");
        implicit_tls.ldap_host = Some("ldap.example.com".into());
        let config = ldap_config_from_provider(&implicit_tls, "jwt-secret").unwrap();
        assert!(config.use_tls);
        assert_eq!(config.port, 636);

        let mut explicit_plaintext = ldap_provider("jwt-secret");
        explicit_plaintext.ldap_host = Some("ldap://ldap.example.com".into());
        let config = ldap_config_from_provider(&explicit_plaintext, "jwt-secret").unwrap();
        assert!(!config.use_tls);
        assert_eq!(config.port, 389);

        let mut invalid = ldap_provider("jwt-secret");
        invalid.ldap_user_filter = Some("(objectClass=person)".into());
        assert!(ldap_config_from_provider(&invalid, "jwt-secret").is_err());
        assert!(ldap_config_from_provider(&ldap_provider("other-secret"), "jwt-secret").is_err());
    }

    #[tokio::test]
    async fn provisions_and_syncs_ldap_identity_without_a_local_password() {
        let db = rg_db::connect("sqlite::memory:").await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let created = resolve_ldap_identity(
            &db,
            None,
            1,
            crate::auth::ldap::LdapUser {
                username: "alice".into(),
                email: Some("alice@example.com".into()),
                display_name: Some("Alice".into()),
                dn: "uid=alice,dc=example,dc=com".into(),
                uid: Some("alice".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(created.auth_provider, "ldap");
        assert_eq!(created.ldap_provider_id, Some(1));
        assert!(created.password_hash.is_empty());
        assert!(!created.is_admin);

        let synced = resolve_ldap_identity(
            &db,
            Some(&created),
            1,
            crate::auth::ldap::LdapUser {
                username: "alice".into(),
                email: Some("changed@example.com".into()),
                display_name: Some("Alice Updated".into()),
                dn: "uid=alice,ou=people,dc=example,dc=com".into(),
                uid: Some("alice".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(synced.id, created.id);
        assert_eq!(synced.email, "alice@example.com");
        assert_eq!(synced.display_name.as_deref(), Some("Alice Updated"));
        assert_eq!(
            synced.ldap_dn.as_deref(),
            Some("uid=alice,ou=people,dc=example,dc=com")
        );
    }

    /// A concurrent request can pass the preflight before the winner writes its
    /// row. Recreate that post-check state directly: the second INSERT reaches
    /// the real UNIQUE constraint and must reuse the directory identity rather
    /// than report a valid bind as bad credentials.
    #[tokio::test]
    async fn a_lost_ldap_first_login_race_reuses_the_winner_identity() {
        let db = rg_db::connect("sqlite::memory:").await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();

        let winner = user_ops::create_ldap_user(
            &db,
            1,
            "alice_winner",
            "alice@example.com",
            Some("Alice"),
            "uid=directory-alice,dc=example,dc=com",
            Some("directory-alice"),
        )
        .await
        .expect("create the concurrent winner");

        let resolved = create_or_resolve_ldap_identity(
            &db,
            1,
            "alice",
            "alice@example.com",
            Some("Alice"),
            "uid=directory-alice,dc=example,dc=com",
            Some("directory-alice"),
        )
        .await
        .expect("the losing first login reuses the winner");

        assert_eq!(resolved.id, winner.id);
        assert_eq!(
            user_ops::find_by_ldap_provider_and_uid(&db, 1, "directory-alice")
                .await
                .expect("read LDAP identity")
                .map(|user| user.id),
            Some(winner.id),
            "the provider and uid still name one ForgeKeep identity"
        );
        assert!(
            user_ops::find_by_username(&db, "alice")
                .await
                .expect("read losing username")
                .is_none(),
            "the race must not create a second user"
        );
    }

    #[tokio::test]
    async fn a_username_collision_does_not_adopt_a_local_account_as_ldap() {
        let db = rg_db::connect("sqlite::memory:").await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();

        let local = user_ops::create_user(
            &db,
            "alice",
            "local-alice@example.com",
            "not-an-ldap-password",
            "Local Alice",
        )
        .await
        .expect("create unrelated local account");

        let error = create_or_resolve_ldap_identity(
            &db,
            1,
            "alice",
            "directory-alice@example.com",
            Some("Directory Alice"),
            "uid=directory-alice,dc=example,dc=com",
            Some("directory-alice"),
        )
        .await
        .expect_err("a bare username collision is not the LDAP identity");

        assert_eq!(error.to_string(), LDAP_IDENTITY_CONFLICT);
        assert_eq!(
            user_ops::find_by_username(&db, "alice")
                .await
                .expect("read local account")
                .map(|user| user.id),
            Some(local.id),
            "the LDAP login must not take over the local account"
        );
    }

    // ── H-5: forgot-password timing (card_47f0deb254ef) ─────────────

    /// An SMTP endpoint that accepts the TCP connection and then says nothing.
    /// A client that awaits the send blocks on the missing `220` greeting until
    /// lettre's own timeout (~60 s) — which is exactly the "slow SMTP" the
    /// enumeration test needs, without depending on an unreachable host.
    async fn blackhole_smtp() -> crate::email::SmtpConfig {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Hold the listener alive for the whole test; never write a greeting.
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        crate::email::SmtpConfig::new("127.0.0.1", port, "user", "pass", "forge@example.com")
    }

    async fn seed_user(db: &DatabaseConnection, username: &str, provider: &str) {
        let now = Utc::now();
        user_ops::create(
            db,
            UserActiveModel {
                username: Set(username.to_string()),
                email: Set(format!("{username}@example.com")),
                password_hash: Set(String::new()),
                auth_provider: Set(provider.to_string()),
                is_admin: Set(false),
                is_active: Set(true),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unknown_login_burns_dummy_verification_when_no_ldap_bind_runs() {
        let db = rg_db::connect("sqlite::memory:").await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();

        password::reset_dummy_verification_burns();
        let error = match login_with_configured_auth(
            &db,
            "missing-user",
            "definitely-not-the-password",
            "jwt-secret",
            "encryption-key",
        )
        .await
        {
            Ok(_) => panic!("unknown user must be rejected"),
            Err(error) => error,
        };

        assert!(
            format!("{error:#}").contains("invalid credentials"),
            "unexpected login error: {error:#}"
        );
        assert_eq!(
            password::dummy_verification_burns(),
            1,
            "an unknown login that never reaches LDAP bind must burn exactly one dummy verification"
        );
    }

    /// The H-5 property: response time must not tell the caller whether the
    /// address belongs to an account. Every branch is padded to one deadline
    /// measured from entry, and the SMTP send is detached — so the branch that
    /// only runs for a *real* local account cannot be the slow one.
    ///
    /// The upper bound is a hang-guard, not a deadline (same reasoning as
    /// `task_tracker::close_then_wait_drains_a_spawned_task`): under the old
    /// `sleep(100ms)`-per-branch code the existing-user branch awaited the
    /// blackholed SMTP and took ~60 s, so any finite bound catches the
    /// regression, while a tight one would just turn machine load into a red
    /// suite.
    #[tokio::test]
    async fn forgot_password_pads_every_branch_to_one_deadline() {
        let db = rg_db::connect("sqlite::memory:").await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        seed_user(&db, "alice", "local").await;
        seed_user(&db, "ldapuser", "ldap").await;

        let smtp = blackhole_smtp().await;
        let slack = std::time::Duration::from_secs(5);

        let mut elapsed = Vec::new();
        for address in [
            "nobody@example.com",   // no such account
            "ldapuser@example.com", // exists, but not a local account
            "alice@example.com",    // exists, local — the branch that sends mail
        ] {
            let start = std::time::Instant::now();
            forgot_password(&db, address, Some(&smtp), "https://forge.example.com")
                .await
                .unwrap();
            let took = start.elapsed();

            assert!(
                took >= FORGOT_PASSWORD_BUDGET,
                "{address}: returned in {took:?}, before the {FORGOT_PASSWORD_BUDGET:?} budget"
            );
            assert!(
                took < FORGOT_PASSWORD_BUDGET + slack,
                "{address}: took {took:?} — the SMTP round-trip is back on the request path"
            );
            elapsed.push(took);
        }

        // Same corollary stated as the card's acceptance check: the existing
        // address must not answer measurably slower than the unknown one.
        let (unknown, existing) = (elapsed[0], elapsed[2]);
        assert!(
            existing < unknown + slack,
            "existing address answered in {existing:?} vs {unknown:?} for an unknown one"
        );
    }

    /// Detaching the mail must not detach the work that precedes it: the reset
    /// token is still written before the call returns.
    #[tokio::test]
    async fn forgot_password_still_issues_a_token_for_a_local_account() {
        let db = rg_db::connect("sqlite::memory:").await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        seed_user(&db, "alice", "local").await;

        let smtp = blackhole_smtp().await;
        forgot_password(
            &db,
            "alice@example.com",
            Some(&smtp),
            "https://forge.example.com",
        )
        .await
        .unwrap();

        use sea_orm::EntityTrait;
        let user = user_ops::find_by_email(&db, "alice@example.com")
            .await
            .unwrap()
            .unwrap();
        let tokens = rg_db::entities::password_reset_token::Entity::find()
            .all(&db)
            .await
            .unwrap();
        assert_eq!(
            tokens.len(),
            1,
            "expected exactly one reset token to be written before the call returned"
        );
        assert_eq!(tokens[0].user_id, user.id);
    }
}
