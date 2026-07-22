//! MFA (Multi-Factor Authentication) API endpoints.
//!
//! Endpoints:
//!   POST   /users/mfa/setup    — Generate TOTP secret + QR code
//!   POST   /users/mfa/enable   — Verify TOTP code and enable MFA
//!   POST   /users/mfa/disable  — Disable MFA (requires password)
//!   POST   /users/mfa/verify   — Verify TOTP code (during login)
//!   GET    /users/mfa/backup   — Get backup codes
//!   POST   /users/mfa/backup   — Verify and use a backup code

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use tracing;
use utoipa::ToSchema;

use crate::api::auth::{extract_user_id, AUTH_COOKIE_NAME};
use crate::error::AppError;
use crate::AppState;

pub(crate) const MFA_CHALLENGE_COOKIE: &str = "ironforge_mfa_challenge";

pub(crate) fn build_mfa_challenge_cookie(token: &str, is_https: bool) -> String {
    format!(
        "{}={}; HttpOnly; Path=/; SameSite=Strict; Max-Age=300{}",
        MFA_CHALLENGE_COOKIE,
        token,
        if is_https { "; Secure" } else { "" }
    )
}

fn clear_mfa_challenge_cookie(is_https: bool) -> String {
    format!(
        "{}=; HttpOnly; Path=/; SameSite=Strict; Max-Age=0{}",
        MFA_CHALLENGE_COOKIE,
        if is_https { "; Secure" } else { "" }
    )
}

fn extract_mfa_challenge(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|cookie| cookie.strip_prefix(&format!("{}=", MFA_CHALLENGE_COOKIE)))
        .filter(|token| !token.is_empty())
}

async fn record_mfa_attempt(
    state: &AppState,
    headers: &HeaderMap,
    user: &rg_db::entities::user::Model,
    auth_provider: &str,
    success: bool,
) -> bool {
    let locked = if success {
        if let Err(error) = rg_db::ops::user_ops::record_successful_login(&state.db, user.id).await
        {
            tracing::warn!(user_id = user.id, %error, "failed to record completed MFA login");
        }
        false
    } else {
        rg_db::ops::user_ops::record_failed_login(&state.db, user.id, 5)
            .await
            .unwrap_or(false)
    };
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(headers);
    if let Err(error) = rg_db::ops::login_log_ops::log_attempt(
        &state.db,
        Some(user.id),
        &user.username,
        auth_provider,
        ip_address.as_deref(),
        user_agent.as_deref(),
        success,
        (!success).then_some(if locked {
            "mfa_account_locked"
        } else {
            "invalid_mfa_code"
        }),
    )
    .await
    {
        tracing::warn!(user_id = user.id, %error, "failed to record MFA login attempt");
    }
    locked
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SetupMfaResponse {
    secret: String,
    otpauth_url: String,
    qr_svg: String,
}

/// POST /users/mfa/setup
/// Generate a new TOTP secret and return an otpauth URL + QR code SVG.
#[utoipa::path(
    post,
    path = "/users/mfa/setup",
    tag = "MFA",
    responses(
        (status = 200, description = "TOTP secret and QR code generated", body = SetupMfaResponse),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "User not found"),
        (status = 500, description = "Internal server error"),
    ),
)]
pub async fn setup_mfa(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SetupMfaResponse>, AppError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("unauthorized"))?;

    // Get username to include in TOTP label
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            AppError::internal("database error")
        })?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    let (secret, otpauth_url, _qr_text) =
        rg_core::auth::totp::generate_secret(&user.username, "IronForge").map_err(|e| {
            tracing::error!("TOTP error: {}", e);
            AppError::internal("TOTP generation failed")
        })?;

    let qr_svg = rg_core::auth::totp::generate_qr_svg(&otpauth_url);

    // Store the secret temporarily (encrypted) but don't enable MFA yet
    let enc_key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
    let enc_secret = rg_core::auth::encryption::encrypt(&secret, &enc_key).map_err(|e| {
        tracing::error!("Encryption error: {}", e);
        AppError::internal("encryption failed")
    })?;

    rg_db::ops::user_ops::update_totp_secret(&state.db, user_id, &enc_secret)
        .await
        .map_err(|e| {
            tracing::error!("DB error: {}", e);
            AppError::internal("database error")
        })?;

    Ok(Json(SetupMfaResponse {
        secret,
        otpauth_url,
        qr_svg,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EnableMfaRequest {
    code: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnableMfaResponse {
    enabled: bool,
    backup_codes: Vec<String>,
}

/// POST /users/mfa/enable
/// Verify the setup TOTP code and enable MFA, generating backup codes.
#[utoipa::path(
    post,
    path = "/users/mfa/enable",
    tag = "MFA",
    request_body = EnableMfaRequest,
    responses(
        (status = 200, description = "MFA enabled successfully with backup codes", body = EnableMfaResponse),
        (status = 400, description = "Invalid TOTP code or MFA not set up"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "User not found"),
        (status = 500, description = "Internal server error"),
    ),
)]
pub async fn enable_mfa(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<EnableMfaRequest>,
) -> Result<Json<EnableMfaResponse>, AppError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("unauthorized"))?;

    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    // Decrypt the TOTP secret
    let enc_key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
    let totp_secret = match &user.totp_secret {
        Some(s) => rg_core::auth::encryption::decrypt(s, &enc_key).map_err(|e| {
            tracing::error!("Decryption error: {}", e);
            AppError::internal("decryption failed")
        })?,
        None => return Err(AppError::bad_request("MFA not set up yet")),
    };

    // Verify the TOTP code
    let valid =
        rg_core::auth::totp::verify_code(&totp_secret, &req.code).map_err(AppError::from)?;

    if !valid {
        return Err(AppError::bad_request("invalid TOTP code"));
    }

    // Enable MFA and store re-encrypted secret
    rg_db::ops::user_ops::enable_mfa(&state.db, user_id, "totp")
        .await
        .map_err(AppError::from)?;

    // Generate backup codes
    let backup_codes = rg_db::ops::mfa_backup_code_ops::generate_codes(8);
    rg_db::ops::mfa_backup_code_ops::set_codes(&state.db, user_id, &backup_codes)
        .await
        .map_err(AppError::from)?;

    Ok(Json(EnableMfaResponse {
        enabled: true,
        backup_codes,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct VerifyMfaRequest {
    username: String,
    code: String,
    /// If true, verify using a backup code instead of TOTP
    #[serde(default)]
    backup: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VerifyMfaResponse {
    token: String,
    user_id: i64,
    username: String,
}

/// POST /users/mfa/verify
/// Second step of login: verify MFA code and issue JWT.
#[utoipa::path(
    post,
    path = "/users/mfa/verify",
    tag = "MFA",
    request_body = VerifyMfaRequest,
    responses(
        (status = 200, description = "MFA verified and JWT issued", body = VerifyMfaResponse),
        (status = 400, description = "MFA not enabled"),
        (status = 401, description = "Invalid credentials or MFA code"),
        (status = 500, description = "Internal server error"),
    ),
)]
pub async fn verify_mfa(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<VerifyMfaRequest>,
) -> Result<impl IntoResponse, AppError> {
    let challenge = extract_mfa_challenge(&headers)
        .and_then(|token| rg_core::auth::jwt::validate_mfa_challenge(token, &state.jwt_secret))
        .ok_or_else(|| AppError::unauthorized("MFA login challenge is missing or expired"))?;
    if challenge.username != req.username {
        return Err(AppError::unauthorized("invalid MFA login challenge"));
    }
    let user_id = challenge
        .sub
        .parse::<i64>()
        .map_err(|_| AppError::unauthorized("invalid MFA login challenge"))?;
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| {
            tracing::warn!(user_id, "MFA verify: user not found");
            AppError::unauthorized("invalid credentials")
        })?;
    if user.username != challenge.username || !user.is_active {
        return Err(AppError::unauthorized("invalid credentials"));
    }
    if user
        .locked_until
        .is_some_and(|locked_until| locked_until > chrono::Utc::now())
    {
        return Err(AppError::unauthorized("account is temporarily locked"));
    }

    if !user.mfa_enabled {
        return Err(AppError::bad_request("MFA not enabled"));
    }

    if req.backup {
        // Verify backup code
        let valid =
            rg_db::ops::mfa_backup_code_ops::verify_and_consume(&state.db, user.id, &req.code)
                .await
                .map_err(AppError::from)?;

        if !valid {
            let locked =
                record_mfa_attempt(&state, &headers, &user, &challenge.auth_provider, false).await;
            return Err(AppError::unauthorized(if locked {
                "account is temporarily locked"
            } else {
                "invalid backup code"
            }));
        }
    } else {
        // Verify TOTP code
        let enc_key = rg_core::auth::encryption::derive_key(&state.jwt_secret);
        let totp_secret = user
            .totp_secret
            .as_ref()
            .ok_or_else(|| AppError::internal("MFA secret missing"))?;

        let secret =
            rg_core::auth::encryption::decrypt(totp_secret, &enc_key).map_err(AppError::from)?;

        let valid = rg_core::auth::totp::verify_code(&secret, &req.code).map_err(AppError::from)?;

        if !valid {
            let locked =
                record_mfa_attempt(&state, &headers, &user, &challenge.auth_provider, false).await;
            return Err(AppError::unauthorized(if locked {
                "account is temporarily locked"
            } else {
                "invalid TOTP code"
            }));
        }
    }

    record_mfa_attempt(&state, &headers, &user, &challenge.auth_provider, true).await;

    // Issue JWT
    let token = rg_core::auth::jwt::generate_token(user.id, &user.username, &state.jwt_secret, 7)
        .map_err(AppError::from)?;

    // M-4: Set HttpOnly cookie for browser-based auth
    let is_https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "https")
        .unwrap_or(false);
    let cookie_value = format!(
        "{}={}; HttpOnly; Path=/; SameSite=Strict; Max-Age=604800{}",
        AUTH_COOKIE_NAME,
        token,
        if is_https { "; Secure" } else { "" }
    );

    let mut response = (
        StatusCode::OK,
        [(axum::http::header::SET_COOKIE, cookie_value)],
        Json(VerifyMfaResponse {
            token,
            user_id: user.id,
            username: user.username,
        }),
    )
        .into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&clear_mfa_challenge_cookie(is_https)) {
        response
            .headers_mut()
            .append(axum::http::header::SET_COOKIE, value);
    }
    Ok(response)
}

/// GET /users/mfa/backup
/// Get existing backup codes status (does not reveal unused codes).
#[utoipa::path(
    get,
    path = "/users/mfa/backup",
    tag = "MFA",
    responses(
        (status = 200, description = "Backup codes status summary", body = serde_json::Value),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error"),
    ),
)]
pub async fn get_backup_codes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("unauthorized"))?;

    let codes = rg_db::ops::mfa_backup_code_ops::list_codes(&state.db, user_id)
        .await
        .map_err(AppError::from)?;

    let summary: Vec<serde_json::Value> = codes
        .iter()
        .map(|c| {
            serde_json::json!({
                "used": c.used,
                "used_at": c.used_at,
                "created_at": c.created_at,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "total": codes.len(),
        "unused": codes.iter().filter(|c| !c.used).count(),
        "codes": summary,
    })))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DisableMfaRequest {
    password: String,
}

/// POST /users/mfa/disable
/// Disable MFA (requires current password for security).
#[utoipa::path(
    post,
    path = "/users/mfa/disable",
    tag = "MFA",
    request_body = DisableMfaRequest,
    responses(
        (status = 200, description = "MFA disabled successfully"),
        (status = 401, description = "Unauthorized or invalid password"),
        (status = 404, description = "User not found"),
        (status = 500, description = "Internal server error"),
    ),
)]
pub async fn disable_mfa(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DisableMfaRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("unauthorized"))?;

    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    // Verify password before disabling MFA
    let password_ok = rg_core::auth::password::verify_password(&req.password, &user.password_hash)
        .map_err(|_| AppError::unauthorized("invalid password"))?;
    if !password_ok {
        return Err(AppError::unauthorized("invalid password"));
    }

    rg_db::ops::user_ops::disable_mfa(&state.db, user_id)
        .await
        .map_err(AppError::from)?;

    Ok(Json(serde_json::json!({"disabled": true})))
}
