//! Sudo mode: re-prove the password from inside a session.
//!
//! `POST /users/me/sudo` — confirm the password (and the second factor, when
//! one is enrolled) and get the same session back in sudo mode.
//!
//! A bearer session lasts seven days, and the routes that mint credentials
//! outliving it — SSH keys, personal access tokens, passkeys, SSO links — used
//! to ask nothing more of it than that it was valid. One stolen session was
//! therefore permanent access: add a key, and the theft survives the logout
//! that was meant to end it. Those routes now take
//! [`SudoUser`](crate::api::auth::SudoUser), which demands a session whose
//! holder was seen within [`rg_core::auth::jwt::SUDO_TTL`]; this is the door
//! through which a session is seen.
//!
//! The answer is the login's answer — the token in the body and in the HttpOnly
//! cookie — because it *is* the login's token re-issued with a window on it
//! (`Claims::sudo_exp`); the session's subject, generation and expiry do not
//! move. The password check is [`confirm_account_password`], the one every
//! other password door behind a session shares, so a guess here strikes the
//! same account-wide lockout `POST /users/login` records (security audit
//! finding #7).

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::access_audit::{grant_actor, record_credential};
use crate::api::auth::{session_claims, SessionUser};
use crate::api::mfa::confirm_account_password;
use crate::api::users::{build_auth_cookie, AuthResponse};
use crate::error::AppError;
use crate::AppState;

/// The door's name in the login log, so a run of failures can be told from
/// the web login's.
const CHANNEL: &str = "sudo";

#[derive(Deserialize, ToSchema)]
pub struct SudoRequest {
    /// The account's current password. Ignored — and may be empty — for an
    /// account whose password lives with an identity provider and that
    /// confirms with its second factor instead.
    #[serde(default)]
    pub password: String,
    /// The authenticator's current code, required when MFA is enrolled and no
    /// `backup_code` is given.
    #[serde(default)]
    pub totp_code: Option<String>,
    /// One unused backup code, as an alternative to `totp_code`.
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
        (status = 200, description = "The same session, re-issued in sudo mode for ten minutes: \
            the token in the body and in the HttpOnly cookie, exactly as the login answers", body = AuthResponse),
        (status = 400, description = "MFA is enrolled and neither a TOTP nor a backup code was given", body = serde_json::Value),
        (status = 401, description = "The password or the code is wrong, or the account is locked", body = serde_json::Value),
        (status = 403, description = "A personal access token cannot step up; or the account has \
            no password and no second factor to confirm with here", body = serde_json::Value),
        (status = 503, description = "Password verification is at capacity — retry shortly", body = serde_json::Value),
    ),
)]
pub async fn step_up(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    headers: HeaderMap,
    Json(body): Json<SudoRequest>,
) -> Response {
    // `SessionUser` has already validated the token and refused a PAT; it is
    // read again here because the re-issue keeps its expiry and generation.
    let Some(claims) = session_claims(&headers, &state.jwt_secret) else {
        return AppError::unauthorized("authentication required").into_response();
    };
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
        match confirm_second_factor(&state, &headers, &user, &body).await {
            Ok(factor) => Some(factor),
            Err(error) => return error.into_response(),
        }
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
    crate::metrics::recorder::auth_event("sudo", "success");

    // A step-up is the event that *precedes* a new key or token in the
    // journal; without it a credential appearing ten minutes after a login
    // from an unfamiliar address reads as one act rather than two.
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
        [(axum::http::header::SET_COOKIE, build_auth_cookie(&token, is_https))],
        Json(AuthResponse {
            token,
            user_id: user.id,
            username: user.username,
            mfa_required: false,
        }),
    )
        .into_response()
}

/// Re-prove whatever this account signs in with. Returns the method, for the
/// journal.
///
/// A local account re-proves its password through the shared door, lockout
/// included. An LDAP account binds against its directory again. An account
/// that signs in through OAuth/OIDC has no password here at all, so its
/// second factor is the only thing it can re-prove — and without one enrolled
/// there is nothing to confirm with, which is refused rather than waved
/// through: the alternative would make sudo mode free for exactly the accounts
/// whose sessions arrive from a browser redirect.
async fn confirm_first_factor(
    state: &AppState,
    headers: &HeaderMap,
    user: &rg_db::entities::user::Model,
    password: &str,
) -> Result<&'static str, AppError> {
    match user.auth_provider.as_str() {
        "local" => {
            confirm_account_password(state, user, password, CHANNEL, headers).await?;
            Ok("password")
        }
        "ldap" => {
            confirm_ldap_password(state, headers, user, password).await?;
            Ok("ldap")
        }
        provider if user.mfa_enabled => {
            // The code is checked by the caller; the account is only locked
            // out here, as `POST /users/mfa/verify` would refuse it.
            if user
                .locked_until
                .is_some_and(|locked_until| locked_until > chrono::Utc::now())
            {
                return Err(AppError::unauthorized("account is temporarily locked"));
            }
            tracing::debug!(user_id = user.id, provider, "sudo: confirming by second factor only");
            Ok("second_factor_only")
        }
        provider => Err(AppError::forbidden(format!(
            "this account signs in through {provider} and has neither a password nor a second \
             factor to confirm with here; enrol multi-factor authentication to create credentials"
        ))),
    }
}

/// Bind against the directory again, and settle the attempt through the same
/// lockout every other password door uses.
async fn confirm_ldap_password(
    state: &AppState,
    headers: &HeaderMap,
    user: &rg_db::entities::user::Model,
    password: &str,
) -> Result<(), AppError> {
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
        Ok(_) => true,
        // The directory said no. Anything that is not a verdict — the
        // directory unreachable, a secret that would not decrypt — is not a
        // wrong password and must not strike the counter.
        Err(error) if error.to_string() == "invalid credentials" => false,
        Err(error) if error.to_string() == "account is temporarily locked" => {
            return Err(AppError::unauthorized("account is temporarily locked"));
        }
        Err(error) => return Err(AppError::from(error)),
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

/// Verify the TOTP or backup code, exactly as the login's second step does:
/// a TOTP step is spent so a captured code cannot be replayed, a backup code is
/// consumed, and a wrong one advances the account-wide brute-force counter.
/// Returns which kind was used, for the journal.
async fn confirm_second_factor(
    state: &AppState,
    headers: &HeaderMap,
    user: &rg_db::entities::user::Model,
    body: &SudoRequest,
) -> Result<&'static str, AppError> {
    let backup = body
        .backup_code
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty());
    let totp = body
        .totp_code
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty());

    let (kind, valid) = match (backup, totp) {
        (Some(code), _) => (
            "backup_code",
            rg_db::ops::mfa_backup_code_ops::verify_and_consume(&state.db, user.id, code)
                .await
                .map_err(AppError::from)?,
        ),
        (None, Some(code)) => {
            let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
            let totp_secret = user
                .totp_secret
                .as_ref()
                .ok_or_else(|| AppError::internal("MFA secret missing"))?;
            let secret = rg_core::auth::encryption::decrypt(totp_secret, &enc_key)
                .map_err(AppError::from)?;
            let valid = match rg_core::auth::totp::verify_code_step(&secret, code)
                .map_err(AppError::from)?
            {
                Some(step) => rg_db::ops::user_ops::consume_totp_step(&state.db, user.id, step)
                    .await
                    .map_err(AppError::from)?,
                None => false,
            };
            ("totp", valid)
        }
        (None, None) => {
            return Err(AppError::bad_request(
                "this account has multi-factor authentication enabled; send totp_code or backup_code",
            ));
        }
    };

    if valid {
        return Ok(kind);
    }
    let locked = record_failed_second_factor(state, headers, user).await;
    crate::metrics::recorder::auth_event("sudo", "failure");
    crate::metrics::recorder::failed_login("mfa");
    Err(AppError::unauthorized(if locked {
        "account is temporarily locked"
    } else if kind == "backup_code" {
        "invalid backup code"
    } else {
        "invalid TOTP code"
    }))
}

/// Strike the account for a wrong code and file the attempt, as the login's
/// second step does. `false` on a failed write — the attempt already failed,
/// only the silence is refused.
async fn record_failed_second_factor(
    state: &AppState,
    headers: &HeaderMap,
    user: &rg_db::entities::user::Model,
) -> bool {
    let locked = match rg_db::ops::user_ops::record_failed_login(
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
                "failed to record a failed sudo MFA attempt, brute-force counter did not advance"
            );
            false
        }
    };
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(headers);
    if let Err(error) = rg_db::ops::login_log_ops::log_attempt(
        &state.db,
        Some(user.id),
        &user.username,
        CHANNEL,
        ip_address.as_deref(),
        user_agent.as_deref(),
        false,
        Some(if locked {
            "mfa_account_locked"
        } else {
            "invalid_mfa_code"
        }),
    )
    .await
    {
        tracing::warn!(user_id = user.id, error = %format!("{error:#}"), "failed to record sudo MFA attempt");
    }
    locked
}
