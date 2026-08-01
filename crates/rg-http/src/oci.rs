//! OCI Distribution HTTP handlers.
//!
//! Implements OCI Distribution Spec v1.0 endpoints at `/v2/`.
//! Each handler follows the OCI error response format (RFC 7807).

use anyhow::Context as _;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sea_orm::DatabaseConnection;
use tokio::io::AsyncWriteExt;

use rg_core::auth::jwt;
use rg_core::auth::oci_token::{
    build_www_authenticate, generate_oci_token, validate_oci_token, ParsedScope,
};
use rg_core::package_registry::oci::{
    error_codes, is_client_digest_fault, media_types, ErrorDetail, ErrorResponse, FinalizedBlob,
    ParsedManifest, Reference, StoredManifest, TagListResponse, API_VERSION,
};

use crate::api::repo_access;
use crate::error::AppError;
use crate::AppState;

// Docker distribution custom headers
const DOCKER_CONTENT_DIGEST: HeaderName = HeaderName::from_static("docker-content-digest");
const DOCKER_UPLOAD_UUID: HeaderName = HeaderName::from_static("docker-upload-uuid");
const RANGE: HeaderName = HeaderName::from_static("range");
const DOCKER_API_VERSION: HeaderName = HeaderName::from_static("docker-distribution-api-version");

/// The `sub` an OCI token minted without credentials carries. It is a literal,
/// not a username: no account may hold it, and `token_subject` reads it as
/// "there is nobody behind this token", not as "look this account up".
const ANONYMOUS_SUBJECT: &str = "anonymous";

// ── helpers ──────────────────────────────────────────────────

/// Build an OCI `{errors:[{code,message}]}` envelope.
///
/// `message` is the only diagnostic a `docker push` / `pull` ever prints, and
/// this path does not go through `AppError`, so nothing else logs the cause.
/// Callers must therefore pass `&format!("{e:#}")`, never `&e.to_string()` —
/// the latter prints the outermost `.context(...)` alone and drops the io /
/// digest / db error underneath it (card_a997f30c142c).
fn oci_err(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorResponse {
            errors: vec![ErrorDetail {
                code: code.to_string(),
                message: message.to_string(),
                detail: None,
            }],
        }),
    )
        .into_response()
}

fn oci_not_found(code: &str, message: &str) -> Response {
    oci_err(StatusCode::NOT_FOUND, code, message)
}

/// One actionable line for a filesystem failure on a local OCI blob.
///
/// `OciStorage::blob_local_path` derives the file from the digest inside the
/// storage backend, so neither the request nor the OCI error envelope ever
/// names it — a `docker pull` against a repo_root whose bind-mount lost its
/// permissions reports `failed to stat blob`, or a bare errno, and the operator
/// has no file to go and look at.
fn blob_path_error(what: &str, path: &std::path::Path, error: &std::io::Error) -> String {
    rg_core::platform::fs::describe_path_error(
        what,
        path,
        error,
        rg_core::platform::fs::BLOB_STORAGE_HINT,
    )
}

/// Classify a DB-layer error into the HTTP status for an OCI response, while
/// leaving the OCI error-envelope untouched.
///
/// The registry can't route errors through `AppError` the way the JSON API
/// does: docker/podman expect the OCI-conformant `{errors:[{code,message}]}`
/// body that `oci_err` emits, and swapping in the `AppError` JSON would break
/// the protocol. So instead of converting the error, we classify only the
/// *status*: a connection-level `sea_orm::DbErr` (pool closed / acquire timeout
/// / dropped connection) is a transient, retryable outage → 503; everything
/// else stays 500. The outage predicate is shared with the JSON API via
/// `AppError::is_db_outage`, so a database outage on `/v2/...` classifies
/// identically to one on the rest of the API.
///
/// Implemented for both error shapes the DB sites surface: `find_oci_repo` /
/// `check_access` / `find_or_create_oci_repo` return `anyhow::Result` (a
/// `DbErr` wrapped through `.context()`), while the `rg_db::ops::oci_ops::*`
/// helpers return `Result<_, sea_orm::DbErr>` directly.
trait OciDbStatus {
    fn oci_status(&self) -> StatusCode;
}

impl OciDbStatus for sea_orm::DbErr {
    fn oci_status(&self) -> StatusCode {
        if crate::error::AppError::is_db_outage(self) {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

impl OciDbStatus for anyhow::Error {
    fn oci_status(&self) -> StatusCode {
        // `downcast_ref` sees through any `.context()` layers to the original
        // `DbErr`, mirroring `From<anyhow::Error> for AppError`.
        match self.downcast_ref::<sea_orm::DbErr>() {
            Some(db_err) => db_err.oci_status(),
            None => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Status for a DB-touching error, preserving the OCI envelope: connection-level
/// DB outages become 503, all else 500. See [`OciDbStatus`].
fn oci_status_for<E: OciDbStatus>(e: &E) -> StatusCode {
    e.oci_status()
}

fn oci_unauthorized(message: &str) -> Response {
    oci_err(StatusCode::UNAUTHORIZED, error_codes::UNAUTHORIZED, message)
}

/// The `Authorization: Bearer` token this request carries, whichever kind.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// The ForgeKeep user behind a normal user JWT in `Authorization: Bearer`.
///
/// Deliberately *not* `api::auth::extract_user_id`: that one also accepts the
/// browser session cookie, and the registry is not a browser surface. An OCI
/// scoped token yields nothing here — it names a scope, not a user, and used to
/// be reported as the sentinel user `0`, a user id that exists in no database
/// and only ever reached a permission check by accident.
fn bearer_user_id(headers: &HeaderMap, jwt_secret: &str) -> Option<i64> {
    let claims = jwt::validate_token(bearer_token(headers)?, jwt_secret)?;
    claims.sub.parse::<i64>().ok().filter(|uid| *uid > 0)
}

/// Fold the shared repository gate's verdict into the boolean this protocol
/// needs, without losing the difference the gate draws: a denial is "not
/// allowed", a check that could not *run* stays an error. Collapsing the second
/// into the first is what makes a database outage look like a credentials
/// problem — the registry then answers 401, and docker comes straight back for
/// another token instead of backing off.
fn granted(decision: Result<(), AppError>) -> Result<bool, AppError> {
    match decision {
        Ok(()) => Ok(true),
        Err(error) if repo_access::is_access_denial(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Who an OCI scoped token was minted for, as far as the *present* is
/// concerned.
///
/// A scoped token names its subject by username, which is not something the
/// repository gate takes and not something the revocation middleware can read
/// (`api::auth::session_standing_middleware` parses `sub` as a user id, so an
/// OCI token is invisible to it). Resolving the name back to a live account is
/// therefore this file's job, and it is the same question
/// `api::lfs::signer_still_stands` asks of a signed URL's actor.
enum TokenSubject {
    /// The token names no account at all — issued to `anonymous`, so the gate
    /// answers it as it answers a caller with no credentials.
    Anonymous,
    /// A live account the gate can be asked about.
    User(i64),
    /// The named account is gone or deactivated. The token keeps its signature
    /// and its scope; it no longer has anybody behind it.
    Gone,
}

/// Resolve a scoped token's `sub` into the actor the repository gate takes.
///
/// Fails closed on a denial ([`TokenSubject::Gone`]) and keeps a *failed*
/// lookup an error, so a database outage stays a 503 instead of collapsing into
/// the 401 that sends docker straight back for another token.
async fn token_subject(state: &AppState, sub: &str) -> Result<TokenSubject, AppError> {
    if sub == ANONYMOUS_SUBJECT {
        return Ok(TokenSubject::Anonymous);
    }
    match rg_db::ops::user_ops::find_by_username(&state.db, sub).await {
        Ok(Some(user)) if user.is_usable() => Ok(TokenSubject::User(user.id)),
        Ok(_) => {
            tracing::warn!(
                subject = sub,
                "rejecting an OCI scoped token: the account behind it is disabled or gone"
            );
            Ok(TokenSubject::Gone)
        }
        Err(error) => {
            tracing::error!(
                subject = sub,
                error = %format!("{error:#}"),
                "could not verify the account behind an OCI scoped token"
            );
            Err(AppError::from(error))
        }
    }
}

/// Check if the request has access to perform an OCI repo action.
///
/// The registry decides *who* is calling — a ForgeKeep user JWT, an OCI scoped
/// bearer token, or nobody — and the shared repository gate in
/// `api::repo_access` decides what that caller may do. That holds for the
/// scoped token too: it names its caller by username rather than by id, so
/// resolving it costs a lookup ([`token_subject`]), but the permission decision
/// is still the gate's and is still taken now rather than read off a capability
/// minted up to five minutes ago.
async fn check_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    required_action: &str,
) -> Result<(bool, Option<i64>), AppError> {
    let repo_model = match rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, repo)
        .await
        .map_err(AppError::from)?
    {
        Some(repo) => repo,
        None => return Ok((false, None)),
    };

    // A normal ForgeKeep JWT is a user, so the answer is the same one the REST
    // API would give that user for this repository.
    if let Some(uid) = bearer_user_id(headers, &state.jwt_secret) {
        let allowed = match required_action {
            "pull" => granted(repo_access::check_read_for(state, &repo_model, Some(uid)).await)?,
            "push" => granted(repo_access::check_write_for(state, &repo_model, Some(uid)).await)?,
            _ => false,
        };
        return Ok((allowed, allowed.then_some(uid)));
    }

    // An OCI scoped token. Its scope is a *capability*: `get_token` ran this
    // same gate to mint it, and the scope string is that answer, frozen. What
    // it does not carry is the answer's shelf life — drop the collaborator or
    // deactivate the account and the token keeps saying `pull,push` for the
    // rest of its 300 seconds, which is time enough to push a tag into a
    // private registry. So the scope only decides whether this token is *about*
    // this repository and this action; whether that is still allowed is asked
    // again, of the gate, against the account the token names.
    if let Some(claims) =
        bearer_token(headers).and_then(|token| validate_oci_token(token, &state.jwt_secret))
    {
        let scoped_for_this = claims
            .scope
            .iter()
            .flat_map(|s| s.split_whitespace())
            .any(|s| {
                ParsedScope::parse(s).is_some_and(|parsed| {
                    parsed.matches_repo(owner, repo) && parsed.has_action(required_action)
                })
            });

        if scoped_for_this {
            let actor = match token_subject(state, &claims.sub).await? {
                TokenSubject::Anonymous => None,
                TokenSubject::User(uid) => Some(uid),
                // Nobody behind the token: it grants nothing of its own. The
                // request carries on as an unauthenticated one, so a public
                // repository still answers a pull — the token cannot leave its
                // holder worse off than presenting no credentials at all.
                TokenSubject::Gone => {
                    return anonymous_pull(state, &repo_model, required_action).await
                }
            };

            let allowed = match required_action {
                "pull" => granted(repo_access::check_read_for(state, &repo_model, actor).await)?,
                "push" => granted(repo_access::check_write_for(state, &repo_model, actor).await)?,
                _ => false,
            };
            if allowed {
                return Ok((true, actor));
            }
            // Denied on this token's own terms; the public-pull fallback below
            // is the only thing that can still admit it.
        }
    }

    anonymous_pull(state, &repo_model, required_action).await
}

/// What this repository owes a caller with no credentials: a public repository
/// answers a pull, everything else is a denial.
async fn anonymous_pull(
    state: &AppState,
    repo_model: &rg_db::entities::repository::Model,
    required_action: &str,
) -> Result<(bool, Option<i64>), AppError> {
    if required_action == "pull" {
        let allowed = granted(repo_access::check_read_for(state, repo_model, None).await)?;
        return Ok((allowed, None));
    }

    Ok((false, None))
}

async fn require_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    required_action: &str,
) -> Result<Option<i64>, Response> {
    match check_access(state, headers, owner, repo, required_action).await {
        Ok((true, user_id)) => Ok(user_id),
        Ok((false, _)) => Err(oci_unauthorized("authentication required")),
        // The gate's own status (503 on a database outage, 500 otherwise)
        // inside the OCI envelope docker expects.
        Err(e) => Err(oci_err(e.status(), "UNKNOWN", &e.to_string())),
    }
}

/// Resolve owner/repo from OCI namespace string.
/// In ForgeKeep, the OCI name is always "{owner}/{repo}".
fn parse_namespace(name: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = name.splitn(2, '/').collect();
    if parts.len() == 2 {
        Some((parts[0], parts[1]))
    } else {
        None
    }
}

/// Build the WWW-Authenticate header for Docker auth challenge.
fn www_authenticate(realm: &str, service: &str, scope: &str) -> String {
    build_www_authenticate(realm, service, scope)
}

// ── API Version Check ────────────────────────────────────────

/// `GET /v2/` — API version check.
/// Docker clients call this first to verify the registry is available.
/// Returns 401 with WWW-Authenticate if authentication is required.
pub async fn api_version_check(State(_state): State<AppState>, headers: HeaderMap) -> Response {
    // Unconditionally 401 + challenge: this endpoint exists so a client learns
    // *where* to get its token, and it answers the same to everyone. Nothing is
    // authenticated here, which is why no credential is read — the token this
    // used to validate and then discard proved nothing about the response.

    // The realm is not decoration: a client does not guess where to get its
    // token, it reads this path out of the challenge and goes there. It has to
    // be the path `build_v2_routes` registers for `oci::get_token`.
    let realm = format!("{}/v2/auth/token", get_base_url(&headers));
    let service = "forgekeep-registry";

    (
        StatusCode::UNAUTHORIZED,
        [
            (DOCKER_API_VERSION, API_VERSION),
            (
                header::WWW_AUTHENTICATE,
                www_authenticate(&realm, service, "registry:catalog:*").as_str(),
            ),
        ],
    )
        .into_response()
}

// ── Token Endpoint ─────────────────────────────────────
//
// `GET /v2/auth/token` — OCI Distribution token endpoint.
//
// Query parameters:
//   - `service`: The service name (must match `aud` in token)
//   - `scope`: Requested scope (e.g., `repository:alice/hello:pull,push`)
//   - `offline_token`: (optional) for refreshing
//   - `client_id`: (optional) client identifier
//
// Authentication:
//   - Anonymous: returns token with limited scope (public pull)
//   - Basic Auth: validates username/password, returns full scope token
//
/// Resolve Basic-auth credentials from the request headers.
///
/// Returns the authenticated `(username, user_id)` when valid Docker-login
/// credentials are present, or the anonymous default (`"anonymous"`, `None`)
/// when the request carries no usable credentials (missing header, malformed
/// value, unknown user, bad password).
///
/// A credential check that *failed* is `Err`, never the anonymous default: an
/// unreachable database or a stored hash the verifier cannot parse says nothing
/// about who is calling, and answering "anonymous" mints a token whose scope was
/// narrowed for a reason the client can't see. Docker then bounces off the first
/// pull with a 401 — the one answer that sends it straight back to this endpoint
/// — and loops instead of surfacing the outage.
async fn authenticate_basic(
    db: &DatabaseConnection,
    headers: &HeaderMap,
) -> anyhow::Result<(String, Option<i64>)> {
    let anonymous = || Ok((ANONYMOUS_SUBJECT.to_string(), None));

    let Some(auth_header) = headers.get(header::AUTHORIZATION) else {
        return anonymous();
    };
    let Ok(auth_str) = auth_header.to_str() else {
        return anonymous();
    };
    let Some(b64) = auth_str.strip_prefix("Basic ") else {
        return anonymous();
    };
    use base64::Engine as _;
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return anonymous();
    };
    let Ok(creds) = std::str::from_utf8(&decoded) else {
        return anonymous();
    };
    let parts: Vec<&str> = creds.splitn(2, ':').collect();
    let [user, pass] = parts[..] else {
        return anonymous();
    };

    // Verify unconditionally — an unknown username must cost the same Argon2
    // work as a real one, or the response time enumerates accounts. (A failed
    // *lookup* returns early without that work, which leaks nothing: the
    // database is either up for every account or down for all of them.)
    let found = rg_db::ops::user_ops::find_by_username(db, user)
        .await
        .with_context(|| format!("registry basic auth: looking up '{user}'"))?;
    let password_ok = rg_core::auth::password::verify_password_or_dummy(
        pass,
        found.as_ref().map(|u| u.password_hash.as_str()),
    )
    .with_context(|| format!("registry basic auth: verifying the password of '{user}'"))?;

    // Settled after the hash, so neither a deactivated nor a locked account is
    // distinguishable from a wrong password by how fast the registry says no.
    // The same helper runs on the SSH password door: without it `docker login`
    // was an unmetered, unlogged place to guess passwords the web login stops
    // after five tries.
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(headers);
    let attempt = rg_core::auth::lockout::settle_password_attempt(
        db,
        found.as_ref(),
        password_ok,
        rg_core::auth::lockout::AttemptOrigin {
            login: user,
            channel: "registry",
            ip_address: ip_address.as_deref(),
            user_agent: user_agent.as_deref(),
        },
    )
    .await;

    match attempt {
        rg_core::auth::lockout::PasswordAttempt::Accepted => {
            let user_id = found
                .as_ref()
                .map(|found| found.id)
                .expect("an accepted password attempt resolved to an account");
            Ok((user.to_string(), Some(user_id)))
        }
        rg_core::auth::lockout::PasswordAttempt::Rejected { .. } => anonymous(),
    }
}

/// Resolve a single `repository:...` scope request into the granted scope
/// string, or `Ok(None)` when the repo is missing or the caller has no access.
///
/// A permission query that *failed* is reported as `Err`, not folded into
/// "no access": silently narrowing the scope hands the client a token that
/// then bounces off every pull with a 401, so docker re-runs the auth flow in a
/// loop instead of seeing the outage and backing off.
async fn grant_repository_scope(
    state: &AppState,
    parsed: &ParsedScope,
    authenticated_user_id: Option<i64>,
) -> Result<Option<String>, AppError> {
    let Some((scope_owner, scope_repo)) = parse_namespace(&parsed.name) else {
        return Ok(None);
    };
    let repo_model =
        match rg_core::repo::service::find_repo_by_owner_name(&state.db, scope_owner, scope_repo)
            .await
            .map_err(AppError::from)?
        {
            Some(repo) => repo,
            None => return Ok(None),
        };

    // Minting a scope *is* an access decision — it hands out a capability the
    // pull/push handlers then trust without re-deriving it — so it is taken by
    // the same gate, against the same user, as a request on the REST API.
    let mut allowed_actions = Vec::new();
    if parsed.has_action("pull")
        && granted(repo_access::check_read_for(state, &repo_model, authenticated_user_id).await)?
    {
        allowed_actions.push("pull");
    }
    if parsed.has_action("push")
        && authenticated_user_id.is_some()
        && granted(repo_access::check_write_for(state, &repo_model, authenticated_user_id).await)?
    {
        allowed_actions.push("push");
    }

    Ok(if allowed_actions.is_empty() {
        None
    } else {
        Some(format!(
            "repository:{}:{}",
            parsed.name,
            allowed_actions.join(",")
        ))
    })
}

/// Evaluate every requested scope against the caller's permissions and return
/// the subset of scope strings that are actually granted.
async fn resolve_granted_scopes(
    state: &AppState,
    scope: &str,
    authenticated_user_id: Option<i64>,
) -> Result<Vec<String>, AppError> {
    let mut granted_scopes = Vec::new();
    for scope_part in scope.split_whitespace() {
        let Some(parsed) = ParsedScope::parse(scope_part) else {
            continue;
        };

        if parsed.scope_type == "repository" {
            if let Some(scope) =
                grant_repository_scope(state, &parsed, authenticated_user_id).await?
            {
                granted_scopes.push(scope);
            }
        } else if parsed.scope_type == "registry"
            && parsed.name == "catalog"
            && authenticated_user_id.is_some()
        {
            granted_scopes.push(scope_part.to_string());
        }
    }
    Ok(granted_scopes)
}

/// `GET /v2/auth/token` — issue an OCI Bearer token.
pub async fn get_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let _service = params
        .get("service")
        .cloned()
        .unwrap_or_else(|| "forgekeep-registry".to_string());
    let scope = params.get("scope").cloned().unwrap_or_default();

    let (username, authenticated_user_id) = match authenticate_basic(&state.db, &headers).await {
        Ok(identity) => identity,
        Err(e) => {
            // Nothing downstream logs this — `oci_err` only writes the client's
            // envelope — and the message is deliberately generic: the caller
            // gets a status it can back off on, the operator gets the account
            // and the cause.
            tracing::error!(error = %format!("{e:#}"), "registry basic auth could not be evaluated");
            return oci_err(
                oci_status_for(&e),
                "UNKNOWN",
                "authentication is temporarily unavailable",
            );
        }
    };

    // A token minted while the permission lookups were failing would carry a
    // silently narrowed scope, and the client would spend the next pull being
    // told 401 — the one answer that makes it come straight back here. Report
    // the outage instead so docker/podman can back off and retry.
    let granted_scope = match resolve_granted_scopes(&state, &scope, authenticated_user_id).await {
        Ok(scopes) => scopes.join(" "),
        Err(e) => return oci_err(e.status(), "UNKNOWN", &e.to_string()),
    };

    // Generate token (TTL: 300s for normal, 60s for anonymous)
    let ttl = if username == ANONYMOUS_SUBJECT {
        60
    } else {
        300
    };
    let token = match generate_oci_token(&username, &granted_scope, &state.jwt_secret, ttl) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Failed to generate OCI token: {}", e);
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                "token generation failed",
            );
        }
    };

    // Return token response (OCI Distribution Spec format)
    Json(serde_json::json!({
        "token": token,
        "expires_in": ttl,
        "issued_at": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

// ── Tags ─────────────────────────────────────────────────────

/// `GET /v2/{owner}/{repo}/tags/list` — list tags.
pub async fn list_tags(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "pull").await {
        return resp;
    }

    let oci_repo = match find_oci_repo(&state.db, &owner, &repo).await {
        Ok(Some(r)) => r,
        Ok(None) => return oci_not_found(error_codes::NAME_UNKNOWN, "repository not found"),
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    let tags = match rg_db::ops::oci_ops::list_tags(&state.db, oci_repo.id).await {
        Ok(t) => t,
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    (
        StatusCode::OK,
        Json(TagListResponse {
            name: format!("{owner}/{repo}"),
            tags,
        }),
    )
        .into_response()
}

// ── Manifest ─────────────────────────────────────────────────

/// `HEAD /v2/{owner}/{repo}/manifests/{reference}` — check manifest existence.
pub async fn head_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, reference)): Path<(String, String, String)>,
) -> Response {
    get_manifest_impl(State(state), headers, Path((owner, repo, reference)), true).await
}

/// `GET /v2/{owner}/{repo}/manifests/{reference}` — pull manifest.
pub async fn get_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, reference)): Path<(String, String, String)>,
) -> Response {
    get_manifest_impl(State(state), headers, Path((owner, repo, reference)), false).await
}

async fn get_manifest_impl(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, reference)): Path<(String, String, String)>,
    head_only: bool,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "pull").await {
        return resp;
    }

    let oci_repo = match find_oci_repo(&state.db, &owner, &repo).await {
        Ok(Some(r)) => r,
        Ok(None) => return oci_not_found(error_codes::NAME_UNKNOWN, "repository not found"),
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    let rf = Reference::parse(&reference);

    // Look up manifest
    let manifest = match &rf {
        Reference::Digest(d) => {
            rg_db::ops::oci_ops::find_manifest_by_digest(&state.db, oci_repo.id, d).await
        }
        Reference::Tag(t) => {
            rg_db::ops::oci_ops::find_manifest_by_tag(&state.db, oci_repo.id, t).await
        }
    };

    let manifest = match manifest {
        Ok(Some(m)) => m,
        Ok(None) => return oci_not_found(error_codes::MANIFEST_UNKNOWN, "manifest not found"),
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    // Compute digest in Docker format
    let docker_digest = format!(
        "{}:{}",
        manifest.digest.split(':').next().unwrap_or("sha256"),
        manifest
            .digest
            .split(':')
            .nth(1)
            .unwrap_or(&manifest.digest)
    );

    if head_only {
        (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, manifest.media_type.as_str()),
                (header::CONTENT_LENGTH, manifest.size.to_string().as_str()),
                (DOCKER_CONTENT_DIGEST, docker_digest.as_str()),
            ],
            String::new(),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, manifest.media_type.as_str()),
                (DOCKER_CONTENT_DIGEST, docker_digest.as_str()),
            ],
            manifest.manifest_json,
        )
            .into_response()
    }
}

/// `PUT /v2/{owner}/{repo}/manifests/{reference}` — push manifest.
pub async fn put_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, reference)): Path<(String, String, String)>,
    body: String,
) -> Response {
    let user_id = match require_access(&state, &headers, &owner, &repo, "push").await {
        Ok(user_id) => user_id,
        Err(resp) => return resp,
    };

    let oci_repo = match find_oci_repo(&state.db, &owner, &repo).await {
        Ok(Some(r)) => r,
        Ok(None) => return oci_not_found(error_codes::NAME_UNKNOWN, "repository not found"),
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    // Validate media type
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !media_types::MANIFEST_TYPES.contains(&content_type) {
        // Try OCI types too
        if content_type != media_types::OCI_MANIFEST_V1
            && content_type != media_types::OCI_INDEX_V1
            && content_type != media_types::MANIFEST_V2
            && content_type != media_types::MANIFEST_LIST_V2
        {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::MANIFEST_INVALID,
                "unsupported manifest media type",
            );
        }
    }

    // Parse manifest
    let parsed = match ParsedManifest::parse(body.as_bytes()) {
        Ok(p) => p,
        Err(e) => {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::MANIFEST_INVALID,
                &format!("invalid manifest: {e}"),
            );
        }
    };

    let referenced_blobs = parsed.referenced_blobs();

    // Verify all referenced blobs exist
    for blob_digest in &referenced_blobs {
        let exists = match state
            .oci_storage
            .blob_exists(&owner, &repo, blob_digest)
            .await
        {
            Ok(exists) => exists,
            Err(error) => {
                return oci_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "UNKNOWN",
                    &format!("{error:#}"),
                );
            }
        };
        if !exists {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::MANIFEST_BLOB_UNKNOWN,
                &format!("blob {} not found", blob_digest),
            );
        }
    }

    // Store manifest on disk
    let stored_manifest = match state
        .oci_storage
        .store_manifest(&owner, &repo, &parsed.digest, body.as_bytes())
        .await
    {
        Ok(stored) => stored,
        Err(e) => {
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!("{e:#}"),
            );
        }
    };

    let rf = Reference::parse(&reference);
    let tag = if rf.is_tag() {
        match &rf {
            Reference::Tag(t) => Some(t.as_str()),
            _ => None,
        }
    } else {
        None
    };

    // Insert or update manifest in DB. Digest references are content-addressed;
    // tag writes keep lookup, replacement and blob-reference claims in the same
    // transaction, so no database failure can turn into an INSERT or leave a
    // tag deleted after a failed move.
    let manifest_write = if let Some(tag) = tag {
        rg_db::ops::oci_ops::upsert_tag_manifest(
            &state.db,
            oci_repo.id,
            tag,
            &parsed.digest,
            content_type,
            parsed.size as i64,
            &body,
            parsed.manifest.schema_version as i32,
            user_id,
            &referenced_blobs,
        )
        .await
        .map(|_| ())
    } else {
        rg_db::ops::oci_ops::insert_digest_manifest(
            &state.db,
            oci_repo.id,
            &parsed.digest,
            content_type,
            parsed.size as i64,
            &body,
            parsed.manifest.schema_version as i32,
            user_id,
            &referenced_blobs,
        )
        .await
        .map(|_| ())
    };

    match manifest_write {
        Ok(_) => {}
        Err(e) => {
            tracing::error!(
                owner,
                repo,
                reference,
                digest = %parsed.digest,
                error = %format!("{e:#}"),
                "failed to record OCI manifest"
            );
            rollback_unrecorded_manifest(&state, &owner, &repo, &parsed.digest, &stored_manifest)
                .await;
            return oci_err(oci_status_for(&e), "UNKNOWN", "failed to record manifest");
        }
    }

    let docker_digest = format!(
        "{}:{}",
        parsed.digest.split(':').next().unwrap_or("sha256"),
        parsed.digest.split(':').nth(1).unwrap_or(&parsed.digest),
    );

    (
        StatusCode::CREATED,
        [
            (DOCKER_CONTENT_DIGEST, docker_digest.as_str()),
            (
                header::LOCATION,
                format!(
                    "/v2/{owner}/{repo}/manifests/{digest}",
                    digest = parsed.digest
                )
                .as_str(),
            ),
        ],
        String::new(),
    )
        .into_response()
}

/// Remove a manifest object published by a request whose database transaction failed.
///
/// A content-addressed retry may find bytes from an earlier successful push.
/// Those are never ours to delete. A final database recheck also protects the
/// narrow race where another request committed the same digest while this one
/// was failing.
async fn rollback_unrecorded_manifest(
    state: &AppState,
    owner: &str,
    repo: &str,
    digest: &str,
    stored: &StoredManifest,
) {
    if !stored.published {
        return;
    }

    match find_oci_repo(&state.db, owner, repo).await {
        Ok(Some(oci_repo)) => {
            match rg_db::ops::oci_ops::find_manifest_by_digest(&state.db, oci_repo.id, digest).await
            {
                Ok(Some(_)) => return,
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        %owner,
                        %repo,
                        %digest,
                        storage_path = %stored.storage_path,
                        error = %error,
                        "possibly orphaned OCI manifest: the database recheck failed, so rollback kept the object"
                    );
                    return;
                }
            }
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                %owner,
                %repo,
                %digest,
                storage_path = %stored.storage_path,
                error = %format!("{error:#}"),
                "possibly orphaned OCI manifest: the repository recheck failed, so rollback kept the object"
            );
            return;
        }
    }

    let cleanup = match rg_core::blob_storage::BlobKey::new(&stored.storage_path) {
        Ok(key) => state
            .blob_storage
            .delete(&key)
            .await
            .err()
            .map(|error| error.to_string()),
        Err(error) => Some(error.to_string()),
    };
    if let Some(reason) = cleanup {
        tracing::warn!(
            %owner,
            %repo,
            %digest,
            storage_path = %stored.storage_path,
            error = %reason,
            "orphaned OCI manifest: the oci_manifests row was not created and the rollback delete failed too"
        );
    }
}

// ── Blob ─────────────────────────────────────────────────────

/// `HEAD /v2/{owner}/{repo}/blobs/{digest}` — check blob existence.
pub async fn head_blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, digest)): Path<(String, String, String)>,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "pull").await {
        return resp;
    }

    let exists = match state.oci_storage.blob_exists(&owner, &repo, &digest).await {
        Ok(exists) => exists,
        Err(error) => {
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!("{error:#}"),
            );
        }
    };
    if exists {
        // The metadata row is the registry's source of truth for the size. Bytes
        // without that row are an inconsistent object, not a zero-byte blob.
        let oci_repo = match find_oci_repo(&state.db, &owner, &repo).await {
            Ok(Some(oci_repo)) => oci_repo,
            Ok(None) => {
                return oci_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "UNKNOWN",
                    &format!(
                        "OCI repository metadata is missing for {owner}/{repo}, but blob {digest} exists in storage"
                    ),
                );
            }
            Err(error) => {
                return oci_err(oci_status_for(&error), "UNKNOWN", &format!("{error:#}"));
            }
        };
        let size = match rg_db::ops::oci_ops::find_blob(&state.db, oci_repo.id, &digest).await {
            Ok(Some(blob)) => blob.size,
            Ok(None) => {
                return oci_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "UNKNOWN",
                    &format!(
                        "OCI blob metadata is missing for {owner}/{repo}@{digest}, but the blob exists in storage"
                    ),
                );
            }
            Err(error) => {
                return oci_err(oci_status_for(&error), "UNKNOWN", &format!("{error:#}"));
            }
        };
        let size_hdr = size.to_string();
        (
            StatusCode::OK,
            [
                (header::CONTENT_LENGTH, size_hdr.as_str()),
                (DOCKER_CONTENT_DIGEST, digest.as_str()),
            ],
            String::new(),
        )
            .into_response()
    } else {
        oci_not_found(error_codes::BLOB_UNKNOWN, "blob not found")
    }
}

/// `GET /v2/{owner}/{repo}/blobs/{digest}` — pull blob (download layer).
/// Streams the blob file directly — never loads the entire blob into memory.
pub async fn get_blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, digest)): Path<(String, String, String)>,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "pull").await {
        return resp;
    }

    match state.oci_storage.blob_local_path(&owner, &repo, &digest) {
        Ok(Some(path)) => match tokio::fs::File::open(&path).await {
            Ok(file) => {
                let size = match file.metadata().await {
                    Ok(m) => m.len(),
                    Err(error) => {
                        let message = blob_path_error("OCI blob", &path, &error);
                        // This handler keeps the OCI error envelope and so does
                        // not route through `AppError` — nothing else logs it.
                        tracing::error!(error = %message, "failed to stat OCI blob");
                        return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &message);
                    }
                };
                let stream = tokio_util::io::ReaderStream::new(file);
                let stream_body =
                    http_body_util::StreamBody::new(futures::StreamExt::map(stream, |item| {
                        item.map(http_body::Frame::data)
                    }));
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, "application/octet-stream"),
                        (header::CONTENT_LENGTH, size.to_string().as_str()),
                        (DOCKER_CONTENT_DIGEST, digest.as_str()),
                    ],
                    Body::new(stream_body),
                )
                    .into_response()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                oci_not_found(error_codes::BLOB_UNKNOWN, "blob not found")
            }
            Err(error) => {
                let message = blob_path_error("OCI blob", &path, &error);
                tracing::error!(error = %message, "failed to open OCI blob");
                oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &message)
            }
        },
        Ok(None) => match state.oci_storage.read_blob(&owner, &repo, &digest).await {
            Ok(data) => {
                // Remote (non-local) OCI storage returns the whole blob as a
                // `Vec`; the local-path branch above already streams from disk.
                // Serve the buffer as a backpressure-sensitive, idle-guarded
                // stream so a slow/stalled client can't pin the blob-sized `Vec`
                // in server memory until the kernel resets the dead connection
                // (card_444e03f1ca15).
                let len = data.len();
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, "application/octet-stream"),
                        (header::CONTENT_LENGTH, len.to_string().as_str()),
                        (DOCKER_CONTENT_DIGEST, digest.as_str()),
                    ],
                    crate::http_stream::buffered_body_with_idle(data, state.git_idle_timeout_secs),
                )
                    .into_response()
            }
            // Only a blob the registry genuinely does not have is BLOB_UNKNOWN.
            // Discarding the error made a blob store that is present but
            // unreachable indistinguishable from a missing layer, and `docker
            // pull` reports that as a broken image — sending the operator to
            // inspect the manifest while the storage is what is down.
            Err(error) if error.downcast_ref::<rg_core::error::NotFound>().is_some() => {
                oci_not_found(error_codes::BLOB_UNKNOWN, "blob not found")
            }
            Err(error) => oci_err(oci_status_for(&error), "UNKNOWN", &format!("{error:#}")),
        },
        Err(error) => oci_err(
            StatusCode::BAD_REQUEST,
            error_codes::DIGEST_INVALID,
            &format!("{error:#}"),
        ),
    }
}

// ── Upload ───────────────────────────────────────────────────

/// `POST /v2/{owner}/{repo}/blobs/uploads/` — start a blob upload session.
pub async fn start_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "push").await {
        return resp;
    }

    // Check for cross-repo mount
    if let (Some(mount), Some(from)) = (params.get("mount"), params.get("from")) {
        return handle_mount(&state, &headers, &owner, &repo, mount, from).await;
    }

    let oci_repo = match find_or_create_oci_repo(&state.db, &owner, &repo).await {
        Ok(r) => r,
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    match state.oci_storage.create_upload(&owner, &repo).await {
        Ok((uuid, upload_path)) => {
            // Record upload in DB
            if let Err(e) =
                rg_db::ops::oci_ops::create_upload(&state.db, oci_repo.id, &uuid, &upload_path)
                    .await
            {
                return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}"));
            }

            let location = format!("/v2/{owner}/{repo}/blobs/uploads/{uuid}");
            (
                StatusCode::ACCEPTED,
                [
                    (header::LOCATION, location.as_str()),
                    (RANGE, "0-0"),
                    (DOCKER_UPLOAD_UUID, uuid.as_str()),
                ],
                String::new(),
            )
                .into_response()
        }
        Err(e) => oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &format!("{e:#}"),
        ),
    }
}

/// Handle cross-repository blob mount.
async fn handle_mount(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    mount_digest: &str,
    from: &str,
) -> Response {
    let (from_owner, from_repo) = match parse_namespace(from) {
        Some(p) => p,
        None => {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::NAME_INVALID,
                "invalid mount source",
            );
        }
    };

    if let Err(resp) = require_access(state, headers, from_owner, from_repo, "pull").await {
        return resp;
    }
    if let Err(resp) = require_access(state, headers, owner, repo, "push").await {
        return resp;
    }

    // Check source blob exists. A backend that cannot answer is not the same as
    // an answer of "no": `unwrap_or(false)` turned an unreachable blob store
    // into a 404, which tells the pushing client the source image is missing
    // layers. `head_blob` and `put_manifest` already keep the two apart.
    match state
        .oci_storage
        .blob_exists(from_owner, from_repo, mount_digest)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return oci_not_found(error_codes::BLOB_UNKNOWN, "mount source blob not found");
        }
        Err(error) => {
            return oci_err(oci_status_for(&error), "UNKNOWN", &format!("{error:#}"));
        }
    }

    // The row that owns the mounted blob must exist before bytes are copied.
    // Otherwise a repository-row failure leaves an object behind that no OCI
    // read or reclamation path can discover.
    let oci_repo = match find_or_create_oci_repo(&state.db, owner, repo).await {
        Ok(repo) => repo,
        Err(error) => return oci_err(oci_status_for(&error), "UNKNOWN", &format!("{error:#}")),
    };

    // Copy blob file via hardlink (or fallback to streaming copy) — avoids memory copy
    match state
        .oci_storage
        .copy_blob_file(from_owner, from_repo, owner, repo, mount_digest)
        .await
    {
        Ok(blob) => {
            let FinalizedBlob {
                digest,
                size,
                storage_path,
                published,
            } = blob;
            if let Err(error) = rg_db::ops::oci_ops::insert_blob(
                &state.db,
                oci_repo.id,
                &digest,
                "application/octet-stream",
                size,
                &storage_path,
            )
            .await
            {
                rollback_unrecorded_blob(state, owner, repo, &digest, &storage_path, published)
                    .await;
                return oci_err(
                    oci_status_for(&error),
                    "UNKNOWN",
                    &format!("failed to record mounted blob {digest}: {error}"),
                );
            }

            let location = format!("/v2/{owner}/{repo}/blobs/{digest}");
            (
                StatusCode::CREATED,
                [
                    (header::LOCATION, location.as_str()),
                    (DOCKER_CONTENT_DIGEST, digest.as_str()),
                ],
                String::new(),
            )
                .into_response()
        }
        Err(e) => oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &format!("{e:#}"),
        ),
    }
}

/// Remove bytes published by a request whose database row was not recorded.
///
/// A deduplicated publish must never be rolled back: those bytes predate this
/// request and may already be referenced by a successful push or mount.
async fn rollback_unrecorded_blob(
    state: &AppState,
    owner: &str,
    repo: &str,
    digest: &str,
    storage_path: &str,
    published: bool,
) {
    if !published {
        return;
    }

    let cleanup = match rg_core::blob_storage::BlobKey::new(storage_path) {
        Ok(key) => state
            .blob_storage
            .delete(&key)
            .await
            .err()
            .map(|error| error.to_string()),
        Err(error) => Some(error.to_string()),
    };
    if let Some(reason) = cleanup {
        tracing::warn!(
            %owner,
            %repo,
            %digest,
            %storage_path,
            error = %reason,
            "orphaned OCI blob: the oci_blobs row was not created and the rollback delete failed too"
        );
    }
}

/// Fetch an upload session and re-anchor it to the repository the caller was
/// authorized for.
///
/// `{uuid}` is an instance-wide key: `oci_upload` rows are found by it alone,
/// while `require_access` only ever proves something about the `{owner}/{repo}`
/// in the path. Resolving the session without tying the two together let a
/// caller with `push` on *any* repository name somebody else's active session
/// and drive the writes that follow — the progress row and, on finalize, the
/// row's deletion. The bytes never crossed over (the staging path is built from
/// `{owner}/{repo}`), so what leaked was not content but control: the victim's
/// client was handed a `Range` for a session it no longer owned.
///
/// A session in another repository answers exactly like one that never existed
/// — `404 BLOB_UPLOAD_UNKNOWN` — because the alternative confirms the uuid to
/// whoever guessed it, and OCI clients already know that code.
///
/// It is a named helper rather than an inline comparison because
/// `global_id_anchor_guard` can read a call and cannot read an `if`.
async fn upload_in_repo(
    state: &AppState,
    owner: &str,
    repo: &str,
    uuid: &str,
) -> Result<rg_db::entities::oci_upload::Model, Response> {
    let unknown = || oci_not_found(error_codes::BLOB_UPLOAD_UNKNOWN, "upload session not found");

    // No `oci_repository` row means no session can belong here — including the
    // case where the repository itself is gone. `find_oci_repo` never creates.
    let oci_repo = match find_oci_repo(&state.db, owner, repo).await {
        Ok(Some(oci_repo)) => oci_repo,
        Ok(None) => return Err(unknown()),
        Err(e) => return Err(oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}"))),
    };

    match rg_db::ops::oci_ops::find_upload(&state.db, uuid).await {
        Ok(Some(upload)) if upload.oci_repository_id == oci_repo.id => Ok(upload),
        Ok(_) => Err(unknown()),
        Err(e) => Err(oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}"))),
    }
}

/// `PATCH /v2/{owner}/{repo}/blobs/uploads/{uuid}` — chunked upload.
/// Streams the request body directly to the upload file—never buffers
/// the entire chunk in memory.
pub async fn chunk_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, uuid)): Path<(String, String, String)>,
    body: Body,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "push").await {
        return resp;
    }

    // Verify the upload session exists *in the gated repository*.
    let upload = match upload_in_repo(&state, &owner, &repo, &uuid).await {
        Ok(upload) => upload,
        Err(resp) => return resp,
    };

    // Stream body to upload file
    let file_path = state.oci_storage.upload_file(&owner, &repo, &uuid);
    match stream_body_to_file(body, &file_path).await {
        Ok(total_size) => {
            // The `Range` below tells the client where to resume from. Reporting
            // it while the session row still holds the old offset hands the
            // client a position the server does not agree with, so a failed
            // progress write has to fail the chunk rather than be swallowed.
            match rg_db::ops::oci_ops::update_upload_progress(
                &state.db,
                upload.oci_repository_id,
                &uuid,
                total_size,
            )
            .await
            {
                Ok(0) => {
                    // The row was there a moment ago and matched this
                    // repository, so nothing was written because the session
                    // has since gone (expiry sweep, a concurrent finalize).
                    // Reporting a `Range` off a row that no longer exists is
                    // the same lie as reporting one off a stale row.
                    return oci_not_found(
                        error_codes::BLOB_UPLOAD_UNKNOWN,
                        "upload session not found",
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    return oci_err(
                        oci_status_for(&e),
                        "UNKNOWN",
                        &format!("failed to record upload progress for session {uuid}: {e}"),
                    );
                }
            }

            let range_end = total_size.saturating_sub(1);
            let location = format!("/v2/{owner}/{repo}/blobs/uploads/{uuid}");
            (
                StatusCode::ACCEPTED,
                [
                    (header::LOCATION, location.as_str()),
                    (RANGE, format!("0-{range_end}").as_str()),
                    (DOCKER_UPLOAD_UUID, uuid.as_str()),
                ],
                String::new(),
            )
                .into_response()
        }
        Err(e) => oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &format!("{e:#}"),
        ),
    }
}

/// `PUT /v2/{owner}/{repo}/blobs/uploads/{uuid}?digest=sha256:...` — finalize upload.
/// If the body contains data (single-chunk upload), streams it to the upload file first.
pub async fn complete_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, uuid)): Path<(String, String, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    body: Body,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "push").await {
        return resp;
    }
    let expected_digest = match params.get("digest") {
        Some(d) => d.clone(),
        None => {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::DIGEST_INVALID,
                "digest parameter required",
            );
        }
    };

    // The session this finalizes must belong to the gated repository. The
    // staging file below is keyed by `{owner}/{repo}` and so was never anybody
    // else's, but the row is not: finalizing ends by deleting the session, and
    // keyed on the uuid alone that delete landed on whichever repository held
    // it — a stranger's `docker push` cancelled from outside.
    let upload = match upload_in_repo(&state, &owner, &repo, &uuid).await {
        Ok(upload) => upload,
        Err(resp) => return resp,
    };

    // If body is provided (single-chunk upload), stream it to the upload file first
    // Check by reading the first frame: if there's data, stream the rest
    let file_path = state.oci_storage.upload_file(&owner, &repo, &uuid);
    if let Err(e) = stream_body_to_file(body, &file_path).await {
        return oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &format!("{e:#}"),
        );
    }

    // The repository the session was anchored to, not a second lookup that
    // could resolve — or create — a different one.
    let oci_repo_id = upload.oci_repository_id;

    // Finalize: stream-read upload file, verify digest, move to blob storage
    match state
        .oci_storage
        .finalize_upload(&owner, &repo, &uuid, &expected_digest)
        .await
    {
        Ok(blob) => {
            let FinalizedBlob {
                digest,
                size,
                storage_path,
                published,
            } = blob;
            // Record blob in DB. `insert_blob` treats the content-addressed
            // `(repository, digest)` conflict as an idempotent success, so a
            // retry and two concurrent finalizers both reach the 201 below.
            //
            // The bytes are already in blob storage; without this row nothing
            // can find them. Answering `201 Created` anyway is how a push
            // "succeeds" into a repository whose very next `HEAD .../blobs/`
            // returns 404 — the client has no reason to retry something it was
            // told worked, so the failure has to reach it.
            if let Err(e) = rg_db::ops::oci_ops::insert_blob(
                &state.db,
                oci_repo_id,
                &digest,
                "application/octet-stream",
                size,
                &storage_path,
            )
            .await
            {
                // Compensation on the error path, and only where it is ours to
                // make: `published` means this request is what put the bytes at
                // that key, so nothing else can be pointing at them. Every way
                // of reaching an OCI blob for deletion goes through its
                // `oci_blobs` row, so bytes left without one are unreachable —
                // a layer-sized object no reclamation, present or future, can
                // come back for.
                //
                // The `false` arm is not an omission. Finalizing deduplicates: a
                // key that already held these bytes is reused untouched, and an
                // earlier push has a row for it. Deleting there would answer a
                // failed push by making somebody else's image unpullable, which
                // is strictly worse than the leak this compensates.
                //
                // Duplicate digests never enter this branch. A real database
                // failure must still reach the client, so a failed rollback
                // can only be reported here.
                rollback_unrecorded_blob(&state, &owner, &repo, &digest, &storage_path, published)
                    .await;
                return oci_err(
                    oci_status_for(&e),
                    "UNKNOWN",
                    &format!("failed to record blob {digest}: {e}"),
                );
            }

            // Clean up upload session. The blob is committed at this point, so a
            // failure here leaks a session row rather than losing data — worth a
            // warning, not a failed push.
            if let Err(e) = rg_db::ops::oci_ops::delete_upload(&state.db, oci_repo_id, &uuid).await
            {
                tracing::warn!(
                    upload_uuid = %uuid,
                    error = %format!("{e:#}"),
                    "failed to delete the OCI upload session after committing its blob; \
                     the session row is orphaned"
                );
            }

            let location = format!("/v2/{owner}/{repo}/blobs/{digest}");
            (
                StatusCode::CREATED,
                [
                    (header::LOCATION, location.as_str()),
                    (DOCKER_CONTENT_DIGEST, digest.as_str()),
                ],
                String::new(),
            )
                .into_response()
        }
        // Finalizing fails for three unrelated reasons and only one of them is
        // the client's: a digest that does not match the bytes. An unreadable
        // staging file (`_oci_uploads/` gone or not writable) and a blob store
        // that refuses the publish are both ours. `docker push` does not retry
        // a 400 — it prints `digest invalid` and stops — so answering one for a
        // broken `repo_root` tells the operator to go inspect an image that was
        // never the problem. Ask the error which side is at fault.
        Err(e) if is_client_digest_fault(&e) => oci_err(
            StatusCode::BAD_REQUEST,
            error_codes::DIGEST_INVALID,
            &format!("{e:#}"),
        ),
        Err(e) => oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &format!("{e:#}"),
        ),
    }
}

// ── stream helper ──────────────────────────────────────────────

/// Stream an Axum `Body` to a file, appending to any existing content.
/// Never buffers the entire body in memory—each frame is written directly.
/// Returns the total file size after the write.
///
/// This is the write path of every `docker push`: the staging path is derived
/// from `repo_root` plus a generated upload UUID, so a bare `?` on the io error
/// hands the client an errno and nothing else. Every failure names the file.
async fn stream_body_to_file(body: Body, file_path: &std::path::Path) -> anyhow::Result<i64> {
    let staged = |error: &std::io::Error| {
        rg_core::platform::fs::path_error(
            "OCI upload file",
            file_path,
            error,
            "chunked OCI uploads are staged in `_oci_uploads/` under the `[server].repo_root` \
             directory; that directory must be writable by the user running forgekeep",
        )
    };

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)
        .await
        .map_err(|error| staged(&error))?;

    use futures::StreamExt;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let data = chunk.map_err(|e| anyhow::anyhow!("body stream error: {}", e))?;
        file.write_all(&data)
            .await
            .map_err(|error| staged(&error))?;
    }

    // `tokio::fs::File` buffers, and `write_all` returns once the bytes are
    // queued for the blocking pool — not once they are in the file. `metadata`
    // asks the *file*, so without this flush the size below is whatever had
    // landed by then: under load a chunk reads back as the offset before it,
    // and that number is both the `Range` the client resumes from and the
    // `bytes_uploaded` recorded for the session. The client then re-sends bytes
    // it already sent, and the push dies at the digest — as the client's fault.
    file.flush().await.map_err(|error| staged(&error))?;

    let size = file.metadata().await.map_err(|error| staged(&error))?.len() as i64;
    Ok(size)
}

// ── DB helpers ────────────────────────────────────────────────

/// Find an OCI repository, auto-creating if it doesn't exist.
/// Uses the ForgeKeep repo as the owner.
async fn find_oci_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
) -> anyhow::Result<Option<rg_db::entities::oci_repository::Model>> {
    // Look up the ForgeKeep repository
    let forgekeep_repo = rg_core::repo::service::find_repo_by_owner_name(db, owner, repo).await?;
    match forgekeep_repo {
        Some(r) => {
            let oci_repo = rg_db::ops::oci_ops::find_repo_by_id(db, r.id).await?;
            Ok(oci_repo)
        }
        None => Ok(None),
    }
}

/// The `oci_repository` row for `owner/repo`, created on the first push.
///
/// The namespace is `owner/repo` — the path the client asked for — and it is
/// resolved as such. It used to be resolved against *the caller* instead
/// whenever one was known (`find_by_owner_and_name(actor_id, repo)`), which is
/// only the same query when the caller happens to be the owner. A collaborator
/// pushing the first image of a repository they do not own looked up
/// `their-id/repo`, found nothing, and got `500 repository not found` for a
/// push the gate had just allowed. Nobody noticed because the actor reaching
/// here was almost always `None`: docker authenticates with a scoped token, and
/// that branch of `check_access` returned no actor at all.
///
/// `owner_id` on the row means the namespace's owner, so it comes off the
/// repository, not off whoever happens to be pushing.
async fn find_or_create_oci_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
) -> anyhow::Result<rg_db::entities::oci_repository::Model> {
    let forgekeep_repo = rg_core::repo::service::find_repo_by_owner_name(db, owner, repo)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository {}/{} not found", owner, repo))?;

    let namespace = format!("{}/{}", owner, repo);
    rg_db::ops::oci_ops::find_or_create_repo(
        db,
        forgekeep_repo.id,
        &namespace,
        forgekeep_repo.owner_id,
    )
    .await
    .map_err(Into::into)
}

fn get_base_url(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|host| {
            let scheme = if host.starts_with("localhost") || host.starts_with("127.") {
                "http"
            } else {
                "https"
            };
            format!("{}://{}", scheme, host)
        })
        .unwrap_or_else(|| "http://localhost".into())
}

#[cfg(test)]
mod blob_path_error_tests {
    use super::*;

    /// `docker pull` prints the OCI envelope message and nothing else, and this
    /// handler does not route through `AppError`, so a blob whose file is
    /// unreadable used to reach the operator as `failed to stat blob` — no
    /// path, no errno — or as an errno with the path stripped off.
    #[test]
    fn blob_failure_names_the_file_and_the_remedy() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("blobs").join("sha256").join("deadbeef");
        let error = std::fs::File::open(&missing).unwrap_err();

        let rendered = blob_path_error("OCI blob", &missing, &error);

        assert!(
            rendered.contains(&missing.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("OCI blob"), "{rendered}");
        assert!(rendered.contains("[server].repo_root"), "{rendered}");
    }
}
