//! Personal Access Token / JWT authentication middleware.
//!
//! Bridges Personal Access Tokens (PATs) to the JWT-only API handlers and
//! extracts the authenticated actor for the Git-over-HTTP endpoints.

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use sea_orm::DatabaseConnection;

use crate::api;
use crate::error;
use crate::AppState;

/// API docs endpoint access requires an authenticated JWT/PAT bearer token.
pub(crate) async fn docs_auth_middleware(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if api::auth::extract_bearer_claims(req.headers(), &state.jwt_secret).is_none() {
        let mut response =
            (StatusCode::UNAUTHORIZED, "api docs requires authentication").into_response();
        if let Ok(challenge) = HeaderValue::from_str("Bearer") {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, challenge);
        }
        return response;
    }
    next.run(req).await
}

/// Resolve a Personal Access Token, honouring expiry.
async fn resolve_pat(
    db: &DatabaseConnection,
    token: &str,
) -> Option<rg_db::entities::access_token::Model> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let hash = format!("{:x}", hasher.finalize());

    let tok = rg_db::ops::token_ops::find_by_hash(db, &hash)
        .await
        .ok()??;
    if let Some(expires_at) = tok.expires_at {
        if expires_at < chrono::Utc::now() {
            return None; // expired
        }
    }
    Some(tok)
}

/// Scope required when a PAT is used for a REST request.
///
/// A scope only controls which API family the token may enter. Normal user,
/// repository and administrator authorization is still enforced by handlers.
fn required_pat_scope(path: &str) -> Option<&'static str> {
    if path.starts_with("/api-docs") {
        return None;
    }
    // Axum may expose either the original URI or the path with the nested
    // `/api/v1` prefix stripped, depending on which router layer is running.
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    if path.starts_with("/admin") || path == "/runners/register" {
        return Some("admin");
    }
    if path.starts_with("/users")
        || path.starts_with("/notifications")
        || path.starts_with("/auth")
        || path == "/ws/notifications"
    {
        return Some("user");
    }
    Some("repo")
}

/// Axum middleware: translate a valid Personal Access Token into an equivalent
/// `Authorization: Bearer <JWT>` so the JWT-only API handlers accept PATs.
///
/// Requests already carrying a valid JWT, or no credentials, pass through
/// unchanged. The PAT may be presented as `Bearer <pat>`, `token <pat>`, or
/// HTTP Basic auth (`user:pat`).
pub(crate) async fn pat_auth_middleware(
    State(state): State<AppState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let required_scope = required_pat_scope(req.uri().path());
    match pat_to_bearer_jwt(&state, req.headers(), required_scope).await {
        Ok(Some(jwt)) => {
            if let Ok(value) = format!("Bearer {jwt}").parse() {
                req.headers_mut().insert(header::AUTHORIZATION, value);
            }
        }
        Ok(None) => {}
        Err(()) => {
            return error::AppError::forbidden("personal access token scope denied").into_response()
        }
    }
    next.run(req).await
}

/// If the Authorization header carries a valid PAT (and not already a valid
/// JWT), return a freshly-minted JWT for the token's owner. Returns the
/// existing JWT for a Basic-auth request that carries one. Otherwise `None`.
async fn pat_to_bearer_jwt(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    required_scope: Option<&str>,
) -> Result<Option<String>, ()> {
    let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(None);
    };

    let candidates: Vec<String> = if let Some(t) = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("token "))
    {
        // A valid JWT bearer needs no translation.
        if rg_core::auth::jwt::validate_token(t, &state.jwt_secret).is_some() {
            return Ok(None);
        }
        vec![t.to_string()]
    } else if let Some(encoded) = auth.strip_prefix("Basic ") {
        use base64::Engine as _;
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            return Ok(None);
        };
        let Ok(creds) = String::from_utf8(decoded) else {
            return Ok(None);
        };
        let Some((user, pass)) = creds.split_once(':') else {
            return Ok(None);
        };
        // Token may be in either the password (`user:token`) or username field.
        [pass, user]
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|c| c.to_string())
            .collect()
    } else {
        return Ok(None);
    };

    for cand in candidates {
        // A JWT carried via Basic auth: pass it through as a Bearer token.
        if rg_core::auth::jwt::validate_token(&cand, &state.jwt_secret).is_some() {
            return Ok(Some(cand));
        }
        if let Some(pat) = resolve_pat(&state.db, &cand).await {
            if required_scope
                .is_some_and(|scope| !rg_core::auth::pat_scope::has_scope(&pat.scopes, scope))
            {
                return Err(());
            }
            let username = rg_db::ops::user_ops::find_by_id(&state.db, pat.user_id)
                .await
                .ok()
                .flatten()
                .map(|u| u.username)
                .unwrap_or_default();
            if let Ok(jwt) =
                rg_core::auth::jwt::generate_token(pat.user_id, &username, &state.jwt_secret, 1)
            {
                return Ok(Some(jwt));
            }
        }
    }
    Ok(None)
}

/// Extract the authenticated user id from a git-over-HTTP request.
///
/// Supports both JWT session tokens and Personal Access Tokens (PATs),
/// presented either as `Authorization: Bearer <token>` or HTTP Basic auth
/// (`git clone https://user:<token>@host/...`). Returns `None` for anonymous
/// access (public repos still work; private repos are then rejected).
pub(crate) async fn extract_actor_id(
    db: &DatabaseConnection,
    headers: &axum::http::HeaderMap,
    jwt_secret: &str,
) -> Option<i64> {
    let auth_str = headers.get(header::AUTHORIZATION)?.to_str().ok()?;

    if let Some(token) = auth_str.strip_prefix("Bearer ") {
        // JWT session token first, then fall back to a PAT.
        if let Some(claims) = rg_core::auth::jwt::validate_token(token, jwt_secret) {
            return claims.sub.parse().ok();
        }
        return resolve_pat(db, token)
            .await
            .filter(|pat| rg_core::auth::pat_scope::has_scope(&pat.scopes, "repo"))
            .map(|pat| pat.user_id);
    }

    if let Some(encoded) = auth_str.strip_prefix("Basic ") {
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let credentials = String::from_utf8(decoded).ok()?;
        let (username, password) = credentials.split_once(':')?;
        // Git clients carry the token in either the password (`user:token`) or
        // the username (`token:x-oauth-basic`) field — try both, JWT then PAT.
        for candidate in [password, username] {
            if candidate.is_empty() {
                continue;
            }
            if let Some(claims) = rg_core::auth::jwt::validate_token(candidate, jwt_secret) {
                return claims.sub.parse().ok();
            }
            if let Some(pat) = resolve_pat(db, candidate).await {
                if rg_core::auth::pat_scope::has_scope(&pat.scopes, "repo") {
                    return Some(pat.user_id);
                }
            }
        }
    }

    None
}
