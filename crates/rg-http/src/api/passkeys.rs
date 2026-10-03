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
//! [`rg_core::auth::webauthn`]) — Plombir Git keeps no server-side session store.
//! The one thing the server does keep is that a ceremony's challenge has been
//! answered (`rg_db::ops::webauthn_ceremony_ops::spend`), because single use is
//! not a property a signature can carry and both *finish* calls depend on it.

use std::collections::BTreeSet;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::access_audit::{grant_actor, record_credential};
use crate::api::auth::{AuthUser, SessionUser, AUTH_COOKIE_NAME};
use crate::error::AppError;
use crate::AppState;
use rg_core::auth::webauthn as wa;

/// Cookie holding the sealed passkey **registration** ceremony state.
const PASSKEY_REG_COOKIE: &str = "plombir_git_passkey_reg";
/// Cookie holding the sealed passkey **authentication** ceremony state.
const PASSKEY_AUTH_COOKIE: &str = "plombir_git_passkey_auth";
/// Ceremony state lifetime (seconds) — matches the browser dialog timeout.
const CEREMONY_TTL_SECS: i64 = 300;
/// How long a spent ceremony stays on record.
///
/// It has to outlast the last instant its cookie can still be unsealed, or the
/// record is dropped while the challenge it refuses is still live and the
/// replay window reopens. That is longer than [`CEREMONY_TTL_SECS`]:
/// `jsonwebtoken`'s default validation allows 60 seconds of leeway past `exp`,
/// and two instances of Plombir Git need not agree on the clock to the second.
/// Three minutes of margin costs a table that holds a few more minutes of
/// ceremonies; being short by one second costs the property.
const CEREMONY_SPEND_RETENTION_SECS: i64 = CEREMONY_TTL_SECS + 180;

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

/// The relying-party identity under which one WebAuthn ceremony runs.
///
/// Both fields are sealed into the short-lived ceremony state. `rp_id` is the
/// durable credential boundary; `origin` is also bound because WebAuthn checks
/// it while completing the ceremony.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct RelyingParty {
    rp_id: String,
    origin: String,
}

/// Resolve the WebAuthn relying-party id and origin for this request.
///
/// Prefers the configured `external_url`; otherwise derives them from the
/// request `Host` header + forwarded scheme. `rp_id` is the host without a port;
/// `origin` is the exact `scheme://host[:port]` the browser will report.
fn resolve_rp(state: &AppState, headers: &HeaderMap) -> Result<RelyingParty, AppError> {
    if let Some(ext) = state.external_url.as_deref() {
        let url = wa::Url::parse(ext)
            .map_err(|_| AppError::internal("configured external_url is not a valid URL"))?;
        let rp_id = url
            .host_str()
            .ok_or_else(|| AppError::internal("configured external_url has no host"))?
            .to_string();
        let origin = url.origin().ascii_serialization();
        return Ok(RelyingParty { rp_id, origin });
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

    Ok(RelyingParty { rp_id, origin })
}

fn webauthn_for(rp: &RelyingParty) -> Result<wa::Webauthn, AppError> {
    wa::build(&rp.rp_id, &rp.origin).map_err(AppError::from)
}

/// Refuse a start/finish pair whose public URL changed mid-ceremony.
///
/// A different hostname means a different credential namespace, while a
/// different origin under the same hostname also fails WebAuthn verification.
/// Naming this before the cryptographic check keeps an operator from chasing a
/// phantom "invalid credential" after a proxy or `external_url` change.
fn require_same_ceremony_rp(
    ceremony: &str,
    started: &RelyingParty,
    current: &RelyingParty,
) -> Result<(), AppError> {
    if started == current {
        return Ok(());
    }

    tracing::warn!(
        ceremony,
        started_rp_id = %started.rp_id,
        current_rp_id = %current.rp_id,
        started_origin = %started.origin,
        current_origin = %current.origin,
        "passkey ceremony changed relying party between start and finish"
    );
    Err(AppError::bad_request(format!(
        "passkey {ceremony} must finish at the same external URL where it started; restart it at {}",
        started.origin
    )))
}

// ── Sealed ceremony state ─────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct RegState {
    user_id: i64,
    /// Identifies this ceremony so its challenge can be spent exactly once.
    ///
    /// No `serde(default)`: a cookie issued before this field existed simply
    /// stops unsealing, which costs an in-flight ceremony a restart during a
    /// deploy and never leaves one running with nothing to spend.
    ceremony_id: String,
    rp: RelyingParty,
    reg: wa::PasskeyRegistration,
}

#[derive(Serialize, Deserialize)]
struct AuthState {
    user_id: i64,
    username: String,
    /// Identifies this ceremony so its challenge can be spent exactly once.
    /// See [`RegState::ceremony_id`].
    ceremony_id: String,
    rp: RelyingParty,
    auth: wa::PasskeyAuthentication,
}

/// Spend the challenge of the ceremony that just verified, answering whether
/// this request is the one that spent it.
///
/// A WebAuthn challenge is single-use, and a signed cookie cannot carry that on
/// its own: signature and `exp` say "we issued this, and it is still young",
/// never "it has already been answered". Without the spend, one intercepted
/// `finish` request — the ceremony cookie and the assertion body, exactly the
/// pair a phishing proxy sees — was replayable for the full
/// `CEREMONY_TTL_SECS`, and the signature-counter compare-and-swap does not
/// cover it: a platform passkey reporting `signCount = 0` re-stores a
/// byte-identical credential, which is an honest `Stored`.
///
/// Called after the assertion or attestation has verified, so a request that
/// proves nothing cannot burn a live ceremony, and before anything is issued or
/// stored, so nothing outlives a refusal.
/// The refusal is indistinguishable from the one a bad signature gets, but the
/// server does not have to be equally uninformed: a second `finish` for a
/// ceremony answered four minutes ago is a replay, and one arriving a second
/// after the first is a client that retried. `first_spent_at` is what separates
/// them, and this warning is the only place it is ever read — deliberately not
/// the response, where it would confirm to whoever is replaying that the
/// ceremony they intercepted was real (card_b70de2169bd6).
async fn spend_ceremony(state: &AppState, ceremony_id: &str) -> Result<bool, AppError> {
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(CEREMONY_SPEND_RETENTION_SECS);
    let outcome = rg_db::ops::webauthn_ceremony_ops::spend(&state.db, ceremony_id, expires_at)
        .await
        .map_err(AppError::from)?;
    if let rg_db::ops::webauthn_ceremony_ops::SpendOutcome::AlreadySpent { first_spent_at } =
        outcome
    {
        tracing::warn!(
            ceremony_id,
            first_spent_at = %first_spent_at,
            age_seconds = (chrono::Utc::now() - first_spent_at).num_seconds(),
            "a passkey ceremony was presented again after it had already been answered; the \
             request is refused exactly as a bad signature would be"
        );
    }
    Ok(outcome.is_spent())
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

/// Stored credentials partitioned by the RP that the caller is using now.
struct PasskeyRowsForRp {
    matching: Vec<rg_db::entities::passkey_credential::Model>,
    different_rp_ids: BTreeSet<String>,
}

/// Keep legacy rows selectable, but never treat a recorded different RP as a
/// candidate. A legacy `NULL` means the pre-migration database genuinely does
/// not know the RP, so rejecting it would needlessly lock out users who still
/// arrive through their original hostname.
fn select_passkey_rows_for_rp(
    rows: Vec<rg_db::entities::passkey_credential::Model>,
    rp_id: &str,
) -> PasskeyRowsForRp {
    let different_rp_ids = rows
        .iter()
        .filter_map(|row| row.rp_id.as_ref())
        .filter(|stored_rp_id| stored_rp_id.as_str() != rp_id)
        .cloned()
        .collect();
    let matching = rows
        .into_iter()
        .filter(|row| match row.rp_id.as_deref() {
            Some(stored_rp_id) => stored_rp_id == rp_id,
            None => true,
        })
        .collect();
    PasskeyRowsForRp {
        matching,
        different_rp_ids,
    }
}

struct LoadedPasskeysForRp {
    passkeys: Vec<(rg_db::entities::passkey_credential::Model, wa::Passkey)>,
    different_rp_ids: BTreeSet<String>,
}

async fn load_passkeys_for_rp(
    state: &AppState,
    user_id: i64,
    rp_id: &str,
) -> Result<LoadedPasskeysForRp, AppError> {
    let rows = rg_db::ops::passkey_credential_ops::list_by_user(&state.db, user_id)
        .await
        .map_err(AppError::from)?;
    let selected = select_passkey_rows_for_rp(rows, rp_id);
    let mut passkeys = Vec::with_capacity(selected.matching.len());
    for row in selected.matching {
        match wa::passkey_from_json(&row.passkey) {
            Ok(pk) => passkeys.push((row, pk)),
            Err(error) => {
                tracing::warn!(passkey_id = row.id, error = %format!("{error:#}"), "skipping corrupt stored passkey");
            }
        }
    }
    Ok(LoadedPasskeysForRp {
        passkeys,
        different_rp_ids: selected.different_rp_ids,
    })
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
        (status = 403, description = "A login session is required to create credentials", body = serde_json::Value),
        (status = 404, description = "User not found", body = serde_json::Value),
    ),
)]
pub async fn register_start(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))?;

    let rp = resolve_rp(&state, &headers)?;
    let webauthn = webauthn_for(&rp)?;
    let exclude = load_passkeys_for_rp(&state, user_id, &rp.rp_id)
        .await?
        .passkeys
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
        RegState {
            user_id,
            ceremony_id: wa::new_ceremony_id(),
            rp,
            reg,
        },
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
        (status = 400, description = "Missing, expired, already-answered, or invalid registration challenge", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = serde_json::Value),
        (status = 403, description = "A login session is required to create credentials", body = serde_json::Value),
        (status = 409, description = "Passkey already registered", body = serde_json::Value),
    ),
)]
pub async fn register_finish(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
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

    let current_rp = resolve_rp(&state, &headers)?;
    require_same_ceremony_rp("registration", &sealed.rp, &current_rp)?;
    let webauthn = webauthn_for(&sealed.rp)?;
    let passkey =
        wa::finish_registration(&webauthn, &req.credential, &sealed.reg).map_err(|error| {
            tracing::warn!(user_id, error = %format!("{error:#}"), "passkey registration verification failed");
            AppError::bad_request("passkey registration could not be verified")
        })?;

    // The attestation verified; this challenge is now spent. Registration is
    // the cheaper half of the class — a replayed enrolment collides on
    // `credential_id` — but the window is the same 300 seconds, and the refusal
    // has to be the one a bad attestation gets, so a replay learns nothing
    // about whether the material it carried was genuine.
    if !spend_ceremony(&state, &sealed.ceremony_id).await? {
        tracing::warn!(
            user_id,
            "passkey registration challenge had already been answered; refusing the replay"
        );
        return Err(AppError::bad_request(
            "passkey registration could not be verified",
        ));
    }

    let credential_id = wa::credential_id_b64(passkey.cred_id());
    let passkey_json = wa::passkey_to_json(&passkey).map_err(AppError::from)?;
    let name = sanitize_name(&req.name);

    let audit_actor = grant_actor(&state, user_id).await?;

    rg_db::ops::passkey_credential_ops::create(
        &state.db,
        user_id,
        &credential_id,
        &passkey_json,
        &name,
        &sealed.rp.rp_id,
    )
    .await
    .map_err(|error| {
        tracing::warn!(user_id, error = %format!("{error:#}"), "failed to store passkey");
        passkey_create_error(error)
    })?;

    // A passkey is a way in that needs no password, so its appearance is the
    // same kind of event as an SSH key's. The label is chosen by whoever
    // enrolled it and identifies nothing on its own; the head of the credential
    // id is what lets a review match this entry against the row and against the
    // logins that follow. The serialized `Passkey` — public key and all — stays
    // out: a journal is read, not parsed.
    record_credential(
        &state,
        &audit_actor,
        "user.add_passkey",
        user_id,
        &headers,
        serde_json::json!({
            "name": name,
            "credential_id_prefix": credential_id_prefix(&credential_id),
            "rp_id": sealed.rp.rp_id,
        }),
    )
    .await;

    let is_https = is_https_request(&headers);
    let passkeys: Vec<PasskeyInfo> =
        rg_db::ops::passkey_credential_ops::list_by_user(&state.db, user_id)
            .await?
            .into_iter()
            .map(PasskeyInfo::from)
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

/// The head of a credential id, which is what a journal entry may carry.
///
/// Enough to match an entry against a row or against a later login, and not the
/// whole identifier: the id is not a secret, but the entry is about the event,
/// and a wall of base64 is what makes a journal unread.
fn credential_id_prefix(credential_id: &str) -> String {
    credential_id.chars().take(12).collect()
}

/// Preserve the conflict contract only for the database constraint that proves
/// another registration already owns this credential id.
fn passkey_create_error(error: rg_db::sea_orm::DbErr) -> AppError {
    if rg_db::is_unique_violation(&error) {
        AppError::conflict("this passkey is already registered")
    } else {
        AppError::from(error)
    }
}

/// Name the two deployment states in which a browser can appear to "lose" a
/// passkey before it ever sends an assertion to the server.
///
/// This is intentionally a warning rather than a startup failure. A legacy
/// row has no recoverable RP provenance, and an instance can still serve it at
/// its original host; refusing to start would turn a diagnostic upgrade into a
/// lockout.
pub(crate) async fn warn_about_rp_configuration(
    db: &rg_db::DatabaseConnection,
    external_url: Option<&str>,
) -> Result<(), rg_db::sea_orm::DbErr> {
    let registered = rg_db::ops::passkey_credential_ops::count_all(db).await?;
    if registered == 0 {
        return Ok(());
    }

    if external_url.is_none() {
        tracing::warn!(
            registered_passkey_count = registered,
            "passkeys are registered but [server].external_url is unset; WebAuthn derives its relying-party id from each request Host, so a different hostname will not expose the credential. Set external_url to the canonical browser URL before enrolling more passkeys"
        );
    }

    let legacy = rg_db::ops::passkey_credential_ops::count_legacy_without_rp_id(db).await?;
    if legacy > 0 {
        tracing::warn!(
            legacy_passkey_count = legacy,
            "passkeys created before relying-party ids were persisted have unknown RP provenance; they remain eligible for backward compatibility, but configure external_url and re-enrol them at the canonical host"
        );
    }
    Ok(())
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
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    // Read before the delete, because after it the row is gone and `#id` alone
    // says nothing: whoever reads the journal wants to know *which*
    // authenticator stopped working, and the id is not reissued to answer them.
    let removed_key = rg_db::ops::passkey_credential_ops::list_by_user(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .into_iter()
        .find(|key| key.id == id);

    let audit_actor = grant_actor(&state, user_id).await?;

    let removed = rg_db::ops::passkey_credential_ops::delete(&state.db, user_id, id)
        .await
        .map_err(AppError::from)?;
    if !removed {
        return Err(AppError::not_found("passkey not found"));
    }

    // The lookup above and the delete are separate statements, so a concurrent
    // request can take the row in between. This one owns the deletion — the
    // delete reported it — and the entry says what it could still read rather
    // than nothing at all.
    record_credential(
        &state,
        &audit_actor,
        "user.remove_passkey",
        user_id,
        &headers,
        serde_json::json!({
            "passkey_id": id,
            "name": removed_key.as_ref().map(|key| key.name.clone()),
            "credential_id_prefix": removed_key
                .as_ref()
                .map(|key| credential_id_prefix(&key.credential_id)),
        }),
    )
    .await;

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

    let rp = resolve_rp(&state, &headers)?;
    let selected = load_passkeys_for_rp(&state, user.id, &rp.rp_id).await?;
    let passkeys: Vec<wa::Passkey> = selected.passkeys.into_iter().map(|(_, pk)| pk).collect();
    if passkeys.is_empty() {
        if !selected.different_rp_ids.is_empty() {
            tracing::warn!(
                user_id = user.id,
                current_rp_id = %rp.rp_id,
                registered_rp_ids = ?selected.different_rp_ids,
                "passkey login has credentials for a different relying-party id; they were intentionally excluded"
            );
        }
        return Err(no_passkey());
    }

    let webauthn = webauthn_for(&rp)?;
    let (rcr, auth) = wa::start_authentication(&webauthn, &passkeys).map_err(AppError::from)?;

    let token = wa::seal_state(
        AuthState {
            user_id: user.id,
            username: user.username.clone(),
            ceremony_id: wa::new_ceremony_id(),
            rp,
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
        (status = 401, description = "Missing, expired, or already-answered challenge, invalid credential, or locked account", body = serde_json::Value),
        (status = 409, description = "Another assertion advanced this credential first; retry the login", body = serde_json::Value),
        (status = 500, description = "The advanced signature counter could not be serialized or stored", body = serde_json::Value),
        (status = 503, description = "The advanced signature counter could not be stored: the database is unreachable", body = serde_json::Value),
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

    let current_rp = resolve_rp(&state, &headers)?;
    require_same_ceremony_rp("login", &sealed.rp, &current_rp)?;
    let webauthn = webauthn_for(&sealed.rp)?;
    let result = wa::finish_authentication(&webauthn, &credential, &sealed.auth).map_err(|error| {
        tracing::warn!(user_id = user.id, error = %format!("{error:#}"), "passkey authentication verification failed");
        AppError::unauthorized("passkey authentication failed")
    })?;

    // The assertion verified, so this ceremony's challenge is spent here — once,
    // by whichever request gets there first. Everything below issues or mutates
    // something, and none of it may happen twice for one challenge.
    //
    // Deliberately the same `401 passkey authentication failed` a bad signature
    // gets: a distinct status would tell whoever is replaying an intercepted
    // request that the cookie and assertion they hold were real.
    if !spend_ceremony(&state, &sealed.ceremony_id).await? {
        tracing::warn!(
            user_id = user.id,
            "passkey login challenge had already been answered; refusing the replay"
        );
        return Err(AppError::unauthorized("passkey authentication failed"));
    }

    // Advance the stored signature counter for the credential that just signed.
    //
    // This is ceremony material, not bookkeeping. The counter and backup state
    // the authenticator just reported are exactly what the *next* assertion is
    // checked against to detect a cloned or replayed credential, so a login
    // whose new counter we could not store is an incomplete ceremony. Every
    // failure below therefore fails the login: issuing a token for state we
    // did not keep is the one outcome that must not happen.
    let matched_id = wa::credential_id_b64(result.cred_id());
    let (model, mut passkey) = load_passkeys_for_rp(&state, user.id, &sealed.rp.rp_id)
        .await?
        .passkeys
        .into_iter()
        .find(|(m, _)| m.credential_id == matched_id)
        .ok_or_else(|| {
            // The assertion verified against the challenge's allow-list, so the
            // credential was registered when the ceremony began. It is gone now:
            // revoked mid-flight, or its stored blob no longer parses (see
            // `load_passkeys`). Either way it is no longer a credential this
            // account can be signed in with.
            tracing::warn!(
                user_id = user.id,
                "passkey assertion verified against a credential that is no longer usable"
            );
            AppError::unauthorized("passkey authentication failed")
        })?;
    if passkey.update_credential(&result).is_none() {
        // `update_credential` answers `None` only when the credential id inside
        // the stored blob differs from the one that signed — which the row we
        // just matched by `credential_id` says it should not. The column and
        // the blob have diverged, and the counter we would write back is not
        // the one that just advanced.
        tracing::error!(
            user_id = user.id,
            passkey_id = model.id,
            "stored passkey does not carry the credential id its row is indexed by"
        );
        return Err(AppError::internal(
            "stored passkey diverges from its credential id",
        ));
    }
    // No fallback to the row's previous JSON here: re-storing that would write
    // back the *pre-assertion* counter and report success for it.
    let json = wa::passkey_to_json(&passkey).map_err(AppError::from)?;
    // Compare-and-swap against the exact blob this assertion was verified
    // against. Two logins that both verified against one stored credential must
    // not both be answered success: the loser's write would put its own — and
    // possibly lower — signature counter back over the winner's, which is the
    // material the *next* assertion uses to spot a clone.
    let stored = rg_db::ops::passkey_credential_ops::touch_and_update(
        &state.db,
        model.id,
        &model.passkey,
        &json,
    )
    .await
    .map_err(AppError::from)?;
    match stored {
        rg_db::ops::passkey_credential_ops::CounterWrite::Stored => {}
        rg_db::ops::passkey_credential_ops::CounterWrite::Missing => {
            tracing::warn!(
                user_id = user.id,
                passkey_id = model.id,
                "passkey row disappeared before its advanced counter could be stored"
            );
            return Err(AppError::unauthorized("passkey authentication failed"));
        }
        rg_db::ops::passkey_credential_ops::CounterWrite::Conflict => {
            // The assertion was genuine — the holder of the authenticator is
            // told to try again, not that it failed — but this ceremony ends
            // here: its counter is not the stored one, and issuing a token now
            // would be issuing it for state the server did not keep.
            tracing::warn!(
                user_id = user.id,
                passkey_id = model.id,
                "a concurrent assertion advanced this passkey first; refusing to roll its counter back"
            );
            return Err(AppError::conflict(
                "another sign-in advanced this passkey at the same time; please try again",
            ));
        }
    }

    // This is the lifecycle linearization point after the assertion and its
    // counter were stored. Retirement or physical deletion is an authentication
    // rejection, while an unavailable write is a server error; neither may be
    // followed by a success log or a token.
    let user = crate::api::users::finalized_login_user(
        user.id,
        rg_db::ops::user_ops::record_successful_login(&state.db, user.id).await,
    )?;
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

    let token = rg_core::auth::jwt::generate_token(
        user.id,
        &user.username,
        user.session_version,
        &state.jwt_secret,
        7,
    )
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

#[cfg(test)]
mod tests {
    use super::{
        passkey_create_error, require_same_ceremony_rp, select_passkey_rows_for_rp, AppError,
        RelyingParty,
    };
    use axum::http::StatusCode;
    use rg_db::entities::passkey_credential;
    use rg_db::sea_orm::{
        ConnAcquireErr, ConnectOptions, ConnectionTrait, Database, DatabaseBackend, DbErr,
        Statement,
    };

    /// The actual UNIQUE error SQLite raises for a duplicate credential must
    /// keep the public 409 contract of `register_finish`.
    #[tokio::test]
    async fn duplicate_passkey_credential_is_a_conflict() {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options)
            .await
            .expect("connect in-memory database");
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "CREATE TABLE passkey_credentials (credential_id TEXT NOT NULL UNIQUE)",
        ))
        .await
        .expect("create passkey table");
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "INSERT INTO passkey_credentials (credential_id) VALUES ('credential-id')",
        ))
        .await
        .expect("store first credential");

        let error = db
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                "INSERT INTO passkey_credentials (credential_id) VALUES ('credential-id')",
            ))
            .await
            .expect_err("duplicate credential must violate UNIQUE");

        let error = passkey_create_error(error);
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert!(matches!(
            error,
            AppError::Conflict(message) if message == "this passkey is already registered"
        ));
    }

    /// A failed write is not evidence of a duplicate. In particular, the pool
    /// outage shape returned by `register_finish` must remain retryable.
    #[test]
    fn database_outage_is_not_a_duplicate_conflict() {
        let error = passkey_create_error(DbErr::ConnectionAcquire(ConnAcquireErr::Timeout));
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    fn passkey_row(id: i64, rp_id: Option<&str>) -> passkey_credential::Model {
        passkey_credential::Model {
            id,
            user_id: 1,
            credential_id: format!("credential-{id}"),
            passkey: "{}".to_string(),
            name: "Passkey".to_string(),
            rp_id: rp_id.map(str::to_string),
            created_at: chrono::Utc::now(),
            last_used_at: None,
        }
    }

    /// A credential recorded for one Host must never enter a challenge for a
    /// different Host. A pre-RP row remains eligible only because its original
    /// Host is unknowable after the fact; startup emits an operator warning for
    /// that migration state.
    #[test]
    fn stored_passkeys_are_scoped_to_their_relying_party_id() {
        let selected = select_passkey_rows_for_rp(
            vec![
                passkey_row(1, Some("git.example.test")),
                passkey_row(2, Some("internal.example.test")),
                passkey_row(3, None),
            ],
            "git.example.test",
        );

        assert_eq!(
            selected
                .matching
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![1, 3],
            "a differently bound credential must not be offered to WebAuthn"
        );
        assert_eq!(
            selected.different_rp_ids.into_iter().collect::<Vec<_>>(),
            vec!["internal.example.test"],
            "the login path needs the actual conflicting RP for its warning"
        );
    }

    #[test]
    fn a_ceremony_host_change_is_named_before_webauthn_verification() {
        let error = require_same_ceremony_rp(
            "login",
            &RelyingParty {
                rp_id: "git.example.test".to_string(),
                origin: "https://git.example.test".to_string(),
            },
            &RelyingParty {
                rp_id: "internal.example.test".to_string(),
                origin: "https://internal.example.test".to_string(),
            },
        )
        .expect_err("a ceremony may not switch its relying party");

        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert!(
            error.to_string().contains("same external URL")
                && error.to_string().contains("https://git.example.test"),
            "the client must learn that the Host changed instead of seeing an invalid credential"
        );
    }
}
