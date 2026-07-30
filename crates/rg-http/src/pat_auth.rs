//! Personal Access Token / JWT authentication middleware.
//!
//! Bridges Personal Access Tokens (PATs) to the JWT-only API handlers and
//! extracts the authenticated actor for the Git-over-HTTP endpoints.

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use sea_orm::DatabaseConnection;

use crate::error;
use crate::AppState;

/// Resolve a Personal Access Token, honouring expiry and the owner's standing.
///
/// Returns the token together with the account it belongs to, so no caller has
/// to remember to look the owner up: a PAT is a standing delegation of that
/// account's rights, and deactivating the account has to revoke it. Resolving
/// the owner here rather than at each call site is what makes that true for
/// both the REST middleware and git-over-HTTP at once.
async fn resolve_pat(
    db: &DatabaseConnection,
    token: &str,
) -> Option<(
    rg_db::entities::access_token::Model,
    rg_db::entities::user::Model,
)> {
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
    let owner = rg_db::ops::user_ops::find_by_id(db, tok.user_id)
        .await
        .ok()??;
    if !owner.is_usable() {
        tracing::warn!(
            user_id = tok.user_id,
            token_id = tok.id,
            "rejecting personal access token: account is disabled or gone"
        );
        return None;
    }
    Some((tok, owner))
}

/// Scope required when a PAT is used for a REST request.
///
/// A scope only controls which API family the token may enter. Normal user,
/// repository and administrator authorization is still enforced by handlers.
///
/// The decision is made from the path *string*, because this middleware is an
/// outer layer: it runs before axum has matched anything, so a `MatchedPath`
/// does not exist yet and the route table cannot be consulted here. That is a
/// constraint, not a licence — the level each route requires is declared in
/// `route_table::Access`, and this function has to agree with it. It drifted
/// once already, which is why `/runners/register` is named literally below.
///
/// `pub` so that agreement can be *checked* rather than reviewed:
/// `route_access_sweep_tests::every_instance_admin_route_demands_the_admin_pat_scope`
/// walks the whole table and holds the two statements against each other in
/// both directions.
pub fn required_pat_scope(path: &str) -> Option<&'static str> {
    if path.starts_with("/api-docs") {
        return None;
    }
    // Axum may expose either the original URI or the path with the nested
    // `/api/v1` prefix stripped, depending on which router layer is running.
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    // `/runners/register` is the one administrative route that does not live
    // under `/admin`: it is what hands a runner its token, so the credential
    // that may call it is an instance-admin session and its declared level is
    // `Access::InstanceAdmin`. Named here rather than inferred, and checked
    // from both sides by the sweep test — if the route stops being
    // instance-admin, this line has to go with it.
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
        if let Some((pat, owner)) = resolve_pat(&state.db, &cand).await {
            if required_scope
                .is_some_and(|scope| !rg_core::auth::pat_scope::has_scope(&pat.scopes, scope))
            {
                return Err(());
            }
            if let Ok(jwt) = rg_core::auth::jwt::generate_token(
                pat.user_id,
                &owner.username,
                &state.jwt_secret,
                1,
            ) {
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
            .filter(|(pat, _)| rg_core::auth::pat_scope::has_scope(&pat.scopes, "repo"))
            .map(|(pat, _)| pat.user_id);
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
            if let Some((pat, _)) = resolve_pat(db, candidate).await {
                if rg_core::auth::pat_scope::has_scope(&pat.scopes, "repo") {
                    return Some(pat.user_id);
                }
            }
        }
    }

    None
}
