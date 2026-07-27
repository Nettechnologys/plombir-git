//! WebAuthn / passkey API endpoints.
//!
//! Registration (authenticated — settings page):
//!   POST /users/passkeys/register/start   — begin enrolment, returns a challenge
//!   POST /users/passkeys/register/finish  — store the new credential
//!   GET  /users/passkeys                   — list the caller's passkeys
//!   DELETE /users/passkeys/{id}            — remove one
//!
//! Login (unauthenticated — passwordless):
//!   POST /users/passkeys/login/start   — begin assertion for a username
//!   POST /users/passkeys/login/finish  — verify assertion, issue a session JWT
//!
//! The in-progress ceremony state is carried between the *start* and *finish*
//! calls in a short-lived, HttpOnly, signed cookie (see
//! [`rg_core::auth::webauthn`]) — ForgeKeep keeps no server-side session store.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::auth::{AuthUser, AUTH_COOKIE_NAME};
use crate::error::AppError;
use crate::AppState;
use rg_core::auth::webauthn as wa;

/// Cookie holding the sealed passkey **registration** ceremony state.
const PASSKEY_REG_COOKIE: &str = "forgekeep_passkey_reg";
/// Cookie holding the sealed passkey **authentication** ceremony state.
const PASSKEY_AUTH_COOKIE: &str = "forgekeep_passkey_auth";
/// Ceremony state lifetime (seconds) — matches the browser dialog timeout.
const CEREMONY_TTL_SECS: i64 = 300;

// ── Cookie helpers ────────────────────────────────────────────────────────

fn is_https_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "https")
        .unwrap_or(false)
}

fn build_state_cookie(name: &str, token: &str, is_https: bool) -> String {
    format!(
        "{}={}; HttpOnly; Path=/; SameSite=Strict; Max-Age={}{}",
        name,
        token,
        CEREMONY_TTL_SECS,
        if is_https { "; Secure" } else { "" }
    )
}

fn clear_state_cookie(name: &str, is_https: bool) -> String {
    format!(
        "{}=; HttpOnly; Path=/; SameSite=Strict; Max-Age=0{}",
        name,
        if is_https { "; Secure" } else { "" }
    )
}

fn build_auth_cookie(token: &str, is_https: bool) -> String {
    format!(
        "{}={}; HttpOnly; Path=/; SameSite=Strict; Max-Age=604800{}",
        AUTH_COOKIE_NAME,
        token,
        if is_https { "; Secure" } else { "" }
    )
}

fn extract_cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|cookie| cookie.strip_prefix(&format!("{}=", name)))
        .filter(|token| !token.is_empty())
}

// ── Relying-party resolution ──────────────────────────────────────────────

/// Resolve the WebAuthn relying-party id and origin for this request.
///
/// Prefers the configured `external_url`; otherwise derives them from the
/// request `Host` header + forwarded scheme. `rp_id` is the host without a port;
/// `origin` is the exact `scheme://host[:port]` the browser will report.
fn resolve_rp(state: &AppState, headers: &HeaderMap) -> Result<(String, String), AppError> {
    if let Some(ext) = state.external_url.as_deref() {
        let url = wa::Url::parse(ext)
            .map_err(|_| AppError::internal("configured external_url is not a valid URL"))?;
        let rp_id = url
            .host_str()
            .ok_or_else(|| AppError::internal("configured external_url has no host"))?
            .to_string();
        let origin = url.origin().ascii_serialization();
        return Ok((rp_id, origin));
    }

    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .filter(|h| !h.is_empty())
        .ok_or_else(|| AppError::bad_request("missing Host header"))?;
    let scheme = if is_https_request(headers) {
        "https"
    } else {
        "http"
    };
    let rp_id = host.split(':').next().unwrap_or(host).to_string();
    let origin = format!("{scheme}://{host}");

    // WebAuthn forbids an IP address as the relying-party id — it must be a
    // registrable domain. Fail with a clear message instead of a raw 500.
    if rp_id.parse::<std::net::IpAddr>().is_ok() {
        return Err(AppError::bad_request(
            "passkeys require a domain name; they cannot be used over a bare IP address",
        ));
    }

    Ok((rp_id, origin))
}

fn webauthn_for(state: &AppState, headers: &HeaderMap) -> Result<wa::Webauthn, AppError> {
    let (rp_id, origin) = resolve_rp(state, headers)?;
    wa::build(&rp_id, &origin).map_err(AppError::from)
}

// ── Sealed ceremony state ─────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct RegState {
    user_id: i64,
    reg: wa::PasskeyRegistration,
}

#[derive(Serialize, Deserialize)]
struct AuthState {
    user_id: i64,
    username: String,
    auth: wa::PasskeyAuthentication,
}

// ── Public response shapes ────────────────────────────────────────────────

/// One registered passkey, as shown in account settings.
#[derive(Serialize, ToSchema)]
pub struct PasskeyInfo {
    pub id: i64,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Wrapper returned by WebAuthn registration start.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyRegisterStartResponse {
    pub public_key: PasskeyCreationOptions,
}

/// WebAuthn credential creation options sent to the browser.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyCreationOptions {
    pub rp: PasskeyRelyingParty,
    pub user: PasskeyUserEntity,
    pub challenge: String,
    pub pub_key_cred_params: Vec<PasskeyCredentialParameter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_credentials: Option<Vec<PasskeyCredentialDescriptor>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authenticator_selection: Option<PasskeyAuthenticatorSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attestation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attestation_formats: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct PasskeyRelyingParty {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyUserEntity {
    pub id: String,
    pub name: String,
    pub display_name: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct PasskeyCredentialParameter {
    #[serde(rename = "type")]
    pub type_: String,
    pub alg: i64,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct PasskeyCredentialDescriptor {
    #[serde(rename = "type")]
    pub type_: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transports: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyAuthenticatorSelection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authenticator_attachment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resident_key: Option<String>,
    pub require_resident_key: bool,
    pub user_verification: String,
}

/// Browser credential returned from `navigator.credentials.create()`.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct PasskeyRegistrationCredential {
    pub id: String,
    #[serde(rename = "rawId")]
    pub raw_id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub response: PasskeyAttestationResponse,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct PasskeyAttestationResponse {
    #[serde(rename = "attestationObject")]
    pub attestation_object: String,
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transports: Option<Vec<String>>,
}

/// Wrapper returned by WebAuthn login start.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyLoginStartResponse {
    pub public_key: PasskeyRequestOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mediation: Option<String>,
}

/// WebAuthn credential request options sent to the browser.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyRequestOptions {
    pub challenge: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
    pub rp_id: String,
    pub allow_credentials: Vec<PasskeyCredentialDescriptor>,
    pub user_verification: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
}

/// Browser credential returned from `navigator.credentials.get()`.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct PasskeyAuthenticationCredential {
    pub id: String,
    #[serde(rename = "rawId")]
    pub raw_id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub response: PasskeyAssertionResponse,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct PasskeyAssertionResponse {
    #[serde(rename = "authenticatorData")]
    pub authenticator_data: String,
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    pub signature: String,
    #[serde(rename = "userHandle", skip_serializing_if = "Option::is_none")]
    pub user_handle: Option<String>,
}

impl From<rg_db::entities::passkey_credential::Model> for PasskeyInfo {
    fn from(m: rg_db::entities::passkey_credential::Model) -> Self {
        Self {
            id: m.id,
            name: m.name,
            created_at: m.created_at,
            last_used_at: m.last_used_at,
        }
    }
}

async fn load_passkeys(
    state: &AppState,
    user_id: i64,
) -> Result<Vec<(rg_db::entities::passkey_credential::Model, wa::Passkey)>, AppError> {
    let rows = rg_db::ops::passkey_credential_ops::list_by_user(&state.db, user_id)
        .await
        .map_err(AppError::from)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match wa::passkey_from_json(&row.passkey) {
            Ok(pk) => out.push((row, pk)),
            Err(error) => {
                tracing::warn!(passkey_id = row.id, error = %format!("{error:#}"), "skipping corrupt stored passkey");
            }
        }
    }
    Ok(out)
}

// ── Registration ──────────────────────────────────────────────────────────

/// POST /users/passkeys/register/start
#[utoipa::path(
    post,
    path = "/users/passkeys/register/start",
    tag = "Passkeys",
    responses(
        (status = 200, description = "Registration challenge issued", body = PasskeyRegisterStartResponse),
        (status = 400, description = "Invalid WebAuthn relying-party configuration", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = serde_json::Value),
        (status = 404, description = "User not found", body = serde_json::Value),
    ),
)]
pub async fn register_start(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    let webauthn = webauthn_for(&state, &headers)?;
    let exclude = load_passkeys(&state, user_id)
        .await?
        .iter()
        .map(|(_, pk)| pk.cred_id().clone())
        .collect();

    let display = user
        .display_name
        .clone()
        .unwrap_or_else(|| user.username.clone());
    let (ccr, reg) = wa::start_registration(&webauthn, user_id, &user.username, &display, exclude)
        .map_err(AppError::from)?;

    let token = wa::seal_state(
        RegState { user_id, reg },
        "reg",
        &state.jwt_secret,
        CEREMONY_TTL_SECS,
    )
    .map_err(AppError::from)?;

    let is_https = is_https_request(&headers);
    Ok((
        StatusCode::OK,
        [(
            axum::http::header::SET_COOKIE,
            build_state_cookie(PASSKEY_REG_COOKIE, &token, is_https),
        )],
        Json(ccr),
    ))
}

/// POST /users/passkeys/register/finish
#[derive(Deserialize, ToSchema)]
pub struct RegisterFinishRequest {
    /// User-supplied label for the authenticator.
    #[serde(default)]
    pub name: String,
    /// The browser's `navigator.credentials.create()` result.
    #[schema(value_type = PasskeyRegistrationCredential)]
    pub credential: wa::RegisterPublicKeyCredential,
}

/// POST /users/passkeys/register/finish
#[utoipa::path(
    post,
    path = "/users/passkeys/register/finish",
    tag = "Passkeys",
    request_body = RegisterFinishRequest,
    responses(
        (status = 200, description = "Passkey registered", body = Vec<PasskeyInfo>),
        (status = 400, description = "Missing, expired, or invalid registration challenge", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = serde_json::Value),
        (status = 409, description = "Passkey already registered", body = serde_json::Value),
    ),
)]
pub async fn register_finish(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Json(req): Json<RegisterFinishRequest>,
) -> Result<impl IntoResponse, AppError> {
    let sealed = extract_cookie(&headers, PASSKEY_REG_COOKIE)
        .and_then(|t| wa::unseal_state::<RegState>(t, "reg", &state.jwt_secret))
        .ok_or_else(|| {
            AppError::bad_request("passkey registration challenge missing or expired")
        })?;
    if sealed.user_id != user_id {
        return Err(AppError::unauthorized(
            "registration challenge does not match session",
        ));
    }

    let webauthn = webauthn_for(&state, &headers)?;
    let passkey =
        wa::finish_registration(&webauthn, &req.credential, &sealed.reg).map_err(|error| {
            tracing::warn!(user_id, error = %format!("{error:#}"), "passkey registration verification failed");
            AppError::bad_request("passkey registration could not be verified")
        })?;

    let credential_id = wa::credential_id_b64(passkey.cred_id());
    let passkey_json = wa::passkey_to_json(&passkey).map_err(AppError::from)?;
    let name = sanitize_name(&req.name);

    rg_db::ops::passkey_credential_ops::create(
        &state.db,
        user_id,
        &credential_id,
        &passkey_json,
        &name,
    )
    .await
    .map_err(|error| {
        tracing::warn!(user_id, error = %format!("{error:#}"), "failed to store passkey (possible duplicate)");
        AppError::conflict("this passkey is already registered")
    })?;

    let is_https = is_https_request(&headers);
    let passkeys: Vec<PasskeyInfo> = load_passkeys(&state, user_id)
        .await?
        .into_iter()
        .map(|(m, _)| m.into())
        .collect();

    Ok((
        StatusCode::OK,
        [(
            axum::http::header::SET_COOKIE,
            clear_state_cookie(PASSKEY_REG_COOKIE, is_https),
        )],
        Json(passkeys),
    ))
}

/// Trim, cap, and default the user-supplied passkey label.
fn sanitize_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Passkey".to_string();
    }
    trimmed.chars().take(64).collect()
}

/// GET /users/passkeys
#[utoipa::path(
    get,
    path = "/users/passkeys",
    tag = "Passkeys",
    responses(
        (status = 200, description = "Registered passkeys", body = Vec<PasskeyInfo>),
        (status = 401, description = "Authentication required", body = serde_json::Value),
    ),
)]
pub async fn list_passkeys(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> Result<Json<Vec<PasskeyInfo>>, AppError> {
    let rows = rg_db::ops::passkey_credential_ops::list_by_user(&state.db, user_id)
        .await
        .map_err(AppError::from)?;
    Ok(Json(rows.into_iter().map(PasskeyInfo::from).collect()))
}

/// DELETE /users/passkeys/{id}
#[utoipa::path(
    delete,
    path = "/users/passkeys/{id}",
    tag = "Passkeys",
    params(
        ("id" = i64, Path, description = "Passkey id"),
    ),
    responses(
        (status = 204, description = "Passkey deleted"),
        (status = 401, description = "Authentication required", body = serde_json::Value),
        (status = 404, description = "Passkey not found", body = serde_json::Value),
    ),
)]
pub async fn delete_passkey(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let removed = rg_db::ops::passkey_credential_ops::delete(&state.db, user_id, id)
        .await
        .map_err(AppError::from)?;
    if !removed {
        return Err(AppError::not_found("passkey not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── Login (passwordless) ──────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct LoginStartRequest {
    pub username: String,
}

/// POST /users/passkeys/login/start
#[utoipa::path(
    post,
    path = "/users/passkeys/login/start",
    tag = "Passkeys",
    request_body = LoginStartRequest,
    responses(
        (status = 200, description = "Login challenge issued", body = PasskeyLoginStartResponse),
        (status = 400, description = "No registered passkey or invalid WebAuthn relying-party configuration", body = serde_json::Value),
    ),
)]
pub async fn login_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LoginStartRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Uniform error for "unknown user" and "no passkeys" to limit enumeration.
    let no_passkey = || AppError::bad_request("no passkey is registered for this account");

    let user = rg_db::ops::user_ops::find_by_username(&state.db, &req.username)
        .await
        .map_err(AppError::from)?
        .filter(|u| u.is_usable())
        .ok_or_else(no_passkey)?;

    let passkeys: Vec<wa::Passkey> = load_passkeys(&state, user.id)
        .await?
        .into_iter()
        .map(|(_, pk)| pk)
        .collect();
    if passkeys.is_empty() {
        return Err(no_passkey());
    }

    let webauthn = webauthn_for(&state, &headers)?;
    let (rcr, auth) = wa::start_authentication(&webauthn, &passkeys).map_err(AppError::from)?;

    let token = wa::seal_state(
        AuthState {
            user_id: user.id,
            username: user.username.clone(),
            auth,
        },
        "auth",
        &state.jwt_secret,
        CEREMONY_TTL_SECS,
    )
    .map_err(AppError::from)?;

    let is_https = is_https_request(&headers);
    Ok((
        StatusCode::OK,
        [(
            axum::http::header::SET_COOKIE,
            build_state_cookie(PASSKEY_AUTH_COOKIE, &token, is_https),
        )],
        Json(rcr),
    ))
}

#[derive(Serialize, ToSchema)]
pub struct PasskeyLoginResponse {
    pub token: String,
    pub user_id: i64,
    pub username: String,
}

/// POST /users/passkeys/login/finish
///
/// A successful passkey assertion is phishing-resistant strong authentication,
/// so it completes login on its own — it is not gated behind the TOTP second
/// factor.
#[utoipa::path(
    post,
    path = "/users/passkeys/login/finish",
    tag = "Passkeys",
    request_body = PasskeyAuthenticationCredential,
    responses(
        (status = 200, description = "Login successful", body = PasskeyLoginResponse),
        (status = 401, description = "Missing challenge, invalid credential, or locked account", body = serde_json::Value),
    ),
)]
pub async fn login_finish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(credential): Json<wa::PublicKeyCredential>,
) -> Result<impl IntoResponse, AppError> {
    let sealed = extract_cookie(&headers, PASSKEY_AUTH_COOKIE)
        .and_then(|t| wa::unseal_state::<AuthState>(t, "auth", &state.jwt_secret))
        .ok_or_else(|| AppError::unauthorized("passkey login challenge missing or expired"))?;

    let user = rg_db::ops::user_ops::find_by_id(&state.db, sealed.user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::unauthorized("invalid credentials"))?;
    if user.username != sealed.username || !user.is_usable() {
        return Err(AppError::unauthorized("invalid credentials"));
    }
    if user
        .locked_until
        .is_some_and(|locked_until| locked_until > chrono::Utc::now())
    {
        return Err(AppError::unauthorized("account is temporarily locked"));
    }

    let webauthn = webauthn_for(&state, &headers)?;
    let result = wa::finish_authentication(&webauthn, &credential, &sealed.auth).map_err(|error| {
        tracing::warn!(user_id = user.id, error = %format!("{error:#}"), "passkey authentication verification failed");
        AppError::unauthorized("passkey authentication failed")
    })?;

    // Advance the stored signature counter for the credential that just signed.
    let matched_id = wa::credential_id_b64(result.cred_id());
    if let Some((model, mut passkey)) = load_passkeys(&state, user.id)
        .await?
        .into_iter()
        .find(|(m, _)| m.credential_id == matched_id)
    {
        passkey.update_credential(&result);
        let json = wa::passkey_to_json(&passkey).unwrap_or(model.passkey.clone());
        if let Err(error) =
            rg_db::ops::passkey_credential_ops::touch_and_update(&state.db, model.id, &json).await
        {
            tracing::warn!(user_id = user.id, error = %format!("{error:#}"), "failed to update passkey after login");
        }
    }

    // Record the successful login the same way the MFA path does.
    if let Err(error) = rg_db::ops::user_ops::record_successful_login(&state.db, user.id).await {
        tracing::warn!(
            user_id = user.id,
            error = %format!("{error:#}"),
            "failed to update login state after passkey login"
        );
    }
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(&headers);
    if let Err(error) = rg_db::ops::login_log_ops::log_attempt(
        &state.db,
        Some(user.id),
        &user.username,
        "passkey",
        ip_address.as_deref(),
        user_agent.as_deref(),
        true,
        None,
    )
    .await
    {
        tracing::warn!(user_id = user.id, error = %format!("{error:#}"), "failed to record passkey login attempt");
    }

    let token = rg_core::auth::jwt::generate_token(user.id, &user.username, &state.jwt_secret, 7)
        .map_err(AppError::from)?;

    let is_https = is_https_request(&headers);
    let mut response = (
        StatusCode::OK,
        [(
            axum::http::header::SET_COOKIE,
            build_auth_cookie(&token, is_https),
        )],
        Json(PasskeyLoginResponse {
            token,
            user_id: user.id,
            username: user.username,
        }),
    )
        .into_response();
    if let Ok(value) =
        axum::http::HeaderValue::from_str(&clear_state_cookie(PASSKEY_AUTH_COOKIE, is_https))
    {
        response
            .headers_mut()
            .append(axum::http::header::SET_COOKIE, value);
    }
    Ok(response)
}
