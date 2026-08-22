//! SSO (Single Sign-On) API endpoints.
//!
//! Endpoints:
//!   GET  /auth/sso/providers                — List enabled SSO providers
//!   GET  /auth/sso/{slug}                    — Redirect to provider's auth page
//!   GET  /auth/sso/{slug}/callback           — OAuth2/OIDC callback
//!   POST /auth/sso/{slug}/refresh            — Refresh OAuth2 access token
//!   DELETE /auth/sso/{slug}/unlink           — Unlink OAuth account
//!   GET  /users/me/sso                       — List this account's linked identities

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Redirect},
    Json,
};
use serde::{Deserialize, Serialize};
use tracing;
use utoipa::ToSchema;

use crate::api::auth::AuthUser;
use crate::error::AppError;
use crate::AppState;

// ── Cookie helpers ───────────────────────────────────────────────

/// Cookie names for secure OAuth2 flow.
const SSO_STATE_COOKIE: &str = "forgekeep_sso_state";
const SSO_VERIFIER_COOKIE: &str = "forgekeep_sso_code_verifier";

fn append_set_cookie(response: &mut axum::response::Response, cookie: String) {
    if let Ok(header_value) = HeaderValue::from_str(&cookie) {
        response
            .headers_mut()
            .append(header::SET_COOKIE, header_value);
    } else {
        tracing::warn!("Failed to parse Set-Cookie header value");
    }
}

/// Set a short-lived signed cookie for CSRF/PKCE state.
fn set_state_cookie(
    response: &mut axum::response::Response,
    name: &str,
    value: &str,
    jwt_secret: &str,
) {
    // Sign the value with HMAC for integrity
    let signature = sign_cookie_value(value, jwt_secret);
    let cookie_value = format!("{}:{}", value, signature);

    // Max-Age: 600 seconds (10 min) — matches typical OAuth2 code expiry
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600",
        name, cookie_value
    );
    append_set_cookie(response, cookie);
}

fn clear_state_cookie(response: &mut axum::response::Response, name: &str) {
    append_set_cookie(
        response,
        format!("{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0", name),
    );
}

fn build_auth_cookie(token: &str, is_https: bool) -> String {
    let mut cookie = format!(
        "{}={}; HttpOnly; Path=/; SameSite=Strict; Max-Age=604800",
        crate::api::auth::AUTH_COOKIE_NAME,
        token
    );
    if is_https {
        cookie.push_str("; Secure");
    }
    cookie
}

fn build_clear_auth_cookie(is_https: bool) -> String {
    format!(
        "{}=; HttpOnly; Path=/; SameSite=Strict; Max-Age=0{}",
        crate::api::auth::AUTH_COOKIE_NAME,
        if is_https { "; Secure" } else { "" }
    )
}

fn is_https_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "https")
        .unwrap_or(false)
}

fn encode_query_component(value: &str) -> String {
    let mut out = String::new();
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Verify and extract a signed cookie value. Returns None if missing or invalid.
///
/// The signature is verified in **constant time** via `Mac::verify_slice`
/// (guards against timing oracles), mirroring `verify_hub_signature`.
fn verify_state_cookie(headers: &HeaderMap, name: &str, jwt_secret: &str) -> Option<String> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let cookie_header = headers.get("cookie")?.to_str().ok()?;
    let prefix = format!("{}=", name);

    for part in cookie_header.split(';') {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix(&prefix) {
            // Split value:signature
            if let Some((val, sig)) = value.rsplit_once(':') {
                // Decode the provided hex signature; skip malformed cookies.
                let Ok(provided) = hex::decode(sig) else {
                    continue;
                };
                // HMAC accepts a key of any length, so init cannot fail.
                let mut mac = HmacSha256::new_from_slice(jwt_secret.as_bytes())
                    .expect("HMAC accepts keys of any length");
                mac.update(val.as_bytes());
                // Constant-time comparison — no early-exit timing side channel.
                if mac.verify_slice(&provided).is_ok() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// HMAC-SHA256 cookie signing keyed by the JWT secret, hex-encoded.
///
/// Uses a proper HMAC construction (not `SHA256(secret ‖ value)`, which is
/// vulnerable to length-extension) so a signed `value:sig` pair can't be
/// extended into another valid pair.
fn sign_cookie_value(value: &str, secret: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    // HMAC accepts a key of any length, so init cannot fail.
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(value.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

// ── Extract base URL ─────────────────────────────────────────────

fn get_base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:8080");
    let scheme = if host.contains(":443") || host.contains(":8443") {
        "https"
    } else {
        "http"
    };
    format!("{}://{}", scheme, host)
}

fn get_api_base_url(state: &AppState, headers: &HeaderMap) -> String {
    let base = state
        .external_url
        .as_ref()
        .map(|url| url.trim_end_matches('/').to_string())
        .unwrap_or_else(|| get_base_url(headers));
    format!("{}/api/v1", base.trim_end_matches('/'))
}

// ── Provider resolution ──────────────────────────────────────────

/// Resolve the provider behind `slug` **in a state where it may be used**.
///
/// `enabled` used to be a convention: every entry point looked the provider up
/// by slug and was expected to remember to re-read the flag afterwards.
/// `authorize` and `callback` remembered; `refresh_token` did not — so an
/// operator could switch a provider off and already-linked accounts kept
/// renewing their OAuth tokens through it indefinitely. The question is asked
/// once, here, and a door that does not ask it never gets a provider at all.
///
/// [`unlink_oauth_account`] deliberately does **not** come through here: a user
/// must be able to drop a link to a provider the operator has since switched
/// off. That is an exception with a reason, not a fourth handler that forgot.
async fn resolve_usable_provider(
    state: &AppState,
    slug: &str,
) -> Result<rg_db::entities::sso_provider::Model, AppError> {
    let provider = rg_db::ops::sso_provider_ops::find_by_slug(&state.db, slug)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found(format!("SSO provider '{}' not found", slug)))?;

    if !provider.enabled {
        return Err(AppError::forbidden("SSO provider is disabled"));
    }

    Ok(provider)
}

/// Build the OAuth2/OIDC client config for a provider already proven usable.
///
/// The three doors used to carry their own copy of this, and the copies had
/// drifted: `refresh_token`'s swallowed a decryption failure into an empty
/// client secret and then asked the provider to refresh with it, so a wrong
/// `[auth].encryption_key` surfaced as the provider's rejection rather than as
/// ours. One body, one answer — a secret that will not decrypt is a 500 here
/// too.
fn provider_config(
    provider: &rg_db::entities::sso_provider::Model,
    enc_key: &[u8; 32],
    redirect_url: String,
) -> Result<rg_core::auth::sso::SsoProviderConfig, AppError> {
    let client_secret = provider
        .client_secret_enc
        .as_ref()
        .map(|secret| rg_core::auth::encryption::decrypt(secret, enc_key))
        .transpose()
        .map_err(|error| {
            tracing::error!("Decryption error: {}", error);
            AppError::internal("decryption failed")
        })?
        .unwrap_or_default();

    // The row is not supposed to get here without a client id — the admin API
    // refuses to store an enabled provider without one. A row that predates
    // that check, or one written around it, used to become `client_id=""` and
    // go out to the IdP anyway: the IdP said no, and our missing field was
    // reported to the operator as the provider's refusal. It is ours, so it is
    // a 500 that names the field and never leaves the process.
    let client_id = provider
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|client_id| !client_id.is_empty())
        .ok_or_else(|| {
            tracing::error!(
                provider_id = provider.id,
                provider_slug = %provider.slug,
                "SSO provider has no client_id; refusing to build an authorization request"
            );
            AppError::internal("SSO provider is missing its client ID")
        })?
        .to_string();

    Ok(rg_core::auth::sso::SsoProviderConfig {
        slug: provider.slug.clone(),
        provider_type: provider.provider_type.clone(),
        client_id,
        client_secret,
        redirect_url,
        scopes: provider
            .scopes
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        discovery_url: provider.discovery_url.clone(),
    })
}

// ── Types ────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct SsoProviderInfo {
    slug: String,
    name: String,
    provider_type: String,
    icon_url: Option<String>,
}

/// One external identity linked to the calling account, as shown in account
/// settings.
///
/// The account settings page needs the same slug `DELETE
/// /auth/sso/{slug}/unlink` takes, so the link is reported by the slug it was
/// created under rather than by its row id. `provider_enabled` is what lets the
/// page describe a link to a provider the operator has since switched off
/// without hiding it: the link still exists, still grants nothing, and — by the
/// deliberate exception in [`unlink_oauth_account`] — can still be dropped.
#[derive(Debug, Serialize, ToSchema)]
pub struct SsoLinkInfo {
    slug: String,
    /// Operator-facing provider name, falling back to the slug when the
    /// provider row is gone: a link outlives the provider it was made through.
    name: String,
    provider_username: String,
    email: String,
    linked_at: chrono::DateTime<chrono::Utc>,
    provider_enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct SsoCallbackQuery {
    code: String,
    state: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponse {
    token: String,
    user_id: i64,
    username: String,
    mfa_required: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RefreshRequest {
    refresh_token: Option<String>,
}

// ── List providers ───────────────────────────────────────────────

/// GET /auth/sso/providers
#[utoipa::path(
    get,
    path = "/auth/sso/providers",
    tag = "SSO",
    responses(
        (status = 200, description = "List of enabled SSO providers", body = Vec<SsoProviderInfo>),
        (status = 500, description = "Internal server error"),
    ),
)]
pub async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<Vec<SsoProviderInfo>>, AppError> {
    let providers = rg_db::ops::sso_provider_ops::list_enabled(&state.db)
        .await
        .map_err(AppError::from)?;

    let infos: Vec<SsoProviderInfo> = providers
        .into_iter()
        .map(|p| SsoProviderInfo {
            slug: p.slug,
            name: p.name,
            provider_type: p.provider_type,
            icon_url: p.icon_url,
        })
        .collect();

    Ok(Json(infos))
}

// ── List this account's linked identities ────────────────────────

/// GET /users/me/sso
#[utoipa::path(
    get,
    path = "/users/me/sso",
    tag = "SSO",
    responses(
        (status = 200, description = "External identities linked to this account", body = Vec<SsoLinkInfo>),
        (status = 401, description = "Authentication required"),
    ),
)]
pub async fn list_my_links(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<SsoLinkInfo>>, AppError> {
    let accounts = rg_db::ops::oauth_account_ops::find_by_user_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?;

    // Read the provider table once and match on the slug the links carry, not
    // once per link: a slug that no longer resolves is a link to a provider the
    // operator removed, and that link must still be listed — it is the only way
    // its owner can find it in order to drop it.
    let providers = rg_db::ops::sso_provider_ops::list_all(&state.db)
        .await
        .map_err(AppError::from)?;

    let links = accounts
        .into_iter()
        .map(|account| {
            let provider = providers.iter().find(|p| p.slug == account.provider);
            SsoLinkInfo {
                name: provider
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| account.provider.clone()),
                provider_enabled: provider.is_some_and(|p| p.enabled),
                slug: account.provider,
                provider_username: account.provider_username,
                email: account.email,
                linked_at: account.created_at,
            }
        })
        .collect();

    Ok(Json(links))
}

// ── Authorize (redirect to provider) ─────────────────────────────

/// GET /auth/sso/{slug}
#[utoipa::path(
    get,
    path = "/auth/sso/{slug}",
    tag = "SSO",
    params(
        ("slug" = String, Path, description = "SSO provider slug"),
    ),
    responses(
        (status = 302, description = "Redirect to provider authorization page"),
        (status = 403, description = "SSO provider is disabled"),
        (status = 404, description = "SSO provider not found"),
    ),
)]
pub async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let provider = resolve_usable_provider(&state, &slug).await?;

    let base_url = get_api_base_url(&state, &headers);
    let redirect_url = format!("{}/auth/sso/{}/callback", base_url, slug);

    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let config = provider_config(&provider, &enc_key, redirect_url)?;

    let (auth_url, csrf_state, code_verifier) = rg_core::auth::sso::oauth2_authorize_url(&config)
        .await
        .map_err(|e| {
            tracing::error!("SSO authorize error: {}", e);
            AppError::internal("SSO authorization failed")
        })?;

    // Build a redirect response with CSRF & PKCE cookies
    let mut redirect = Redirect::temporary(&auth_url).into_response();
    set_state_cookie(
        &mut redirect,
        SSO_STATE_COOKIE,
        &csrf_state,
        &state.jwt_secret,
    );
    set_state_cookie(
        &mut redirect,
        SSO_VERIFIER_COOKIE,
        &code_verifier,
        &state.jwt_secret,
    );

    Ok(redirect)
}

// ── Callback ─────────────────────────────────────────────────────

/// GET /auth/sso/{slug}/callback
#[utoipa::path(
    get,
    path = "/auth/sso/{slug}/callback",
    tag = "SSO",
    params(
        ("slug" = String, Path, description = "SSO provider slug"),
        ("code" = String, Query, description = "OAuth authorization code"),
        ("state" = Option<String>, Query, description = "OAuth state parameter"),
    ),
    responses(
        (status = 200, description = "Login successful", body = LoginResponse),
        (status = 400, description = "Token exchange failed"),
        (status = 403, description = "CSRF state mismatch"),
        (status = 404, description = "SSO provider not found"),
    ),
)]
pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(query): Query<SsoCallbackQuery>,
) -> Result<impl IntoResponse, AppError> {
    // ── CSRF state validation ────────────────────────────────────
    let expected_state = verify_state_cookie(&headers, SSO_STATE_COOKIE, &state.jwt_secret);
    let code_verifier = verify_state_cookie(&headers, SSO_VERIFIER_COOKIE, &state.jwt_secret);

    match (&query.state, &expected_state) {
        (Some(returned), Some(expected)) if returned == expected => {
            // Valid
        }
        (Some(returned), Some(expected)) => {
            tracing::warn!(
                "SSO CSRF state mismatch: expected={}, got={}",
                expected,
                returned
            );
            return Err(AppError::forbidden("CSRF state mismatch — possible attack"));
        }
        (Some(_), None) => {
            tracing::warn!("SSO CSRF: no expected state cookie found");
            return Err(AppError::forbidden("missing CSRF state cookie"));
        }
        (None, _) => {
            tracing::warn!("SSO callback without state parameter");
            return Err(AppError::forbidden("missing CSRF state parameter"));
        }
    }

    let code_verifier = code_verifier.ok_or_else(|| {
        tracing::warn!("SSO PKCE: no code verifier cookie found");
        AppError::forbidden("missing PKCE code verifier cookie")
    })?;

    // ── Get provider config ──────────────────────────────────────
    let provider = resolve_usable_provider(&state, &slug).await?;

    let base_url = get_api_base_url(&state, &headers);
    let redirect_url = format!("{}/auth/sso/{}/callback", base_url, slug);

    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let config = provider_config(&provider, &enc_key, redirect_url)?;

    // ── Exchange code for tokens (with PKCE) ─────────────────────
    // Same split as `sso_user_info_error`: a provider that refuses the grant
    // (an expired or already-redeemed `code`) is the one failure here the
    // person signing in can act on, and `rg_core` marks exactly that case as an
    // `InvalidRequest` → `400`. A token endpoint that timed out or answered
    // `5xx` is marked `UpstreamUnavailable` → `502`; flattening both into
    // `bad_request` told the client to fix a request that was already correct.
    let token_response =
        rg_core::auth::sso::oauth2_exchange_code(&config, &query.code, &code_verifier)
            .await
            .map_err(|error| {
                tracing::error!(
                    provider = %provider.slug,
                    error = %format!("{error:#}"),
                    "failed to exchange authorization code"
                );
                AppError::from(error)
            })?;

    // ── Fetch user info ──────────────────────────────────────────
    let user_info =
        rg_core::auth::sso::oauth2_fetch_user_info(&config, &token_response.access_token)
            .await
            .map_err(|error| sso_user_info_error(&provider.slug, error))?;

    // ── Find or create user ──────────────────────────────────────
    let user_id = find_or_create_sso_user(&state, &provider, &user_info, &token_response).await?;

    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::internal("user not found after creation"))?;
    if !user.is_usable() {
        return Err(AppError::unauthorized("account is disabled"));
    }
    if user
        .locked_until
        .is_some_and(|locked_until| locked_until > chrono::Utc::now())
    {
        return Err(AppError::unauthorized("account is temporarily locked"));
    }

    // ── Log successful login ─────────────────────────────────────
    // A login the audit trail never recorded is a login nobody can review
    // afterwards. Refusing the sign-in over a failed audit write would be worse
    // than the gap, so this warns loudly instead — but it must not be silent.
    if let Err(error) = rg_db::ops::login_log_ops::log_attempt(
        &state.db,
        Some(user_id),
        &user.username,
        &provider.slug,
        None,
        None,
        true,
        None,
    )
    .await
    {
        tracing::warn!(
            user_id,
            username = %user.username,
            provider = %provider.slug,
            error = %format!("{error:#}"),
            "failed to record a successful SSO login; this sign-in is missing from the audit trail"
        );
    }

    // ── If MFA enabled, require second factor ────────────────────
    if user.mfa_enabled {
        let challenge = rg_core::auth::jwt::generate_mfa_challenge(
            user.id,
            &user.username,
            &provider.slug,
            &state.jwt_secret,
        )
        .map_err(AppError::from)?;
        let target = format!(
            "/login?sso_mfa_required=1&username={}",
            encode_query_component(&user.username)
        );
        let mut redirect = Redirect::temporary(&target).into_response();
        append_set_cookie(
            &mut redirect,
            crate::api::mfa::build_mfa_challenge_cookie(&challenge, is_https_request(&headers)),
        );
        append_set_cookie(
            &mut redirect,
            build_clear_auth_cookie(is_https_request(&headers)),
        );
        clear_state_cookie(&mut redirect, SSO_STATE_COOKIE);
        clear_state_cookie(&mut redirect, SSO_VERIFIER_COOKIE);
        return Ok(redirect);
    }

    if let Err(error) = rg_db::ops::user_ops::record_successful_login(&state.db, user.id).await {
        tracing::warn!(user_id = user.id, error = %format!("{error:#}"), "failed to record successful SSO login");
    }

    // ── Issue JWT ────────────────────────────────────────────────
    let token = rg_core::auth::jwt::generate_token(
        user.id,
        &user.username,
        user.session_version,
        &state.jwt_secret,
        7,
    )
    .map_err(AppError::from)?;

    let mut redirect = Redirect::temporary("/dashboard").into_response();
    append_set_cookie(
        &mut redirect,
        build_auth_cookie(&token, is_https_request(&headers)),
    );
    clear_state_cookie(&mut redirect, SSO_STATE_COOKIE);
    clear_state_cookie(&mut redirect, SSO_VERIFIER_COOKIE);
    Ok(redirect)
}

// ── Refresh token ────────────────────────────────────────────────

/// POST /auth/sso/{slug}/refresh
/// Refresh an OAuth2 access token using a stored refresh_token.
#[utoipa::path(
    post,
    path = "/auth/sso/{slug}/refresh",
    tag = "SSO",
    params(
        ("slug" = String, Path, description = "SSO provider slug"),
    ),
    request_body = RefreshRequest,
    responses(
        (status = 200, description = "Token refreshed"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "SSO provider is disabled"),
        (status = 404, description = "SSO provider not found"),
    ),
)]
pub async fn refresh_token(
    State(state): State<AppState>,
    crate::api::auth::AuthUser(user_id): crate::api::auth::AuthUser,
    Path(slug): Path<String>,
    Json(body): Json<RefreshRequest>,
) -> Result<impl IntoResponse, AppError> {
    let provider = resolve_usable_provider(&state, &slug).await?;

    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    // The redirect URL is not part of a refresh grant.
    let config = provider_config(&provider, &enc_key, String::new())?;

    // Use provided refresh_token or look up from stored OAuth account
    let refresh_token = if let Some(rt) = body.refresh_token {
        rt
    } else {
        // Look up the user's OAuth account for stored refresh_token
        let accounts = rg_db::ops::oauth_account_ops::find_by_user_id(&state.db, user_id)
            .await
            .map_err(AppError::from)?;

        let account = accounts
            .iter()
            .find(|a| a.provider == slug)
            .ok_or_else(|| AppError::not_found("no OAuth account linked"))?;

        let stored_rt = account
            .refresh_token
            .as_ref()
            .ok_or_else(|| AppError::not_found("no refresh token available"))?;

        rg_core::auth::encryption::decrypt(stored_rt, &enc_key).map_err(|e| {
            tracing::error!("Decryption error: {}", e);
            AppError::internal("decryption failed")
        })?
    };

    // The same classification as the callback's exchange, and for the same
    // reason: leaving this door on a blanket `bad_request` is how the two
    // halves of one flow end up disagreeing about whose fault an outage is. A
    // provider that refuses the refresh grant (revoked token) is a `400`; a
    // provider that did not answer is a `502`.
    let token_response = rg_core::auth::sso::oauth2_refresh_token(&config, &refresh_token)
        .await
        .map_err(|error| {
            tracing::error!(
                provider = %slug,
                error = %format!("{error:#}"),
                "failed to refresh token"
            );
            AppError::from(error)
        })?;

    store_refreshed_oauth_tokens(
        &state.db,
        &state.encryption_key,
        user_id,
        &slug,
        &token_response,
    )
    .await?;

    Ok(Json(serde_json::json!({
        "access_token": token_response.access_token,
        "expires_in": token_response.expires_in,
        "refresh_token": token_response.refresh_token,
    })))
}

async fn store_refreshed_oauth_tokens(
    db: &sea_orm::DatabaseConnection,
    encryption_key: &str,
    user_id: i64,
    provider_slug: &str,
    token_response: &rg_core::auth::sso::OAuth2TokenResponse,
) -> Result<(), AppError> {
    let accounts = rg_db::ops::oauth_account_ops::find_by_user_id(db, user_id)
        .await
        .map_err(AppError::from)?;
    let account = accounts
        .into_iter()
        .find(|account| account.provider == provider_slug)
        .ok_or_else(|| AppError::not_found("no OAuth account linked"))?;

    let enc_key = rg_core::auth::encryption::derive_key(encryption_key);
    let enc_access = rg_core::auth::encryption::encrypt(&token_response.access_token, &enc_key)
        .map_err(|_| AppError::internal("failed to encrypt the OAuth access token"))?;
    let enc_refresh = token_response
        .refresh_token
        .as_ref()
        .map(|refresh| {
            rg_core::auth::encryption::encrypt(refresh, &enc_key)
                .map_err(|_| AppError::internal("failed to encrypt the OAuth refresh token"))
        })
        .transpose()?;
    let expires_at = token_response
        .expires_in
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs as i64));

    rg_db::ops::oauth_account_ops::upsert(
        db,
        account.user_id,
        provider_slug,
        &account.provider_user_id,
        &account.provider_username,
        &account.email,
        Some(&enc_access),
        enc_refresh.as_deref(),
        expires_at,
    )
    .await
    .map_err(AppError::from)?;

    Ok(())
}

// ── Unlink OAuth account ─────────────────────────────────────────

/// DELETE /auth/sso/{slug}/unlink
#[utoipa::path(
    delete,
    path = "/auth/sso/{slug}/unlink",
    tag = "SSO",
    params(
        ("slug" = String, Path, description = "SSO provider slug"),
    ),
    responses(
        (status = 200, description = "Account unlinked"),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "No OAuth account linked"),
    ),
)]
pub async fn unlink_oauth_account(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // No `resolve_usable_provider` here, on purpose: dropping a link must keep
    // working after the operator switches the provider off, and it touches only
    // this user's own row. The provider row is not read at all — the link is
    // addressed by the slug the user already holds.
    //
    // Find and delete the OAuth account link
    let accounts = rg_db::ops::oauth_account_ops::find_by_user_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?;

    let account = accounts
        .iter()
        .find(|a| a.provider == slug)
        .ok_or_else(|| AppError::not_found("no OAuth account linked"))?;

    // The lookup above and the delete below are separate statements. A second
    // unlink of the same link can pass the lookup while the first one is still
    // in flight, so only the request whose DELETE actually removed the row may
    // report an unlink; the loser gets the same 404 as a request that never
    // had the link. A failed DELETE stays an error — it is not "already gone".
    let removed = rg_db::ops::oauth_account_ops::delete_by_id(&state.db, account.id, user_id)
        .await
        .map_err(AppError::from)?;
    if !removed {
        return Err(AppError::not_found("no OAuth account linked"));
    }

    Ok(Json(serde_json::json!({"unlinked": true})))
}

// ── User helpers ─────────────────────────────────────────────────

/// Classify a failed user-info fetch.
///
/// "The provider is unreachable" and "the provider answered, and its answer
/// identifies nobody" are two different things to whoever is signing in, and
/// they are not even the same *kind* of answer:
///
/// * The profile that identifies nobody is a `400` with the reason, because the
///   fix (grant the `user:email` scope, confirm the address) is on that side and
///   is only actionable if we say which defect it was.
/// * The provider that did not answer is **not** the client's fault. It used to
///   be a `400` too — "the request is what cannot proceed" — which tells the
///   person signing in to correct a request that was never wrong, and tells
///   every retry layer between us and them that retrying is pointless. It is a
///   `502` now: `AppError::from` routes the `UpstreamUnavailable` marker
///   `rg_core::auth::sso` attaches to every failed provider call.
///
/// Anything that is neither — an unsupported provider slug, say — is a
/// misconfiguration of ours and falls through to a `500`, which is where an
/// operator should be looking for it.
fn sso_user_info_error(provider_slug: &str, error: anyhow::Error) -> AppError {
    if let Some(defect) = error.downcast_ref::<rg_core::auth::sso::SsoIdentityDefect>() {
        tracing::warn!(
            provider = %provider_slug,
            defect = ?defect,
            "SSO profile carries no usable identity key; refusing the login"
        );
        return AppError::bad_request(defect.message());
    }

    tracing::error!(
        provider = %provider_slug,
        error = %format!("{error:#}"),
        "failed to fetch user info"
    );
    AppError::from(error)
}

/// Turn a refused first-login provisioning into the answer the person signing
/// in gets.
///
/// A `403` and not a `500`: nothing failed. The provider authenticated them,
/// this instance simply does not hand out accounts through that door — and the
/// message says which of the two rules refused, because the remedies differ.
fn sso_provisioning_refused(
    provider_slug: &str,
    user_info: &rg_core::auth::sso::SsoIdentity,
    refusal: rg_core::user::provisioning::ProvisioningRefusal,
) -> AppError {
    crate::metrics::recorder::provisioning_refused(refusal.reason());
    tracing::warn!(
        provider = %provider_slug,
        provider_username = %user_info.provider_username,
        reason = refusal.reason(),
        "SSO first login refused: this provider may not create accounts here"
    );
    AppError::forbidden(refusal.message())
}

async fn find_or_create_sso_user(
    state: &AppState,
    provider: &rg_db::entities::sso_provider::Model,
    user_info: &rg_core::auth::sso::SsoIdentity,
    token_response: &rg_core::auth::sso::OAuth2TokenResponse,
) -> Result<i64, AppError> {
    let db = &state.db;
    let provider_slug = provider.slug.as_str();

    // Check if OAuth account already exists
    if let Some(oauth) = rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
        db,
        provider_slug,
        &user_info.provider_user_id,
    )
    .await
    .map_err(AppError::from)?
    {
        // Update stored tokens
        let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
        // `unwrap_or_default()` here would store an empty string in place of the
        // access token — a row that looks populated and authenticates nothing.
        let enc_access = rg_core::auth::encryption::encrypt(&token_response.access_token, &enc_key)
            .map_err(|_| AppError::internal("failed to encrypt the OAuth access token"))?;
        let enc_refresh = token_response
            .refresh_token
            .as_ref()
            .and_then(|rt| rg_core::auth::encryption::encrypt(rt, &enc_key).ok());
        let expires_at = token_response
            .expires_in
            .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs as i64));

        // Swallowing this returned a successful login whose refreshed tokens
        // were never stored: the session works until the access token expires,
        // then the refresh reads whatever stale row was there before.
        rg_db::ops::oauth_account_ops::upsert(
            db,
            oauth.user_id,
            provider_slug,
            &user_info.provider_user_id,
            &user_info.provider_username,
            &user_info.email,
            Some(&enc_access),
            enc_refresh.as_deref(),
            expires_at,
        )
        .await
        .map_err(AppError::from)?;

        return Ok(oauth.user_id);
    }

    // Check if user with this email already exists.
    //
    // This is a *merge*: whoever holds this address gets the new provider link
    // attached to their account. `SsoIdentity` is what makes it safe to run —
    // an absent email would arrive here as `""` and match whichever account was
    // provisioned with it first, which is a login into a stranger's account
    // rather than a merge.
    let user_id = match rg_db::ops::user_ops::find_by_email(db, &user_info.email)
        .await
        .map_err(AppError::from)?
    {
        Some(existing) => existing.id,
        None => {
            // The only branch on this path that *creates* an account, and so
            // the only one the provisioning policy governs. Both branches above
            // sign in an account that already exists; refusing them would log
            // people out of the instance instead of keeping strangers out of it.
            rg_core::user::provisioning::authorize(provider, &user_info.email)
                .map_err(|refusal| sso_provisioning_refused(provider_slug, user_info, refusal))?;
            provision_sso_user(db, provider_slug, user_info).await?
        }
    };

    // Encrypt and store tokens
    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    // Same reason as the linked-account branch above: `unwrap_or_default()`
    // stores an empty string in place of the access token, and the row then
    // looks populated while authenticating nothing.
    let enc_access = rg_core::auth::encryption::encrypt(&token_response.access_token, &enc_key)
        .map_err(|_| AppError::internal("failed to encrypt the OAuth access token"))?;
    let enc_refresh = token_response
        .refresh_token
        .as_ref()
        .and_then(|rt| rg_core::auth::encryption::encrypt(rt, &enc_key).ok());
    let expires_at = token_response
        .expires_in
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs as i64));

    // Upsert OAuth account with encrypted tokens
    rg_db::ops::oauth_account_ops::upsert(
        db,
        user_id,
        provider_slug,
        &user_info.provider_user_id,
        &user_info.provider_username,
        &user_info.email,
        Some(&enc_access),
        enc_refresh.as_deref(),
        expires_at,
    )
    .await
    .map_err(AppError::from)?;

    Ok(user_id)
}

/// How many times a first-login provision may lose the race for its generated
/// username before giving up.
///
/// Each extra pass costs one `SELECT` plus one `INSERT`, and it only runs while
/// another request is provisioning the *same* username at the same moment, so
/// a small bound is enough to cover a real collision without turning a
/// persistent constraint failure into a spin.
const SSO_PROVISION_ATTEMPTS: usize = 3;

/// Create the ForgeKeep account behind a first SSO login, tolerating a
/// concurrent callback for the same identity.
///
/// `users.username` and `users.email` are both UNIQUE
/// (`m20260424_000001_create_users`), and this path reaches the `INSERT` after
/// two separate reads: no OAuth link for `(provider, provider_user_id)`, no
/// account on that email. Two callbacks of the same first login both pass those
/// reads, so one of them meets the constraint. Losing that race is neither the
/// client's fault nor a failed login — the winner created exactly the account
/// this call was about to — so the loser adopts it instead of answering 500 and
/// leaving the user staring at a broken first sign-in.
///
/// Two things it deliberately does not do:
///
/// * **Adopt an account merely because it holds the username.** The username is
///   derived from the provider's, and an unrelated local user may legitimately
///   own it; treating that collision as "this is me" would hand the login
///   someone else's account. Only the OAuth link and the email identify this
///   person — a bare username collision is retried with a new candidate.
/// * **Count the provision twice.** `user_provisioned("sso")` fires only on the
///   branch that actually inserted the row, so a raced double callback adds one
///   registration to the funnel, not two.
async fn provision_sso_user(
    db: &sea_orm::DatabaseConnection,
    provider_slug: &str,
    user_info: &rg_core::auth::sso::SsoIdentity,
) -> Result<i64, AppError> {
    for attempt in 1..=SSO_PROVISION_ATTEMPTS {
        let username = generate_unique_username(db, &user_info.provider_username)
            .await
            .map_err(AppError::from)?;

        // The base satisfies the local rule because it came through
        // `SsoIdentity`; the uniqueness suffix is the only thing added after
        // that proof. Re-checking the value actually written keeps the
        // guarantee attached to the row rather than to a function two crates
        // away — and if it ever fires it is our arithmetic, not the client's
        // request, so it is a 500 and not a 400.
        rg_core::user::service::validate_username(&username).map_err(|error| {
            AppError::internal(format!(
                "generated SSO username `{username}` breaks the local username rule: {error}"
            ))
        })?;

        let error = match rg_db::ops::user_ops::create_user(
            db,
            &username,
            &user_info.email,
            "", // no password for SSO users
            user_info.display_name.as_deref().unwrap_or(&username),
        )
        .await
        {
            Ok(created) => {
                // SSO first-login provision is a new account: count it in the
                // `users_registered_total` funnel with `sso` provenance.
                crate::metrics::recorder::user_provisioned("sso");
                return Ok(created.id);
            }
            Err(error) => error,
        };

        // Anything that is not a UNIQUE violation is a real write failure and
        // keeps its classification — `AppError::from` still tells a connection
        // outage (503) apart from a statement-level fault (500).
        if !rg_db::is_unique_violation_anyhow(&error) {
            return Err(AppError::from(error));
        }

        // A race was lost — but to whom? Re-read the two keys that identify
        // *this* login. A hit means a concurrent callback already built the
        // account, and reusing it is the correct answer.
        if let Some(user_id) = resolve_raced_sso_user(db, provider_slug, user_info).await? {
            return Ok(user_id);
        }

        // Neither key is taken, so the collision was on the generated username
        // alone — someone else's account, not this one. A fresh candidate is a
        // different row; try again.
        tracing::debug!(
            provider = provider_slug,
            attempt,
            username = %username,
            "SSO first-login username was taken concurrently; regenerating"
        );
    }

    Err(AppError::internal(
        "could not allocate a username for the new SSO account",
    ))
}

/// Find the account a concurrent SSO callback created for this same identity.
///
/// Both keys belong to the login itself: the OAuth link is `(provider,
/// provider_user_id)` — precisely the row this callback was going to write —
/// and the email is the key the non-racing path merges on. Nothing else
/// (username, display name) identifies the person, so nothing else is consulted.
///
/// Both are read off an [`SsoIdentity`](rg_core::auth::sso::SsoIdentity), so
/// neither can be the empty string here. That matters more on this path than on
/// the ordinary one: it runs *after* a UNIQUE violation, where an empty key
/// would reliably match the row that just caused it.
async fn resolve_raced_sso_user(
    db: &sea_orm::DatabaseConnection,
    provider_slug: &str,
    user_info: &rg_core::auth::sso::SsoIdentity,
) -> Result<Option<i64>, AppError> {
    if let Some(oauth) = rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
        db,
        provider_slug,
        &user_info.provider_user_id,
    )
    .await
    .map_err(AppError::from)?
    {
        return Ok(Some(oauth.user_id));
    }

    Ok(rg_db::ops::user_ops::find_by_email(db, &user_info.email)
        .await
        .map_err(AppError::from)?
        .map(|user| user.id))
}

/// Generate a unique username based on the provider username.
async fn generate_unique_username(
    db: &sea_orm::DatabaseConnection,
    base: &str,
) -> Result<String, anyhow::Error> {
    // A reserved base is unavailable in exactly the way a taken one is: nobody
    // holds it, and nobody may. Treating it as free instead sent the provision
    // into `validate_username` below, which answers `InvalidRequest` — and that
    // call site turns any refusal into a 500, so an identity provider with a
    // user named `admin` would have failed its first login with a server error
    // rather than being provisioned as `admin_1`.
    if !rg_core::namespace::is_reserved_segment(base)
        && rg_db::ops::user_ops::find_by_username(db, base)
            .await?
            .is_none()
    {
        return Ok(base.to_string());
    }
    for i in 1..100 {
        let candidate = format!("{}_{}", base, i);
        if rg_db::ops::user_ops::find_by_username(db, &candidate)
            .await?
            .is_none()
        {
            return Ok(candidate);
        }
    }
    let suffix: String = std::iter::repeat_n((), 6)
        .map(|_| rand::random::<u8>() % 26 + b'a')
        .map(|c| c as char)
        .collect();
    Ok(format!("{}_{}", base, suffix))
}

#[cfg(test)]
mod tests {
    use super::{
        build_auth_cookie, encode_query_component, provision_sso_user, resolve_raced_sso_user,
        set_state_cookie, store_refreshed_oauth_tokens, verify_state_cookie, SSO_STATE_COOKIE,
        SSO_VERIFIER_COOKIE,
    };
    use axum::http::{header, HeaderMap};
    use axum::response::IntoResponse;

    /// Serialises the two tests that assert a delta on the process-wide
    /// `users_registered_total` counter. Under `cargo nextest` each test is its
    /// own process and this is a no-op; under a threaded `cargo test` it keeps
    /// one test's increment out of the other's reading.
    /// Async-aware on purpose: the guard is held across the database awaits,
    /// which `clippy::await_holding_lock` denies for a `std` mutex.
    static PROVISION_COUNTER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn sso_profile(uid: &str, username: &str, email: &str) -> rg_core::auth::sso::SsoUserInfo {
        rg_core::auth::sso::SsoUserInfo {
            provider_user_id: uid.to_string(),
            provider_username: username.to_string(),
            email: email.to_string(),
            email_verified: None,
            display_name: Some("Alice".to_string()),
            avatar_url: None,
        }
    }

    fn sso_user_info(uid: &str, username: &str, email: &str) -> rg_core::auth::sso::SsoIdentity {
        sso_profile(uid, username, email)
            .into_identity()
            .expect("the fixture profile carries usable identity keys")
    }

    async fn migrated_db() -> sea_orm::DatabaseConnection {
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("connect test database");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db
    }

    /// Reads `forgekeep_users_registered_total`, initialising the registry the
    /// first time so the counter exists to be read at all.
    fn registered_total() -> u64 {
        #[allow(
            clippy::let_underscore_must_use,
            reason = "the registry is process-global; a second init is the expected no-op"
        )]
        let _ = crate::metrics::init_registry();
        crate::metrics::business::USERS_REGISTERED
            .get()
            .expect("the counter exists once the registry is initialised")
            .get()
    }

    #[tokio::test]
    async fn a_lost_first_login_race_is_resolved_through_the_winner_s_oauth_link() {
        let db = migrated_db().await;

        // The winner already created the account and linked the identity. Its
        // email deliberately differs from what this callback carries, so only
        // the OAuth link can identify it.
        let winner =
            rg_db::ops::user_ops::create_user(&db, "winner", "winner@example.com", "", "Winner")
                .await
                .expect("create the winning account");
        rg_db::ops::oauth_account_ops::upsert(
            &db,
            winner.id,
            "gitea",
            "provider-uid-1",
            "alice",
            "winner@example.com",
            Some("access"),
            None,
            None,
        )
        .await
        .expect("link the winning account");

        let resolved = resolve_raced_sso_user(
            &db,
            "gitea",
            &sso_user_info("provider-uid-1", "alice", "alice@example.com"),
        )
        .await
        .expect("resolve");

        assert_eq!(resolved, Some(winner.id));
    }

    #[tokio::test]
    async fn a_lost_first_login_race_is_resolved_through_the_email_before_the_link_exists() {
        let db = migrated_db().await;

        // The winner is between its two writes: the user row is in, the OAuth
        // link is not yet.
        let winner =
            rg_db::ops::user_ops::create_user(&db, "alice", "alice@example.com", "", "Alice")
                .await
                .expect("create the winning account");

        let resolved = resolve_raced_sso_user(
            &db,
            "gitea",
            &sso_user_info("provider-uid-1", "alice", "alice@example.com"),
        )
        .await
        .expect("resolve");

        assert_eq!(resolved, Some(winner.id));
    }

    #[tokio::test]
    async fn holding_the_username_alone_does_not_make_an_account_this_sso_identity() {
        let db = migrated_db().await;

        // An unrelated local account that happens to be called `alice`. The
        // generated username is derived from the provider's, so this collision
        // is ordinary — and adopting this row would hand the login someone
        // else's account.
        rg_db::ops::user_ops::create_user(&db, "alice", "someone-else@example.com", "", "Someone")
            .await
            .expect("create the unrelated account");

        let resolved = resolve_raced_sso_user(
            &db,
            "gitea",
            &sso_user_info("provider-uid-1", "alice", "alice@example.com"),
        )
        .await
        .expect("resolve");

        assert_eq!(resolved, None, "a username is not an identity");
    }

    #[tokio::test]
    async fn provisioning_a_first_login_creates_one_account_and_counts_one_registration() {
        let guard = PROVISION_COUNTER_LOCK.lock().await;
        let db = migrated_db().await;
        let before = registered_total();

        let user_id = provision_sso_user(
            &db,
            "gitea",
            &sso_user_info("provider-uid-1", "alice", "alice@example.com"),
        )
        .await
        .expect("provision");

        let created = rg_db::ops::user_ops::find_by_id(&db, user_id)
            .await
            .expect("read back")
            .expect("the account exists");
        assert_eq!(created.email, "alice@example.com");
        assert_eq!(created.username, "alice");
        assert_eq!(
            registered_total() - before,
            1,
            "a genuine first login is one registration",
        );
        drop(guard);
    }

    /// `users.username` addresses `/{username}/{repo}` and shares a namespace
    /// with organisation names, but a first SSO login used to write whatever
    /// the provider's profile carried: an OIDC `preferred_username` of
    /// `"John Doe"` became exactly that row, and `"a/../b"` put a path
    /// separator in it. The local rule now runs on the provisioning path too.
    #[tokio::test]
    async fn a_provisioned_username_satisfies_the_same_rule_self_registration_does() {
        let guard = PROVISION_COUNTER_LOCK.lock().await;
        let db = migrated_db().await;

        // The last two are names the *shape* rules pass and the URL space
        // refuses: an identity provider whose directory has a user called
        // `admin` must be provisioned as `admin_1`, not turned away with a 500
        // by the re-check below.
        for (index, provider_name) in ["John Doe", "a/../b", "ünïcode", "admin", "Settings"]
            .iter()
            .enumerate()
        {
            let user_id = provision_sso_user(
                &db,
                "gitea",
                &sso_user_info(
                    &format!("provider-uid-{index}"),
                    provider_name,
                    &format!("person{index}@example.com"),
                ),
            )
            .await
            .expect("a provider name the local rule refuses is repaired, not refused");

            let created = rg_db::ops::user_ops::find_by_id(&db, user_id)
                .await
                .expect("read back")
                .expect("the account exists");

            rg_core::user::service::validate_username(&created.username).unwrap_or_else(|error| {
                panic!(
                    "`{}` (from `{provider_name}`) must pass the local rule: {error}",
                    created.username
                )
            });
            assert!(
                !created.username.contains('/') && !created.username.contains(".."),
                "`{}` still addresses a URL path",
                created.username
            );
        }
        drop(guard);
    }

    #[tokio::test]
    async fn losing_the_race_reuses_the_winner_s_account_without_counting_it_twice() {
        let guard = PROVISION_COUNTER_LOCK.lock().await;
        let db = migrated_db().await;

        // The concurrent callback got there first: the account is in, under a
        // different username, on the email this login carries. `users.email` is
        // UNIQUE, so the INSERT below really does fail — no injection needed.
        let winner = rg_db::ops::user_ops::create_user(
            &db,
            "alice_from_the_other_callback",
            "alice@example.com",
            "",
            "Alice",
        )
        .await
        .expect("create the winning account");

        let before = registered_total();
        let user_id = provision_sso_user(
            &db,
            "gitea",
            &sso_user_info("provider-uid-1", "alice", "alice@example.com"),
        )
        .await
        .expect("a lost race is not a failed login");

        assert_eq!(user_id, winner.id, "both callbacks resolve to one identity");
        assert_eq!(
            registered_total() - before,
            0,
            "adopting an account someone else created is not a new registration",
        );
        drop(guard);
    }

    #[test]
    fn sso_state_and_pkce_cookies_are_both_set() {
        let mut response = axum::response::Response::new(axum::body::Body::empty());

        set_state_cookie(&mut response, SSO_STATE_COOKIE, "state-1", "secret");
        set_state_cookie(&mut response, SSO_VERIFIER_COOKIE, "verifier-1", "secret");

        let cookies: Vec<_> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .collect();
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0]
            .to_str()
            .unwrap()
            .starts_with("forgekeep_sso_state="));
        assert!(cookies[1]
            .to_str()
            .unwrap()
            .starts_with("forgekeep_sso_code_verifier="));
    }

    #[test]
    fn sso_state_cookie_round_trips_with_signature() {
        let mut response = axum::response::Response::new(axum::body::Body::empty());
        set_state_cookie(&mut response, SSO_STATE_COOKIE, "state-1", "secret");

        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());

        assert_eq!(
            verify_state_cookie(&headers, SSO_STATE_COOKIE, "secret"),
            Some("state-1".to_string())
        );
        assert_eq!(
            verify_state_cookie(&headers, SSO_STATE_COOKIE, "wrong"),
            None
        );
    }

    #[test]
    fn sso_auth_cookie_uses_secure_flag_only_for_https() {
        assert!(!build_auth_cookie("token", false).contains("; Secure"));
        assert!(build_auth_cookie("token", true).contains("; Secure"));
    }

    #[test]
    fn sso_mfa_redirect_username_is_query_encoded() {
        assert_eq!(encode_query_component("alice"), "alice");
        assert_eq!(
            encode_query_component("alice bob+root"),
            "alice%20bob%2Broot"
        );
    }

    #[tokio::test]
    async fn refreshed_tokens_are_stored_on_the_linked_oauth_account() {
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("connect test database");
        rg_db::run_migrations(&db).await.expect("run migrations");

        let user = rg_db::ops::user_ops::create_user(
            &db,
            "sso_refresh_user",
            "sso-refresh@example.com",
            "",
            "SSO Refresh",
        )
        .await
        .expect("create user");

        let encryption_key = "test-encryption-key";
        let enc_key = rg_core::auth::encryption::derive_key(encryption_key);
        let old_access = rg_core::auth::encryption::encrypt("old-access", &enc_key).unwrap();
        let old_refresh = rg_core::auth::encryption::encrypt("old-refresh", &enc_key).unwrap();
        rg_db::ops::oauth_account_ops::upsert(
            &db,
            user.id,
            "oidc",
            "provider-user-1",
            "alice",
            "alice@example.com",
            Some(&old_access),
            Some(&old_refresh),
            None,
        )
        .await
        .expect("insert OAuth account");

        let token_response = rg_core::auth::sso::OAuth2TokenResponse {
            access_token: "new-access".to_string(),
            refresh_token: Some("new-refresh".to_string()),
            expires_in: Some(3600),
        };

        store_refreshed_oauth_tokens(&db, encryption_key, user.id, "oidc", &token_response)
            .await
            .expect("store refreshed tokens");

        let account =
            rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "oidc", "provider-user-1")
                .await
                .expect("query OAuth account")
                .expect("OAuth account exists");
        let updated_access =
            rg_core::auth::encryption::decrypt(account.access_token.as_deref().unwrap(), &enc_key)
                .unwrap();
        let updated_refresh =
            rg_core::auth::encryption::decrypt(account.refresh_token.as_deref().unwrap(), &enc_key)
                .unwrap();

        assert_eq!(updated_access, "new-access");
        assert_eq!(updated_refresh, "new-refresh");
        assert!(account.token_expires_at.is_some());
    }

    #[test]
    fn redirect_response_can_carry_multiple_set_cookie_headers() {
        let mut response = axum::response::Redirect::temporary("/dashboard").into_response();
        set_state_cookie(&mut response, SSO_STATE_COOKIE, "state-1", "secret");
        set_state_cookie(&mut response, SSO_VERIFIER_COOKIE, "verifier-1", "secret");

        assert_eq!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .count(),
            2
        );
    }
}
