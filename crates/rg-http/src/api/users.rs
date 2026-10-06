//! User registration and login REST API.
//!
//! POST /api/v1/users/register
//! POST /api/v1/users/login
//! GET  /api/v1/users/me  (requires authenticated user cookie or Bearer token)

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::auth::{AuthUser, SessionUser, AUTH_COOKIE_NAME};
use crate::error::AppError;
use crate::AppState;

/// M-4: Build a `Set-Cookie` header value for the HttpOnly auth cookie.
fn build_auth_cookie(token: &str, is_https: bool) -> String {
    let mut cookie = format!(
        "{}={}; HttpOnly; Path=/; SameSite=Strict; Max-Age=604800",
        AUTH_COOKIE_NAME, token
    );
    if is_https {
        cookie.push_str("; Secure");
    }
    cookie
}

/// M-4: Build a `Set-Cookie` header value that clears the auth cookie.
fn build_clear_cookie(is_https: bool) -> String {
    let mut cookie = format!(
        "{}=; HttpOnly; Path=/; SameSite=Strict; Max-Age=0",
        AUTH_COOKIE_NAME
    );
    if is_https {
        cookie.push_str("; Secure");
    }
    cookie
}

/// Helper to record audit log (fire-and-forget, does not fail the main operation).
/// POST /api/v1/users/register
#[derive(Deserialize, ToSchema)]
pub struct RegisterRequest {
    pub username: String,
    pub email: String,
    pub password: String,
}

/// Login request body.
#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    /// Username or email. Accepts `username` as an alias for ergonomics /
    /// consistency with the register endpoint, which uses `username`.
    #[serde(alias = "username")]
    pub login: String,
    pub password: String,
}

/// Login/Register success response.
#[derive(serde::Serialize, ToSchema)]
pub struct AuthResponse {
    pub token: String,
    pub user_id: i64,
    pub username: String,
    /// Whether MFA verification is still required
    #[serde(default)]
    pub mfa_required: bool,
}

/// User profile response.
#[derive(serde::Serialize, ToSchema)]
pub struct UserProfile {
    pub id: i64,
    pub username: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub is_admin: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[utoipa::path(
    post,
    path = "/users/register",
    tag = "Users",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "User registered successfully", body = AuthResponse),
        (status = 400, description = "Invalid input", body = serde_json::Value),
        (status = 403, description = "Self-service registration is closed on this instance", body = serde_json::Value),
        (status = 409, description = "The username or email is already taken", body = serde_json::Value),
        (status = 503, description = "The database is unreachable", body = serde_json::Value),
    )
)]
pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegisterRequest>,
) -> impl IntoResponse {
    // Ask before spending anything: a closed instance answers 403 here, ahead
    // of the password hash (deliberately expensive) and ahead of every row this
    // request would otherwise write.
    let permit = match rg_core::user::registration::authorize(&state.db, state.registration).await {
        Ok(Some(permit)) => permit,
        Ok(None) => {
            crate::metrics::recorder::auth_event("register", "closed");
            tracing::info!(
                username = %body.username,
                "registration refused: self-service registration is closed on this instance"
            );
            return AppError::Forbidden(
                "self-service registration is closed on this instance; ask an administrator for an account"
                    .to_string(),
            )
            .into_response();
        }
        Err(error) => {
            // Whether registration is allowed could not be *read*. Failing open
            // here would turn a database blip into the very thing the setting
            // forbids, so this is ours and it is a 5xx.
            crate::metrics::recorder::auth_event("register", "failure");
            tracing::error!(
                error = %format!("{error:#}"),
                "could not read the registration policy; refusing to create an account"
            );
            return AppError::from(error).into_response();
        }
    };

    let outcome = rg_core::user::service::register(
        &state.db,
        permit,
        &body.username,
        &body.email,
        &body.password,
        &state.jwt_secret,
    )
    .await;

    match outcome {
        Ok(resp) => {
            // Record audit log
            let details = serde_json::json!({
                "email": body.email,
                "username": body.username
            });
            // Resolved after the fact: the account exists and the request has
            // already succeeded, so a name lookup that fails must not turn a
            // completed registration or login into a `5xx`. It records the id
            // with no name and says why in the log — never a blank name, which
            // the journal cannot tell apart from an action nobody performed.
            let audit_actor =
                rg_core::audit::AuditActor::resolve_after_the_fact(&state.db, resp.user_id).await;
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "user.register",
                Some("user"),
                Some(resp.user_id),
                Some(&resp.username),
                Some(&headers),
                Some(details),
            )
            .await;

            crate::metrics::recorder::user_registered();
            crate::metrics::recorder::auth_event("register", "success");
            (StatusCode::CREATED, Json(serde_json::json!(resp))).into_response()
        }
        Err(e) => {
            crate::metrics::recorder::auth_event("register", "failure");
            // The service types every rule the registration can break (taken
            // name/email, malformed address, weak password) as `InvalidRequest`.
            // Hashing the password and inserting the row are ours: with the old
            // blanket `bad_request` a broken database told the visitor to pick a
            // different username, and the operator log said nothing at all.
            AppError::from(e).into_response()
        }
    }
}

/// Turn the login-state finalizer's three outcomes into the HTTP boundary.
///
/// `Some` is a fresh row read in the same transaction as the conditional
/// update. It is the only source for the MFA decision and session generation.
/// `None` means retirement or physical deletion won after credential proof;
/// `Err` means the server could not establish either outcome. Neither may be
/// collapsed into a session.
pub(crate) fn finalized_login_user(
    user_id: i64,
    finalization: anyhow::Result<Option<rg_db::entities::user::Model>>,
) -> Result<rg_db::entities::user::Model, AppError> {
    match finalization {
        Ok(Some(user)) => Ok(user),
        Ok(None) => {
            tracing::warn!(
                user_id,
                "login: the account retired between credential verification and login completion"
            );
            Err(AppError::unauthorized("invalid credentials"))
        }
        Err(error) => {
            tracing::error!(
                user_id,
                error = %format!("{error:#}"),
                "login: could not finalize the account state; refusing to publish success"
            );
            Err(AppError::from(error))
        }
    }
}

#[utoipa::path(
    post,
    path = "/users/login",
    tag = "Users",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Login successful", body = AuthResponse),
        (status = 401, description = "Invalid credentials", body = serde_json::Value),
        (status = 500, description = "The login could not be finalized", body = serde_json::Value),
        (status = 503, description = "The database is unreachable", body = serde_json::Value),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> impl IntoResponse {
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(&headers);
    // First authenticate credentials
    match rg_core::user::service::login_with_configured_auth(
        &state.db,
        &body.login,
        &body.password,
        &state.encryption_key,
        &state.ldap_transport_policy,
    )
    .await
    {
        Ok(outcome) => {
            // First-factor credentials verified. Full-login vs MFA-challenge is
            // decided by the fresh row returned from the lifecycle finalizer;
            // the second factor is counted in `verify_mfa`.
            let login_method = match outcome.method {
                rg_core::user::service::LoginMethod::Password => "password",
                rg_core::user::service::LoginMethod::Ldap => "ldap",
            };
            let verified_user = outcome.user;
            let finalized = match finalized_login_user(
                verified_user.id,
                rg_db::ops::user_ops::finalize_primary_login(&state.db, verified_user.id).await,
            ) {
                Ok(user) => user,
                Err(error) => {
                    crate::metrics::recorder::auth_event("login", "failure");
                    return error.into_response();
                }
            };
            let mfa_required = finalized.mfa_enabled;
            let mut resp = AuthResponse {
                token: String::new(),
                user_id: finalized.id,
                username: finalized.username.clone(),
                mfa_required: false,
            };
            if !mfa_required {
                resp.token = match rg_core::auth::jwt::generate_token(
                    finalized.id,
                    &finalized.username,
                    finalized.session_version,
                    &state.jwt_secret,
                    7,
                ) {
                    Ok(token) => token,
                    Err(error) => return AppError::from(error).into_response(),
                };
            }
            crate::metrics::recorder::auth_event("login", "success");

            // Record audit log
            let details = serde_json::json!({
                "mfa_required": mfa_required,
                "login_method": login_method
            });
            // Resolved after the fact: the account exists and the request has
            // already succeeded, so a name lookup that fails must not turn a
            // completed registration or login into a `5xx`. It records the id
            // with no name and says why in the log — never a blank name, which
            // the journal cannot tell apart from an action nobody performed.
            let audit_actor =
                rg_core::audit::AuditActor::resolve_after_the_fact(&state.db, resp.user_id).await;
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "user.login",
                Some("user"),
                Some(resp.user_id),
                Some(&resp.username),
                Some(&headers),
                Some(details),
            )
            .await;

            if let Err(error) = rg_db::ops::login_log_ops::log_attempt(
                &state.db,
                Some(resp.user_id),
                &resp.username,
                login_method,
                ip_address.as_deref(),
                user_agent.as_deref(),
                true,
                None,
            )
            .await
            {
                tracing::warn!(error = %format!("{error:#}"), "failed to record successful login attempt");
            }
            if mfa_required {
                // Prove the first factor with a short-lived, HttpOnly challenge
                // cookie. The MFA endpoint refuses username-only verification.
                let challenge = match rg_core::auth::jwt::generate_mfa_challenge(
                    resp.user_id,
                    &resp.username,
                    login_method,
                    &state.jwt_secret,
                ) {
                    Ok(challenge) => challenge,
                    Err(error) => return AppError::from(error).into_response(),
                };
                let is_https = crate::public_url::request_is_https(&state, &headers);
                let challenge_cookie =
                    crate::api::mfa::build_mfa_challenge_cookie(&challenge, is_https);
                let mfa_resp = serde_json::json!({
                    "token": "",
                    "user_id": resp.user_id,
                    "username": resp.username,
                    "mfa_required": true,
                });
                let mut response = (
                    StatusCode::OK,
                    [(axum::http::header::SET_COOKIE, challenge_cookie)],
                    Json(mfa_resp),
                )
                    .into_response();
                if let Ok(value) = axum::http::HeaderValue::from_str(&build_clear_cookie(is_https))
                {
                    response
                        .headers_mut()
                        .append(axum::http::header::SET_COOKIE, value);
                }
                response
            } else {
                // M-4: Set HttpOnly cookie with JWT for browser-based auth
                let is_https = crate::public_url::request_is_https(&state, &headers);
                let cookie = build_auth_cookie(&resp.token, is_https);
                (
                    StatusCode::OK,
                    [(axum::http::header::SET_COOKIE, cookie)],
                    Json(serde_json::json!(resp)),
                )
                    .into_response()
            }
        }
        Err(error) => {
            // Not every failure of `login_with_configured_auth` is a rejected
            // credential, and the two must not share an answer. A verdict is
            // one of the three the service bails with, listed in
            // `CREDENTIAL_VERDICTS`; anything else — an unreachable database on
            // the account lookup, a stored hash the verifier cannot use, a
            // token that would not sign — never produced a verdict at all.
            //
            // Answering 401 to those tells the account holder they mistyped a
            // password that was very possibly right, keeps the outage out of
            // the operator's 5xx rate entirely, and lets the brute-force
            // counter below rack up strikes against them for our breakage.
            //
            // Enumerating the *verdicts* rather than the failures is what makes
            // this fail-closed: a message this list does not know becomes a
            // retryable 5xx with the full chain in the operator log, not a
            // silent 401. The old form enumerated one failure
            // (`UnusablePasswordHash`) and called everything else a rejection.
            const CREDENTIAL_VERDICTS: [&str; 3] = [
                "invalid credentials",
                "account is disabled",
                "account is temporarily locked",
            ];

            // A directory bind that succeeded and a provisioning policy that
            // said no. Neither a rejected credential (the password was right,
            // so 401 would be a lie and the brute-force counter below would
            // punish someone for our policy) nor a server failure (nothing
            // broke). It travels as a typed refusal precisely so this door can
            // tell it apart from both, and answers with the rule that refused.
            if let Some(refusal) =
                error.downcast_ref::<rg_core::user::provisioning::ProvisioningRefusal>()
            {
                crate::metrics::recorder::provisioning_refused(refusal.reason());
                tracing::warn!(
                    login = %body.login,
                    reason = refusal.reason(),
                    "login refused: the directory that authenticated this person may not create accounts here"
                );
                return AppError::Forbidden(refusal.message().to_string()).into_response();
            }

            let verdict = error.to_string();
            if !CREDENTIAL_VERDICTS.contains(&verdict.as_str()) {
                crate::metrics::recorder::auth_event("login", "failure");
                return AppError::from(error).into_response();
            }

            // A lookup that could not run is not a lookup that found nobody.
            // Flattening the error away turns a degraded database into "no such
            // user", which skips the brute-force counter below and files the
            // attempt with no user id — silently, for as long as the outage
            // lasts. Keep the anonymous fallback, but never lose the reason.
            let lookup = if body.login.contains('@') {
                rg_db::ops::user_ops::find_by_email(&state.db, &body.login).await
            } else {
                rg_db::ops::user_ops::find_by_username(&state.db, &body.login).await
            };
            let user = match lookup {
                Ok(user) => user,
                Err(error) => {
                    tracing::warn!(
                        error = %format!("{error:#}"),
                        "failed to resolve the account for a rejected login, brute-force counter skipped"
                    );
                    None
                }
            };
            let mut locked = verdict == "account is temporarily locked";
            if !locked && verdict == "invalid credentials" {
                if let Some(user) = &user {
                    // `false` means "not locked" — which is also what a failed
                    // write would report, so a silent error here degrades the
                    // brute-force protection into a no-op with nothing in the
                    // log. Default stays permissive (the login already failed);
                    // only the silence goes away.
                    // The threshold is shared with the SSH and registry password
                    // doors, so raising it here cannot leave them counting to a
                    // different number.
                    locked = match rg_db::ops::user_ops::record_failed_login(
                        &state.db,
                        user.id,
                        rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS,
                    )
                    .await
                    {
                        Ok(locked) => locked,
                        Err(error) => {
                            tracing::warn!(
                                user_id = user.id,
                                error = %format!("{error:#}"),
                                "failed to record a failed login, brute-force counter did not advance"
                            );
                            false
                        }
                    };
                }
            }
            let auth_provider = user
                .as_ref()
                .map(|user| user.auth_provider.as_str())
                .unwrap_or("unknown");
            if let Err(log_error) = rg_db::ops::login_log_ops::log_attempt(
                &state.db,
                user.as_ref().map(|user| user.id),
                &body.login,
                auth_provider,
                ip_address.as_deref(),
                user_agent.as_deref(),
                false,
                Some(if locked {
                    "account_locked"
                } else {
                    "invalid_credentials"
                }),
            )
            .await
            {
                tracing::warn!(%log_error, "failed to record unsuccessful login attempt");
            }
            let reason = if locked {
                "account_locked"
            } else {
                "invalid_credentials"
            };
            crate::metrics::recorder::auth_event("login", "failure");
            crate::metrics::recorder::failed_login(reason);
            AppError::unauthorized(if locked {
                "account is temporarily locked"
            } else {
                "invalid credentials"
            })
            .into_response()
        }
    }
}

#[utoipa::path(
    post,
    path = "/users/logout",
    tag = "Users",
    responses(
        (status = 200, description = "All bearer sessions revoked and auth cookie cleared", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    )
)]
/// POST /api/v1/users/logout — revoke every bearer session and clear the cookie.
pub async fn logout(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = rg_db::ops::user_ops::invalidate_sessions(&state.db, user_id).await {
        return AppError::from(error).into_response();
    }
    let is_https = crate::public_url::request_is_https(&state, &headers);
    let cookie = build_clear_cookie(is_https);
    (
        StatusCode::OK,
        [(axum::http::header::SET_COOKIE, cookie)],
        Json(serde_json::json!({"logged_out": true})),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/users/me",
    tag = "Users",
    responses(
        (status = 200, description = "Current user profile", body = UserProfile),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "User not found", body = serde_json::Value),
    )
)]
/// GET /api/v1/users/me — returns the current user's profile.
pub async fn me(State(state): State<AppState>, AuthUser(user_id): AuthUser) -> impl IntoResponse {
    match rg_db::ops::user_ops::find_by_id(&state.db, user_id).await {
        Ok(Some(user)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "id": user.id,
                "username": user.username,
                "email": user.email,
                "display_name": user.display_name,
                "avatar_url": user.avatar_url,
                "is_admin": user.is_admin,
                "created_at": user.created_at,
            })),
        )
            .into_response(),
        Ok(None) => AppError::not_found("user not found".to_string()).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── PAT (Personal Access Token) handlers ────────────────────────────────

use sha2::{Digest, Sha256};

#[derive(serde::Deserialize, ToSchema)]
pub struct CreateTokenRequest {
    pub name: String,
    pub scopes: Option<String>,
    pub expires_at: Option<String>,
    /// Repositories, MCP tools and protected branches the token is confined
    /// to — see [`super::bots::TokenNarrowing`].
    #[serde(flatten)]
    pub narrowing: super::bots::TokenNarrowing,
}

#[derive(serde::Serialize, ToSchema)]
pub struct AccessTokenResponse {
    pub id: i64,
    pub name: String,
    pub scopes: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(flatten)]
    pub narrowing: super::bots::TokenNarrowingResponse,
}

/// Generate a cryptographically strong Personal Access Token.
///
/// 32 random bytes (256 bits) drawn from the OS CSPRNG (`OsRng`), hex-encoded
/// and prefixed with `ifp_` so the token type stays recognisable (GitHub
/// `ghp_` style) and scannable by secret-detection tools. Every byte of
/// entropy comes from the CSPRNG — there is deliberately no timestamp- or
/// PID-derived fallback that could make a token predictable. Mirrors
/// `generate_jwt_secret` in `rg-cli`. If the OS entropy source is unavailable
/// `fill_bytes` panics rather than emitting a guessable token.
pub(crate) fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("ifp_{}", hex::encode(bytes))
}

pub(crate) fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(crate) fn parse_token_expiration(
    value: Option<&str>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, AppError> {
    value
        .map(|value| {
            chrono::DateTime::parse_from_rfc3339(value)
                .map(|date| date.with_timezone(&chrono::Utc))
                .map_err(|_| {
                    AppError::bad_request(
                        "expires_at must be an RFC 3339 timestamp with an offset, \
                         e.g. 2030-01-01T00:00:00Z",
                    )
                })
        })
        .transpose()
}

/// GET /api/v1/users/tokens
#[utoipa::path(
    get,
    path = "/users/tokens",
    tag = "Users",
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_tokens(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> impl IntoResponse {
    match rg_db::ops::token_ops::list_by_user(&state.db, user_id).await {
        Ok(tokens) => {
            let mut narrowings = match super::bots::narrowing_responses(&state, &tokens).await {
                Ok(narrowings) => narrowings,
                Err(error) => return error.into_response(),
            };
            let tokens: Vec<AccessTokenResponse> = tokens
                .into_iter()
                .filter_map(|token| {
                    let narrowing = narrowings.remove(&token.id)?;
                    Some(AccessTokenResponse {
                        id: token.id,
                        name: token.name,
                        scopes: token.scopes,
                        expires_at: token.expires_at,
                        last_used_at: token.last_used_at,
                        created_at: token.created_at,
                        narrowing,
                    })
                })
                .collect();

            (StatusCode::OK, Json(tokens)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// POST /api/v1/users/tokens
#[utoipa::path(
    post,
    path = "/users/tokens",
    tag = "Users",
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "A login session is required to create credentials", body = serde_json::Value),
    ),
)]
pub async fn create_token(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    headers: HeaderMap,
    Json(body): Json<CreateTokenRequest>,
) -> impl IntoResponse {
    // Named before the token exists, so a failed lookup is a 500 from a request
    // that minted nothing rather than a live credential with a blank author.
    let audit_actor = match crate::api::access_audit::grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if body.name.trim().is_empty() {
        return AppError::bad_request("token name cannot be empty".to_string()).into_response();
    }
    let scopes = match rg_core::auth::pat_scope::normalize_scopes(
        body.scopes.as_deref().unwrap_or("repo"),
    ) {
        Ok(scopes) => scopes,
        // Client-input validator: `normalize_scopes` is a pure parse of the
        // caller's `scopes` string and does no I/O, so 400 is the only outcome it
        // can have.
        Err(e) => return AppError::bad_request(e.to_string()).into_response(),
    };
    let expires_at = match parse_token_expiration(body.expires_at.as_deref()) {
        Ok(expires_at) => expires_at,
        Err(error) => return error.into_response(),
    };
    let narrowing =
        match super::bots::resolve_narrowing(&state, user_id, &body.narrowing, false).await {
            Ok(narrowing) => narrowing,
            Err(error) => return error.into_response(),
        };
    // Generate the credential only after every client-supplied field is known
    // to be valid. A rejected request must never mint even a transient raw PAT.
    let raw_token = generate_token();
    let token_hash = hash_token(&raw_token);
    let now = chrono::Utc::now();
    let model = rg_db::entities::access_token::ActiveModel {
        id: sea_orm::NotSet,
        user_id: sea_orm::Set(user_id),
        name: sea_orm::Set(body.name),
        token_hash: sea_orm::Set(token_hash),
        scopes: sea_orm::Set(scopes),
        expires_at: sea_orm::Set(expires_at),
        last_used_at: sea_orm::Set(None),
        created_at: sea_orm::Set(now),
        repo_restricted: sea_orm::Set(narrowing.repository_ids.is_some()),
        mcp_tools: sea_orm::Set(narrowing.mcp_tools),
        deny_protected_merge: sea_orm::Set(narrowing.deny_protected_merge),
    };
    match rg_db::ops::token_ops::create(
        &state.db,
        model,
        narrowing.repository_ids.as_deref().unwrap_or_default(),
    )
    .await
    {
        Ok(token) => {
            // The name and the scopes, and nothing else. `raw_token` is the
            // credential and `token_hash` is what the server authenticates by;
            // either in the journal would turn a list operators read into a
            // second credential store.
            crate::api::access_audit::record_credential(
                &state,
                &audit_actor,
                "user.create_token",
                user_id,
                &headers,
                serde_json::json!({
                    "token_id": token.id,
                    "token_name": token.name,
                    "scopes": token.scopes,
                    "expires_at": token.expires_at,
                    "repositories": body.narrowing.repositories,
                    "mcp_tools": token.mcp_tools,
                    "deny_protected_merge": token.deny_protected_merge,
                }),
            )
            .await;
            let mut narrowings = match super::bots::narrowing_responses(
                &state,
                std::slice::from_ref(&token),
            )
            .await
            {
                Ok(narrowings) => narrowings,
                Err(error) => return error.into_response(),
            };
            let narrowing = narrowings.remove(&token.id);
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": token.id,
                    "name": token.name,
                    "token": raw_token,
                    "scopes": token.scopes,
                    "expires_at": token.expires_at,
                    "created_at": token.created_at,
                    "repositories": narrowing.as_ref().and_then(|n| n.repositories.clone()),
                    "mcp_tools": narrowing.as_ref().and_then(|n| n.mcp_tools.clone()),
                    "deny_protected_merge": token.deny_protected_merge,
                })),
            )
                .into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/users/tokens/:id
#[utoipa::path(
    delete,
    path = "/users/tokens/{id}",
    tag = "Users",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "Token not found, or owned by another account", body = serde_json::Value),
    ),
)]
pub async fn delete_token(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let audit_actor = match crate::api::access_audit::grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let token = match rg_db::ops::token_ops::find_by_id(&state.db, id).await {
        Ok(Some(t)) => t,
        Ok(None) => return AppError::not_found("token not found".to_string()).into_response(),
        // A lookup that could not run is not a lookup that said "no row": the
        // catch-all arm here reported a database outage as a missing token.
        Err(e) => return AppError::from(e).into_response(),
    };
    // Another account's token answers 404, not 403: a 403 would confirm the id
    // exists, and `{id}` is a global primary key, so the pair would turn this
    // route into an enumeration oracle over every token on the instance.
    if token.user_id != user_id {
        return AppError::not_found("token not found".to_string()).into_response();
    }
    // The lookup above and this `DELETE` are two statements. A concurrent
    // revocation that lands in between leaves this one deleting nothing, and
    // "revoked" is the most expensive answer to get wrong — so the 204 comes
    // from `rows_affected`, not from the lookup that preceded it.
    match rg_db::ops::token_ops::delete_by_id(&state.db, id).await {
        Ok(true) => {
            // The row read above is the only place the revoked token's name and
            // scopes still exist — "token #4 was revoked" tells a review nothing
            // about what stopped working.
            crate::api::access_audit::record_credential(
                &state,
                &audit_actor,
                "user.revoke_token",
                user_id,
                &headers,
                serde_json::json!({
                    "token_id": token.id,
                    "token_name": token.name,
                    "scopes": token.scopes,
                }),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => AppError::not_found("token not found".to_string()).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Password Reset ──────────────────────────────────────────────

/// POST /api/v1/users/forgot-password
///
/// Request body: { "email": "user@example.com" }
/// Always returns 200 (to prevent email enumeration).
#[derive(serde::Deserialize, ToSchema)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

#[utoipa::path(
    post,
    path = "/users/forgot-password",
    tag = "Users",
    request_body = ForgotPasswordRequest,
    responses(
        (status = 200, description = "If email exists, a reset link has been sent", body = serde_json::Value),
        (status = 500, description = "Internal server error"),
    ),
)]
pub async fn forgot_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ForgotPasswordRequest>,
) -> impl IntoResponse {
    // The link in the mail names the instance's public address: the configured
    // `external_url` first, which the old Host-only derivation never read
    // (card_f78054e9e98f).
    let base_url = match crate::public_url::require_public_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };

    match rg_core::user::service::forgot_password(
        &state.db,
        &body.email,
        state.smtp_config.as_ref(),
        &base_url,
    )
    .await
    {
        Ok(issued) => {
            tracing::info!("password reset requested for email: {}", body.email);
            // A reset link is a one-time way back into the account, so its issue
            // is a credential event — and the journal must learn about it only
            // when one was actually issued, or it answers "does this address
            // have an account" more plainly than the response ever could
            // (card_80f1b25cf114).
            //
            // Written detached, for the same reason the mail is: this endpoint
            // pads every branch to one deadline
            // (`FORGOT_PASSWORD_BUDGET`, 100ms) so that the branch which issues
            // a token cannot be told from the branch which does nothing. An
            // extra row inserted before the response would put that difference
            // straight back — a contended SQLite write is not a rounding error
            // — so the write goes through the tracker the stop path drains,
            // outside the measured window.
            if let Some(issued) = issued {
                let db = state.db.clone();
                let headers = headers.clone();
                rg_core::task_tracker::delivery_tracker().spawn(async move {
                    // After the fact, and it has to be: the link is already in
                    // the mail. Resolving first cannot un-issue it, and failing
                    // the request on a name lookup would answer differently for
                    // an address that has an account — the one thing this
                    // endpoint exists to hide.
                    let actor =
                        rg_core::audit::AuditActor::resolve_after_the_fact(&db, issued.user_id)
                            .await;
                    rg_core::audit::record(
                        &db,
                        &actor,
                        "user.request_password_reset",
                        Some("user"),
                        Some(issued.user_id),
                        Some(&issued.username),
                        Some(&headers),
                        None,
                    )
                    .await;
                });
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({ "message": "If the email exists, a reset link has been sent" })),
            )
                .into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// POST /api/v1/users/reset-password
///
/// Request body: { "token": "...", "new_password": "..." }
#[derive(serde::Deserialize, ToSchema)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub new_password: String,
}

/// Journal the replacement of an account's password.
///
/// The chain a takeover leaves in the journal used to read `user.login`,
/// nothing, `user.create_token` — and the missing middle is the step that made
/// the rest possible: whoever held the link from the mail replaced the main
/// secret of the account (card_80f1b25cf114). Both endings of the reset write
/// it, because the password is already gone in both.
///
/// The actor is the account itself, resolved after the fact. There is no
/// session to read it from — that is the point of a reset — and resolving first
/// could not have prevented anything: by the time this runs the old password no
/// longer exists, so failing the request on a name lookup would report a reset
/// that did happen as one that did not.
async fn journal_password_reset(
    state: &AppState,
    headers: &HeaderMap,
    user_id: i64,
    username: &str,
    mfa_required: bool,
) {
    let actor = rg_core::audit::AuditActor::resolve_after_the_fact(&state.db, user_id).await;
    rg_core::audit::record(
        &state.db,
        &actor,
        "user.reset_password",
        Some("user"),
        Some(user_id),
        Some(username),
        Some(headers),
        // Which way the reset ended is what a review asks next: a reset that
        // handed back a session finished the takeover, one that stopped at the
        // second factor did not.
        Some(serde_json::json!({ "mfa_required": mfa_required })),
    )
    .await;
}

#[utoipa::path(
    post,
    path = "/users/reset-password",
    tag = "Users",
    request_body = ResetPasswordRequest,
    responses(
        (status = 200, description = "Password reset successfully. An `mfa_enabled` account is \
            answered with `mfa_required: true`, an empty token and an MFA challenge cookie \
            instead of a session — exactly as `POST /users/login` answers it.", body = serde_json::Value),
        (status = 400, description = "Invalid or expired token, or invalid password"),
    ),
)]
pub async fn reset_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ResetPasswordRequest>,
) -> impl IntoResponse {
    use rg_core::user::service::PasswordResetOutcome;

    match rg_core::user::service::reset_password(
        &state.db,
        &body.token,
        &body.new_password,
        &state.jwt_secret,
    )
    .await
    {
        Ok(PasswordResetOutcome::Session(resp)) => {
            tracing::info!(user_id = resp.user_id, "password reset successful");
            journal_password_reset(&state, &headers, resp.user_id, &resp.username, false).await;
            // M-4: Set HttpOnly cookie so the user stays logged in after reset
            let is_https = crate::public_url::request_is_https(&state, &headers);
            let cookie = build_auth_cookie(&resp.token, is_https);
            (
                StatusCode::OK,
                [(axum::http::header::SET_COOKIE, cookie)],
                Json(serde_json::json!(resp)),
            )
                .into_response()
        }
        // The reset succeeded and the session does not follow from it. Answered
        // in the shape `POST /users/login` already uses for the same account —
        // an empty token, `mfa_required`, and the five-minute challenge cookie
        // that `POST /users/mfa/verify` trades for the real session — so the
        // browser lands on the same second-factor form by the same path.
        Ok(PasswordResetOutcome::SecondFactorRequired { user_id, username }) => {
            tracing::info!(
                user_id,
                "password reset successful; issuing an MFA challenge instead of a session"
            );
            journal_password_reset(&state, &headers, user_id, &username, true).await;
            let challenge = match rg_core::auth::jwt::generate_mfa_challenge(
                user_id,
                &username,
                "password_reset",
                &state.jwt_secret,
            ) {
                Ok(challenge) => challenge,
                Err(error) => return AppError::from(error).into_response(),
            };
            let is_https = crate::public_url::request_is_https(&state, &headers);
            let mut response = (
                StatusCode::OK,
                [(
                    axum::http::header::SET_COOKIE,
                    crate::api::mfa::build_mfa_challenge_cookie(&challenge, is_https),
                )],
                Json(serde_json::json!({
                    "token": "",
                    "user_id": user_id,
                    "username": username,
                    "mfa_required": true,
                })),
            )
                .into_response();
            // Clear whatever session the browser arrived carrying: the password
            // it was minted against no longer exists, and a reset that leaves
            // an older session standing hands back with one hand what it just
            // refused with the other.
            if let Ok(value) = axum::http::HeaderValue::from_str(&build_clear_cookie(is_https)) {
                response
                    .headers_mut()
                    .append(axum::http::header::SET_COOKIE, value);
            }
            response
        }
        // A spent/expired token and a rejected password are typed
        // `InvalidRequest` and stay 400; the password hash, the row update and
        // the token bookkeeping are ours and become a retryable 5xx instead of
        // telling the user their valid reset link is invalid.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// The finalizer runs only after a credential has been proved, so its outage and
/// lifecycle-loss branches need their own classification proof: the ordinary
/// login failure tests cannot make the database fail in precisely that gap.
#[cfg(test)]
mod finalized_login_user_tests {
    use super::finalized_login_user;
    use axum::http::StatusCode;
    use sea_orm::{ConnAcquireErr, DbErr, RuntimeErr};

    #[test]
    fn an_unavailable_finalizer_is_a_server_error() {
        let error = finalized_login_user(
            7,
            Err(
                anyhow::Error::new(DbErr::ConnectionAcquire(ConnAcquireErr::Timeout))
                    .context("db: completed login"),
            ),
        )
        .expect_err("a failed finalizer must not publish a login");
        assert_eq!(
            error.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "an unreachable finalizer database must be retryable"
        );

        let error = finalized_login_user(
            7,
            Err(anyhow::Error::new(DbErr::Exec(RuntimeErr::Internal(
                "no such table: users".into(),
            )))
            .context("db: completed login")),
        )
        .expect_err("a failed finalizer must not publish a login");
        assert_eq!(
            error.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a statement-level finalizer failure is a 500, not a session"
        );
    }

    #[test]
    fn a_vanished_account_does_not_become_a_session() {
        let error = finalized_login_user(7, Ok(None))
            .expect_err("an account that is gone must not be handed a session");
        assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
    }
}

#[cfg(test)]
mod token_tests {
    use super::{generate_token, hash_token};
    use std::collections::HashSet;

    #[test]
    fn pat_has_expected_prefix_and_length() {
        let token = generate_token();
        // `ifp_` (4) + 32 bytes as hex (64) = 68 chars.
        assert!(
            token.starts_with("ifp_"),
            "PAT must carry the ifp_ prefix: {token}"
        );
        assert_eq!(token.len(), 68, "unexpected PAT length: {token}");
    }

    #[test]
    fn pat_body_is_256_bits_of_hex_entropy() {
        let token = generate_token();
        let body = token.strip_prefix("ifp_").expect("prefix");
        assert!(
            body.chars().all(|c| c.is_ascii_hexdigit()),
            "PAT body must be pure hex: {body}"
        );
        let bytes = hex::decode(body).expect("PAT body must be valid hex");
        assert_eq!(
            bytes.len(),
            32,
            "PAT must carry 32 bytes (256 bits) of entropy"
        );
    }

    #[test]
    fn pat_is_unique_across_many_calls() {
        // A CSPRNG token — not a timestamp/PID — must never collide, even when
        // minted in a tight loop within the same process/instant.
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            assert!(
                seen.insert(generate_token()),
                "PAT collision — entropy too weak"
            );
        }
    }

    #[test]
    fn distinct_tokens_hash_distinctly() {
        assert_ne!(hash_token(&generate_token()), hash_token(&generate_token()));
    }
}
