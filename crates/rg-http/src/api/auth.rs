//! Shared JWT authentication helpers.
//! Provides centralized Bearer token extraction to eliminate duplicate
//! auth patterns across API handlers.
//!
//! ## Token types supported
//!
//! - **User tokens**: Full-access tokens with username claim, issued at login.
//!   Validated by `extract_user_id` and `extract_bearer_claims`.
//! - **CI Job tokens** (`CI_JOB_TOKEN`): Least-privilege tokens scoped to a
//!   specific repository. Validated by `extract_ci_job_claims`. Used by CI jobs
//!   to call selected read-only ForgeKeep APIs.
//!
//! ## H-3: Unified Axum Extractor
//!
//! `AuthenticatedUser` implements `FromRequestParts<AppState>`, allowing handlers
//! to declare authentication at the signature level:
//!
//! ```ignore
//! pub async fn handler(
//!     State(state): State<AppState>,
//!     AuthUser(user_id): AuthUser,
//! ) -> impl IntoResponse { ... }
//! ```
//!
//! This provides compile-time auth guarantees — handlers that need auth simply
//! include `AuthUser` in their signature. The legacy `extract_user_id()` helper
//! remains for cases where conditional auth is needed (e.g., anonymous-read repos).

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rg_core::auth::jwt::Claims;

/// H-3: Unified auth extractor — handlers include this in their signature
/// to get compile-time authentication guarantees.
///
/// Extracts user_id from the HttpOnly auth cookie or `Authorization: Bearer <jwt>` header.
/// Returns 401 if the token is missing, invalid, or not a user token.
#[derive(Debug, Clone, Copy)]
pub struct AuthUser(pub i64);

impl FromRequestParts<crate::AppState> for AuthUser {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        let user_id = extract_user_id(&parts.headers, &state.jwt_secret)
            .ok_or((StatusCode::UNAUTHORIZED, "authentication required"))?;
        Ok(AuthUser(user_id))
    }
}

/// Cookie name used for HttpOnly JWT storage (M-4).
pub(crate) const AUTH_COOKIE_NAME: &str = "forgekeep_token";

/// Extract a JWT from the `Cookie` header (M-4: HttpOnly cookie auth).
///
/// Returns the raw token string if a valid `forgekeep_token` cookie is present.
fn extract_token_from_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get("cookie")?.to_str().ok()?;
    for cookie in cookie_header.split(';') {
        let cookie = cookie.trim();
        if let Some(token) = cookie.strip_prefix(&format!("{}=", AUTH_COOKIE_NAME)) {
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// Extract authenticated user_id from either the HttpOnly cookie (M-4, preferred)
/// or the `Authorization: Bearer` header (fallback for API clients / Git).
/// Returns Some(user_id) if the JWT is a valid **user token**, None otherwise.
///
/// CI job tokens are intentionally rejected — use `extract_ci_or_user_id` for
/// repository-scoped operations during CI job execution.
pub(crate) fn extract_user_id(headers: &HeaderMap, jwt_secret: &str) -> Option<i64> {
    // M-4: Check HttpOnly cookie first, then fall back to Bearer header
    if let Some(token) = extract_token_from_cookie(headers) {
        if let Some(claims) = rg_core::auth::jwt::validate_token(&token, jwt_secret) {
            return claims.sub.parse::<i64>().ok();
        }
    }
    extract_bearer_claims(headers, jwt_secret).and_then(|c| c.sub.parse::<i64>().ok())
}

/// Extract and validate the Bearer JWT Claims from the Authorization header.
/// Returns Some(Claims) for valid user tokens, None for invalid or CI tokens.
pub(crate) fn extract_bearer_claims(headers: &HeaderMap, jwt_secret: &str) -> Option<Claims> {
    let auth = headers.get("authorization")?.to_str().ok()?;
    let token = auth.strip_prefix("Bearer ")?;
    rg_core::auth::jwt::validate_token(token, jwt_secret)
}

/// Every session JWT this request presents, resolved to its user id.
///
/// A session may arrive in any of the shapes this server accepts: the HttpOnly
/// cookie, `Authorization: Bearer`, the `token ` spelling some clients use,
/// HTTP Basic with the JWT in either field (how git clients carry it), and —
/// because a browser cannot set headers on a WebSocket handshake — the
/// `bearer.<jwt>` subprotocol or a `?token=` query parameter. All of them are
/// collected here, because a gate that reads one shape is a gate with a
/// documented way around it. A request normally presents exactly one session;
/// duplicates are folded.
///
/// Anything that is not a valid *user* JWT yields nothing: a Personal Access
/// Token, a CI job token, an OCI registry token and an MFA challenge all fail
/// `validate_token` (different claim shape or a domain-separated key), and each
/// carries its own owner check where it is resolved.
fn presented_session_user_ids(
    headers: &HeaderMap,
    query: Option<&str>,
    jwt_secret: &str,
) -> Vec<i64> {
    let mut candidates: Vec<String> = Vec::new();

    if let Some(token) = extract_token_from_cookie(headers) {
        candidates.push(token);
    }

    // WebSocket handshake shapes — `ws::ws_notifications_handler` accepts both.
    if let Some(raw) = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
    {
        for proto in raw.split(',') {
            if let Some(token) = proto.trim().strip_prefix("bearer.") {
                candidates.push(token.to_string());
            }
        }
    }
    if let Some(query) = query {
        for pair in query.split('&') {
            if let Some(token) = pair.strip_prefix("token=") {
                candidates.push(token.to_string());
            }
        }
    }

    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(token) = auth
            .strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("token "))
        {
            candidates.push(token.to_string());
        } else if let Some(encoded) = auth.strip_prefix("Basic ") {
            use base64::Engine as _;
            let creds = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
                .and_then(|raw| String::from_utf8(raw).ok());
            if let Some((user, pass)) = creds.as_deref().and_then(|c| c.split_once(':')) {
                candidates.push(pass.to_string());
                candidates.push(user.to_string());
            }
        }
    }

    let mut ids = Vec::new();
    for candidate in candidates {
        if let Some(claims) = rg_core::auth::jwt::validate_token(&candidate, jwt_secret) {
            if let Ok(id) = claims.sub.parse::<i64>() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

/// Reject a session whose account no longer stands — the revocation gate.
///
/// A JWT is a bearer credential that answers for itself: the signature is
/// valid, the expiry has not passed, and nothing in that answer knows the
/// account was disabled an hour ago. Every other credential ForgeKeep accepts
/// (password, SSH key, PAT, docker login, reset token) resolves an owner and
/// consults [`is_usable`](rg_db::entities::user::Model::is_usable) at that
/// moment; a session issued *before* the deactivation presents no credential to
/// re-check, so without this layer it keeps working for the rest of its seven
/// days. That is the whole distance between "the administrator revoked access"
/// and "the administrator revoked access, eventually".
///
/// This lives as one middleware over the whole router rather than inside
/// `extract_user_id`, for the same reason `is_usable` exists at all: the
/// helpers that read a session are called from ~80 places, and a check copied
/// into each of them is 80 chances to forget one. Here there is exactly one,
/// and it also covers the paths that never call those helpers — git-over-HTTP
/// resolves its actor itself, and the OCI registry has its own extractor.
///
/// Cost is one primary-key read per *authenticated* request; anonymous traffic,
/// `/health` and `/metrics` present no session and touch the database not at
/// all. Measured on SQLite in release (see `session_gate_cost_tests`): 31.8µs
/// for the lookup against 122µs for a whole `GET /users/me` — a quarter of the
/// lightest authenticated endpoint there is, and noise next to anything that
/// actually reads a repository. A cache is deliberately absent: it would buy
/// those microseconds back and pay for them with a revocation window plus the
/// shared-state bug class `PERM_CACHE` is still working through. Re-measure
/// before dismissing this on a networked database, where the read is a round
/// trip rather than a page fetch.
pub(crate) async fn session_standing_middleware(
    State(state): State<crate::AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let presented = presented_session_user_ids(req.headers(), req.uri().query(), &state.jwt_secret);
    for user_id in presented {
        match rg_db::ops::user_ops::find_by_id(&state.db, user_id).await {
            Ok(Some(user)) if user.is_usable() => {}
            Ok(_) => {
                tracing::warn!(
                    user_id,
                    "rejecting session token: account is disabled or gone"
                );
                return (
                    StatusCode::UNAUTHORIZED,
                    "session belongs to a disabled account",
                )
                    .into_response();
            }
            Err(e) => {
                // Fail closed, but say which of the two it is: "revoked" and
                // "could not tell" are different answers and a client retrying
                // a 503 is right where retrying a 401 is not.
                tracing::error!(
                    user_id,
                    error = %format!("{e:#}"),
                    "could not verify account standing for a session token"
                );
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "could not verify account standing",
                )
                    .into_response();
            }
        }
    }
    next.run(req).await
}

/// Extract a CI job token and verify it has the required scope for the target repo.
///
/// Returns the job token claims if valid and authorized. Returns None if the token
/// is missing, invalid, expired, or lacks the required scope/repo access.
pub(crate) fn extract_ci_job_claims(
    headers: &HeaderMap,
    jwt_secret: &str,
    repo_id: i64,
    required_scope: &str,
) -> Option<rg_core::auth::ci_token::CiJobClaims> {
    let auth = headers.get("authorization")?.to_str().ok()?;
    let token = auth.strip_prefix("Bearer ")?;
    rg_core::auth::ci_token::validate_ci_token(token, jwt_secret, repo_id, required_scope)
}

/// Bind a CI job token's claims back to the rows they name, and answer whether
/// the job is still one this token may speak for.
///
/// `validate_ci_token_signature` says it in its own documentation: *callers
/// must still bind the embedded job/pipeline/repository IDs to persisted data*.
/// A signature is not a session — it is fixed when the token is minted and
/// stays good for the whole hour of its TTL, so a job that was cancelled, a
/// pipeline that was deleted, or a token that leaked into a job log keeps its
/// key until the clock runs out. Only the database knows otherwise, and it is
/// asked here so that both consumers — the OIDC exchange and the repository
/// read gate — ask it the same way.
///
/// Denials are `401`/`403` so a caller folding this into a boolean can tell
/// them apart from a failed lookup, which propagates.
pub(crate) async fn ci_job_binding(
    state: &crate::AppState,
    claims: &rg_core::auth::ci_token::CiJobClaims,
) -> Result<
    (
        rg_db::entities::pipeline_job::Model,
        rg_db::entities::pipeline::Model,
    ),
    crate::error::AppError,
> {
    use crate::error::AppError;

    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, claims.job_id).await {
        Ok(Some(job)) if matches!(job.status.as_str(), "assigned" | "running") => job,
        Ok(Some(_)) => return Err(AppError::forbidden("CI job is not running")),
        Ok(None) => return Err(AppError::unauthorized("CI job no longer exists")),
        Err(error) => return Err(AppError::from(error)),
    };
    let stage = match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await {
        Ok(Some(stage)) => stage,
        Ok(None) => return Err(AppError::unauthorized("CI stage no longer exists")),
        Err(error) => return Err(AppError::from(error)),
    };
    let pipeline = match rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id).await
    {
        Ok(Some(pipeline))
            if pipeline.id == claims.pipeline_id && pipeline.repo_id == claims.repo_id =>
        {
            pipeline
        }
        Ok(_) => return Err(AppError::unauthorized("CI token resource binding mismatch")),
        Err(error) => return Err(AppError::from(error)),
    };
    Ok((job, pipeline))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "gate-secret";

    fn headers(name: &str, value: String) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
        headers
    }

    fn session(user_id: i64) -> String {
        rg_core::auth::jwt::generate_token(user_id, "gatekeeper", SECRET, 7).unwrap()
    }

    fn basic(user: &str, pass: &str) -> String {
        use base64::Engine as _;
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
        )
    }

    /// Every shape this server accepts has to reach the revocation gate. A
    /// shape the gate cannot read is a way to keep using a revoked session —
    /// the WebSocket handshake is the sharp edge here, since a browser cannot
    /// set headers on it and the handler grew two header-free spellings.
    #[test]
    fn every_presentation_of_a_session_is_seen() {
        let jwt = session(42);
        for (shape, name, value) in [
            ("Bearer", "authorization", format!("Bearer {jwt}")),
            ("token", "authorization", format!("token {jwt}")),
            ("cookie", "cookie", format!("forgekeep_token={jwt}")),
            ("Basic password", "authorization", basic("alice", &jwt)),
            (
                "Basic username",
                "authorization",
                basic(&jwt, "x-oauth-basic"),
            ),
            (
                "WebSocket subprotocol",
                "sec-websocket-protocol",
                format!("forgekeep, bearer.{jwt}"),
            ),
        ] {
            assert_eq!(
                presented_session_user_ids(&headers(name, value), None, SECRET),
                vec![42],
                "a session presented as {shape} is invisible to the gate"
            );
        }

        assert_eq!(
            presented_session_user_ids(&HeaderMap::new(), Some(&format!("token={jwt}")), SECRET),
            vec![42],
            "a session presented as a WebSocket query parameter is invisible to the gate"
        );
    }

    /// One session presented twice is still one account to look up — the gate
    /// pays one database read per request, not one per header.
    #[test]
    fn the_same_session_in_two_places_is_one_lookup() {
        let jwt = session(7);
        let mut h = headers("authorization", format!("Bearer {jwt}"));
        h.insert("cookie", format!("forgekeep_token={jwt}").parse().unwrap());
        assert_eq!(
            presented_session_user_ids(&h, Some(&format!("token={jwt}")), SECRET),
            vec![7]
        );
    }

    /// Credentials that are not user sessions carry their own owner check where
    /// they are resolved; the gate must not try to read them as one.
    #[test]
    fn non_session_credentials_are_ignored() {
        let foreign = rg_core::auth::jwt::generate_token(1, "mallory", "other-secret", 7).unwrap();
        let challenge =
            rg_core::auth::jwt::generate_mfa_challenge(1, "bob", "ldap", SECRET).unwrap();
        let expired = rg_core::auth::jwt::generate_token(1, "stale", SECRET, -1).unwrap();
        for (what, token) in [
            ("a personal access token", "fk_pat_abcdef".to_string()),
            ("a JWT signed with another secret", foreign),
            ("an MFA challenge", challenge),
            ("an expired session", expired),
        ] {
            assert!(
                presented_session_user_ids(
                    &headers("authorization", format!("Bearer {token}")),
                    None,
                    SECRET
                )
                .is_empty(),
                "{what} was read as a live session"
            );
        }
    }

    /// A malformed Basic payload must not panic or be mistaken for a session.
    #[test]
    fn malformed_credentials_yield_nothing() {
        for value in [
            "Basic !!!not-base64!!!".to_string(),
            format!("Basic {}", {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode("no-colon-here")
            }),
            "Bearer ".to_string(),
            "Negotiate whatever".to_string(),
        ] {
            assert!(
                presented_session_user_ids(&headers("authorization", value.clone()), None, SECRET)
                    .is_empty(),
                "{value} was read as a session"
            );
        }
    }
}
