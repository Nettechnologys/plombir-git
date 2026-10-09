//! User service — business logic for user registration, login, profile, admin management,
//! and password reset.

use anyhow::{bail, Context, Result};
use chrono::{Duration, Utc};
use sea_orm::{ActiveValue::Set, DatabaseConnection};

use rg_db::{entities::user::ActiveModel as UserActiveModel, ops::user_ops};

use crate::auth::{jwt, password};
use crate::user::provisioning::ProvisioningRefusal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    Password,
    Ldap,
}

pub struct LoginOutcome {
    /// Credential-proved account. Session material is deliberately absent: the
    /// HTTP door must first pass this row through its lifecycle finalizer and
    /// mint from the fresh `session_version` returned there.
    pub user: rg_db::entities::user::Model,
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

/// What a username is allowed to *look* like.
///
/// Rules:
/// - Length: 3–30 characters
/// - Must start with an alphanumeric character
/// - May only contain alphanumeric characters, hyphens, and underscores
/// - Must not contain path traversal sequences (`..` or `/`)
///
/// Split out from [`validate_username`] because the two questions have
/// different answers for an account that already exists. Shape is a property of
/// the string and never stops being true; "this name is a page of the
/// application" became true on the day the page was added, and an account that
/// predates it is still a working account whose owner logs in. An LDAP identity
/// is exactly that case — the directory owns the name and Plombir Git cannot
/// rename it — so re-resolving one asks this question and not the other.
///
/// Returns `Ok(())` if valid, `Err` with a descriptive message otherwise.
/// Every rejection here is a rule the *request* broke, so each one carries
/// `InvalidRequest` and is allowed to reach the client verbatim as a 400. This
/// function performs no I/O, so it has no other kind of failure to confuse it
/// with — but its callers do, and they used to answer 400 to those too.
pub fn validate_username_shape(username: &str) -> Result<()> {
    if username.len() < 3 || username.len() > 30 {
        return Err(crate::error::invalid_request(
            "username must be between 3 and 30 characters",
        ));
    }

    // The byte-length rule above is what makes this character exist, but that
    // is a second statement's promise. Reading it fallibly costs nothing and
    // answers with the same rule the caller already broke.
    let first_char = username.chars().next().ok_or_else(|| {
        crate::error::invalid_request("username must be between 3 and 30 characters")
    })?;
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

/// Whether `username` may be given to an owner that does not exist yet.
///
/// [`validate_username_shape`] plus the one rule that is about the URL space
/// rather than about the string: an owner is addressed by the first segment of
/// a path, and the application's own pages and endpoints live in that same
/// segment. A name that collides with one of them registers successfully and
/// then has no profile page at all, because both routers match their own route
/// before the owner one — see [`crate::namespace`].
///
/// This is the door for everything that *creates* an owner: registration,
/// organization creation, and the SSO and LDAP paths that derive a name with no
/// human in the loop. Re-resolving an owner that already exists asks
/// [`validate_username_shape`] instead, so a reservation added today cannot
/// lock somebody out of an account they have had for a year.
pub fn validate_username(username: &str) -> Result<()> {
    validate_username_shape(username)?;
    if crate::namespace::is_reserved_segment(username) {
        return Err(crate::error::invalid_request(format!(
            "'{username}' is reserved for a page of this application and cannot be an account name"
        )));
    }
    Ok(())
}

/// Register a new user.
///
/// Returns an `AuthResponse` with a JWT token.
pub async fn register(
    db: &DatabaseConnection,
    permit: super::registration::RegistrationPermit,
    username: &str,
    email: &str,
    plaintext_password: &str,
    jwt_secret: &str,
    directories: LdapDirectories<'_>,
) -> Result<AuthResponse> {
    // Validate inputs. A taken name or a taken address is the caller's to fix
    // and carries `Conflict`; the lookups performing them are ours, and a
    // failed one stays a 5xx instead of telling the client its own registration
    // was malformed.
    //
    // `Conflict`, not `InvalidRequest`: the body is well-formed and every rule
    // it must satisfy — charset, address shape, password strength — is checked
    // *below* this point and still answers 400. What refuses here is a row that
    // already exists, and no edit to the request removes it; the caller either
    // picks a different name or signs in as the account that holds it. That is
    // the same distinction `POST /user/keys` and `POST /repos/.../keys` already
    // draw when they answer 409 to "this SSH key is already registered".
    //
    // On enumeration: 409 states the account's existence no more loudly than
    // the message beside it already does — this instance names the taken
    // username verbatim, and `require_namespace_create` records why that is a
    // deliberate position rather than an oversight (the namespace is global, so
    // `POST /orgs` discloses the same thing to anyone who can log in). An
    // instance that decides to hide it has to drop the *message*, and the status
    // code follows it; hiding behind 400 while the body spells the name out
    // protects nothing.
    //
    // "Taken" means by an account *or* an organization: both answer to the
    // same `/{owner}` segment (card_4b0594a02218).
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

    // A name or address the directory holds belongs to the directory's person,
    // who signs in through it and gets this account provisioned then. Same
    // `409` and the same words as a name taken locally: the refusal does not
    // say *which* list holds it, so registration is no directory browser.
    if ldap_directory_holds(db, directories, username, Some(email)).await? {
        return Err(crate::error::conflict(format!(
            "username '{username}' or email '{email}' is already taken"
        )));
    }

    let password_hash = password::hash_password(plaintext_password)
        .await
        .context("failed to hash password")?;

    let user = create_registered_account(db, permit, username, email, password_hash).await?;
    let token = jwt::generate_token(user.id, &user.username, user.session_version, jwt_secret, 7)?;

    Ok(AuthResponse {
        token,
        user_id: user.id,
        username: user.username,
    })
}

/// Insert the account a self-registration produces, once every rule has been
/// checked and the password hashed: directly from [`register`], or from a
/// confirmed address (`account::confirm`).
///
/// `permit` is consumed: the first row is committed when this returns, so a
/// waiting registration may observe a non-empty database.
pub(crate) async fn create_registered_account(
    db: &DatabaseConnection,
    permit: super::registration::RegistrationPermit,
    username: &str,
    email: &str,
    password_hash: String,
) -> Result<rg_db::entities::user::Model> {
    let now = Utc::now();
    let model = UserActiveModel {
        username: Set(username.to_string()),
        email: Set(email.to_string()),
        password_hash: Set(password_hash),
        is_admin: Set(permit.grants_instance_admin()),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        ..Default::default()
    };

    let user = user_ops::create(db, model).await.map_err(|error| {
        // The two lookups above keep their precise sequential messages.  A
        // concurrent registration can still lose the gap before this insert;
        // that is the same client-correctable outcome, not an outage — and it
        // carries the same `Conflict` they do, so the status code does not
        // become a side-channel telling the loser it lost a race.
        if rg_db::is_unique_violation_anyhow(&error) {
            crate::error::conflict("username or email is already registered")
        } else {
            error
        }
    })?;
    // The first row is committed, so a waiting registration can now observe a
    // non-empty database. Do not serialise token generation behind the lock.
    // A bootstrap permit also retires the one-time setup token here.
    permit.finish();
    Ok(user)
}

/// What `plombir-git create-admin` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminBootstrap {
    /// A new account was created with the instance-admin flag.
    Created,
    /// The named account existed and was promoted.
    Promoted,
    /// The named account was an instance admin already; nothing changed.
    AlreadyAdmin,
}

/// Create an instance administrator from the host, with the rules
/// self-registration applies — taken name or address, username shape and
/// reservations, password strength — but with no bootstrap permit: the caller
/// is an operator with the database in hand, which is the position of trust the
/// setup token stands in for over HTTP (security audit finding #13).
///
/// Deliberately not routed through [`register`]: that path hands out the admin
/// flag exactly once, to the first row, and an operator adding a second
/// administrator from the CLI is asking for the flag on purpose.
pub async fn create_admin_account(
    db: &DatabaseConnection,
    username: &str,
    email: &str,
    plaintext_password: &str,
) -> Result<rg_db::entities::user::Model> {
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
    validate_username(username)?;
    if !valid_email(email) {
        return Err(crate::error::invalid_request(
            "email must contain '@' with a non-empty local and domain part",
        ));
    }
    password::PasswordValidator::standard()
        .validate_with_username(plaintext_password, username)
        .map_err(|e| crate::error::invalid_request(e.to_string()))?;

    let password_hash = password::hash_password(plaintext_password)
        .await
        .context("failed to hash password")?;
    let now = Utc::now();
    let model = UserActiveModel {
        username: Set(username.to_string()),
        email: Set(email.to_string()),
        password_hash: Set(password_hash),
        is_admin: Set(true),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        ..Default::default()
    };
    user_ops::create(db, model).await.map_err(|error| {
        if rg_db::is_unique_violation_anyhow(&error) {
            crate::error::conflict("username or email is already registered")
        } else {
            error
        }
    })
}

/// Give an existing account the instance-admin flag, from the host.
pub async fn promote_to_admin(
    db: &DatabaseConnection,
    user: rg_db::entities::user::Model,
) -> Result<AdminBootstrap> {
    use sea_orm::ActiveModelTrait;

    if user.is_admin {
        return Ok(AdminBootstrap::AlreadyAdmin);
    }
    let mut active: UserActiveModel = user.into();
    active.is_admin = Set(true);
    active.updated_at = Set(Utc::now());
    active
        .update(db)
        .await
        .context("promote the account to instance admin")?;
    Ok(AdminBootstrap::Promoted)
}

async fn verify_local_login(
    db: &DatabaseConnection,
    username_or_email: &str,
    plaintext_password: &str,
    source: Option<std::net::IpAddr>,
) -> Result<rg_db::entities::user::Model> {
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
    //
    // `source` is the address the server resolved, so one client cannot take
    // every place in the process-wide password limiter (card_a0f0cc7aed3a).
    let password_ok = password::verify_password_or_dummy(
        plaintext_password,
        user.as_ref().map(|u| u.password_hash.as_str()),
        source,
    )
    .await
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
    Ok(user)
}

/// Authenticate through the account's configured provider. Unknown users may
/// be provisioned only after a successful bind against an enabled LDAP source.
///
/// `source` is the client address when the caller knows it for certain — see
/// [`password::verify_password_or_dummy`]. It bounds the share of the password
/// limiter one client can hold.
pub async fn login_with_configured_auth(
    db: &DatabaseConnection,
    username_or_email: &str,
    plaintext_password: &str,
    encryption_key: &str,
    ldap_transport_policy: &crate::auth::ldap::LdapTransportPolicy,
    source: Option<std::net::IpAddr>,
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
            user: verify_local_login(db, username_or_email, plaintext_password, source).await?,
            method: LoginMethod::Password,
        }),
        Some("ldap") | None => {
            login_via_ldap(
                db,
                existing,
                username_or_email,
                plaintext_password,
                encryption_key,
                ldap_transport_policy,
                source,
            )
            .await
        }
        Some(_) => {
            // Account exists but authenticates through a provider no password
            // reaches — burn the same Argon2 work the local branch would. A
            // shed burn answers with the shed, as the local branch would.
            password::burn_dummy_verification(plaintext_password, source).await?;
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
    encryption_key: &str,
    ldap_transport_policy: &crate::auth::ldap::LdapTransportPolicy,
    source: Option<std::net::IpAddr>,
) -> Result<LoginOutcome> {
    let mut attempted_bind = false;
    let outcome = login_via_ldap_inner(
        db,
        existing,
        username_or_email,
        plaintext_password,
        encryption_key,
        ldap_transport_policy,
        &mut attempted_bind,
    )
    .await;
    if outcome.is_err() && !attempted_bind {
        // A shed burn replaces the rejection: answering an unknown account
        // with a fast 401 while a known one gets a 503 would put the
        // enumeration oracle back, only under load.
        password::burn_dummy_verification(plaintext_password, source).await?;
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
    encryption_key: &str,
    ldap_transport_policy: &crate::auth::ldap::LdapTransportPolicy,
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
    // A directory that never answered is not a directory that said no. The
    // loop keeps the first such failure so that, once every provider has been
    // tried and none of them authenticated anybody, the outage — and not the
    // `invalid credentials` verdict below — is what leaves this function. That
    // verdict costs its subject a strike on the brute-force counter at the
    // door, so laundering a ten-minute directory outage into it locks every
    // LDAP account on this instance out for fifteen minutes past the recovery.
    let mut outage: Option<anyhow::Error> = None;
    for provider in providers {
        let config =
            match ldap_config_from_provider(&provider, encryption_key, ldap_transport_policy) {
                Ok(config) => config,
                // Configuration is read before anything is dialled, so a provider
                // that cannot even be built has cost nobody anything: skipping it
                // is right, and it is not an outage as long as another provider
                // answers.
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
                let unreachable = error
                    .downcast_ref::<crate::error::UpstreamUnavailable>()
                    .is_some();
                tracing::warn!(
                    provider_id = provider.id,
                    unreachable,
                    error = %format!("{error:#}"),
                    "LDAP authentication attempt failed"
                );
                if unreachable {
                    outage.get_or_insert(error);
                }
                continue;
            }
            Err(_) => {
                tracing::warn!(
                    provider_id = provider.id,
                    "LDAP authentication attempt timed out"
                );
                outage.get_or_insert_with(|| {
                    crate::error::upstream_unavailable(format!(
                        "the LDAP directory of provider {} did not answer within 10s",
                        provider.id
                    ))
                });
                continue;
            }
        };

        let user = match resolve_ldap_identity(db, existing.as_ref(), &provider, ldap_user).await {
            Ok(user) => user,
            // A policy refusal is a decision this instance made, not a failure:
            // it is already logged at its source, it is the same answer for
            // every remaining provider, and it must reach the caller intact so
            // the door can answer 403 instead of "invalid credentials".
            Err(error) if error.downcast_ref::<ProvisioningRefusal>().is_some() => {
                return Err(error);
            }
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
        return Ok(LoginOutcome {
            user,
            method: LoginMethod::Ldap,
        });
    }
    // Nobody was authenticated. If that is because a directory broke, say so:
    // the door enumerates *verdicts*, so an error it does not recognise becomes
    // a retryable `502` with the chain in the operator log — and, crucially,
    // never reaches `record_failed_login`.
    if let Some(error) = outage {
        return Err(error);
    }
    bail!("invalid credentials")
}

/// Build a bindable LDAP config from a stored provider row.
///
/// Takes the *encryption* secret, not the JWT one: the bind password is
/// AES-GCM at rest, and conflating the two is what made a rotated signing
/// secret break every LDAP login with "bind password could not be decrypted"
/// (card_d740512de0a8).
/// What reaching the instance's LDAP directories needs: the key their bind
/// passwords are encrypted under and the transport policy that approves their
/// endpoints.
#[derive(Clone, Copy)]
pub struct LdapDirectories<'a> {
    pub encryption_key: &'a str,
    pub transport_policy: &'a crate::auth::ldap::LdapTransportPolicy,
}

/// Does an enabled LDAP directory already know this username (or address)?
///
/// Asked by the doors that let a *stranger* take a name — self-service
/// registration and organization creation — before the name is taken
/// (card_666fc82dd28d). Login sends a local `alice` to the local password and
/// never tries the directory, so a name squatted here locks the directory's
/// `alice` out for good, and her colleagues grant access to the squatter.
///
/// Fails closed: a directory that did not answer cannot say the name is free,
/// so the outage travels as [`crate::error::UpstreamUnavailable`] instead of
/// letting the name through. A provider whose configuration cannot even be
/// built is skipped, as login skips it — it cannot sign anybody in either.
/// With no LDAP provider configured this is one query and no network.
pub async fn ldap_directory_holds(
    db: &DatabaseConnection,
    directories: LdapDirectories<'_>,
    username: &str,
    email: Option<&str>,
) -> Result<bool> {
    let providers = rg_db::ops::sso_provider_ops::list_enabled(db)
        .await?
        .into_iter()
        .filter(|provider| provider.provider_type == "ldap");
    let mut outage: Option<anyhow::Error> = None;
    for provider in providers {
        let config = match ldap_config_from_provider(
            &provider,
            directories.encryption_key,
            directories.transport_policy,
        ) {
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
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            crate::auth::ldap::directory_holds(&config, username, email),
        )
        .await
        {
            Ok(Ok(true)) => return Ok(true),
            Ok(Ok(false)) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    provider_id = provider.id,
                    error = %format!("{error:#}"),
                    "LDAP directory lookup failed"
                );
                outage.get_or_insert(error);
            }
            Err(_) => {
                outage.get_or_insert_with(|| {
                    crate::error::upstream_unavailable(format!(
                        "the LDAP directory of provider {} did not answer within 10s",
                        provider.id
                    ))
                });
            }
        }
    }
    match outage {
        // Keep (or add) the upstream tag so the door answers 5xx, not 400.
        Some(error)
            if error
                .downcast_ref::<crate::error::UpstreamUnavailable>()
                .is_some() =>
        {
            Err(error)
        }
        Some(error) => Err(error.context(crate::error::UpstreamUnavailable::new(
            "could not ask the LDAP directory whether the name is free",
        ))),
        None => Ok(false),
    }
}

fn ldap_config_from_provider(
    provider: &rg_db::entities::sso_provider::Model,
    encryption_key: &str,
    ldap_transport_policy: &crate::auth::ldap::LdapTransportPolicy,
) -> Result<crate::auth::ldap::LdapConfig> {
    let raw_host = provider
        .ldap_host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .context("LDAP host is missing")?;
    let explicit_port = provider
        .ldap_port
        .map(|port| u16::try_from(port).context("LDAP port is invalid"))
        .transpose()?;
    let endpoint = ldap_transport_policy.resolve_endpoint(raw_host, explicit_port)?;
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
    let plaintext_approved = endpoint.plaintext_approved();
    Ok(crate::auth::ldap::LdapConfig {
        host: endpoint.host,
        port: endpoint.port,
        use_tls: endpoint.use_tls,
        plaintext_approved,
        insecure_skip_tls_verify: false,
        bind_dn: required(provider.ldap_bind_dn.as_deref(), "bind DN")?,
        bind_password,
        base_dn: required(provider.ldap_base_dn.as_deref(), "base DN")?,
        user_filter,
    })
}

/// Dial the directory this provider row describes, for the admin's "test
/// connection" button.
///
/// Three outcomes, and they are three different answers — the whole reason the
/// button exists is to say which one happened (card_a86f0776021c):
///
/// * the row is not an LDAP provider, or its stored configuration cannot be
///   turned into a bindable config at all — [`crate::error::InvalidRequest`],
///   nothing was dialled and the admin has a form to fix;
/// * the directory refused, was unreachable, or never answered in time —
///   [`crate::error::UpstreamUnavailable`], which is not the admin's request to
///   fix and must stay retryable;
/// * it bound, and the button says so.
pub async fn test_ldap_provider_connection(
    provider: &rg_db::entities::sso_provider::Model,
    encryption_key: &str,
    ldap_transport_policy: &crate::auth::ldap::LdapTransportPolicy,
) -> Result<()> {
    if provider.provider_type != "ldap" {
        return Err(crate::error::invalid_request(
            "connection testing is only supported for LDAP providers",
        ));
    }
    // `to_string()`, not `{:#}`: every outer context this helper attaches is one
    // of its own fixed strings ("LDAP host is missing", "LDAP port is invalid"),
    // which is exactly what the admin needs to see and is safe to render
    // verbatim (H-05) — while the layered cause below it stays in the log.
    let config = ldap_config_from_provider(provider, encryption_key, ldap_transport_policy)
        .map_err(|error| {
            let reason = error.to_string();
            error.context(crate::error::InvalidRequest::new(reason))
        })?;
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        crate::auth::ldap::test_connection(&config),
    )
    .await
    {
        Ok(result) => result,
        Err(elapsed) => Err(anyhow::Error::new(elapsed).context(
            crate::error::UpstreamUnavailable::new("the LDAP directory did not answer in time"),
        )),
    }
}

async fn resolve_ldap_identity(
    db: &DatabaseConnection,
    existing: Option<&rg_db::entities::user::Model>,
    provider: &rg_db::entities::sso_provider::Model,
    ldap_user: crate::auth::ldap::LdapUser,
) -> Result<rg_db::entities::user::Model> {
    let ldap_provider_id = provider.id;
    let username = ldap_user
        .uid
        .as_deref()
        .unwrap_or(&ldap_user.username)
        .trim();
    // Shape only, and deliberately: the directory owns this name, Plombir Git
    // cannot rename it, and an account provisioned before a page claimed that
    // segment is still a working account. Refusing the *login* would take
    // everything away to fix an unreachable profile page. The reservation is
    // applied below, on the branch that would create a new one.
    validate_username_shape(username).context("LDAP username is not valid for Plombir Git")?;

    if let Some(user) = existing {
        if user.auth_provider != "ldap"
            || user.username != username
            || user
                .ldap_provider_id
                .is_some_and(|provider_id| provider_id != ldap_provider_id)
        {
            bail!(LDAP_IDENTITY_CONFLICT);
        }
        let synced = user_ops::sync_ldap_identity(
            db,
            user.id,
            ldap_provider_id,
            ldap_user.display_name.as_deref(),
            ldap_user.uid.as_deref(),
        )
        .await?;
        // The bind authenticated the identity that was observed above. If its
        // account disappeared or was rebound before the sync, that observation
        // is stale: do not leak SeaORM's RecordNotUpdated and, critically, do
        // not fall through to the provisioning branch below.
        return synced.ok_or_else(|| anyhow::anyhow!(LDAP_IDENTITY_CONFLICT));
    }

    // An organization holding the name is the same conflict: a provisioned
    // account would answer `/{name}` in its place (card_4b0594a02218).
    if crate::namespace::owner_name_is_taken(db, username).await? {
        bail!(LDAP_IDENTITY_CONFLICT);
    }
    // From here the function creates an account, so the URL-space rule applies
    // in full: a fresh owner named after one of this application's own pages
    // would be reachable nowhere. Nothing can rename a directory entry, so the
    // honest answer is to refuse the provision and say which name it was —
    // silently creating an account with no page is what this refusal replaces.
    validate_username(username)
        .context("LDAP username cannot be provisioned as a new Plombir Git account")?;
    let email = ldap_user
        .email
        .as_deref()
        .map(str::trim)
        .filter(|email| valid_email(email))
        .context("LDAP account does not have a valid email address")?;

    // Everything above this line concerns an account that already exists; from
    // here on the function creates one, which is the single question the
    // provisioning policy answers. A directory that has stopped provisioning
    // still signs in every member who already has an account — the branch that
    // returns above never reaches this check.
    //
    // The bind succeeded, so this is not a rejected credential and must not be
    // dressed as one: the refusal travels as itself and the HTTP layer answers
    // 403 with the reason, rather than telling a member of the directory that
    // their password was wrong.
    // The address comes from the operator's own directory, not from a profile
    // field its holder typed, so the directory's word is the confirmation.
    if let Err(refusal) = crate::user::provisioning::authorize(
        provider,
        email,
        crate::user::provisioning::AddressAssurance::Verified,
    ) {
        tracing::warn!(
            provider_id = ldap_provider_id,
            username,
            reason = refusal.reason(),
            "LDAP first login refused: this directory may not create accounts here"
        );
        return Err(anyhow::Error::new(refusal));
    }

    if user_ops::find_by_email(db, email).await?.is_some() {
        bail!(LDAP_IDENTITY_CONFLICT);
    }

    create_or_resolve_ldap_identity(
        db,
        ldap_provider_id,
        username,
        email,
        ldap_user.display_name.as_deref(),
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
    ldap_uid: Option<&str>,
) -> Result<rg_db::entities::user::Model> {
    let error = match user_ops::create_ldap_user(
        db,
        ldap_provider_id,
        username,
        email,
        display_name,
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

/// The shape every stored address must have, wherever it enters Plombir Git:
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
///
/// `offset` is a row offset, not a page index — the name matters here, because
/// this function is a pass-through and the caller fills it from
/// `PaginationParams::offset()`. Called `page` while carrying an offset, it read
/// as correct at both ends and was wrong in the middle (card_1e3c1cff05b4).
pub async fn list_users_admin(
    db: &DatabaseConnection,
    offset: u64,
    limit: u64,
) -> Result<PaginatedUsers> {
    let (users, total) = user_ops::list_users(db, offset, limit).await?;
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

/// Delete a user (admin only), together with the storage of every repository
/// they own.
///
/// This used to be one `DELETE FROM users`, which was not the harmless row
/// removal it looked like: `repositories.owner_id` is declared
/// `REFERENCES users(id) ON DELETE CASCADE` and foreign keys are enforced on
/// every backend, so that statement *destroyed* the repository rows — while
/// every byte they named stayed live in Git storage, in the `packages` / `lfs`
/// / `releases` / `attachments` prefixes, in the CI cache and artifact trees
/// and in the OCI registry, with no row left to reach them and no sweep left to
/// collect them. The bytes have to be retired first, through the same staged,
/// compensated contract a routed repository deletion uses.
///
/// Two ownerships are refused rather than cascaded, because absorbing them here
/// would repeat the same mistake one level up:
///
/// * **Organizations owned by this account.** `organizations.owner_id` carries
///   no foreign key at all, so the row survives the delete pointing at a user
///   id nothing resolves — while the cascade above still takes its
///   repositories. Deleting or transferring the organization is its own
///   operation and its own decision.
/// * **Repositories in an organization's namespace that still name this account
///   as `owner_id`.** They are reached by the same cascade even though they are
///   not this user's to delete.
///
/// What the account merely *produced* is the opposite case and is not retired
/// here at all: an attachment on somebody else's issue, an asset in somebody
/// else's release, a release published into somebody else's repository. Those
/// rows live in a namespace that is not going anywhere, and
/// `m20260805_000002_uploads_outlive_their_uploader` made all three columns
/// `ON DELETE SET NULL` so the database — not this function — guarantees they
/// survive their author with a ghost uploader. Retiring them instead would mean
/// deleting files out of a working repository because an unrelated account
/// closed (card_1cfc81035e92).
///
/// The repositories are retired one at a time, and each one is individually
/// atomic. A failure part-way through leaves the already-retired ones retired
/// and the user row untouched, so re-running the request resumes where it
/// stopped. What it never does is answer `2xx` with bytes still live.
pub async fn delete_user(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    blob_storage: &dyn crate::blob_storage::BlobStorage,
    oci_storage: &crate::package_registry::oci::storage::OciStorage,
    user_id: i64,
) -> Result<()> {
    // Refused before the claim rather than after it, so the common "you still
    // own an organization" answer never briefly locks an account that is not
    // going anywhere. The claimed retirement re-reads the same two ownerships
    // on every pass, which is where they are actually load-bearing.
    refuse_ownerships_this_deletion_may_not_cascade(db, user_id).await?;

    // Close the namespace before inventorying it. Two concurrent deletes both
    // pass the checks above, and this claim is the one statement only one of
    // them can win; the loser gets the same 404 as a request for an account
    // that was never there rather than a second retirement of the same storage.
    //
    // It is also what makes the inventory below mean something. Without it, a
    // `POST /repos` that resolved this account a moment earlier could commit
    // its repository row after the inventory had been taken — and the final
    // `DELETE FROM users` would then *destroy* that row through
    // `repositories.owner_id ON DELETE CASCADE`, leaving Git, blob, CI and
    // registry bytes with nothing naming them at all (card_da1abc6074ac).
    if !user_ops::begin_user_retirement(db, user_id).await? {
        return Err(crate::error::not_found("user"));
    }

    if let Err(error) =
        retire_account_repositories(db, repo_root, blob_storage, oci_storage, user_id).await
    {
        // The failure is retryable — the account and every repository not yet
        // retired are exactly where they were — so the namespace has to reopen
        // with them.
        release_user_retirement_claim(db, user_id).await;
        return Err(error);
    }

    // The claim outlives every failure until the row it marks is gone, this one
    // included: a deletion that retired the storage and then could not remove
    // the row would otherwise leave a marked account with no repositories left,
    // which no retry could ever claim again and whose owner could never log in.
    //
    // `rg-db` cannot depend on this crate's domain error types, so absence
    // crosses that boundary as a value and becomes a typed 404 here.
    match user_ops::delete_by_id(db, user_id).await {
        // Nothing left to release — the row the marker lived on is gone.
        Ok(true) => Ok(()),
        Ok(false) => Err(crate::error::not_found("user")),
        Err(error) => {
            release_user_retirement_claim(db, user_id).await;
            Err(error)
        }
    }
}

/// How many times the retirement loop re-reads the account's repositories
/// before giving up.
///
/// The claim stops new requests from resolving this account at all, so the only
/// repositories that can still appear are the ones already in flight when it
/// landed. That set is finite and small; a pass that keeps finding more of them
/// means something is creating repositories through a path that ignores the
/// claim, and looping forever would hide that rather than report it.
const MAX_ACCOUNT_RETIREMENT_PASSES: usize = 8;

/// Retire every repository of a claimed account, until a pass finds none.
///
/// One pass is not enough. A repository creation that resolved this account
/// before the claim landed can still commit its row afterwards, so the
/// inventory is re-read until it comes back empty — at which point no row can
/// appear any more, because every request that could have produced one has
/// either committed (and been retired here) or will find the claim and refuse.
///
/// The two ownerships this deletion refuses rather than cascades are re-read on
/// every pass for the same reason: an organization or an organization-scoped
/// repository that commits after a single check would be reached by the same
/// cascade, which is exactly the outcome the check exists to prevent.
async fn retire_account_repositories(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    blob_storage: &dyn crate::blob_storage::BlobStorage,
    oci_storage: &crate::package_registry::oci::storage::OciStorage,
    user_id: i64,
) -> Result<()> {
    for pass in 0..MAX_ACCOUNT_RETIREMENT_PASSES {
        refuse_ownerships_this_deletion_may_not_cascade(db, user_id).await?;

        // Before anything is moved, and covering both namespaces: a database
        // that cannot answer "which repositories does this account own" aborts
        // the deletion instead of reporting a success that leaves their bytes
        // live.
        let owned = rg_db::ops::repo_ops::list_active_by_owner_id(db, user_id)
            .await
            .context(
                "failed to inventory the repositories of this account — its storage cannot be \
                 retired without them",
            )?;
        if owned.is_empty() {
            return Ok(());
        }
        if pass > 0 {
            tracing::info!(
                user_id,
                pass,
                repositories = owned.len(),
                "account deletion found repositories created while it was retiring — retiring \
                 them too"
            );
        }

        for repo in &owned {
            if let Err(error) =
                crate::repo::service::delete_repo(db, repo_root, blob_storage, oci_storage, repo)
                    .await
            {
                tracing::error!(
                    user_id,
                    repo_id = repo.id,
                    repo = %repo.name,
                    error = %format!("{error:#}"),
                    "account deletion stopped: this repository's storage could not be retired, so \
                     the account row was left in place and the request can be retried"
                );
                return Err(error.context(format!(
                    "failed to retire repository '{}' (id {}) while deleting its owner",
                    repo.name, repo.id
                )));
            }
        }
    }
    Err(crate::error::conflict(format!(
        "account {user_id} is still gaining repositories after \
         {MAX_ACCOUNT_RETIREMENT_PASSES} retirement passes; it was left in place"
    )))
}

/// Refuse the two ownerships this deletion is not entitled to absorb.
///
/// Both are reached by `ON DELETE CASCADE` on `repositories.owner_id` — or, for
/// the organization row itself, by no foreign key at all — so absorbing them
/// here would repeat one level up the very mistake this deletion exists to
/// avoid. See [`delete_user`] for what each of them would leave behind.
async fn refuse_ownerships_this_deletion_may_not_cascade(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<()> {
    let owned_orgs = rg_db::ops::org_ops::list_orgs_owned_by(db, user_id)
        .await
        .context(
            "failed to inventory the organizations owned by this account — it cannot be deleted \
             without them",
        )?;
    // A bot answers to its owner and stops working with them; the reference
    // carries no cascade, because deleting the bot row would take its
    // repository rows through `repositories.owner_id` while their bytes stayed
    // in storage. Deleting the bots is the owner's own, storage-safe operation.
    let bots = rg_db::ops::user_ops::list_bots_by_owner(db, user_id)
        .await
        .context("failed to inventory the bots owned by this account")?;
    if !bots.is_empty() {
        let names = bots
            .iter()
            .map(|bot| bot.username.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(crate::error::conflict(format!(
            "account still owns the bot(s) {names}; delete them before deleting the account"
        )));
    }

    if !owned_orgs.is_empty() {
        let names = owned_orgs
            .iter()
            .map(|org| org.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(crate::error::conflict(format!(
            "account still owns the organization(s) {names}; delete or transfer them before \
             deleting the account"
        )));
    }

    let organization_scoped = rg_db::ops::repo_ops::list_active_by_owner_id(db, user_id)
        .await
        .context(
            "failed to inventory the repositories of this account — its storage cannot be retired \
             without them",
        )?
        .into_iter()
        .filter(|repo| repo.org_id.is_some())
        .map(|repo| repo.name)
        .collect::<Vec<_>>();
    if !organization_scoped.is_empty() {
        return Err(crate::error::conflict(format!(
            "account is still recorded as the owner of the organization repositor{} {}; \
             transfer them before deleting the account",
            if organization_scoped.len() == 1 {
                "y"
            } else {
                "ies"
            },
            organization_scoped.join(", ")
        )));
    }
    Ok(())
}

/// Reopen an account whose retirement could not finish.
///
/// The caller is already returning the original failure, so a failed release
/// can only be reported: the account stays in the database, but nobody can log
/// in as it and no repository can be created in its namespace until the marker
/// is cleared by hand or by a retried deletion that succeeds.
async fn release_user_retirement_claim(db: &DatabaseConnection, user_id: i64) {
    if let Err(error) = user_ops::abort_user_retirement(db, user_id).await {
        tracing::error!(
            user_id,
            error = %format!("{error:#}"),
            "failed to reopen an account whose deletion was aborted — it can neither authenticate \
             nor receive repositories until its `deleted_at` marker is cleared"
        );
    }
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

/// How long after one reset link an account gets no other.
///
/// Every request used to mail a new link and void the previous one, so an
/// anonymous loop on one address was a mail bomb that also kept the owner
/// from ever using the link in their inbox (card_0beff149adbd). A request
/// inside the window is answered exactly like every other — same body, same
/// padded deadline — and changes nothing: no mail, the link already sent
/// stays the valid one.
pub const PASSWORD_RESET_COOLDOWN: Duration = Duration::seconds(60);

/// Initiate a password reset. Generates a token and sends an email.
/// Silently succeeds even if the email is not found (to prevent user enumeration).
/// H-5: every code path returns at the same deadline ([`FORGOT_PASSWORD_BUDGET`]
/// after entry) and no path awaits the SMTP send, so the response time carries
/// no signal about whether the address exists.
/// The account a reset link was actually issued to.
///
/// `None` from [`forgot_password`] covers every branch the endpoint answers
/// *identically* to a successful one — an address nobody holds, an account whose
/// password lives in LDAP or an OAuth provider, a deactivated account. The
/// distinction exists for the journal and for nothing else: an entry written on
/// a branch that issued nothing would turn `audit_log` into the enumeration
/// oracle the uniform response and the timing budget are both there to prevent.
#[derive(Debug, Clone)]
pub struct PasswordResetIssued {
    /// The account the link lets back in.
    pub user_id: i64,
    /// Its name at the moment the link was issued.
    pub username: String,
}

pub async fn forgot_password(
    db: &DatabaseConnection,
    email: &str,
    smtp_config: Option<&crate::email::SmtpConfig>,
    base_url: &str,
) -> Result<Option<PasswordResetIssued>> {
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
) -> Result<Option<PasswordResetIssued>> {
    let Some(user) = user_ops::find_by_email(db, email).await? else {
        return Ok(None);
    };

    // Only local users can reset via email (LDAP/OAuth users use their provider)
    if user.auth_provider != "local" {
        return Ok(None);
    }

    // A disabled account gets the same silent no-op an unknown address gets.
    // Otherwise the reset flow is a way back in that needs no administrator:
    // the mail still lands in the mailbox the offboarded user controls, and
    // `reset_password` hands out a working session at the end of it.
    if !user.is_usable() {
        return Ok(None);
    }

    // One link per cooldown: the one already in the inbox stays the one that
    // works, and no second mail goes out.
    if rg_db::ops::password_reset_token_ops::issued_since(
        db,
        user.id,
        Utc::now() - PASSWORD_RESET_COOLDOWN,
    )
    .await?
    {
        return Ok(None);
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
            "We received a request to reset the password for your Plombir Git account ({}). \
             Click the button below to set a new password. This link expires in 15 minutes.",
            user.username
        );
        crate::task_tracker::delivery_tracker().spawn(async move {
            let subject = "Reset your Plombir Git password";
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

    Ok(Some(PasswordResetIssued {
        user_id: user.id,
        username: user.username,
    }))
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

    let new_hash = password::hash_password(new_password)
        .await
        .context("failed to hash new password")?;

    // The expensive Argon2 pass stays outside the transaction. Inside it, the
    // link claim, conditional active-user write, session-generation bump and
    // sibling-token invalidation either all commit or all roll back. A reset
    // that loses to account retirement therefore still holds the same usable
    // link from the client's point of view, rather than a spent link plus a 500.
    let Some(user) = rg_db::ops::password_reset_token_ops::complete_password_reset(
        db, token.id, user.id, &new_hash,
    )
    .await?
    else {
        return Err(crate::error::invalid_request(
            "invalid or expired reset token",
        ));
    };

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
            auto_provision: true,
            allowed_email_domains: None,
            icon_url: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn builds_fail_closed_tls_ldap_config_from_encrypted_provider() {
        let secure_policy = crate::auth::ldap::LdapTransportPolicy::default();
        let config =
            ldap_config_from_provider(&ldap_provider("jwt-secret"), "jwt-secret", &secure_policy)
                .unwrap();
        assert_eq!(config.host, "ldap.example.com");
        assert_eq!(config.port, 636);
        assert!(config.use_tls);
        assert!(!config.insecure_skip_tls_verify);
        assert_eq!(config.bind_password, "bind-secret");

        let mut implicit_tls = ldap_provider("jwt-secret");
        implicit_tls.ldap_host = Some("ldap.example.com".into());
        implicit_tls.ldap_port = Some(1389);
        let config =
            ldap_config_from_provider(&implicit_tls, "jwt-secret", &secure_policy).unwrap();
        assert!(config.use_tls);
        assert_eq!(config.port, 1389);

        let mut explicit_plaintext = ldap_provider("jwt-secret");
        explicit_plaintext.ldap_host = Some("ldap://ldap.example.com".into());
        assert!(
            ldap_config_from_provider(&explicit_plaintext, "jwt-secret", &secure_policy).is_err()
        );
        let plaintext_policy =
            crate::auth::ldap::LdapTransportPolicy::parse(&["ldap://ldap.example.com:389".into()])
                .unwrap();
        let config =
            ldap_config_from_provider(&explicit_plaintext, "jwt-secret", &plaintext_policy)
                .unwrap();
        assert!(!config.use_tls);
        assert_eq!(config.port, 389);

        let mut invalid = ldap_provider("jwt-secret");
        invalid.ldap_user_filter = Some("(objectClass=person)".into());
        assert!(ldap_config_from_provider(&invalid, "jwt-secret", &secure_policy).is_err());
        assert!(ldap_config_from_provider(
            &ldap_provider("other-secret"),
            "jwt-secret",
            &secure_policy,
        )
        .is_err());
    }

    #[tokio::test]
    async fn provisions_and_syncs_ldap_identity_without_a_local_password() {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let created = resolve_ldap_identity(
            &db,
            None,
            &ldap_provider("jwt-secret"),
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
            &ldap_provider("jwt-secret"),
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
        // The DN itself is no longer stored — the identity a bind resolves
        // through is the provider/uid pair, and that is what a sync refreshes
        // (card_b70de2169bd6).
        assert_eq!(synced.ldap_uid.as_deref(), Some("alice"));
        assert_eq!(synced.ldap_provider_id, Some(1));
    }

    fn directory_member() -> crate::auth::ldap::LdapUser {
        crate::auth::ldap::LdapUser {
            username: "alice".into(),
            email: Some("alice@example.com".into()),
            display_name: Some("Alice".into()),
            dn: "uid=alice,dc=example,dc=com".into(),
            uid: Some("alice".into()),
        }
    }

    /// A reserved name is refused at provision and tolerated at sign-in
    /// (card_e3f6f110a622).
    ///
    /// The directory owns the name and Plombir Git cannot rename it, so the two
    /// halves have to answer differently. Creating `search` would make an
    /// account whose `/{owner}` page is the repository search screen and always
    /// will be — refuse it, and say which name. Signing in an account already
    /// holding such a name is a different question entirely: it was provisioned
    /// before the reservation existed, its owner uses it every day, and taking
    /// their login away would be a far larger fault than the unreachable
    /// profile page it "fixes".
    #[tokio::test]
    async fn an_ldap_name_the_url_space_claims_is_refused_at_provision_and_kept_at_sign_in() {
        let db = crate::test_support::migrated_memory_database().await;
        let member = |name: &str| crate::auth::ldap::LdapUser {
            username: name.into(),
            email: Some(format!("{name}@example.com")),
            display_name: Some(name.into()),
            dn: format!("uid={name},dc=example,dc=com"),
            uid: Some(name.into()),
        };

        let error =
            resolve_ldap_identity(&db, None, &ldap_provider("jwt-secret"), member("search"))
                .await
                .expect_err("a new account may not take a name the application answers for");
        let message = format!("{error:#}");
        assert!(
            message.contains("search") && message.contains("reserved"),
            "the refusal did not say which name it was, or why: {message}"
        );

        // The same name on an account that predates the reservation: seeded
        // straight through the op this very function calls, which is how such a
        // row got there before there was a list.
        let existing = user_ops::create_ldap_user(
            &db,
            1,
            "search",
            "search@example.com",
            Some("Search"),
            Some("search"),
        )
        .await
        .expect("seed an LDAP account holding a reserved name");

        let synced = resolve_ldap_identity(
            &db,
            Some(&existing),
            &ldap_provider("jwt-secret"),
            member("search"),
        )
        .await
        .expect("an account that already holds the name still signs in");
        assert_eq!(synced.id, existing.id);
    }

    /// card_0ae3deacd3f0: the LDAP door asks the same policy column the SSO
    /// callback asks. A successful bind proves who someone is at the
    /// directory; it does not decide that this instance hands them an account.
    ///
    /// The refusal travels as a typed [`ProvisioningRefusal`] rather than as
    /// "invalid credentials", because the HTTP door has to answer 403 with the
    /// reason instead of telling a member of the directory their password was
    /// wrong — and has to keep it out of the brute-force counter.
    #[tokio::test]
    async fn a_directory_that_may_not_provision_creates_no_account() {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();

        let mut closed = ldap_provider("jwt-secret");
        closed.auto_provision = false;
        let error = resolve_ldap_identity(&db, None, &closed, directory_member())
            .await
            .expect_err("a directory that may not provision must refuse the first login");
        assert_eq!(
            error.downcast_ref::<ProvisioningRefusal>(),
            Some(&ProvisioningRefusal::AutoProvisionDisabled),
            "the refusal must stay recognisable to the door that answers it: {error:#}"
        );
        assert!(
            user_ops::find_by_username(&db, "alice")
                .await
                .unwrap()
                .is_none(),
            "a refused LDAP first login provisioned an account anyway"
        );

        // Baseline in the same test: only the policy differs, and the identical
        // bind now provisions — so the refusal above is the policy, not a
        // broken fixture.
        let created =
            resolve_ldap_identity(&db, None, &ldap_provider("jwt-secret"), directory_member())
                .await
                .expect("the same first login is provisioned when the directory may");
        assert_eq!(created.username, "alice");

        // And the switch keeps working for people who already have an account:
        // the existing-identity branch never reaches the policy.
        let synced = resolve_ldap_identity(&db, Some(&created), &closed, directory_member())
            .await
            .expect("an existing directory account still signs in through a closed provider");
        assert_eq!(synced.id, created.id);
    }

    /// The narrower rule, on the same door: the directory provisions, but not
    /// for this address's domain.
    #[tokio::test]
    async fn an_ldap_address_outside_the_allowlist_creates_no_account() {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();

        let mut narrowed = ldap_provider("jwt-secret");
        narrowed.allowed_email_domains = Some("corp.example".into());
        let error = resolve_ldap_identity(&db, None, &narrowed, directory_member())
            .await
            .expect_err("an address outside the allowlist must not be provisioned");
        assert_eq!(
            error.downcast_ref::<ProvisioningRefusal>(),
            Some(&ProvisioningRefusal::EmailDomainNotAllowed)
        );
        assert!(user_ops::find_by_username(&db, "alice")
            .await
            .unwrap()
            .is_none());

        narrowed.allowed_email_domains = Some("example.com".into());
        let created = resolve_ldap_identity(&db, None, &narrowed, directory_member())
            .await
            .expect("an address inside the allowlist is provisioned");
        assert_eq!(created.email, "alice@example.com");
    }

    /// A concurrent request can pass the preflight before the winner writes its
    /// row. Recreate that post-check state directly: the second INSERT reaches
    /// the real UNIQUE constraint and must reuse the directory identity rather
    /// than report a valid bind as bad credentials.
    #[tokio::test]
    async fn a_lost_ldap_first_login_race_reuses_the_winner_identity() {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();

        let winner = user_ops::create_ldap_user(
            &db,
            1,
            "alice_winner",
            "alice@example.com",
            Some("Alice"),
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
            "the provider and uid still name one Plombir Git identity"
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
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
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
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();

        password::reset_dummy_verification_burns();
        let error = match login_with_configured_auth(
            &db,
            "missing-user",
            "definitely-not-the-password",
            "encryption-key",
            &crate::auth::ldap::LdapTransportPolicy::default(),
            None,
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
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
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
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
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

    /// card_0beff149adbd: a second request inside the cooldown issues nothing —
    /// the link already mailed stays the valid one — and is answered like any
    /// other; once the cooldown is over, a new link is issued again.
    #[tokio::test]
    async fn forgot_password_issues_one_link_per_cooldown() {
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        seed_user(&db, "alice", "local").await;
        let tokens = || async {
            rg_db::entities::password_reset_token::Entity::find()
                .all(&db)
                .await
                .unwrap()
        };

        let first = forgot_password(&db, "alice@example.com", None, "https://forge.example.com")
            .await
            .unwrap();
        assert!(first.is_some(), "the first request issues a link");
        let issued = tokens().await;
        assert_eq!(issued.len(), 1);

        let second = forgot_password(&db, "alice@example.com", None, "https://forge.example.com")
            .await
            .unwrap();
        assert!(
            second.is_none(),
            "a request inside the cooldown issues nothing"
        );
        assert_eq!(
            tokens().await,
            issued,
            "the link already sent must stay the valid one"
        );

        // Age the link past the cooldown.
        let mut aged: rg_db::entities::password_reset_token::ActiveModel = issued[0].clone().into();
        aged.created_at =
            Set(issued[0].created_at - PASSWORD_RESET_COOLDOWN - Duration::seconds(1));
        aged.update(&db).await.unwrap();

        let third = forgot_password(&db, "alice@example.com", None, "https://forge.example.com")
            .await
            .unwrap();
        assert!(third.is_some(), "after the cooldown a new link is issued");
        let replaced = tokens().await;
        assert_eq!(replaced.len(), 1);
        assert_ne!(replaced[0].token_hash, issued[0].token_hash);
    }
}

/// card_da1abc6074ac: an account's deletion and a repository creation into its
/// namespace are two multi-statement lifecycles over storage no transaction can
/// hold — and `repositories.owner_id` is `ON DELETE CASCADE`, so the losing
/// order does not orphan a repository row, it destroys it. Between them they
/// must never leave a Git tree, a blob prefix or a registry namespace that no
/// row names any more.
#[cfg(test)]
mod account_retirement_race_tests {
    use super::*;
    use crate::blob_storage::LocalBlobStorage;
    use crate::package_registry::oci::storage::OciStorage;
    use rg_db::ops::repo_ops;
    use sea_orm::{ConnectOptions, Database};
    use std::sync::Arc;

    async fn setup_db() -> DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options)
            .await
            .expect("connect in-memory database");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db
    }

    /// A throwaway SQLite file, removed with its WAL siblings on drop.
    struct TempDb {
        path: std::path::PathBuf,
    }

    impl Drop for TempDb {
        #[allow(
            clippy::let_underscore_must_use,
            reason = "cleanup must not mask the assertion that failed the test"
        )]
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
            }
        }
    }

    /// A migrated database with more than one pooled connection, so concurrent
    /// tasks really do run their statements against separate connections.
    async fn setup_pooled_db(label: &str) -> (DatabaseConnection, TempDb) {
        let temp = TempDb {
            path: std::env::temp_dir().join(format!(
                "plombir-git-account-race-{label}-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        };
        let url = format!("sqlite://{}?mode=rwc", temp.path.display());
        let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to throwaway database");
        rg_db::run_migrations(&db).await.expect("run migrations");
        (db, temp)
    }

    fn oci_storage_for(repo_root: &std::path::Path) -> OciStorage {
        OciStorage::from_backend(
            Arc::new(LocalBlobStorage::new(repo_root)),
            repo_root.join("_oci_uploads"),
            Some(repo_root.to_path_buf()),
        )
    }

    /// An account and the root its repositories live under.
    async fn seed_account(
        db: &DatabaseConnection,
        login: &str,
    ) -> (i64, tempfile::TempDir, std::path::PathBuf) {
        let owner = user_ops::create_user(
            db,
            login,
            &format!("{login}@example.invalid"),
            "unused",
            "Race Owner",
        )
        .await
        .expect("create account");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        (owner.id, sandbox, repo_root)
    }

    /// Everything left under a namespace whose account no longer exists.
    ///
    /// This is the shape the account case has to be checked in: the cascade
    /// takes the rows with the account, so "a live row pointing at nobody" is
    /// not the residue to look for — bytes with no row at all is. Deliberately
    /// not filtered to `*.git`: a staging tombstone left behind by a deletion
    /// that reported success is the same failure wearing a different name.
    async fn stranded_storage(
        db: &DatabaseConnection,
        user_id: i64,
        repo_root: &std::path::Path,
        login: &str,
    ) -> Vec<String> {
        if user_ops::find_by_id(db, user_id)
            .await
            .expect("read account")
            .is_some()
        {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(repo_root.join(login)) else {
            return Vec::new();
        };
        entries
            .map(|entry| entry.expect("read namespace entry").file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect()
    }

    /// The request resolved the account before the deletion claimed it, so
    /// nothing on the create path could have refused it up front. It commits
    /// its row, finds the claim, and takes both the row and the Git tree back
    /// out — the alternative is a Git tree whose row the cascade is about to
    /// destroy.
    #[tokio::test]
    async fn a_repository_that_commits_after_the_retirement_claim_undoes_itself() {
        let db = setup_db().await;
        let (owner_id, _sandbox, repo_root) = seed_account(&db, "late-create-owner").await;

        // Exactly what `delete_user` does first, and the only state a create
        // that resolved a moment earlier can still discover.
        assert!(
            user_ops::begin_user_retirement(&db, owner_id)
                .await
                .expect("claim the account"),
            "the first claim on an untouched account must win"
        );

        let error =
            crate::repo::service::create_repo(&db, owner_id, "late", None, false, &repo_root, None)
                .await
                .expect_err("a repository must not be created into a retiring account");
        assert!(
            format!("{error:#}").contains("being deleted"),
            "the refusal does not say the account is going away: {error:#}"
        );

        assert!(
            repo_ops::list_active_by_owner_id(&db, owner_id)
                .await
                .expect("inventory account repositories")
                .is_empty(),
            "the losing create left a live repository row in a retiring account"
        );
        assert!(
            !repo_root.join("late-create-owner/late.git").exists(),
            "the losing create left its Git tree behind"
        );
    }

    /// A second claim is not a second deletion. Two concurrent deletes must not
    /// both walk the same repositories' storage.
    #[tokio::test]
    async fn only_one_deletion_can_claim_an_account() {
        let db = setup_db().await;
        let (owner_id, _sandbox, _repo_root) = seed_account(&db, "claim-owner").await;

        assert!(user_ops::begin_user_retirement(&db, owner_id)
            .await
            .unwrap());
        assert!(
            !user_ops::begin_user_retirement(&db, owner_id)
                .await
                .unwrap(),
            "a second deletion claimed an account already being retired"
        );

        // And the namespace reopens exactly once the claim is released.
        user_ops::abort_user_retirement(&db, owner_id)
            .await
            .unwrap();
        assert!(
            user_ops::begin_user_retirement(&db, owner_id)
                .await
                .unwrap(),
            "a released claim did not reopen the account"
        );
    }

    /// The claim is what `delete_user` itself takes, not just an op sitting
    /// next to it: an account another deletion is already retiring must not
    /// have its storage walked a second time, and the loser answers like a name
    /// that is not there.
    #[tokio::test]
    async fn a_deletion_refuses_an_account_another_deletion_already_claimed() {
        let db = setup_db().await;
        let (owner_id, _sandbox, repo_root) = seed_account(&db, "second-delete-owner").await;
        crate::repo::service::create_repo(&db, owner_id, "held", None, false, &repo_root, None)
            .await
            .expect("create account repository");

        assert!(user_ops::begin_user_retirement(&db, owner_id)
            .await
            .unwrap());

        let blob_storage = LocalBlobStorage::new(&repo_root);
        let refused = delete_user(
            &db,
            &repo_root,
            &blob_storage,
            &oci_storage_for(&repo_root),
            owner_id,
        )
        .await
        .expect_err("a second deletion retired an account already being retired");
        assert!(
            format!("{refused:#}").contains("user"),
            "the loser's refusal does not name the account: {refused:#}"
        );
        assert!(
            repo_root.join("second-delete-owner/held.git").exists(),
            "the losing deletion walked the storage the first one owns"
        );
        assert!(
            user_ops::find_by_id(&db, owner_id).await.unwrap().is_some(),
            "the losing deletion removed the account row the first one claimed"
        );
    }

    /// A deletion that cannot retire a repository leaves everything retryable —
    /// including the namespace. A claim that outlived its failed deletion would
    /// be an account nobody can log in as and no request can reopen.
    #[tokio::test]
    async fn a_failed_deletion_reopens_the_namespace_it_claimed() {
        let db = setup_db().await;
        let (owner_id, _sandbox, repo_root) = seed_account(&db, "reopen-owner").await;
        crate::repo::service::create_repo(&db, owner_id, "kept", None, false, &repo_root, None)
            .await
            .expect("create account repository");

        // A repository whose storage cannot be staged fails the deletion: the
        // blob root is a file, so the OCI upload tree under it cannot be made.
        let broken_root = repo_root.join("_oci_uploads");
        std::fs::create_dir_all(&repo_root).unwrap();
        std::fs::write(&broken_root, b"not a directory").unwrap();
        let blob_storage = LocalBlobStorage::new(&repo_root);
        let failed = delete_user(
            &db,
            &repo_root,
            &blob_storage,
            &OciStorage::from_backend(
                Arc::new(LocalBlobStorage::new(&repo_root)),
                broken_root.join("nested"),
                Some(repo_root.clone()),
            ),
            owner_id,
        )
        .await;
        assert!(
            failed.is_err(),
            "a repository whose storage could not be retired was reported as a deleted account"
        );

        assert!(
            user_ops::user_namespace_is_open(&db, owner_id)
                .await
                .expect("read account retirement state"),
            "a failed deletion left the account claimed and its namespace closed"
        );
        // Which is only meaningful if a create can actually use it again.
        crate::repo::service::create_repo(
            &db,
            owner_id,
            "after-retry",
            None,
            false,
            &repo_root,
            None,
        )
        .await
        .expect("the reopened namespace still refuses new repositories");
    }

    /// The invariant under real concurrency, both orderings included: whichever
    /// of the two wins, no Git tree may outlive the account row that named it.
    /// The create may lose and undo itself, or commit early enough for the
    /// deletion's re-inventory to retire it — never neither.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_concurrent_create_and_delete_never_strand_repository_storage() {
        for attempt in 0..12 {
            // Pooled and file-backed on purpose. `sqlite::memory:` with one
            // connection makes the two tasks take turns on that connection, so
            // the interleaving this test exists for never happens and it would
            // pass against code with no protocol at all.
            let (db, _temp) = setup_pooled_db(&format!("account-race-{attempt}")).await;
            let login = format!("race-owner-{attempt}");
            let (owner_id, _sandbox, repo_root) = seed_account(&db, &login).await;
            std::fs::create_dir_all(&repo_root).unwrap();

            let create_db = db.clone();
            let create_root = repo_root.clone();
            let create = async move {
                crate::repo::service::create_repo(
                    &create_db,
                    owner_id,
                    "contested",
                    None,
                    false,
                    &create_root,
                    None,
                )
                .await
            };
            let delete_db = db.clone();
            let delete_root = repo_root.clone();
            let delete = async move {
                let blob_storage = LocalBlobStorage::new(&delete_root);
                delete_user(
                    &delete_db,
                    &delete_root,
                    &blob_storage,
                    &oci_storage_for(&delete_root),
                    owner_id,
                )
                .await
            };
            let (created, deleted) = tokio::join!(create, delete);

            let stranded = stranded_storage(&db, owner_id, &repo_root, &login).await;
            assert!(
                stranded.is_empty(),
                "attempt {attempt}: the account is gone but this storage is still on disk: \
                 {stranded:?} (create: {:?}, delete: {:?})",
                created
                    .as_ref()
                    .map(|repo| repo.id)
                    .map_err(|e| format!("{e:#}")),
                deleted.as_ref().map_err(|e| format!("{e:#}")),
            );
            // A create that committed early enough to be seen by the deletion's
            // re-inventory is retired by it, so a successful create whose row is
            // gone is the protocol working, not failing — what may never happen
            // is that row disappearing with its bytes left behind, which is
            // what the assertion above is about.
        }
    }
}
