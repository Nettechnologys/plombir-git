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

use crate::api::auth::AUTH_COOKIE_NAME;
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

/// Check if the request was made over HTTPS (for Secure cookie flag).
fn is_https_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "https")
        .unwrap_or(false)
}

/// Helper to record audit log (fire-and-forget, does not fail the main operation).
#[allow(clippy::too_many_arguments)]
async fn record_audit(
    db: &sea_orm::DatabaseConnection,
    user_id: i64,
    username: &str,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<i64>,
    resource_name: Option<&str>,
    headers: &HeaderMap,
    details: Option<serde_json::Value>,
) {
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(headers);

    let entry = rg_db::entities::audit_log::ActiveModel {
        id: sea_orm::NotSet,
        user_id: sea_orm::Set(Some(user_id)),
        username: sea_orm::Set(Some(username.to_string())),
        action: sea_orm::Set(action.to_string()),
        resource_type: sea_orm::Set(resource_type.map(|s| s.to_string())),
        resource_id: sea_orm::Set(resource_id),
        resource_name: sea_orm::Set(resource_name.map(|s| s.to_string())),
        ip_address: sea_orm::Set(ip_address),
        user_agent: sea_orm::Set(user_agent),
        details: sea_orm::Set(details.map(|v| v.to_string())),
        created_at: sea_orm::Set(chrono::Utc::now()),
    };

    if let Err(e) = rg_db::ops::audit_log_ops::insert(db, entry).await {
        tracing::warn!(error = %e, "failed to record audit log");
    }
}

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
    )
)]
pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegisterRequest>,
) -> impl IntoResponse {
    match rg_core::user::service::register(
        &state.db,
        &body.username,
        &body.email,
        &body.password,
        &state.jwt_secret,
    )
    .await
    {
        Ok(resp) => {
            // Record audit log
            let details = serde_json::json!({
                "email": body.email,
                "username": body.username
            });
            record_audit(
                &state.db,
                resp.user_id,
                &resp.username,
                "user.register",
                Some("user"),
                Some(resp.user_id),
                Some(&resp.username),
                &headers,
                Some(details),
            )
            .await;

            (StatusCode::CREATED, Json(serde_json::json!(resp))).into_response()
        }
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
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
        &state.jwt_secret,
    )
    .await
    {
        Ok(outcome) => {
            let resp = outcome.response;
            let login_method = match outcome.method {
                rg_core::user::service::LoginMethod::Password => "password",
                rg_core::user::service::LoginMethod::Ldap => "ldap",
            };
            // Check if MFA is enabled for this user
            let mfa_required = match rg_db::ops::user_ops::find_by_id(&state.db, resp.user_id).await
            {
                Ok(Some(user)) => user.mfa_enabled,
                _ => false,
            };

            // Record audit log
            let details = serde_json::json!({
                "mfa_required": mfa_required,
                "login_method": login_method
            });
            record_audit(
                &state.db,
                resp.user_id,
                &resp.username,
                "user.login",
                Some("user"),
                Some(resp.user_id),
                Some(&resp.username),
                &headers,
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
                tracing::warn!(%error, "failed to record successful login attempt");
            }
            if !mfa_required {
                if let Err(error) =
                    rg_db::ops::user_ops::record_successful_login(&state.db, resp.user_id).await
                {
                    tracing::warn!(user_id = resp.user_id, %error, "failed to update login state");
                }
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
                    Err(error) => return AppError::internal(error).into_response(),
                };
                let is_https = is_https_request(&headers);
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
                let is_https = is_https_request(&headers);
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
            let user = if body.login.contains('@') {
                rg_db::ops::user_ops::find_by_email(&state.db, &body.login)
                    .await
                    .ok()
                    .flatten()
            } else {
                rg_db::ops::user_ops::find_by_username(&state.db, &body.login)
                    .await
                    .ok()
                    .flatten()
            };
            let mut locked = error.to_string() == "account is temporarily locked";
            if !locked && error.to_string() == "invalid credentials" {
                if let Some(user) = &user {
                    locked = rg_db::ops::user_ops::record_failed_login(&state.db, user.id, 5)
                        .await
                        .unwrap_or(false);
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
            AppError::unauthorized(if locked {
                "account is temporarily locked"
            } else {
                "invalid credentials"
            })
            .into_response()
        }
    }
}

/// POST /api/v1/users/logout — clears the HttpOnly auth cookie (M-4).
///
/// The frontend calls this on logout to invalidate the cookie.
/// The JWT itself remains valid until expiry (stateless JWT), but the
/// browser will no longer send it.
pub async fn logout(headers: HeaderMap) -> impl IntoResponse {
    let is_https = is_https_request(&headers);
    let cookie = build_clear_cookie(is_https);
    (
        StatusCode::OK,
        [(axum::http::header::SET_COOKIE, cookie)],
        Json(serde_json::json!({"logged_out": true})),
    )
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
pub async fn me(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => return AppError::unauthorized("missing or invalid token").into_response(),
    };

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
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

// ── PAT (Personal Access Token) handlers ────────────────────────────────

use sha2::{Digest, Sha256};

#[derive(serde::Deserialize, ToSchema)]
pub struct CreateTokenRequest {
    pub name: String,
    pub scopes: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(serde::Serialize, ToSchema)]
pub struct AccessTokenResponse {
    pub id: i64,
    pub name: String,
    pub scopes: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let entropy: u128 = {
        let mut buf = [0u8; 16];
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = std::io::Read::read_exact(&mut f, &mut buf);
            u128::from_le_bytes(buf)
        } else {
            let pid = std::process::id() as u128;
            (nanos << 32) | pid
        }
    };
    format!("ifp_{:016x}{:032x}", nanos, entropy)
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
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
pub async fn list_tokens(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required".to_string()).into_response();
        }
    };

    match rg_db::ops::token_ops::list_by_user(&state.db, user_id).await {
        Ok(tokens) => {
            let tokens: Vec<AccessTokenResponse> = tokens
                .into_iter()
                .map(|token| AccessTokenResponse {
                    id: token.id,
                    name: token.name,
                    scopes: token.scopes,
                    expires_at: token.expires_at,
                    last_used_at: token.last_used_at,
                    created_at: token.created_at,
                })
                .collect();

            (StatusCode::OK, Json(tokens)).into_response()
        }
        Err(e) => AppError::internal(e.to_string()).into_response(),
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
    ),
)]
pub async fn create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateTokenRequest>,
) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required".to_string()).into_response();
        }
    };

    if body.name.trim().is_empty() {
        return AppError::bad_request("token name cannot be empty".to_string()).into_response();
    }
    let raw_token = generate_token();
    let token_hash = hash_token(&raw_token);
    let scopes = match rg_core::auth::pat_scope::normalize_scopes(
        body.scopes.as_deref().unwrap_or("repo"),
    ) {
        Ok(scopes) => scopes,
        Err(e) => return AppError::bad_request(e.to_string()).into_response(),
    };
    let expires_at = body
        .expires_at
        .as_deref()
        .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
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
    };
    match rg_db::ops::token_ops::create(&state.db, model).await {
        Ok(token) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": token.id,
                "name": token.name,
                "token": raw_token,
                "scopes": token.scopes,
                "expires_at": token.expires_at,
                "created_at": token.created_at,
            })),
        )
            .into_response(),
        Err(e) => AppError::internal(e.to_string()).into_response(),
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
    ),
)]
pub async fn delete_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required".to_string()).into_response();
        }
    };

    let token = match rg_db::ops::token_ops::find_by_id(&state.db, id).await {
        Ok(Some(t)) => t,
        _ => {
            return AppError::not_found("token not found".to_string()).into_response();
        }
    };
    if token.user_id != user_id {
        return AppError::forbidden("you can only revoke your own tokens".to_string())
            .into_response();
    }
    match rg_db::ops::token_ops::delete_by_id(&state.db, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AppError::internal(e.to_string()).into_response(),
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
    // Determine base URL from Host header (fallback to localhost)
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:3000");
    let scheme = if host.contains("localhost") || host.contains("127.0.0.1") {
        "http"
    } else {
        "https"
    };
    let base_url = format!("{}://{}", scheme, host);

    match rg_core::user::service::forgot_password(
        &state.db,
        &body.email,
        state.smtp_config.as_ref(),
        &base_url,
    )
    .await
    {
        Ok(()) => {
            tracing::info!("password reset requested for email: {}", body.email);
            (
                StatusCode::OK,
                Json(serde_json::json!({ "message": "If the email exists, a reset link has been sent" })),
            )
                .into_response()
        }
        Err(e) => AppError::internal(e.to_string()).into_response(),
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

#[utoipa::path(
    post,
    path = "/users/reset-password",
    tag = "Users",
    request_body = ResetPasswordRequest,
    responses(
        (status = 200, description = "Password reset successfully", body = serde_json::Value),
        (status = 400, description = "Invalid or expired token, or invalid password"),
    ),
)]
pub async fn reset_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ResetPasswordRequest>,
) -> impl IntoResponse {
    match rg_core::user::service::reset_password(
        &state.db,
        &body.token,
        &body.new_password,
        &state.jwt_secret,
    )
    .await
    {
        Ok(resp) => {
            tracing::info!(user_id = resp.user_id, "password reset successful");
            // M-4: Set HttpOnly cookie so the user stays logged in after reset
            let is_https = is_https_request(&headers);
            let cookie = build_auth_cookie(&resp.token, is_https);
            (
                StatusCode::OK,
                [(axum::http::header::SET_COOKIE, cookie)],
                Json(serde_json::json!(resp)),
            )
                .into_response()
        }
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}
