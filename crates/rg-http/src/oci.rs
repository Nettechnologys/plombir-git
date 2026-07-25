//! OCI Distribution HTTP handlers.
//!
//! Implements OCI Distribution Spec v1.0 endpoints at `/v2/`.
//! Each handler follows the OCI error response format (RFC 7807).

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
    error_codes, media_types, ErrorDetail, ErrorResponse, ParsedManifest, Reference,
    TagListResponse, API_VERSION,
};

use crate::AppState;

// Docker distribution custom headers
const DOCKER_CONTENT_DIGEST: HeaderName = HeaderName::from_static("docker-content-digest");
const DOCKER_UPLOAD_UUID: HeaderName = HeaderName::from_static("docker-upload-uuid");
const RANGE: HeaderName = HeaderName::from_static("range");
const DOCKER_API_VERSION: HeaderName = HeaderName::from_static("docker-distribution-api-version");

// ── helpers ──────────────────────────────────────────────────

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

/// H-3: Extract Bearer JWT token, returning user_id.
/// Intentionally separate from `auth::extract_user_id` because OCI endpoints
/// also accept OCI-scoped bearer tokens (not just user JWTs).
/// For standard user auth, use `auth::AuthUser` extractor or `auth::extract_user_id`.
fn extract_user(headers: &HeaderMap, jwt_secret: &str) -> Option<i64> {
    let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = auth.strip_prefix("Bearer ")?;

    // Try normal JWT first (sub is user_id)
    if let Some(claims) = jwt::validate_token(token, jwt_secret) {
        return claims.sub.parse().ok();
    }

    // Try OCI Bearer token (sub is username)
    if let Some(_claims) = validate_oci_token(token, jwt_secret) {
        // OCI token doesn't directly carry user_id
        // Return 0 as sentinel (caller should use check_repo_access for authZ)
        return Some(0);
    }

    None
}

/// Check if the request has access to perform an OCI repo action.
///
/// Normal ForgeKeep JWTs are checked against repo `can_read_repo`/`can_write_repo`.
/// OCI scoped tokens are trusted only for the exact signed scope they carry;
/// token issuance is constrained by the same repo permission checks below.
/// Anonymous pull is allowed only when the backing ForgeKeep repo is public.
async fn check_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    required_action: &str,
) -> anyhow::Result<(bool, Option<i64>)> {
    let repo_model =
        match rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, repo).await? {
            Some(repo) => repo,
            None => return Ok((false, None)),
        };

    // Try normal JWT first (sub is user_id)
    if let Some(claims) = jwt::validate_token(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or(""),
        &state.jwt_secret,
    ) {
        if let Ok(uid) = claims.sub.parse::<i64>() {
            if uid > 0 {
                let allowed = match required_action {
                    "pull" => {
                        rg_core::repo::service::can_read_repo(&state.db, &repo_model, Some(uid))
                            .await?
                    }
                    "push" => {
                        rg_core::repo::service::can_write_repo(&state.db, &repo_model, Some(uid))
                            .await?
                    }
                    _ => false,
                };
                return Ok((allowed, allowed.then_some(uid)));
            }
        }
    }

    // Try OCI Bearer token (scope-based).
    if let Some(claims) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|token| validate_oci_token(token, &state.jwt_secret))
    {
        if let Some(scope_str) = claims.scope {
            for single_scope in scope_str.split_whitespace() {
                if let Some(parsed) = ParsedScope::parse(single_scope) {
                    if parsed.matches_repo(owner, repo) && parsed.has_action(required_action) {
                        return Ok((true, None));
                    }
                }
            }
        }
    }

    // For pull, allow anonymous only when the backing ForgeKeep repo is public.
    if required_action == "pull" {
        let allowed = rg_core::repo::service::can_read_repo(&state.db, &repo_model, None).await?;
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
        Err(e) => Err(oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string())),
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
pub async fn api_version_check(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // Return 401 to trigger Docker auth flow
    let _ = extract_user(&headers, &state.jwt_secret);

    let realm = format!("{}/v2/token", get_base_url(&headers));
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
/// otherwise (missing header, malformed value, unknown user, bad password).
async fn authenticate_basic(db: &DatabaseConnection, headers: &HeaderMap) -> (String, Option<i64>) {
    let anonymous = || ("anonymous".to_string(), None);

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

    match rg_db::ops::user_ops::find_by_username(db, user).await {
        Ok(Some(u))
            if rg_core::auth::password::verify_password(pass, &u.password_hash)
                .unwrap_or(false) =>
        {
            (user.to_string(), Some(u.id))
        }
        _ => anonymous(),
    }
}

/// Resolve a single `repository:...` scope request into the granted scope
/// string, or `None` when the repo is missing or the caller has no access.
async fn grant_repository_scope(
    db: &DatabaseConnection,
    parsed: &ParsedScope,
    authenticated_user_id: Option<i64>,
) -> Option<String> {
    let (scope_owner, scope_repo) = parse_namespace(&parsed.name)?;
    let repo_model =
        match rg_core::repo::service::find_repo_by_owner_name(db, scope_owner, scope_repo).await {
            Ok(Some(repo)) => repo,
            _ => return None,
        };

    let mut allowed_actions = Vec::new();
    if parsed.has_action("pull") {
        let can_pull = rg_core::repo::service::can_read_repo(db, &repo_model, authenticated_user_id)
            .await
            .unwrap_or(false);
        if can_pull {
            allowed_actions.push("pull");
        }
    }
    if parsed.has_action("push") {
        if let Some(user_id) = authenticated_user_id {
            let can_push = rg_core::repo::service::can_write_repo(db, &repo_model, Some(user_id))
                .await
                .unwrap_or(false);
            if can_push {
                allowed_actions.push("push");
            }
        }
    }

    if allowed_actions.is_empty() {
        None
    } else {
        Some(format!(
            "repository:{}:{}",
            parsed.name,
            allowed_actions.join(",")
        ))
    }
}

/// Evaluate every requested scope against the caller's permissions and return
/// the subset of scope strings that are actually granted.
async fn resolve_granted_scopes(
    db: &DatabaseConnection,
    scope: &str,
    authenticated_user_id: Option<i64>,
) -> Vec<String> {
    let mut granted_scopes = Vec::new();
    for scope_part in scope.split_whitespace() {
        let Some(parsed) = ParsedScope::parse(scope_part) else {
            continue;
        };

        if parsed.scope_type == "repository" {
            if let Some(granted) =
                grant_repository_scope(db, &parsed, authenticated_user_id).await
            {
                granted_scopes.push(granted);
            }
        } else if parsed.scope_type == "registry"
            && parsed.name == "catalog"
            && authenticated_user_id.is_some()
        {
            granted_scopes.push(scope_part.to_string());
        }
    }
    granted_scopes
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

    let (username, authenticated_user_id) = authenticate_basic(&state.db, &headers).await;

    let granted_scope = resolve_granted_scopes(&state.db, &scope, authenticated_user_id)
        .await
        .join(" ");

    // Generate token (TTL: 300s for normal, 60s for anonymous)
    let ttl = if username == "anonymous" { 60 } else { 300 };
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
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string()),
    };

    let tags = match rg_db::ops::oci_ops::list_tags(&state.db, oci_repo.id).await {
        Ok(t) => t,
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string()),
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
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string()),
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
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string()),
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
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string()),
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

    // Verify all referenced blobs exist
    for blob_digest in parsed.referenced_blobs() {
        let exists = match state
            .oci_storage
            .blob_exists(&owner, &repo, &blob_digest)
            .await
        {
            Ok(exists) => exists,
            Err(error) => {
                return oci_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "UNKNOWN",
                    &error.to_string(),
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
    if let Err(e) = state
        .oci_storage
        .store_manifest(&owner, &repo, &parsed.digest, body.as_bytes())
        .await
    {
        return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string());
    }

    let rf = Reference::parse(&reference);
    let tag = if rf.is_tag() {
        match &rf {
            Reference::Tag(t) => Some(t.as_str()),
            _ => None,
        }
    } else {
        None
    };

    // Insert or update manifest in DB
    let result = if let Some(tag) = tag {
        // Check if tag already exists — update it
        match rg_db::ops::oci_ops::find_manifest_by_tag(&state.db, oci_repo.id, tag).await {
            Ok(Some(_)) => {
                rg_db::ops::oci_ops::update_manifest_tag(
                    &state.db,
                    oci_repo.id,
                    tag,
                    &parsed.digest,
                    content_type,
                    parsed.size as i64,
                    &body,
                    parsed.manifest.schema_version as i32,
                    user_id,
                )
                .await
            }
            _ => {
                rg_db::ops::oci_ops::insert_manifest(
                    &state.db,
                    oci_repo.id,
                    &parsed.digest,
                    Some(tag),
                    content_type,
                    parsed.size as i64,
                    &body,
                    parsed.manifest.schema_version as i32,
                    user_id,
                )
                .await
            }
        }
    } else {
        rg_db::ops::oci_ops::insert_manifest(
            &state.db,
            oci_repo.id,
            &parsed.digest,
            None,
            content_type,
            parsed.size as i64,
            &body,
            parsed.manifest.schema_version as i32,
            user_id,
        )
        .await
    };

    let _manifest = match result {
        Ok(m) => m,
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string()),
    };

    // Increment blob ref counts.
    //
    // A blob whose ref count was not incremented is a blob the GC is free to
    // delete while this manifest still points at it — a corrupted image
    // discovered long after the push that caused it. Over-counting on a client
    // retry is harmless, under-counting is not, so a failure here fails the
    // push instead of being swallowed.
    for blob_digest in parsed.referenced_blobs() {
        let blob =
            match rg_db::ops::oci_ops::find_blob(&state.db, oci_repo.id, &blob_digest).await {
                Ok(Some(blob)) => blob,
                Ok(None) => continue,
                Err(e) => {
                    return oci_err(
                        oci_status_for(&e),
                        "UNKNOWN",
                        &format!("failed to look up referenced blob {blob_digest}: {e}"),
                    )
                }
            };
        if let Err(e) = rg_db::ops::oci_ops::increment_blob_ref(&state.db, blob.id).await {
            return oci_err(
                oci_status_for(&e),
                "UNKNOWN",
                &format!("failed to increment the ref count of blob {blob_digest}: {e}"),
            );
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
                &error.to_string(),
            );
        }
    };
    if exists {
        // Get blob size from DB if available
        let size = match find_oci_repo(&state.db, &owner, &repo).await {
            Ok(Some(oci_repo)) => rg_db::ops::oci_ops::find_blob(&state.db, oci_repo.id, &digest)
                .await
                .ok()
                .flatten()
                .map(|b| b.size),
            _ => None,
        };
        let size_hdr = size
            .map(|s| s.to_string())
            .unwrap_or_else(|| "0".to_string());
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
                    Err(_) => {
                        return oci_err(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "UNKNOWN",
                            "failed to stat blob",
                        );
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
            Err(error) => oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &error.to_string(),
            ),
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
            Err(_) => oci_not_found(error_codes::BLOB_UNKNOWN, "blob not found"),
        },
        Err(error) => oci_err(
            StatusCode::BAD_REQUEST,
            error_codes::DIGEST_INVALID,
            &error.to_string(),
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
    let user_id = match require_access(&state, &headers, &owner, &repo, "push").await {
        Ok(user_id) => user_id,
        Err(resp) => return resp,
    };

    // Check for cross-repo mount
    if let (Some(mount), Some(from)) = (params.get("mount"), params.get("from")) {
        return handle_mount(&state, &headers, &owner, &repo, mount, from).await;
    }

    let oci_repo = match find_or_create_oci_repo(&state.db, &owner, &repo, user_id).await {
        Ok(r) => r,
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string()),
    };

    match state.oci_storage.create_upload(&owner, &repo).await {
        Ok((uuid, upload_path)) => {
            // Record upload in DB
            if let Err(e) =
                rg_db::ops::oci_ops::create_upload(&state.db, oci_repo.id, &uuid, &upload_path)
                    .await
            {
                return oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string());
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
        Err(e) => oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
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

    // Check source blob exists
    if !state
        .oci_storage
        .blob_exists(from_owner, from_repo, mount_digest)
        .await
        .unwrap_or(false)
    {
        return oci_not_found(error_codes::BLOB_UNKNOWN, "mount source blob not found");
    }

    // Copy blob file via hardlink (or fallback to streaming copy) — avoids memory copy
    match state
        .oci_storage
        .copy_blob_file(from_owner, from_repo, owner, repo, mount_digest)
        .await
    {
        Ok(_) => {
            let location = format!("/v2/{owner}/{repo}/blobs/{mount_digest}");
            (
                StatusCode::CREATED,
                [
                    (header::LOCATION, location.as_str()),
                    (DOCKER_CONTENT_DIGEST, mount_digest),
                ],
                String::new(),
            )
                .into_response()
        }
        Err(e) => oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
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

    // Verify upload session exists
    match rg_db::ops::oci_ops::find_upload(&state.db, &uuid).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return oci_not_found(error_codes::BLOB_UPLOAD_UNKNOWN, "upload session not found");
        }
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string()),
    }

    // Stream body to upload file
    let file_path = state.oci_storage.upload_file(&owner, &repo, &uuid);
    match stream_body_to_file(body, &file_path).await {
        Ok(total_size) => {
            // The `Range` below tells the client where to resume from. Reporting
            // it while the session row still holds the old offset hands the
            // client a position the server does not agree with, so a failed
            // progress write has to fail the chunk rather than be swallowed.
            if let Err(e) =
                rg_db::ops::oci_ops::update_upload_progress(&state.db, &uuid, total_size).await
            {
                return oci_err(
                    oci_status_for(&e),
                    "UNKNOWN",
                    &format!("failed to record upload progress for session {uuid}: {e}"),
                );
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
        Err(e) => oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
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
    let user_id = match require_access(&state, &headers, &owner, &repo, "push").await {
        Ok(user_id) => user_id,
        Err(resp) => return resp,
    };
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

    // If body is provided (single-chunk upload), stream it to the upload file first
    // Check by reading the first frame: if there's data, stream the rest
    let file_path = state.oci_storage.upload_file(&owner, &repo, &uuid);
    if let Err(e) = stream_body_to_file(body, &file_path).await {
        return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string());
    }

    let oci_repo = match find_or_create_oci_repo(&state.db, &owner, &repo, user_id).await {
        Ok(r) => r,
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &e.to_string()),
    };

    // Finalize: stream-read upload file, verify digest, move to blob storage
    match state
        .oci_storage
        .finalize_upload(&owner, &repo, &uuid, &expected_digest)
        .await
    {
        Ok((digest, size, storage_path)) => {
            // Record blob in DB.
            //
            // The bytes are already in blob storage; without this row nothing
            // can find them. Answering `201 Created` anyway is how a push
            // "succeeds" into a repository whose very next `HEAD .../blobs/`
            // returns 404 — the client has no reason to retry something it was
            // told worked, so the failure has to reach it.
            if let Err(e) = rg_db::ops::oci_ops::insert_blob(
                &state.db,
                oci_repo.id,
                &digest,
                "application/octet-stream",
                size,
                &storage_path,
            )
            .await
            {
                return oci_err(
                    oci_status_for(&e),
                    "UNKNOWN",
                    &format!("failed to record blob {digest}: {e}"),
                );
            }

            // Clean up upload session. The blob is committed at this point, so a
            // failure here leaks a session row rather than losing data — worth a
            // warning, not a failed push.
            if let Err(e) = rg_db::ops::oci_ops::delete_upload(&state.db, &uuid).await {
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
        Err(e) => oci_err(
            StatusCode::BAD_REQUEST,
            error_codes::DIGEST_INVALID,
            &e.to_string(),
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
        file.write_all(&data).await.map_err(|error| staged(&error))?;
    }

    let size = file
        .metadata()
        .await
        .map_err(|error| staged(&error))?
        .len() as i64;
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

async fn find_or_create_oci_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    owner_id: Option<i64>,
) -> anyhow::Result<rg_db::entities::oci_repository::Model> {
    let forgekeep_repo = if let Some(id) = owner_id.filter(|&id| id > 0) {
        rg_db::ops::repo_ops::find_by_owner_and_name(db, id, repo)
            .await?
            .ok_or_else(|| anyhow::anyhow!("repository {}/{} not found", owner, repo))?
    } else {
        rg_core::repo::service::find_repo_by_owner_name(db, owner, repo)
            .await?
            .ok_or_else(|| anyhow::anyhow!("repository {}/{} not found", owner, repo))?
    };

    let namespace = format!("{}/{}", owner, repo);
    rg_db::ops::oci_ops::find_or_create_repo(
        db,
        forgekeep_repo.id,
        &namespace,
        owner_id.unwrap_or(0),
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
