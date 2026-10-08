//! MFA (Multi-Factor Authentication) API endpoints.
//!
//! Endpoints:
//!   POST   /users/mfa/setup    — Stage a TOTP secret + QR code
//!   POST   /users/mfa/enable   — Verify the staged TOTP code and enable MFA
//!   POST   /users/mfa/disable  — Disable MFA (requires password)
//!   POST   /users/mfa/verify   — Verify TOTP code (during login)
//!   GET    /users/mfa/backup   — Backup code status (never the codes themselves)
//!   POST   /users/mfa/backup/regenerate — Replace the unused backup codes
//!
//! A backup code is redeemed through `POST /users/mfa/verify` with `backup:
//! true`; the `POST /users/mfa/backup` this header used to advertise has never
//! been routed.
//!
//! Enrolment is two steps and only the second one writes anything the login
//! path reads: `setup` parks the new secret in `users.pending_totp_secret`, and
//! `enable` promotes it once a code proves somebody holds it. An enrolment that
//! is abandoned in between therefore costs the account nothing — which it used
//! to cost everything (card_08400088bb40).

use anyhow::Context as _;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use tracing;
use utoipa::ToSchema;

use crate::api::access_audit::{grant_actor, record_credential};
use crate::api::auth::{AuthUser, AUTH_COOKIE_NAME};
use crate::error::AppError;
use crate::AppState;

pub(crate) const MFA_CHALLENGE_COOKIE: &str = "plombir_git_mfa_challenge";

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

async fn record_failed_mfa_attempt(
    state: &AppState,
    headers: &HeaderMap,
    user: &rg_db::entities::user::Model,
    auth_provider: &str,
) -> bool {
    // `false` means "not locked", and that is also what a failed write would
    // report — so a silent error turns second-factor brute-force protection
    // into a no-op with nothing in the log. Keep the permissive default (the
    // attempt already failed), lose only the silence.
    let locked = match rg_db::ops::user_ops::record_failed_login(&state.db, user.id, 5).await {
        Ok(locked) => locked,
        Err(error) => {
            tracing::warn!(
                user_id = user.id,
                error = %format!("{error:#}"),
                "failed to record a failed MFA attempt, brute-force counter did not advance"
            );
            false
        }
    };
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(headers);
    if let Err(error) = rg_db::ops::login_log_ops::log_attempt(
        &state.db,
        Some(user.id),
        &user.username,
        auth_provider,
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
        tracing::warn!(user_id = user.id, error = %format!("{error:#}"), "failed to record MFA login attempt");
    }
    locked
}

async fn finalize_mfa_login(
    state: &AppState,
    headers: &HeaderMap,
    user_id: i64,
    auth_provider: &str,
) -> Result<rg_db::entities::user::Model, AppError> {
    let user = crate::api::users::finalized_login_user(
        user_id,
        rg_db::ops::user_ops::record_successful_login(&state.db, user_id).await,
    )?;
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(headers);
    if let Err(error) = rg_db::ops::login_log_ops::log_attempt(
        &state.db,
        Some(user.id),
        &user.username,
        auth_provider,
        ip_address.as_deref(),
        user_agent.as_deref(),
        true,
        None,
    )
    .await
    {
        tracing::warn!(user_id = user.id, error = %format!("{error:#}"), "failed to record MFA login attempt");
    }
    Ok(user)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SetupMfaResponse {
    secret: String,
    otpauth_url: String,
    qr_svg: String,
}

/// How long a handed-out enrolment secret may still be armed by `enable`.
///
/// The setup response is the one place the plaintext secret is ever shown, and
/// nothing about it expires on its own: without a bound, a QR code screenshotted
/// months ago stays a way to arm a second factor on the account. Long enough for
/// a person to fetch their phone, find the app and type a code; short enough
/// that a wizard abandoned yesterday is not still live today.
const PENDING_ENROLMENT_TTL: chrono::Duration = chrono::Duration::minutes(30);

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
    AuthUser(user_id): AuthUser,
) -> Result<Json<SetupMfaResponse>, AppError> {
    // Get username to include in TOTP label
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    let (secret, otpauth_url, _qr_text) =
        rg_core::auth::totp::generate_secret(&user.username, "Plombir Git").map_err(|e| {
            tracing::error!("TOTP error: {}", e);
            AppError::internal("TOTP generation failed")
        })?;

    let qr_svg = rg_core::auth::totp::generate_qr_svg(&otpauth_url);

    // Sealed and parked in the *pending* slot, never in `totp_secret`.
    //
    // This is the preparatory step: nothing here proves that anybody holds the
    // secret being handed out, so it must not become the secret the login path
    // verifies against. It used to. An account with a working authenticator
    // whose owner merely re-opened this wizard — to move to a new phone, or
    // because a failed backup-code read told the page MFA was off — kept
    // `mfa_enabled = true` against a secret no authenticator had, and the only
    // way back in was a backup code (card_08400088bb40). `enable` promotes what
    // is parked here, and only after a code computed from it comes back.
    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let enc_secret = rg_core::auth::encryption::encrypt(&secret, &enc_key).map_err(|e| {
        tracing::error!("Encryption error: {}", e);
        AppError::internal("encryption failed")
    })?;

    rg_db::ops::user_ops::stage_pending_totp_secret(&state.db, user_id, &enc_secret)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    Ok(Json(SetupMfaResponse {
        secret,
        otpauth_url,
        qr_svg,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EnableMfaRequest {
    code: String,
    /// The account password, required only when this call would *replace* a
    /// second factor that is currently protecting the account.
    ///
    /// Optional on a first enrolment: there the caller is adding a protection,
    /// not removing one, and the session they already hold is what authorises
    /// it. On a rotation the same call retires the authenticator the account is
    /// standing on, which is the event `POST /users/mfa/disable` asks for a
    /// password before allowing — a stolen session must not be enough to move
    /// the second factor onto the thief's own phone.
    #[serde(default)]
    password: Option<String>,
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
        (status = 400, description = "Invalid TOTP code, no setup in flight, or a replacement without the account password"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "User not found"),
        (status = 500, description = "Internal server error"),
    ),
)]
pub async fn enable_mfa(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Json(req): Json<EnableMfaRequest>,
) -> Result<Json<EnableMfaResponse>, AppError> {
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    // The enrolment in flight, never the live factor. Verifying against
    // `totp_secret` would let a code from the authenticator the account is
    // *already* using re-run enrolment and re-issue backup codes.
    let Some(staged) = user.pending_totp_secret.as_deref() else {
        return Err(AppError::bad_request("MFA not set up yet"));
    };
    if user
        .pending_totp_secret_at
        .is_none_or(|issued| chrono::Utc::now() - issued > PENDING_ENROLMENT_TTL)
    {
        return Err(AppError::bad_request(
            "this MFA setup has expired, start it again",
        ));
    }

    // The step that retires the live factor, so it is the step that asks for a
    // password — the same question `POST /users/mfa/disable` asks, for the same
    // event. A first enrolment protects an account that has no second factor
    // yet and answers no such question.
    if user.mfa_enabled {
        let Some(password) = req.password.as_deref().filter(|p| !p.is_empty()) else {
            return Err(AppError::bad_request(
                "replacing the current authenticator requires the account password",
            ));
        };
        confirm_account_password(&state, &user, password, "mfa-rotate", &headers).await?;
    }

    // Decrypt the staged TOTP secret
    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let totp_secret = rg_core::auth::encryption::decrypt(staged, &enc_key).map_err(|e| {
        tracing::error!("Decryption error: {}", e);
        AppError::internal("decryption failed")
    })?;

    // Verify the TOTP code.
    //
    // Deliberately the plain `bool` and not the step-spending form `verify_mfa`
    // uses: enrolment is not a pass of the second factor — the factor does not
    // exist yet — it is the authenticator proving it holds the secret this
    // endpoint's caller was just handed, over an already-authenticated session.
    // Spending the step here would also make enrolment un-retryable for 30
    // seconds after a failure that rolled the enrolment back, which is exactly
    // the path `mfa_enable_atomicity_tests` drives.
    let valid =
        rg_core::auth::totp::verify_code(&totp_secret, &req.code).map_err(AppError::from)?;

    if !valid {
        return Err(AppError::bad_request("invalid TOTP code"));
    }

    // Generate backup codes
    let backup_codes = rg_db::ops::mfa_backup_code_ops::generate_codes(
        rg_db::ops::mfa_backup_code_ops::BACKUP_CODE_COUNT,
    );

    // Named before the factor is switched on: a failed lookup afterwards would
    // leave the one record of who armed this account's second factor blank.
    let audit_actor = grant_actor(&state, user_id).await?;

    // The promotion, the flag and the codes go in as one commit. The response
    // below is the only place these codes are ever shown, so switching the
    // second factor on first and failing on the codes afterwards is how an
    // account ends up locked out — with a `500` telling its owner that nothing
    // was enabled. The promotion belongs in the same commit for the same
    // reason: a live secret replaced without the recovery set that goes with it
    // is the lockout this endpoint exists to avoid.
    rg_db::ops::user_ops::enable_mfa_with_backup_codes(&state.db, user_id, &backup_codes)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    // The method and how many codes were issued, and nothing else. The TOTP
    // secret is the factor and every backup code is a single-use password for
    // the account — neither belongs in a list operators read, and a hash of a
    // backup code is a dictionary of one, so hashes are no better than the
    // codes.
    record_credential(
        &state,
        &audit_actor,
        "user.enable_mfa",
        user_id,
        &headers,
        serde_json::json!({
            "method": "totp",
            "backup_codes_issued": backup_codes.len(),
            // The half a review cannot reconstruct afterwards: the account was
            // already protected, and this entry is where the authenticator that
            // was protecting it stopped working. Same shape as the count above
            // — the journal answers "what stopped working", not only "what
            // started".
            "replaced_existing_factor": user.mfa_enabled,
        }),
    )
    .await;

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
    if user.username != challenge.username || !user.is_usable() {
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
                record_failed_mfa_attempt(&state, &headers, &user, &challenge.auth_provider).await;
            crate::metrics::recorder::auth_event("mfa", "failure");
            crate::metrics::recorder::failed_login("mfa");
            return Err(AppError::unauthorized(if locked {
                "account is temporarily locked"
            } else {
                "invalid backup code"
            }));
        }
    } else {
        // Verify TOTP code
        let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
        let totp_secret = user
            .totp_secret
            .as_ref()
            .ok_or_else(|| AppError::internal("MFA secret missing"))?;

        let secret =
            rg_core::auth::encryption::decrypt(totp_secret, &enc_key).map_err(AppError::from)?;

        // Two statements, not one check: a TOTP code is a pure function of the
        // secret and the clock, so verifying it proves possession but consumes
        // nothing. With `skew = 1` over a 30-second step, one intercepted code
        // used to pass this gate for its whole ~90-second window, and two
        // concurrent requests carrying it were answered two sessions. RFC 6238
        // §5.2 requires the second attempt to be refused, which needs the step
        // the code came from to be *spent* — see `consume_totp_step`.
        //
        // A replay therefore lands in the branch below, indistinguishable from a
        // wrong code: same `401`, same lockout accounting. Telling the two apart
        // would confirm to whoever replayed it that the code was genuine.
        let valid = match rg_core::auth::totp::verify_code_step(&secret, &req.code)
            .map_err(AppError::from)?
        {
            Some(step) => rg_db::ops::user_ops::consume_totp_step(&state.db, user.id, step)
                .await
                .map_err(AppError::from)?,
            None => false,
        };

        if !valid {
            let locked =
                record_failed_mfa_attempt(&state, &headers, &user, &challenge.auth_provider).await;
            crate::metrics::recorder::auth_event("mfa", "failure");
            crate::metrics::recorder::failed_login("mfa");
            return Err(AppError::unauthorized(if locked {
                "account is temporarily locked"
            } else {
                "invalid TOTP code"
            }));
        }
    }

    let user = finalize_mfa_login(&state, &headers, user.id, &challenge.auth_provider).await?;
    crate::metrics::recorder::auth_event("mfa", "success");

    // Issue JWT
    let token = rg_core::auth::jwt::generate_token(
        user.id,
        &user.username,
        user.session_version,
        &state.jwt_secret,
        7,
    )
    .map_err(AppError::from)?;

    // M-4: Set HttpOnly cookie for browser-based auth
    let is_https = crate::public_url::request_is_https(&state, &headers);
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
    AuthUser(user_id): AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
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

/// Re-confirm the account password on a door that sits *behind* a session.
///
/// Shared by every MFA operation whose success would make a stolen session
/// permanent — taking the second factor off, and minting a fresh set of backup
/// codes, which is the same thing by another route. Keeping it in one function
/// is what stops the next such door from being added with the verification but
/// without the lockout: guessing here costs only Argon2 (~50 ms a try), and an
/// attempt that advances no counter leaves neither the audit log nor the admin's
/// view of the account showing a thousand tries.
///
/// `channel` names the door in the attempt record; the strike itself is the
/// account-wide one `POST /users/login`, SSH and `docker login` record.
async fn confirm_account_password(
    state: &AppState,
    user: &rg_db::entities::user::Model,
    password: &str,
    channel: &'static str,
    headers: &HeaderMap,
) -> Result<(), AppError> {
    // `map_err(|_| unauthorized(...))` would answer "invalid password" to a
    // hash the verifier could not use and throw the reason away — the caller
    // here is already authenticated, so that tells a legitimate user their own
    // password is wrong and leaves the operator nothing. Only a genuine
    // mismatch is a 401.
    let password_ok = rg_core::auth::password::verify_password(password, &user.password_hash)
        .await
        .with_context(|| format!("cannot verify the password of user {}", user.id))
        .map_err(AppError::from)?;

    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(headers);
    let attempt = rg_core::auth::lockout::settle_password_attempt(
        &state.db,
        Some(user),
        password_ok,
        rg_core::auth::lockout::AttemptOrigin {
            login: &user.username,
            channel,
            ip_address: ip_address.as_deref(),
            user_agent: user_agent.as_deref(),
        },
    )
    .await
    .map_err(AppError::from)?;

    if let rg_core::auth::lockout::PasswordAttempt::Rejected { locked } = attempt {
        // The caller is authenticated as this very account, so naming the lock
        // leaks nothing — it is what `POST /users/mfa/verify` already answers,
        // and the alternative is telling the owner their password is wrong when
        // it is not.
        return Err(AppError::unauthorized(if locked {
            "account is temporarily locked"
        } else {
            "invalid password"
        }));
    }

    Ok(())
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RegenerateBackupCodesRequest {
    password: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RegenerateBackupCodesResponse {
    backup_codes: Vec<String>,
}

/// POST /users/mfa/backup/regenerate
/// Replace the unused backup codes with a fresh set (requires current password).
///
/// Without this the only way to refresh a spent or leaked printout was to turn
/// MFA off and on again — i.e. to drop the second factor in order to renew the
/// material that exists precisely for when the second factor is unavailable.
#[utoipa::path(
    post,
    path = "/users/mfa/backup/regenerate",
    tag = "MFA",
    request_body = RegenerateBackupCodesRequest,
    responses(
        (status = 200, description = "New backup codes, shown once", body = RegenerateBackupCodesResponse),
        (status = 400, description = "MFA is not enabled for this account"),
        (status = 401, description = "Unauthorized, invalid password, or account temporarily locked"),
        (status = 404, description = "User not found"),
        (status = 500, description = "Internal server error"),
        (status = 503, description = "Password verification is at capacity — retry shortly"),
    ),
)]
pub async fn regenerate_backup_codes(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Json(req): Json<RegenerateBackupCodesRequest>,
) -> Result<Json<RegenerateBackupCodesResponse>, AppError> {
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    if !user.mfa_enabled {
        return Err(AppError::bad_request("MFA is not enabled for this account"));
    }

    confirm_account_password(
        &state,
        &user,
        &req.password,
        "mfa-backup-regenerate",
        &headers,
    )
    .await?;

    let backup_codes = rg_db::ops::mfa_backup_code_ops::generate_codes(
        rg_db::ops::mfa_backup_code_ops::BACKUP_CODE_COUNT,
    );

    let audit_actor = grant_actor(&state, user_id).await?;

    // `set_codes` replaces the unused rows inside one transaction, so the old
    // set stops working exactly when the new one starts — there is no window
    // where both, or neither, are live.
    rg_db::ops::mfa_backup_code_ops::set_codes(&state.db, user_id, &backup_codes)
        .await
        .map_err(AppError::from)?;

    // What makes this event worth a line is not that the codes are new — it is
    // that every code the account was holding stopped working. The count is
    // what lets a review tell "re-issued and filed away" from "re-issued and
    // already spent" when the next entry is a backup-code login.
    record_credential(
        &state,
        &audit_actor,
        "user.regenerate_backup_codes",
        user_id,
        &headers,
        serde_json::json!({ "backup_codes_issued": backup_codes.len() }),
    )
    .await;

    // The one and only time these are ever shown.
    Ok(Json(RegenerateBackupCodesResponse { backup_codes }))
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
        (status = 401, description = "Unauthorized, invalid password, or account temporarily locked"),
        (status = 404, description = "User not found"),
        (status = 500, description = "Internal server error"),
        (status = 503, description = "Password verification is at capacity — retry shortly"),
    ),
)]
pub async fn disable_mfa(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Json(req): Json<DisableMfaRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    // The fourth password door, and the one that is easiest to miss: it sits
    // behind a valid session, so it is not a way *in* — it is the way a stolen
    // session is made permanent. Both the verification and the lockout live in
    // `confirm_account_password`, shared with the backup-code re-issue, so a
    // strike here is the same strike `POST /users/login`, SSH and `docker
    // login` record: the whole account locks, not just this door. Locking the
    // account from a door behind a session grants an attacker no new
    // denial-of-service — anyone who merely knows a username can already trip
    // the same lock from the login form — and a door-local counter would not
    // stop the guessed password from being used everywhere else, which is the
    // point of guessing it.
    confirm_account_password(&state, &user, &req.password, "mfa-disable", &headers).await?;

    let audit_actor = grant_actor(&state, user_id).await?;

    rg_db::ops::user_ops::disable_mfa(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    // The step an account takeover makes right after the stolen password gets
    // in. The journal already had the login; without this line it had nothing
    // between that and everything the attacker did next.
    record_credential(
        &state,
        &audit_actor,
        "user.disable_mfa",
        user_id,
        &headers,
        serde_json::json!({ "method": "totp" }),
    )
    .await;

    Ok(Json(serde_json::json!({"disabled": true})))
}
