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
    type Rejection = crate::error::AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        let user_id = extract_user_id(&parts.headers, &state.jwt_secret)
            .ok_or_else(|| crate::error::AppError::unauthorized("authentication required"))?;
        Ok(AuthUser(user_id))
    }
}

/// Cookie name used for HttpOnly JWT storage (M-4).
pub(crate) const AUTH_COOKIE_NAME: &str = "forgekeep_token";

/// Extract a JWT from the `Cookie` header (M-4: HttpOnly cookie auth).
///
/// Returns the raw token string if a non-empty [`AUTH_COOKIE_NAME`] cookie is
/// present. This is the *only* reader of that cookie in the crate — a second one
/// lived in [`crate::ws`] with the name written out as a literal, which is how a
/// rename would have quietly stopped authenticating browser WebSockets
/// (card_24a8ef566056).
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
    extract_user_session(headers, jwt_secret).map(|(user_id, _)| user_id)
}

/// The account this request's credentials name, *and* which credential named
/// it — a session with its generation, or the personal access token the
/// request presented.
///
/// [`extract_user_session`] is this reading with the distinction collapsed,
/// which is all a caller behind [`session_standing_middleware`] needs. The
/// exception is a caller that mints something outliving the request: a
/// presigned LFS action URL is redeemed hours later by a request carrying no
/// credentials at all, so *what to re-check* has to travel inside the
/// capability — and the two credentials are revoked by different acts
/// (card_e4e177acd095).
pub(crate) fn extract_user_credential(
    headers: &HeaderMap,
    jwt_secret: &str,
) -> Option<(i64, rg_core::lfs::service::LfsCredential)> {
    let claims = extract_token_from_cookie(headers)
        .and_then(|token| rg_core::auth::jwt::validate_token(&token, jwt_secret))
        .or_else(|| extract_bearer_claims(headers, jwt_secret))?;
    let user_id = claims.sub.parse::<i64>().ok()?;
    let credential = match claims.pat_id {
        Some(id) => rg_core::lfs::service::LfsCredential::Token { id },
        None => rg_core::lfs::service::LfsCredential::Session {
            version: claims.session_version,
        },
    };
    Some((user_id, credential))
}

/// The account this request's credentials name, *and* the session generation
/// they were minted under.
///
/// [`extract_user_id`] is this reading with the generation thrown away, which is
/// all most callers need: they are behind
/// [`session_standing_middleware`], which has already refused any request
/// carrying an older generation. The exception is a caller that mints something
/// outliving the request — a presigned LFS action URL is redeemed hours later,
/// by a request that presents no session at all, so the generation has to be
/// carried into the capability for the redeeming side to have anything to
/// compare (card_c742da1794e4).
pub(crate) fn extract_user_session(headers: &HeaderMap, jwt_secret: &str) -> Option<(i64, i64)> {
    // M-4: Check HttpOnly cookie first, then fall back to Bearer header
    let claims = extract_token_from_cookie(headers)
        .and_then(|token| rg_core::auth::jwt::validate_token(&token, jwt_secret))
        .or_else(|| extract_bearer_claims(headers, jwt_secret))?;
    Some((claims.sub.parse::<i64>().ok()?, claims.session_version))
}

/// Extract and validate the Bearer JWT Claims from the Authorization header.
/// Returns Some(Claims) for valid user tokens, None for invalid or CI tokens.
///
/// Deliberately narrower than `pub(crate)`: this reads **one** of the shapes a
/// session arrives in, so every gate that called it directly was a gate a
/// browser could not pass — the HttpOnly cookie is invisible to it. Four such
/// gates were found and fixed one at a time (card_7210b02c0ae9,
/// card_b38bfb0f2b40, card_64aeec457364, card_fb094ba6d323); the visibility is
/// what stops a fifth from being written. Callers outside this module want
/// [`extract_user_id`] or the [`AuthUser`] extractor, both of which read the
/// cookie first and fall back to this.
fn extract_bearer_claims(headers: &HeaderMap, jwt_secret: &str) -> Option<Claims> {
    let auth = headers.get("authorization")?.to_str().ok()?;
    let token = auth.strip_prefix("Bearer ")?;
    rg_core::auth::jwt::validate_token(token, jwt_secret)
}

/// Every `bearer.<jwt>` token offered in the `Sec-WebSocket-Protocol` header,
/// each paired with the protocol string it was offered as.
///
/// The browser WebSocket API does not allow setting custom headers, so the
/// subprotocol field is the only way to pass a token without exposing it in the
/// URL — query parameters leak into server logs, browser history and the
/// `Referer` header. The protocol string comes back alongside the token because
/// a server that does not echo the client's selected subprotocol fails the
/// handshake (RFC 6455 §4.1).
///
/// One reader, two consumers: [`presented_session_versions`] below checks every
/// offered token against the revocation gate, and [`ws_session`] resolves the
/// first one — the one a handler will actually act on.
fn bearer_subprotocols(headers: &HeaderMap) -> Vec<(String, String)> {
    let Some(raw) = headers
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
    else {
        return Vec::new();
    };
    raw.split(',')
        .map(str::trim)
        .filter_map(|protocol| {
            protocol
                .strip_prefix("bearer.")
                .map(|token| (protocol.to_string(), token.to_string()))
        })
        .collect()
}

/// The account a WebSocket handshake authenticated, and the session generation
/// it was authenticated under.
///
/// The two travel as one value rather than as an id with a version alongside it,
/// because a socket holding the id and not the generation can only ask half of
/// the revocation question — which is precisely what both socket loops did
/// (card_7898025803a6): they re-asked on an interval whether the account still
/// stood, and had nothing left to compare a logout or a password reset against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WsSessionUser {
    pub(crate) user_id: i64,
    /// The `users.session_version` the presented JWT was minted under. A reset
    /// or a `POST /users/logout` bumps that column; see
    /// [`rg_db::ops::user_ops::invalidate_sessions`].
    pub(crate) session_version: i64,
}

/// The session a WebSocket handshake presented, and the subprotocol to echo.
pub(crate) struct WsSession {
    /// The `Sec-WebSocket-Protocol` value to select in the upgrade response,
    /// when the token arrived as one. `None` for the cookie and query shapes,
    /// which offer no subprotocol to echo.
    pub(crate) protocol_echo: Option<String>,
    /// The session this handshake authenticated, or `None` when no shape carried
    /// a valid user session.
    pub(crate) user: Option<WsSessionUser>,
}

/// Resolve the session a WebSocket handshake presents.
///
/// Three spellings are accepted, in this order: the HttpOnly cookie (what a
/// browser sends on a same-origin upgrade, and the *only* shape it can offer on
/// `/ws/notifications`), a `bearer.<jwt>` subprotocol, and a legacy `?token=`
/// query parameter.
///
/// This lives here rather than in [`crate::ws`] because it is a reading of a
/// session, and the copy that lived there had already drifted: it spelled the
/// cookie's name as a literal instead of [`AUTH_COOKIE_NAME`], so renaming the
/// constant would have left the browser's only way into the notification socket
/// looking for a cookie nobody sets — and failing silently, since an
/// unauthenticated notification socket opens and then reports the error in a
/// frame (card_24a8ef566056). Both WebSocket handlers now share this one
/// resolution, and it sits next to [`presented_session_versions`], which has to
/// agree with it about what counts as a presented session.
pub(crate) fn ws_session(
    headers: &HeaderMap,
    query_token: Option<&str>,
    jwt_secret: &str,
) -> WsSession {
    let (protocol_echo, token) = match extract_token_from_cookie(headers) {
        Some(token) => (None, Some(token)),
        None => match bearer_subprotocols(headers).into_iter().next() {
            Some((protocol, token)) => (Some(protocol), Some(token)),
            None => (None, query_token.map(str::to_string)),
        },
    };

    let user = token
        .as_deref()
        .and_then(|token| rg_core::auth::jwt::validate_token(token, jwt_secret))
        .and_then(|claims| {
            claims.sub.parse::<i64>().ok().map(|user_id| WsSessionUser {
                user_id,
                session_version: claims.session_version,
            })
        });

    WsSession {
        protocol_echo,
        user,
    }
}

/// Every session JWT this request presents, resolved to its user id and
/// generation.
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
fn presented_session_versions(
    headers: &HeaderMap,
    query: Option<&str>,
    jwt_secret: &str,
) -> Vec<(i64, i64)> {
    let mut candidates: Vec<String> = Vec::new();

    if let Some(token) = extract_token_from_cookie(headers) {
        candidates.push(token);
    }

    // WebSocket handshake shapes — `ws::ws_notifications_handler` accepts both.
    for (_, token) in bearer_subprotocols(headers) {
        candidates.push(token);
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

    let mut sessions = Vec::new();
    for candidate in candidates {
        if let Some(claims) = rg_core::auth::jwt::validate_token(&candidate, jwt_secret) {
            if let Ok(id) = claims.sub.parse::<i64>() {
                let session = (id, claims.session_version);
                if !sessions.contains(&session) {
                    sessions.push(session);
                }
            }
        }
    }
    sessions
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
    let presented = presented_session_versions(req.headers(), req.uri().query(), &state.jwt_secret);
    for (user_id, session_version) in presented {
        match rg_db::ops::user_ops::find_by_id(&state.db, user_id).await {
            Ok(Some(user)) if user.is_usable() && user.session_version == session_version => {}
            Ok(_) => {
                tracing::warn!(
                    user_id,
                    "rejecting session token: account is disabled, gone, or its session was revoked"
                );
                return session_refusal(req.uri().path(), SessionRefusal::Revoked);
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
                return session_refusal(req.uri().path(), SessionRefusal::Unavailable);
            }
        }
    }
    next.run(req).await
}

/// Which of the two answers `session_standing_middleware` is giving back.
///
/// `Revoked` is a 401 the client should not retry — the session has been
/// invalidated on purpose and the caller has to reauthenticate. `Unavailable`
/// is a 503 the client is right to retry — the middleware could not ask the
/// database whether the account still stood, so failing closed is a claim about
/// this middleware's own state, not about the session.
#[derive(Clone, Copy)]
enum SessionRefusal {
    Revoked,
    Unavailable,
}

/// Answer a session-standing refusal in the envelope of the subtree the request
/// was aimed at.
///
/// The gate is one middleware mounted over the whole router, and it refuses
/// *before* the router picks a subtree — so a text/plain body escapes both the
/// REST API's `AppError` envelope (see [`crate::error::api_rejection_envelope`])
/// and the OCI registry's `{errors:[{code,message}]}` envelope (see
/// [`crate::oci::oci_transport_refusal_envelope`]), and every client under
/// `/api/v1` and `/v2` reads a shape it does not know. This looks at the
/// request path and answers in that subtree's format instead
/// (card_9a848c73f48d).
///
/// `/api/v1/...` → [`crate::error::AppError`] JSON (`{error:{code,message}}`).
/// `/v2` and `/v2/...` → OCI JSON (`{errors:[{code,message}]}`).
/// Everything else (git transport under `/{owner}/{repo}/...` and `/git/...`,
/// SPA fallback, `/health`, `/metrics`) → keeps the plain-text body it had
/// before the fix. Git-HTTP clients recognise `WWW-Authenticate: Basic` and
/// prompt for credentials on a 401, so the challenge is attached there for the
/// same reason [`crate::git_http`] carries one on its own denials.
fn session_refusal(path: &str, kind: SessionRefusal) -> Response {
    use crate::error::AppError;
    use crate::oci;
    use crate::routes;

    let in_api_v1 = routes::is_inside(path, "/api/v1");
    let in_v2 = path == "/v2" || path == "/v2/" || routes::is_inside(path, "/v2");

    match kind {
        SessionRefusal::Revoked => {
            let message = "session is no longer valid";
            if in_api_v1 {
                return AppError::unauthorized(message).into_response();
            }
            if in_v2 {
                return oci::oci_refusal_response(
                    StatusCode::UNAUTHORIZED,
                    "UNAUTHORIZED",
                    message,
                );
            }
            // Git-HTTP clients read the `WWW-Authenticate` header and prompt
            // for credentials; other transports simply see plain text.
            (
                StatusCode::UNAUTHORIZED,
                [(
                    axum::http::header::WWW_AUTHENTICATE,
                    "Basic realm=\"ForgeKeep\"",
                )],
                message,
            )
                .into_response()
        }
        SessionRefusal::Unavailable => {
            let message = "could not verify account standing";
            if in_api_v1 {
                return AppError::service_unavailable(message).into_response();
            }
            if in_v2 {
                return oci::oci_refusal_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "UNAVAILABLE",
                    message,
                );
            }
            (StatusCode::SERVICE_UNAVAILABLE, message).into_response()
        }
    }
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
    let job = match rg_db::ops::pipeline_ops::finalize_ci_job_token_job(&state.db, job.id).await {
        Ok(Some(job)) => job,
        Ok(None) => return Err(AppError::forbidden("CI job is no longer running")),
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
        session_at(user_id, 0)
    }

    fn session_at(user_id: i64, generation: i64) -> String {
        rg_core::auth::jwt::generate_token(user_id, "gatekeeper", generation, SECRET, 7).unwrap()
    }

    fn ws_user(user_id: i64, session_version: i64) -> Option<WsSessionUser> {
        Some(WsSessionUser {
            user_id,
            session_version,
        })
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
            ("cookie", "cookie", format!("{AUTH_COOKIE_NAME}={jwt}")),
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
                presented_session_versions(&headers(name, value), None, SECRET),
                vec![(42, 0)],
                "a session presented as {shape} is invisible to the gate"
            );
        }

        assert_eq!(
            presented_session_versions(&HeaderMap::new(), Some(&format!("token={jwt}")), SECRET),
            vec![(42, 0)],
            "a session presented as a WebSocket query parameter is invisible to the gate"
        );
    }

    /// One session presented twice is still one account to look up — the gate
    /// pays one database read per request, not one per header.
    #[test]
    fn the_same_session_in_two_places_is_one_lookup() {
        let jwt = session(7);
        let mut h = headers("authorization", format!("Bearer {jwt}"));
        h.insert(
            "cookie",
            format!("{AUTH_COOKIE_NAME}={jwt}").parse().unwrap(),
        );
        assert_eq!(
            presented_session_versions(&h, Some(&format!("token={jwt}")), SECRET),
            vec![(7, 0)]
        );
    }

    /// Credentials that are not user sessions carry their own owner check where
    /// they are resolved; the gate must not try to read them as one.
    #[test]
    fn non_session_credentials_are_ignored() {
        let foreign =
            rg_core::auth::jwt::generate_token(1, "mallory", 0, "other-secret", 7).unwrap();
        let challenge =
            rg_core::auth::jwt::generate_mfa_challenge(1, "bob", "ldap", SECRET).unwrap();
        let expired = rg_core::auth::jwt::generate_token(1, "stale", 0, SECRET, -1).unwrap();
        for (what, token) in [
            ("a personal access token", "fk_pat_abcdef".to_string()),
            ("a JWT signed with another secret", foreign),
            ("an MFA challenge", challenge),
            ("an expired session", expired),
        ] {
            assert!(
                presented_session_versions(
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
                presented_session_versions(&headers("authorization", value.clone()), None, SECRET)
                    .is_empty(),
                "{value} was read as a session"
            );
        }
    }

    /// The cookie's name is a wire contract, and this is the one place it is
    /// deliberately typed twice.
    ///
    /// Nothing in the frontend names it — the cookie is HttpOnly, so the browser
    /// attaches it without JavaScript ever seeing it. What *does* name it are
    /// out-of-crate callers a `cargo` build cannot see:
    /// `scripts/browser-admin-smoke.mjs` sets it over CDP, and
    /// `scripts/notification-websocket-contract-check.mjs` documents it as the
    /// notification socket's only credential. Renaming the constant is therefore
    /// legitimate but not free, and this assertion is what says so out loud
    /// instead of letting those two scripts break in a later run.
    #[test]
    fn the_auth_cookie_is_named_on_the_wire_as_the_out_of_crate_callers_expect() {
        assert_eq!(
            AUTH_COOKIE_NAME, "forgekeep_token",
            "renaming the auth cookie also means updating scripts/browser-admin-smoke.mjs \
             and scripts/notification-websocket-contract-check.mjs"
        );
    }

    /// The cookie is the browser's only way into `/ws/notifications`: it cannot
    /// put a header on an upgrade, and the contract check forbids the frontend
    /// from using `?token=` or a subprotocol. So a cookie handshake must resolve
    /// to an account — and must offer no subprotocol to echo, since the client
    /// selected none.
    #[test]
    fn a_handshake_carrying_only_the_auth_cookie_is_a_session() {
        let jwt = session(42);
        let resolved = ws_session(
            &headers("cookie", format!("{AUTH_COOKIE_NAME}={jwt}")),
            None,
            SECRET,
        );
        assert_eq!(resolved.user, ws_user(42, 0));
        assert!(
            resolved.protocol_echo.is_none(),
            "a cookie handshake offers no subprotocol, so none may be echoed"
        );
    }

    /// A `bearer.<jwt>` subprotocol has to come back in the upgrade response:
    /// a client that offers one and is selected none closes the connection.
    #[test]
    fn a_subprotocol_handshake_is_a_session_and_gets_its_protocol_echoed() {
        let jwt = session(7);
        let resolved = ws_session(
            &headers("sec-websocket-protocol", format!("bearer.{jwt}")),
            None,
            SECRET,
        );
        assert_eq!(resolved.user, ws_user(7, 0));
        assert_eq!(resolved.protocol_echo, Some(format!("bearer.{jwt}")));
    }

    /// The legacy `?token=` shape still resolves, and still echoes nothing.
    #[test]
    fn a_query_token_handshake_is_a_session() {
        let jwt = session(9);
        let resolved = ws_session(&HeaderMap::new(), Some(&jwt), SECRET);
        assert_eq!(resolved.user, ws_user(9, 0));
        assert!(resolved.protocol_echo.is_none());
    }

    /// The generation the token was minted under has to survive the handshake,
    /// or the socket loops that outlive it can only ask whether the *account*
    /// still stands — and a logout, which revokes the session and leaves the
    /// account alone, becomes invisible to them (card_7898025803a6).
    #[test]
    fn a_handshake_carries_the_session_generation_it_was_minted_under() {
        for (shape, headers_in, query) in [
            (
                "a cookie",
                headers(
                    "cookie",
                    format!("{AUTH_COOKIE_NAME}={}", session_at(42, 3)),
                ),
                None,
            ),
            (
                "a subprotocol",
                headers(
                    "sec-websocket-protocol",
                    format!("bearer.{}", session_at(42, 3)),
                ),
                None,
            ),
            (
                "a query parameter",
                HeaderMap::new(),
                Some(session_at(42, 3)),
            ),
        ] {
            assert_eq!(
                ws_session(&headers_in, query.as_deref(), SECRET).user,
                ws_user(42, 3),
                "a handshake presenting {shape} dropped its session generation"
            );
        }
    }

    /// No shape carrying a valid user session authenticates nobody — and an
    /// invalid token must not be mistaken for one either.
    #[test]
    fn a_handshake_without_a_valid_session_authenticates_nobody() {
        for (what, headers_in, query) in [
            ("nothing at all", HeaderMap::new(), None),
            (
                "an empty cookie",
                headers("cookie", format!("{AUTH_COOKIE_NAME}=")),
                None,
            ),
            (
                "a cookie under another name",
                headers("cookie", format!("some_other_token={}", session(1))),
                None,
            ),
            (
                "a garbage subprotocol token",
                headers("sec-websocket-protocol", "bearer.not-a-jwt".to_string()),
                None,
            ),
            ("a garbage query token", HeaderMap::new(), Some("not-a-jwt")),
        ] {
            assert_eq!(
                ws_session(&headers_in, query, SECRET).user,
                None,
                "a handshake presenting {what} was read as a session"
            );
        }
    }

    /// Precedence is cookie first, and it is load-bearing for the echo: the
    /// subprotocol that lost must not be selected in the response, because the
    /// server would then be claiming a protocol it did not authenticate with.
    #[test]
    fn the_cookie_is_preferred_over_a_subprotocol_offered_alongside_it() {
        let cookie_session = session(1);
        let protocol_session = session(2);
        let mut h = headers("cookie", format!("{AUTH_COOKIE_NAME}={cookie_session}"));
        h.insert(
            "sec-websocket-protocol",
            format!("bearer.{protocol_session}").parse().unwrap(),
        );

        let resolved = ws_session(&h, None, SECRET);
        assert_eq!(resolved.user, ws_user(1, 0));
        assert!(
            resolved.protocol_echo.is_none(),
            "the cookie won, so the unused subprotocol must not be selected"
        );
    }
}
