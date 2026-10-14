//! Sudo mode: prove it is you again, from inside a session.
//!
//! `POST /users/me/sudo` re-proves the password — the account's own, or its
//! directory's for an LDAP account — plus the second factor when one is
//! enrolled, and answers the same session re-issued with a ten-minute window
//! (`Claims::sudo_exp`). The routes that mint something outliving the session
//! take [`SudoUser`](crate::api::auth::SudoUser) and refuse a session outside
//! that window, so a stolen session cannot make itself permanent (decision
//! #10, security audit finding #7).
//!
//! An account with no password here — one that signs in through an SSO
//! provider — has nothing to send to this door. It enters sudo mode by signing
//! in again through its provider (`POST /auth/sso/{slug}/reauthenticate`) or
//! with a passkey; both are logins, and a login that proved something starts
//! in sudo mode.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::access_audit::{grant_actor, record_credential};
use crate::api::auth::LoginSession;
use crate::api::mfa::{confirm_account_password, confirm_second_factor, SecondFactor};
use crate::api::users::{build_auth_cookie, AuthResponse};
use crate::error::AppError;
use crate::AppState;

/// The door's name in the login log, so a run of failures here can be told
/// from the web login's.
const CHANNEL: &str = "sudo";

#[derive(Deserialize, ToSchema)]
pub struct SudoRequest {
    /// The account password, or the directory password of an LDAP account.
    pub password: String,
    /// The authenticator's current code; required when MFA is enrolled and no
    /// `backup_code` is given.
    #[serde(default)]
    pub totp_code: Option<String>,
    /// One unused backup code, instead of `totp_code`.
    #[serde(default)]
    pub backup_code: Option<String>,
}

/// POST /api/v1/users/me/sudo
#[utoipa::path(
    post,
    path = "/users/me/sudo",
    tag = "Users",
    request_body = SudoRequest,
    responses(
        (status = 200, description = "The same session re-issued in sudo mode for ten minutes: \
            the token in the body and in the HttpOnly cookie, as the login answers", body = AuthResponse),
        (status = 400, description = "No password was sent, MFA is enrolled and no code was sent, \
            or the account has no password here — it re-authenticates through its SSO provider \
            or with a passkey instead", body = serde_json::Value),
        (status = 401, description = "The password or the code is wrong, or the account is locked", body = serde_json::Value),
        (status = 403, description = "A personal access token cannot enter sudo mode", body = serde_json::Value),
        (status = 503, description = "Password verification is at capacity — retry shortly", body = serde_json::Value),
    ),
)]
pub async fn step_up(
    State(state): State<AppState>,
    LoginSession { user_id, claims }: LoginSession,
    headers: HeaderMap,
    Json(body): Json<SudoRequest>,
) -> Response {
    let user = match rg_db::ops::user_ops::find_by_id(&state.db, user_id).await {
        Ok(Some(user)) if user.is_usable() => user,
        Ok(_) => return AppError::unauthorized("authentication required").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };

    let method = match confirm_first_factor(&state, &headers, &user, &body.password).await {
        Ok(method) => method,
        Err(error) => return error.into_response(),
    };
    let second_factor = if user.mfa_enabled {
        let factor = match (
            nonempty(body.backup_code.as_deref()),
            nonempty(body.totp_code.as_deref()),
        ) {
            (Some(code), _) => SecondFactor::BackupCode(code),
            (None, Some(code)) => SecondFactor::Totp(code),
            (None, None) => {
                return AppError::bad_request(
                    "this account has multi-factor authentication enabled; send totp_code or backup_code",
                )
                .into_response();
            }
        };
        if let Err(error) =
            confirm_second_factor(&state, &headers, &user, factor, CHANNEL, CHANNEL).await
        {
            return error.into_response();
        }
        Some(factor.kind())
    } else {
        None
    };

    let actor = match grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let token = match rg_core::auth::jwt::reissue_with_sudo(&claims, &state.jwt_secret) {
        Ok(token) => token,
        Err(error) => return AppError::from(error).into_response(),
    };
    crate::metrics::recorder::auth_event(CHANNEL, "success");

    // The event that precedes a new key or token in the journal: without it a
    // credential appearing after a login from an unfamiliar address reads as
    // one act rather than two.
    record_credential(
        &state,
        &actor,
        "user.sudo",
        user_id,
        &headers,
        serde_json::json!({
            "method": method,
            "second_factor": second_factor,
            "window_seconds": rg_core::auth::jwt::SUDO_TTL.num_seconds(),
        }),
    )
    .await;

    let is_https = crate::public_url::request_is_https(&state, &headers);
    (
        StatusCode::OK,
        [(
            axum::http::header::SET_COOKIE,
            build_auth_cookie(&token, is_https),
        )],
        Json(AuthResponse {
            token,
            user_id: user.id,
            username: user.username,
            mfa_required: false,
        }),
    )
        .into_response()
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Re-prove what this account signs in with, answering the method for the
/// journal: the password through the shared lockout-aware door, or a fresh
/// bind against the account's directory.
async fn confirm_first_factor(
    state: &AppState,
    headers: &HeaderMap,
    user: &rg_db::entities::user::Model,
    password: &str,
) -> Result<&'static str, AppError> {
    if user.auth_provider == "ldap" {
        confirm_directory_password(state, headers, user, password).await?;
        return Ok("ldap");
    }
    if !crate::api::mfa::has_local_password(user) {
        return Err(AppError::bad_request(format!(
            "this account signs in through {}; sign in again through it, or use a passkey",
            user.auth_provider
        )));
    }
    confirm_account_password(state, user, password, CHANNEL, headers).await?;
    Ok("password")
}

/// Bind against the account's directory again, settling the attempt through
/// the lockout every other password door shares.
async fn confirm_directory_password(
    state: &AppState,
    headers: &HeaderMap,
    user: &rg_db::entities::user::Model,
    password: &str,
) -> Result<(), AppError> {
    // A missing password is a malformed request, not a wrong one.
    if password.is_empty() {
        return Err(AppError::bad_request("the directory password is required"));
    }
    let bound = match rg_core::user::service::login_with_configured_auth(
        &state.db,
        &user.username,
        password,
        &state.encryption_key,
        &state.ldap_transport_policy,
        crate::client_ip::from_headers(headers),
    )
    .await
    {
        // The directory has to vouch for *this* account, not for whichever
        // one the name resolved to by the time it answered.
        Ok(outcome) => outcome.user.id == user.id,
        Err(error) => match error.to_string().as_str() {
            "invalid credentials" => false,
            "account is temporarily locked" => {
                return Err(AppError::unauthorized("account is temporarily locked"));
            }
            "account is disabled" => {
                return Err(AppError::unauthorized("account is disabled"));
            }
            // An unreachable directory or a provider that will not decrypt is
            // not a wrong password and must not strike the counter.
            _ => return Err(AppError::from(error)),
        },
    };
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(headers);
    let attempt = rg_core::auth::lockout::settle_password_attempt(
        &state.db,
        Some(user),
        bound,
        rg_core::auth::lockout::AttemptOrigin {
            login: &user.username,
            channel: CHANNEL,
            ip_address: ip_address.as_deref(),
            user_agent: user_agent.as_deref(),
        },
    )
    .await
    .map_err(AppError::from)?;
    match attempt {
        rg_core::auth::lockout::PasswordAttempt::Rejected { locked } => {
            Err(AppError::unauthorized(if locked {
                "account is temporarily locked"
            } else {
                "invalid password"
            }))
        }
        _ => Ok(()),
    }
}
