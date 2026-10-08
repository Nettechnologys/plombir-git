//! SSO (Single Sign-On) API endpoints.
//!
//! Endpoints:
//!   GET  /auth/sso/providers                — List enabled SSO providers
//!   GET  /auth/sso/{slug}                    — Redirect to provider's auth page
//!   GET  /auth/sso/{slug}/callback           — OAuth2/OIDC callback
//!   POST /auth/sso/{slug}/link               — Start linking a provider to the signed-in account
//!   DELETE /auth/sso/{slug}/unlink           — Unlink OAuth account
//!   GET  /users/me/sso                       — List this account's linked identities
//!
//! ## An identity joins an existing account only when that account asks
//!
//! A first sign-in through a provider used to attach the identity to whatever
//! account already held the email address the provider asserted. Local
//! registration does not verify addresses, so anybody could register
//! `victim@corp.com` with a password of their own and wait: the victim's first
//! SSO sign-in then landed in the attacker's account (card_4753cfe7b985). The
//! callback now never matches an account by email. A provider is added to an
//! existing account through [`start_link`], from a signed-in session, and the
//! callback that completes it attaches the identity to *that* session's
//! account and to no other.

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Redirect},
    Json,
};
use serde::{Deserialize, Serialize};
use tracing;
use utoipa::ToSchema;

use crate::api::access_audit::{grant_actor, record_credential};
use crate::api::auth::{AuthUser, SessionUser};
use crate::error::AppError;
use crate::AppState;

// ── Cookie helpers ───────────────────────────────────────────────

/// Cookie names for secure OAuth2 flow.
const SSO_STATE_COOKIE: &str = "plombir_git_sso_state";
const SSO_VERIFIER_COOKIE: &str = "plombir_git_sso_code_verifier";
/// Set by [`start_link`] only: which signed-in account asked to link the
/// identity this round trip returns with. See [`link_intent_cookie_value`].
const SSO_LINK_COOKIE: &str = "plombir_git_sso_link";

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

// ── Link intent ──────────────────────────────────────────────────

/// The MAC behind a link intent: which account, under which session
/// generation, asked to link which provider, in which OAuth round trip.
///
/// The provider slug and the CSRF state are covered by the MAC but not carried
/// in the cookie — the callback supplies both from its own request, so an
/// intent minted for one provider or one round trip verifies against no other.
/// The domain prefix keeps this MAC from ever equalling the plain value MAC of
/// [`sign_cookie_value`], which signs a random state string under the same key.
fn link_intent_mac(
    user_id: i64,
    session_version: i64,
    slug: &str,
    csrf_state: &str,
    secret: &str,
) -> hmac::Hmac<sha2::Sha256> {
    use hmac::Mac;
    // HMAC accepts a key of any length, so init cannot fail.
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(format!("sso-link\0{user_id}\0{session_version}\0{slug}\0{csrf_state}").as_bytes());
    mac
}

/// `user_id.session_version.signature`.
///
/// The callback is a top-level navigation coming back from the identity
/// provider's site, so the `SameSite=Strict` session cookie is not on it: the
/// callback cannot see who is signed in. This cookie is how [`start_link`] —
/// which can — tells it.
fn link_intent_cookie_value(
    user_id: i64,
    session_version: i64,
    slug: &str,
    csrf_state: &str,
    secret: &str,
) -> String {
    use hmac::Mac;
    let signature = link_intent_mac(user_id, session_version, slug, csrf_state, secret)
        .finalize()
        .into_bytes();
    format!("{user_id}.{session_version}.{}", hex::encode(signature))
}

/// What the callback learned about a link request.
#[derive(Debug, PartialEq, Eq)]
enum LinkIntent {
    /// No link cookie: an ordinary sign-in.
    Absent,
    /// A link cookie this server minted for this provider and this round trip.
    Valid { user_id: i64, session_version: i64 },
    /// A link cookie that does not verify. Never downgraded to a sign-in: the
    /// person asked to link, and signing them in somewhere instead is the
    /// outcome they did not ask for.
    Invalid,
}

fn read_link_intent(headers: &HeaderMap, slug: &str, csrf_state: &str, secret: &str) -> LinkIntent {
    use hmac::Mac;

    let Some(raw) = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|part| {
            part.trim()
                .strip_prefix(SSO_LINK_COOKIE)
                .and_then(|rest| rest.strip_prefix('='))
        })
        .filter(|value| !value.is_empty())
    else {
        return LinkIntent::Absent;
    };

    let mut fields = raw.splitn(3, '.');
    let (Some(user_id), Some(session_version), Some(signature)) =
        (fields.next(), fields.next(), fields.next())
    else {
        return LinkIntent::Invalid;
    };
    let (Ok(user_id), Ok(session_version), Ok(signature)) = (
        user_id.parse::<i64>(),
        session_version.parse::<i64>(),
        hex::decode(signature),
    ) else {
        return LinkIntent::Invalid;
    };
    // Constant-time comparison, as for the state cookies.
    if link_intent_mac(user_id, session_version, slug, csrf_state, secret)
        .verify_slice(&signature)
        .is_err()
    {
        return LinkIntent::Invalid;
    }
    LinkIntent::Valid {
        user_id,
        session_version,
    }
}

// ── Extract base URL ─────────────────────────────────────────────

/// The API base the provider redirects the browser back to. It has to be the
/// address the browser can reach and the IdP has registered, so it comes from
/// [`crate::public_url`]: the old port-sniffing (`:443` → https) never fired,
/// because a browser does not write the default port into `Host`, and every
/// SSO round trip on a `[tls]` listener came back to `http://`
/// (card_f78054e9e98f).
fn get_api_base_url(state: &AppState, headers: &HeaderMap) -> Result<String, AppError> {
    let base = crate::public_url::require_public_base_url(state, headers)?;
    Ok(format!("{base}/api/v1"))
}

// ── Provider resolution ──────────────────────────────────────────

/// Resolve the provider behind `slug` **in a state where it may be used**.
///
/// `enabled` used to be a convention: every entry point looked the provider up
/// by slug and was expected to remember to re-read the flag afterwards.
/// `authorize` and `callback` remembered; the OAuth token-refresh door did not
/// — so an operator could switch a provider off and already-linked accounts
/// kept renewing their OAuth tokens through it indefinitely. That door has
/// since been removed for want of any caller (card_76820bc5325e); the question
/// is still asked once, here, and a door that does not ask it never gets a
/// provider at all.
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
/// The doors used to carry their own copy of this, and the copies had drifted:
/// the token-refresh one swallowed a decryption failure into an empty client
/// secret and then asked the provider to refresh with it, so a wrong
/// `[auth].encryption_key` surfaced as the provider's rejection rather than as
/// ours. One body, one answer — a secret that will not decrypt is a 500 here
/// too.
fn provider_config(
    provider: &rg_db::entities::sso_provider::Model,
    enc_key: &[u8; 32],
    redirect_url: String,
    transport_policy: &rg_core::auth::sso::OidcTransportPolicy,
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
        transport_policy: transport_policy.clone(),
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

    let base_url = get_api_base_url(&state, &headers)?;
    let redirect_url = format!("{}/auth/sso/{}/callback", base_url, slug);

    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let config = provider_config(
        &provider,
        &enc_key,
        redirect_url,
        &state.oidc_transport_policy,
    )?;

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
    // A sign-in is not a link. An intent left behind by an abandoned link
    // round trip would no longer verify against the new state anyway; dropping
    // it here keeps that from surfacing as a refused sign-in.
    clear_state_cookie(&mut redirect, SSO_LINK_COOKIE);

    Ok(redirect)
}

// ── Link a provider to the signed-in account ─────────────────────

/// Where the browser goes to authenticate the identity being linked.
#[derive(Debug, Serialize, ToSchema)]
pub struct SsoLinkStart {
    authorize_url: String,
}

/// POST /auth/sso/{slug}/link
///
/// The only way an identity from `slug` joins an account that already exists.
/// Holding a session proves the account; the provider round trip that follows
/// proves the identity; the callback attaches the second to the first.
///
/// A login session, not a PAT: a linked identity is a way to sign in, so a
/// token scoped to anything less than the account must not be able to mint
/// one — that is exactly the escalation [`SessionUser`] exists to refuse.
#[utoipa::path(
    post,
    path = "/auth/sso/{slug}/link",
    tag = "SSO",
    params(
        ("slug" = String, Path, description = "SSO provider slug"),
    ),
    responses(
        (status = 200, description = "Provider authorization URL to send the browser to", body = SsoLinkStart),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "A login session is required, or the provider is disabled"),
        (status = 404, description = "SSO provider not found"),
        (status = 409, description = "This account already has an identity from this provider"),
    ),
)]
pub async fn start_link(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let provider = resolve_usable_provider(&state, &slug).await?;

    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .filter(|user| user.is_usable())
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;

    // Asked here so the person is told before a provider round trip rather than
    // after it; the callback asks again, because this answer can go stale.
    refuse_second_identity_from(&state, user.id, &provider).await?;

    let base_url = get_api_base_url(&state, &headers)?;
    let redirect_url = format!("{}/auth/sso/{}/callback", base_url, slug);
    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let config = provider_config(
        &provider,
        &enc_key,
        redirect_url,
        &state.oidc_transport_policy,
    )?;
    let (authorize_url, csrf_state, code_verifier) =
        rg_core::auth::sso::oauth2_authorize_url(&config)
            .await
            .map_err(|e| {
                tracing::error!("SSO link authorize error: {}", e);
                AppError::internal("SSO authorization failed")
            })?;

    let mut response = Json(SsoLinkStart { authorize_url }).into_response();
    set_state_cookie(
        &mut response,
        SSO_STATE_COOKIE,
        &csrf_state,
        &state.jwt_secret,
    );
    set_state_cookie(
        &mut response,
        SSO_VERIFIER_COOKIE,
        &code_verifier,
        &state.jwt_secret,
    );
    append_set_cookie(
        &mut response,
        format!(
            "{SSO_LINK_COOKIE}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600",
            link_intent_cookie_value(
                user.id,
                user.session_version,
                &provider.slug,
                &csrf_state,
                &state.jwt_secret,
            )
        ),
    );
    Ok(response)
}

/// One identity per provider per account.
///
/// `oauth_accounts` keys uniqueness on the identity, not on the account, so the
/// table would hold two GitHub identities for one account — and every reader of
/// it addresses a link by its provider slug (`DELETE /auth/sso/{slug}/unlink`,
/// the settings page), which would then reach one of the two at random.
async fn refuse_second_identity_from(
    state: &AppState,
    user_id: i64,
    provider: &rg_db::entities::sso_provider::Model,
) -> Result<(), AppError> {
    let links = rg_db::ops::oauth_account_ops::find_by_user_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?;
    if links.iter().any(|link| link.provider == provider.slug) {
        return Err(AppError::conflict(format!(
            "this account is already linked to a {} identity; unlink it first",
            provider.name
        )));
    }
    Ok(())
}

/// Complete a link [`start_link`] began: attach the identity the provider just
/// proved to the account whose session asked for it.
async fn link_identity_to_requesting_account(
    state: &AppState,
    provider: &rg_db::entities::sso_provider::Model,
    user_info: &rg_core::auth::sso::SsoIdentity,
    requester: (i64, i64),
    headers: &HeaderMap,
) -> Result<axum::response::Response, AppError> {
    let (user_id, session_version) = requester;
    let db = &state.db;

    // The intent outlives nothing it was minted under: a logout, a password
    // reset or a deactivation since `start_link` ends the session that asked,
    // and with it the request.
    let user = rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .map_err(AppError::from)?
        .filter(|user| user.is_usable() && user.session_version == session_version)
        .ok_or_else(|| {
            AppError::unauthorized(
                "the session that asked for this link has ended; sign in and start linking again",
            )
        })?;

    let already_linked_elsewhere = || {
        AppError::conflict(format!(
            "this {} identity is already linked to another account",
            provider.name
        ))
    };

    if let Some(existing) = rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
        db,
        &provider.slug,
        &user_info.provider_user_id,
    )
    .await
    .map_err(AppError::from)?
    {
        if existing.user_id != user.id {
            return Err(already_linked_elsewhere());
        }
        // Linked to this very account already — a repeated round trip, not a
        // new way in, so nothing is written and nothing is journalled.
        return Ok(link_completed_redirect(&provider.slug));
    }

    refuse_second_identity_from(state, user.id, provider).await?;

    // Named before the link is written, per the rule in `access_audit`.
    let actor = grant_actor(state, user.id).await?;
    let linked = rg_db::ops::oauth_account_ops::link(
        db,
        user.id,
        &provider.slug,
        &user_info.provider_user_id,
        &user_info.provider_username,
        &user_info.email,
    )
    .await
    .map_err(AppError::from)?
    .ok_or_else(sso_identity_link_changed)?;
    // `link` converges on whoever won a concurrent insert of this identity —
    // and the winner may be another account.
    if linked.user_id != user.id {
        return Err(already_linked_elsewhere());
    }

    record_credential(
        state,
        &actor,
        "user.link_oauth_account",
        user.id,
        headers,
        oauth_link_details(&linked),
    )
    .await;

    Ok(link_completed_redirect(&provider.slug))
}

/// Back to the page the link was started from. No session cookie is set: the
/// browser already holds the session that asked.
fn link_completed_redirect(slug: &str) -> axum::response::Response {
    let mut redirect = Redirect::temporary(&format!(
        "/settings/security?sso_linked={}",
        encode_query_component(slug)
    ))
    .into_response();
    clear_state_cookie(&mut redirect, SSO_STATE_COOKIE);
    clear_state_cookie(&mut redirect, SSO_VERIFIER_COOKIE);
    clear_state_cookie(&mut redirect, SSO_LINK_COOKIE);
    redirect
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
        (status = 409, description = "Identity link changed during callback"),
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

    // Read before anything is exchanged: a link request that does not verify
    // is refused outright rather than spending the provider's code on it.
    let link_intent = read_link_intent(
        &headers,
        &slug,
        query.state.as_deref().unwrap_or_default(),
        &state.jwt_secret,
    );
    if link_intent == LinkIntent::Invalid {
        tracing::warn!(provider = %slug, "SSO link intent cookie did not verify");
        return Err(AppError::forbidden(
            "this provider link request is invalid or has expired; start linking again from your security settings",
        ));
    }

    // ── Get provider config ──────────────────────────────────────
    let provider = resolve_usable_provider(&state, &slug).await?;

    let base_url = get_api_base_url(&state, &headers)?;
    let redirect_url = format!("{}/auth/sso/{}/callback", base_url, slug);

    let enc_key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let config = provider_config(
        &provider,
        &enc_key,
        redirect_url,
        &state.oidc_transport_policy,
    )?;

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

    // ── A link completes here, without signing anybody in ────────
    if let LinkIntent::Valid {
        user_id,
        session_version,
    } = link_intent
    {
        return link_identity_to_requesting_account(
            &state,
            &provider,
            &user_info,
            (user_id, session_version),
            &headers,
        )
        .await;
    }

    // ── Find or create user ──────────────────────────────────────
    let user_id = find_or_create_sso_user(&state, &provider, &user_info, &headers).await?;

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

    // Re-read MFA and session state through the conditional lifecycle
    // finalizer. The OAuth identity was proved above, but a retirement which
    // won afterwards must stop before the success log, challenge, or JWT.
    let user = crate::api::users::finalized_login_user(
        user.id,
        rg_db::ops::user_ops::finalize_primary_login(&state.db, user.id).await,
    )?;

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
            crate::api::mfa::build_mfa_challenge_cookie(
                &challenge,
                crate::public_url::request_is_https(&state, &headers),
            ),
        );
        append_set_cookie(
            &mut redirect,
            build_clear_auth_cookie(crate::public_url::request_is_https(&state, &headers)),
        );
        clear_state_cookie(&mut redirect, SSO_STATE_COOKIE);
        clear_state_cookie(&mut redirect, SSO_VERIFIER_COOKIE);
        return Ok(redirect);
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
        build_auth_cookie(
            &token,
            crate::public_url::request_is_https(&state, &headers),
        ),
    );
    clear_state_cookie(&mut redirect, SSO_STATE_COOKIE);
    clear_state_cookie(&mut redirect, SSO_VERIFIER_COOKIE);
    Ok(redirect)
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
        (status = 409, description = "The link is the account's last way to sign in"),
    ),
)]
pub async fn unlink_oauth_account(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
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

    crate::api::sign_in_methods::refuse_removing_the_last_way_in(
        &state,
        user_id,
        crate::api::sign_in_methods::WayIn::ProviderLink,
        account.id,
    )
    .await?;

    // Named before the link is dropped, per the rule in `access_audit`: a
    // failed name lookup afterwards would leave the one record of who removed
    // a way into this account blank.
    let actor = grant_actor(&state, user_id).await?;
    let details = oauth_link_details(account);

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

    // Only the request that actually removed the row records the removal, for
    // the same reason only it may claim `unlinked: true`.
    record_credential(
        &state,
        &actor,
        "user.unlink_oauth_account",
        user_id,
        &headers,
        details,
    )
    .await;

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
///
/// It also says what to do if they already have an account: since a first
/// sign-in no longer joins an account by its email, "link it from the account"
/// is the remedy for exactly the people this refusal used to wave through. It
/// is said to everybody, whether or not their address is taken here, so the
/// refusal itself tells nobody which addresses are.
fn sso_provisioning_refused(
    provider: &rg_db::entities::sso_provider::Model,
    user_info: &rg_core::auth::sso::SsoIdentity,
    refusal: rg_core::user::provisioning::ProvisioningRefusal,
) -> AppError {
    crate::metrics::recorder::provisioning_refused(refusal.reason());
    tracing::warn!(
        provider = %provider.slug,
        provider_username = %user_info.provider_username,
        reason = refusal.reason(),
        "SSO first login refused: this provider may not create accounts here"
    );
    AppError::forbidden(format!(
        "{}; if you already have an account here, sign in to it and link {} under Settings → Security",
        refusal.message(),
        provider.name
    ))
}

/// The answer to a first sign-in whose address an existing account holds.
///
/// A `409`: the request is well-formed and nothing failed, but a row that
/// already exists stands in the way, and no edit to this request removes it —
/// the same reading `POST /users/register` gives a taken address. Nothing was
/// linked and no session was issued; the person who owns that account adds the
/// provider from inside it.
fn sso_account_link_required(
    provider: &rg_db::entities::sso_provider::Model,
    user_info: &rg_core::auth::sso::SsoIdentity,
) -> AppError {
    tracing::info!(
        provider = %provider.slug,
        provider_username = %user_info.provider_username,
        "SSO first login refused: an existing account holds this address; it has to link the provider itself"
    );
    AppError::conflict(format!(
        "an account on this instance already uses this email address; sign in to that account and link {} under Settings → Security",
        provider.name
    ))
}

fn sso_identity_link_changed() -> AppError {
    AppError::conflict("identity link changed; restart SSO")
}

/// Sign in through a link that already exists.
async fn sign_in_through_link(
    db: &sea_orm::DatabaseConnection,
    oauth: rg_db::entities::oauth_account::Model,
) -> Result<i64, AppError> {
    // Mark the link as used again. Swallowing the failure would report a
    // successful sign-in through a link the database never acknowledged.
    let touched = rg_db::ops::oauth_account_ops::touch_existing(db, oauth.id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(sso_identity_link_changed)?;
    Ok(touched.user_id)
}

/// Resolve the callback's identity to a Plombir Git account: the one it is
/// linked to, or — on a first sign-in — a new one.
///
/// **Never an existing account found by email.** This used to be a merge:
/// whoever held the address the provider asserted got the identity attached.
/// Local registration does not verify addresses, so the holder could be anyone
/// who typed it first (card_4753cfe7b985) — and `email_verified` cannot close
/// that, because it speaks for the provider's copy of the address, not for the
/// person who registered it here. An identity joins an existing account only
/// through [`start_link`], from inside that account.
///
/// The provider's access and refresh tokens are used to read the identity and
/// then dropped. They are deliberately not persisted: the endpoint that read
/// them back was removed once nothing opened it, which left the instance
/// storing somebody else's live credentials to GitHub / GitLab / an OIDC
/// provider for no feature at all, so the columns went too
/// (`m20260822_000002_drop_oauth_account_tokens`, card_51dd82b6dc82). A future
/// feature that needs to act at the provider on a user's behalf has to say so
/// and bring the storage back with a reader attached.
async fn find_or_create_sso_user(
    state: &AppState,
    provider: &rg_db::entities::sso_provider::Model,
    user_info: &rg_core::auth::sso::SsoIdentity,
    headers: &HeaderMap,
) -> Result<i64, AppError> {
    let db = &state.db;
    let provider_slug = provider.slug.as_str();

    if let Some(oauth) = rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
        db,
        provider_slug,
        &user_info.provider_user_id,
    )
    .await
    .map_err(AppError::from)?
    {
        return sign_in_through_link(db, oauth).await;
    }

    if rg_db::ops::user_ops::find_by_email(db, &user_info.email)
        .await
        .map_err(AppError::from)?
        .is_some()
    {
        // A concurrent first sign-in of this same identity commits its account
        // and its link together, so if that is whose address this is, the link
        // is visible now even though it was not a moment ago.
        if let Some(oauth) = rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
            db,
            provider_slug,
            &user_info.provider_user_id,
        )
        .await
        .map_err(AppError::from)?
        {
            return sign_in_through_link(db, oauth).await;
        }

        // A provider that has not vouched for the address gets exactly the
        // answer an untaken address would get from a provider that may not
        // create accounts — so a closed provider is not a way to ask which
        // addresses have accounts here. Someone the provider has verified as
        // the address's owner is told plainly; that tells them nothing new.
        if user_info.email_verified != Some(true) {
            rg_core::user::provisioning::authorize(provider, &user_info.email)
                .map_err(|refusal| sso_provisioning_refused(provider, user_info, refusal))?;
        }
        return Err(sso_account_link_required(provider, user_info));
    }

    // The only branch that *creates* an account, and so the only one the
    // provisioning policy governs. Signing in through an existing link is not
    // its business: refusing that would log people out of the instance
    // instead of keeping strangers out of it.
    rg_core::user::provisioning::authorize(provider, &user_info.email)
        .map_err(|refusal| sso_provisioning_refused(provider, user_info, refusal))?;
    provision_sso_user(db, headers, provider_slug, user_info)
        .await?
        .ok_or_else(|| sso_account_link_required(provider, user_info))
}

/// What a journal entry about an external identity may say.
///
/// The link is a way into the account — which is the whole reason it is
/// journalled — so the entry names the identity on the far side and nothing
/// else. The row also carries `access_token` and `refresh_token`, encrypted:
/// somebody else's long-lived credentials, which must not appear here in any
/// form, ciphertext included. A journal an operator reads over the admin API
/// must not become a second place to steal them from.
fn oauth_link_details(account: &rg_db::entities::oauth_account::Model) -> serde_json::Value {
    serde_json::json!({
        "provider": account.provider,
        "provider_username": account.provider_username,
        "provider_user_id": account.provider_user_id,
    })
}

/// How many times a first-login provision may lose the race for its generated
/// username before giving up.
///
/// Each extra pass costs one `SELECT` plus one `INSERT`, and it only runs while
/// another request is provisioning the *same* username at the same moment, so
/// a small bound is enough to cover a real collision without turning a
/// persistent constraint failure into a spin.
const SSO_PROVISION_ATTEMPTS: usize = 3;

/// Create the Plombir Git account behind a first SSO login together with its
/// link, tolerating a concurrent callback for the same identity.
///
/// `Ok(None)` means the address turned out to be taken by an account this
/// identity is not linked to — a registration that won the gap since the
/// caller looked. That account is not this login's to enter.
///
/// `users.username`, `users.email` and `oauth_accounts (provider,
/// provider_user_id)` are all UNIQUE, and this path reaches the `INSERT` after
/// separate reads. Two callbacks of the same first login both pass those reads,
/// so one of them meets a constraint. Losing that race is neither the client's
/// fault nor a failed login — the winner created exactly the account this call
/// was about to — so the loser signs in to it instead of answering 500.
///
/// The winner is recognised by its **link**, and only by its link: the account
/// and the link are one transaction
/// (`oauth_account_ops::link_with_new_user`), so a winner whose account is
/// visible has its link visible too. An account that merely holds the address
/// or the generated username is somebody else's.
///
/// `user_provisioned("sso")` fires only on the branch that actually inserted
/// the row, so a raced double callback adds one registration to the funnel, not
/// two. The journal entry for the new link is written here, beside the write,
/// like every other credential's.
async fn provision_sso_user(
    db: &sea_orm::DatabaseConnection,
    headers: &HeaderMap,
    provider_slug: &str,
    user_info: &rg_core::auth::sso::SsoIdentity,
) -> Result<Option<i64>, AppError> {
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

        let error = match rg_db::ops::oauth_account_ops::link_with_new_user(
            db,
            &username,
            &user_info.email,
            user_info.display_name.as_deref().unwrap_or(&username),
            provider_slug,
            &user_info.provider_user_id,
            &user_info.provider_username,
        )
        .await
        {
            Ok((created, linked)) => {
                // SSO first-login provision is a new account: count it in the
                // `users_registered_total` funnel with `sso` provenance.
                crate::metrics::recorder::user_provisioned("sso");
                // The account is the actor, and it did not exist a moment ago,
                // so it cannot be named before the write the way an existing
                // one is. The write is done and the sign-in is owed, so a
                // failed name lookup leaves the id rather than failing it.
                let actor =
                    rg_core::audit::AuditActor::resolve_after_the_fact(db, created.id).await;
                // Every later sign-in through this link records nothing: a row
                // per login would bury the one event an incident review is
                // looking for — the moment a new way into this account appeared.
                rg_core::audit::record(
                    db,
                    &actor,
                    "user.link_oauth_account",
                    Some("user"),
                    Some(created.id),
                    actor.name(),
                    Some(headers),
                    Some(oauth_link_details(&linked)),
                )
                .await;
                return Ok(Some(created.id));
            }
            Err(error) => error,
        };

        // Anything that is not a UNIQUE violation is a real write failure and
        // keeps its classification — `AppError::from` still tells a connection
        // outage (503) apart from a statement-level fault (500).
        if !rg_db::is_unique_violation(&error) {
            return Err(AppError::from(error));
        }

        // A race was lost — but to whom? A link for this identity means a
        // concurrent callback built the account, and signing in to it is the
        // correct answer.
        if let Some(user_id) = resolve_raced_sso_user(db, provider_slug, user_info).await? {
            return Ok(Some(user_id));
        }

        // No link: the address is somebody else's now, or the collision was
        // on the generated username alone. Only the second is retried.
        if rg_db::ops::user_ops::find_by_email(db, &user_info.email)
            .await
            .map_err(AppError::from)?
            .is_some()
        {
            return Ok(None);
        }

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
/// The OAuth link is `(provider, provider_user_id)` — precisely the row this
/// callback was going to write — and it is the only key consulted. The email
/// used to be the second one, and it is exactly the key that let an account
/// somebody else registered on this address be "resolved" as this person's
/// (card_4753cfe7b985). Nothing else (username, display name) identifies the
/// person either.
///
/// The key is read off an [`SsoIdentity`](rg_core::auth::sso::SsoIdentity), so
/// it cannot be the empty string here. That matters more on this path than on
/// the ordinary one: it runs *after* a UNIQUE violation, where an empty key
/// would reliably match the row that just caused it.
async fn resolve_raced_sso_user(
    db: &sea_orm::DatabaseConnection,
    provider_slug: &str,
    user_info: &rg_core::auth::sso::SsoIdentity,
) -> Result<Option<i64>, AppError> {
    Ok(rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
        db,
        provider_slug,
        &user_info.provider_user_id,
    )
    .await
    .map_err(AppError::from)?
    .map(|oauth| oauth.user_id))
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
        build_auth_cookie, encode_query_component, link_intent_cookie_value, provision_sso_user,
        read_link_intent, resolve_raced_sso_user, set_state_cookie, verify_state_cookie,
        LinkIntent, SSO_LINK_COOKIE, SSO_STATE_COOKIE, SSO_VERIFIER_COOKIE,
    };
    use axum::http::{header, HeaderMap};
    use axum::response::IntoResponse;
    use sea_orm::EntityTrait;

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

    /// Held for the length of any test that reads
    /// `plombir_git_users_registered_total` as a before/after pair.
    ///
    /// The counter is process-global and this binary runs its tests in parallel
    /// threads, so a provisioning that happens in the neighbouring test lands
    /// inside this one's window and its delta counts somebody else's account.
    /// Both readers below take this first (card_00b2bd65060e).
    static REGISTRATION_COUNTER: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

    /// Reads `plombir_git_users_registered_total`, initialising the registry the
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
        rg_db::ops::oauth_account_ops::link(
            &db,
            winner.id,
            "gitea",
            "provider-uid-1",
            "alice",
            "winner@example.com",
        )
        .await
        .expect("link the winning account")
        .expect("the winning link remains present");

        let resolved = resolve_raced_sso_user(
            &db,
            "gitea",
            &sso_user_info("provider-uid-1", "alice", "alice@example.com"),
        )
        .await
        .expect("resolve");

        assert_eq!(resolved, Some(winner.id));
    }

    /// The key the race resolution used to fall back to, and the one that made
    /// it a pre-hijack: an account holding this address but not this identity
    /// is somebody else's — a registration that got the address first — and a
    /// lost insert is not a reason to sign into it (card_4753cfe7b985). The
    /// winner of a genuine race of this identity commits its link together with
    /// its account, so the link alone finds it.
    #[tokio::test]
    async fn an_account_holding_the_address_without_this_link_is_not_this_identity() {
        let db = migrated_db().await;

        rg_db::ops::user_ops::create_user(&db, "alice", "alice@example.com", "", "Alice")
            .await
            .expect("create the account that holds the address");

        let resolved = resolve_raced_sso_user(
            &db,
            "gitea",
            &sso_user_info("provider-uid-1", "alice", "alice@example.com"),
        )
        .await
        .expect("resolve");

        assert_eq!(resolved, None, "an email address is not an identity");
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
        let _counter = REGISTRATION_COUNTER.lock().await;
        let before = registered_total();

        let user_id = provision_sso_user(
            &db,
            &HeaderMap::new(),
            "gitea",
            &sso_user_info("provider-uid-1", "alice", "alice@example.com"),
        )
        .await
        .expect("provision")
        .expect("a free address is provisioned");

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
        // The account and its link are one write: a winner whose account is
        // visible to a racing callback has its link visible too.
        let link =
            rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "gitea", "provider-uid-1")
                .await
                .expect("read the link")
                .expect("the first login wrote its link");
        assert_eq!(link.user_id, user_id);
        // ...and the moment the new way in appeared is journalled.
        let journalled = rg_db::entities::audit_log::Entity::find()
            .all(&db)
            .await
            .expect("read the journal")
            .into_iter()
            .filter(|row| {
                row.action == "user.link_oauth_account" && row.resource_id == Some(user_id)
            })
            .count();
        assert_eq!(journalled, 1, "the new link has exactly one journal entry");
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
                &HeaderMap::new(),
                "gitea",
                &sso_user_info(
                    &format!("provider-uid-{index}"),
                    provider_name,
                    &format!("person{index}@example.com"),
                ),
            )
            .await
            .expect("a provider name the local rule refuses is repaired, not refused")
            .expect("a free address is provisioned");

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

        // The concurrent callback got there first: the account and its link
        // are in, under a different username, on the email this login carries.
        // `users.email` is UNIQUE, so the INSERT below really does fail — no
        // injection needed.
        let (winner, _) = rg_db::ops::oauth_account_ops::link_with_new_user(
            &db,
            "alice_from_the_other_callback",
            "alice@example.com",
            "Alice",
            "gitea",
            "provider-uid-1",
            "alice",
        )
        .await
        .expect("create the winning account");

        let _counter = REGISTRATION_COUNTER.lock().await;
        let before = registered_total();
        let user_id = provision_sso_user(
            &db,
            &HeaderMap::new(),
            "gitea",
            &sso_user_info("provider-uid-1", "alice", "alice@example.com"),
        )
        .await
        .expect("a lost race is not a failed login");

        assert_eq!(
            user_id,
            Some(winner.id),
            "both callbacks resolve to one identity"
        );
        assert_eq!(
            registered_total() - before,
            0,
            "adopting an account someone else created is not a new registration",
        );
        drop(guard);
    }

    /// The gap between the callback's email check and its insert: a local
    /// registration takes the address in between. The insert then fails on
    /// `users.email`, and the account that holds the address now is not linked
    /// to this identity — so it is not this login's, and nothing is written.
    #[tokio::test]
    async fn an_address_registered_in_the_gap_is_not_adopted_by_a_lost_insert() {
        let guard = PROVISION_COUNTER_LOCK.lock().await;
        let db = migrated_db().await;

        let registered = rg_db::ops::user_ops::create_user(
            &db,
            "squatter",
            "alice@example.com",
            "$argon2id$not-a-real-hash",
            "Squatter",
        )
        .await
        .expect("create the account that registered the address");

        let _counter = REGISTRATION_COUNTER.lock().await;
        let before = registered_total();
        let outcome = provision_sso_user(
            &db,
            &HeaderMap::new(),
            "gitea",
            &sso_user_info("provider-uid-1", "alice", "alice@example.com"),
        )
        .await
        .expect("a taken address is an answer, not an error");

        assert_eq!(outcome, None, "the registered account was adopted");
        assert!(
            rg_db::ops::oauth_account_ops::find_by_user_id(&db, registered.id)
                .await
                .expect("read links")
                .is_empty(),
            "the identity was linked to the account that registered the address"
        );
        assert!(
            rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "gitea", "provider-uid-1")
                .await
                .expect("read the link")
                .is_none(),
            "a refused first login left a link behind"
        );
        assert_eq!(registered_total() - before, 0);
        drop(guard);
    }

    /// The account and its first link are one write. A link that cannot be
    /// written must take the account down with it — otherwise the account
    /// would sit there holding the address and no identity, and the next
    /// sign-in would meet exactly the unlinked-holder case it is refused for.
    #[tokio::test]
    async fn a_first_login_whose_link_fails_leaves_no_account_behind() {
        let db = migrated_db().await;
        let holder =
            rg_db::ops::user_ops::create_user(&db, "holder", "holder@example.com", "", "Holder")
                .await
                .expect("create the identity's holder");
        rg_db::ops::oauth_account_ops::link(
            &db,
            holder.id,
            "gitea",
            "provider-uid-1",
            "holder",
            "holder@example.com",
        )
        .await
        .expect("link")
        .expect("the identity is held");

        let error = rg_db::ops::oauth_account_ops::link_with_new_user(
            &db,
            "newcomer",
            "newcomer@example.com",
            "Newcomer",
            "gitea",
            "provider-uid-1",
            "newcomer",
        )
        .await
        .expect_err("the identity is already linked");

        assert!(rg_db::is_unique_violation(&error), "{error}");
        assert!(
            rg_db::ops::user_ops::find_by_email(&db, "newcomer@example.com")
                .await
                .expect("read back")
                .is_none(),
            "the account outlived the link it was created with"
        );
    }

    fn link_cookie_headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("other=1; {SSO_LINK_COOKIE}={value}")
                .parse()
                .unwrap(),
        );
        headers
    }

    /// The intent is bound to the account, its session generation, the
    /// provider and the OAuth round trip — change any one and it is refused,
    /// rather than read as somebody's request to link.
    #[test]
    fn a_link_intent_verifies_only_for_its_own_provider_and_round_trip() {
        let value = link_intent_cookie_value(7, 3, "github", "state-1", "secret");
        let headers = link_cookie_headers(&value);

        assert_eq!(
            read_link_intent(&headers, "github", "state-1", "secret"),
            LinkIntent::Valid {
                user_id: 7,
                session_version: 3
            }
        );
        assert_eq!(
            read_link_intent(&headers, "gitlab", "state-1", "secret"),
            LinkIntent::Invalid,
            "an intent for one provider completed a link through another"
        );
        assert_eq!(
            read_link_intent(&headers, "github", "state-2", "secret"),
            LinkIntent::Invalid,
            "an intent from one round trip completed another"
        );
        assert_eq!(
            read_link_intent(&headers, "github", "state-1", "other-secret"),
            LinkIntent::Invalid
        );

        let (_, signature) = value.split_at(value.find('.').unwrap());
        let retargeted = link_cookie_headers(&format!("8{signature}"));
        assert_eq!(
            read_link_intent(&retargeted, "github", "state-1", "secret"),
            LinkIntent::Invalid,
            "an intent rewritten to name another account still verified"
        );
        assert_eq!(
            read_link_intent(
                &link_cookie_headers("garbage"),
                "github",
                "state-1",
                "secret"
            ),
            LinkIntent::Invalid
        );
        assert_eq!(
            read_link_intent(&HeaderMap::new(), "github", "state-1", "secret"),
            LinkIntent::Absent
        );
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
            .starts_with("plombir_git_sso_state="));
        assert!(cookies[1]
            .to_str()
            .unwrap()
            .starts_with("plombir_git_sso_code_verifier="));
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
