//! SSO (Single Sign-On) API endpoints.
//!
//! Endpoints:
//!   GET  /auth/sso/providers                — List enabled SSO providers
//!   GET  /auth/sso/{slug}                    — Redirect to provider's auth page
//!   GET  /auth/sso/{slug}/callback           — OAuth2/OIDC callback
//!   POST /auth/sso/{slug}/refresh            — Refresh OAuth2 access token
//!   DELETE /auth/sso/{slug}/unlink           — Unlink OAuth account

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Redirect},
    Json,
};
use serde::{Deserialize, Serialize};
use tracing;
use utoipa::ToSchema;

use crate::error::AppError;
use crate::AppState;

// ── Cookie helpers ───────────────────────────────────────────────

/// Cookie names for secure OAuth2 flow.
const SSO_STATE_COOKIE: &str = "ironforge_sso_state";
const SSO_VERIFIER_COOKIE: &str = "ironforge_sso_code_verifier";

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
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Verify and extract a signed cookie value. Returns None if missing or invalid.
fn verify_state_cookie(headers: &HeaderMap, name: &str, jwt_secret: &str) -> Option<String> {
    let cookie_header = headers.get("cookie")?.to_str().ok()?;
    let prefix = format!("{}=", name);

    for part in cookie_header.split(';') {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix(&prefix) {
            // Split value:signature
            if let Some((val, sig)) = value.rsplit_once(':') {
                let expected_sig = sign_cookie_value(val, jwt_secret);
                if sig == expected_sig {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// SHA256-based cookie signing using JWT secret.
fn sign_cookie_value(value: &str, secret: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(b":");
    hasher.update(value.as_bytes());
    let result = hasher.finalize();
    // Hex encode the digest
    let mut hex = String::with_capacity(64);
    for byte in &result {
        use std::fmt::Write;
        // write! to a String never fails, so unwrap is safe here
        let _ = write!(hex, "{byte:02x}");
    }
    hex
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

// ── Types ────────────────────────────────────────────────────────

#[derive(Debug, Serialize, ToSchema)]
pub struct SsoProviderInfo {
    slug: String,
    name: String,
    provider_type: String,
    icon_url: Option<String>,
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
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            AppError::internal("database error")
        })?;

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
    let provider = rg_db::ops::sso_provider_ops::find_by_slug(&state.db, &slug)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            AppError::internal("database error")
        })?
        .ok_or_else(|| AppError::not_found(format!("SSO provider '{}' not found", slug)))?;

    if !provider.enabled {
        return Err(AppError::forbidden("SSO provider is disabled"));
    }

    let base_url = get_api_base_url(&state, &headers);
    let redirect_url = format!("{}/auth/sso/{}/callback", base_url, slug);

    let enc_key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
    let client_secret = provider
        .client_secret_enc
        .as_ref()
        .map(|s| rg_core::auth::encryption::decrypt(s, &enc_key))
        .transpose()
        .map_err(|e| {
            tracing::error!("Decryption error: {}", e);
            AppError::internal("decryption failed")
        })?
        .unwrap_or_default();

    let config = rg_core::auth::sso::SsoProviderConfig {
        slug: provider.slug.clone(),
        provider_type: provider.provider_type.clone(),
        client_id: provider.client_id.unwrap_or_default(),
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
    };

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
    let provider = rg_db::ops::sso_provider_ops::find_by_slug(&state.db, &slug)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            AppError::internal("database error")
        })?
        .ok_or_else(|| AppError::not_found(format!("SSO provider '{}' not found", slug)))?;

    if !provider.enabled {
        return Err(AppError::forbidden("SSO provider is disabled"));
    }

    let base_url = get_api_base_url(&state, &headers);
    let redirect_url = format!("{}/auth/sso/{}/callback", base_url, slug);

    let enc_key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
    let client_secret = provider
        .client_secret_enc
        .as_ref()
        .map(|s| rg_core::auth::encryption::decrypt(s, &enc_key))
        .transpose()
        .map_err(|e| {
            tracing::error!("Decryption error: {}", e);
            AppError::internal("decryption failed")
        })?
        .unwrap_or_default();

    let config = rg_core::auth::sso::SsoProviderConfig {
        slug: provider.slug.clone(),
        provider_type: provider.provider_type.clone(),
        client_id: provider.client_id.unwrap_or_default(),
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
    };

    // ── Exchange code for tokens (with PKCE) ─────────────────────
    let token_response =
        rg_core::auth::sso::oauth2_exchange_code(&config, &query.code, &code_verifier)
            .await
            .map_err(|e| {
                tracing::error!("SSO token exchange error: {}", e);
                AppError::bad_request("failed to exchange authorization code")
            })?;

    // ── Fetch user info ──────────────────────────────────────────
    let user_info =
        rg_core::auth::sso::oauth2_fetch_user_info(&config, &token_response.access_token)
            .await
            .map_err(|e| {
                tracing::error!("SSO user info error: {}", e);
                AppError::bad_request("failed to fetch user info")
            })?;

    // ── Find or create user ──────────────────────────────────────
    let user_id =
        find_or_create_sso_user(&state, &provider.slug, &user_info, &token_response).await?;

    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::internal("user not found after creation"))?;
    if !user.is_active {
        return Err(AppError::unauthorized("account is disabled"));
    }
    if user
        .locked_until
        .is_some_and(|locked_until| locked_until > chrono::Utc::now())
    {
        return Err(AppError::unauthorized("account is temporarily locked"));
    }

    // ── Log successful login ─────────────────────────────────────
    let _ = rg_db::ops::login_log_ops::log_attempt(
        &state.db,
        Some(user_id),
        &user.username,
        &provider.slug,
        None,
        None,
        true,
        None,
    )
    .await;

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
        tracing::warn!(user_id = user.id, %error, "failed to record successful SSO login");
    }

    // ── Issue JWT ────────────────────────────────────────────────
    let token = rg_core::auth::jwt::generate_token(user.id, &user.username, &state.jwt_secret, 7)
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
        (status = 404, description = "SSO provider not found"),
    ),
)]
pub async fn refresh_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<RefreshRequest>,
) -> Result<impl IntoResponse, AppError> {
    use crate::api::auth::extract_user_id;

    // Require authenticated user
    let user_id = extract_user_id(&headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;

    let provider = rg_db::ops::sso_provider_ops::find_by_slug(&state.db, &slug)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("SSO provider not found"))?;

    let enc_key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
    let client_secret = provider
        .client_secret_enc
        .as_ref()
        .map(|s| rg_core::auth::encryption::decrypt(s, &enc_key))
        .transpose()
        .unwrap_or_default()
        .unwrap_or_default();

    let config = rg_core::auth::sso::SsoProviderConfig {
        slug: provider.slug.clone(),
        provider_type: provider.provider_type.clone(),
        client_id: provider.client_id.unwrap_or_default(),
        client_secret,
        redirect_url: String::new(), // not needed for refresh
        scopes: provider
            .scopes
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        discovery_url: provider.discovery_url.clone(),
    };

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

    let token_response = rg_core::auth::sso::oauth2_refresh_token(&config, &refresh_token)
        .await
        .map_err(|e| {
            tracing::error!("SSO token refresh error: {}", e);
            AppError::bad_request("failed to refresh token")
        })?;

    // Store updated tokens
    let enc_access = rg_core::auth::encryption::encrypt(&token_response.access_token, &enc_key)
        .unwrap_or_default();
    let enc_refresh = token_response
        .refresh_token
        .as_ref()
        .and_then(|rt| rg_core::auth::encryption::encrypt(rt, &enc_key).ok());

    let expires_at = token_response
        .expires_in
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs as i64));

    // Update the OAuth account with new tokens
    if let Some(account) = rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
        &state.db, &slug, "", // We'll find by user
    )
    .await
    .ok()
    .flatten()
    {
        rg_db::ops::oauth_account_ops::upsert(
            &state.db,
            account.user_id,
            &slug,
            &account.provider_user_id,
            &account.provider_username,
            &account.email,
            Some(&enc_access),
            enc_refresh.as_deref(),
            expires_at,
        )
        .await
        .ok();
    }

    Ok(Json(serde_json::json!({
        "access_token": token_response.access_token,
        "expires_in": token_response.expires_in,
        "refresh_token": token_response.refresh_token,
    })))
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
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    use crate::api::auth::extract_user_id;

    let user_id = extract_user_id(&headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;

    // Find and delete the OAuth account link
    let accounts = rg_db::ops::oauth_account_ops::find_by_user_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?;

    let account = accounts
        .iter()
        .find(|a| a.provider == slug)
        .ok_or_else(|| AppError::not_found("no OAuth account linked"))?;

    rg_db::ops::oauth_account_ops::delete_by_id(&state.db, account.id, user_id)
        .await
        .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({"unlinked": true})))
}

// ── User helpers ─────────────────────────────────────────────────

async fn find_or_create_sso_user(
    state: &AppState,
    provider_slug: &str,
    user_info: &rg_core::auth::sso::SsoUserInfo,
    token_response: &rg_core::auth::sso::OAuth2TokenResponse,
) -> Result<i64, AppError> {
    let db = &state.db;

    // Check if OAuth account already exists
    if let Some(oauth) = rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
        db,
        provider_slug,
        &user_info.provider_user_id,
    )
    .await
    .map_err(|_| AppError::internal("database error"))?
    {
        // Update stored tokens
        let enc_key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
        let enc_access = rg_core::auth::encryption::encrypt(&token_response.access_token, &enc_key)
            .unwrap_or_default();
        let enc_refresh = token_response
            .refresh_token
            .as_ref()
            .and_then(|rt| rg_core::auth::encryption::encrypt(rt, &enc_key).ok());
        let expires_at = token_response
            .expires_in
            .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs as i64));

        let _ = rg_db::ops::oauth_account_ops::upsert(
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
        .await;

        return Ok(oauth.user_id);
    }

    // Check if user with this email already exists
    let user_id = if let Some(existing) = rg_db::ops::user_ops::find_by_email(db, &user_info.email)
        .await
        .map_err(|_| AppError::internal("database error"))?
    {
        existing.id
    } else {
        // Create new user
        let username = generate_unique_username(db, &user_info.provider_username)
            .await
            .map_err(|e| {
                tracing::error!("Failed to generate username: {}", e);
                AppError::internal("failed to generate username")
            })?;

        rg_db::ops::user_ops::create_user(
            db,
            &username,
            &user_info.email,
            "", // no password for SSO users
            user_info.display_name.as_deref().unwrap_or(&username),
        )
        .await
        .map_err(|e| {
            tracing::error!("Failed to create SSO user: {}", e);
            AppError::internal("user creation failed")
        })?
        .id
    };

    // Encrypt and store tokens
    let enc_key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
    let enc_access = rg_core::auth::encryption::encrypt(&token_response.access_token, &enc_key)
        .unwrap_or_default();
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
    .map_err(|_| AppError::internal("failed to link OAuth account"))?;

    Ok(user_id)
}

/// Generate a unique username based on the provider username.
async fn generate_unique_username(
    db: &sea_orm::DatabaseConnection,
    base: &str,
) -> Result<String, anyhow::Error> {
    if rg_db::ops::user_ops::find_by_username(db, base)
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
        build_auth_cookie, encode_query_component, set_state_cookie, verify_state_cookie,
        SSO_STATE_COOKIE, SSO_VERIFIER_COOKIE,
    };
    use axum::http::{header, HeaderMap};
    use axum::response::IntoResponse;

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
            .starts_with("ironforge_sso_state="));
        assert!(cookies[1]
            .to_str()
            .unwrap()
            .starts_with("ironforge_sso_code_verifier="));
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
