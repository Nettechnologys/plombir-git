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
/// Normal IronForge JWTs are checked against repo `can_read_repo`/`can_write_repo`.
/// OCI scoped tokens are trusted only for the exact signed scope they carry;
/// token issuance is constrained by the same repo permission checks below.
/// Anonymous pull is allowed only when the backing IronForge repo is public.
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

    // For pull, allow anonymous only when the backing IronForge repo is public.
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
        Err(e) => Err(oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &e.to_string(),
        )),
    }
}

/// Resolve owner/repo from OCI namespace string.
/// In IronForge, the OCI name is always "{owner}/{repo}".
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
    let service = "ironforge-registry";

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
/// `GET /v2/auth/token` — issue an OCI Bearer token.
pub async fn get_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let _service = params
        .get("service")
        .cloned()
        .unwrap_or_else(|| "ironforge-registry".to_string());
    let scope = params.get("scope").cloned().unwrap_or_default();

    // Default: anonymous token (limited scope)
    let mut username = "anonymous".to_string();
    let mut authenticated_user_id = None;

    // Check for Basic Auth (Docker login)
    if let Some(auth_header) = headers.get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(b64) = auth_str.strip_prefix("Basic ") {
                use base64::Engine as _;
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(b64) {
                    if let Ok(creds) = std::str::from_utf8(&decoded) {
                        let parts: Vec<&str> = creds.splitn(2, ':').collect();
                        if parts.len() == 2 {
                            let (user, pass) = (parts[0], parts[1]);
                            // Validate credentials
                            if let Ok(Some(u)) =
                                rg_db::ops::user_ops::find_by_username(&state.db, user).await
                            {
                                // Verify password
                                if rg_core::auth::password::verify_password(pass, &u.password_hash)
                                    .unwrap_or(false)
                                {
                                    username = user.to_string();
                                    authenticated_user_id = Some(u.id);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut granted_scopes = Vec::new();
    for scope_part in scope.split_whitespace() {
        let Some(parsed) = ParsedScope::parse(scope_part) else {
            continue;
        };

        if parsed.scope_type == "repository" {
            let Some((scope_owner, scope_repo)) = parse_namespace(&parsed.name) else {
                continue;
            };
            let repo_model = match rg_core::repo::service::find_repo_by_owner_name(
                &state.db,
                scope_owner,
                scope_repo,
            )
            .await
            {
                Ok(Some(repo)) => repo,
                _ => continue,
            };

            let mut allowed_actions = Vec::new();
            if parsed.has_action("pull") {
                let can_pull = rg_core::repo::service::can_read_repo(
                    &state.db,
                    &repo_model,
                    authenticated_user_id,
                )
                .await
                .unwrap_or(false);
                if can_pull {
                    allowed_actions.push("pull");
                }
            }
            if parsed.has_action("push") {
                if let Some(user_id) = authenticated_user_id {
                    let can_push = rg_core::repo::service::can_write_repo(
                        &state.db,
                        &repo_model,
                        Some(user_id),
                    )
                    .await
                    .unwrap_or(false);
                    if can_push {
                        allowed_actions.push("push");
                    }
                }
            }

            if !allowed_actions.is_empty() {
                granted_scopes.push(format!(
                    "repository:{}:{}",
                    parsed.name,
                    allowed_actions.join(",")
                ));
            }
        } else if parsed.scope_type == "registry"
            && parsed.name == "catalog"
            && authenticated_user_id.is_some()
        {
            granted_scopes.push(scope_part.to_string());
        }
    }

    let granted_scope = granted_scopes.join(" ");

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
        Err(e) => return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
    };

    let tags = match rg_db::ops::oci_ops::list_tags(&state.db, oci_repo.id).await {
        Ok(t) => t,
        Err(e) => return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
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
        Err(e) => return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
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
        Err(e) => return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
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
        Err(e) => return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
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
        Err(e) => return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
    };

    // Increment blob ref counts
    for blob_digest in parsed.referenced_blobs() {
        if let Ok(Some(blob)) =
            rg_db::ops::oci_ops::find_blob(&state.db, oci_repo.id, &blob_digest).await
        {
            let _ = rg_db::ops::oci_ops::increment_blob_ref(&state.db, blob.id).await;
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
            Ok(data) => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "application/octet-stream"),
                    (header::CONTENT_LENGTH, data.len().to_string().as_str()),
                    (DOCKER_CONTENT_DIGEST, digest.as_str()),
                ],
                data,
            )
                .into_response(),
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
        Err(e) => return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
    };

    match state.oci_storage.create_upload(&owner, &repo).await {
        Ok((uuid, upload_path)) => {
            // Record upload in DB
            if let Err(e) =
                rg_db::ops::oci_ops::create_upload(&state.db, oci_repo.id, &uuid, &upload_path)
                    .await
            {
                return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string());
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
        Err(e) => return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
    }

    // Stream body to upload file
    let file_path = state.oci_storage.upload_file(&owner, &repo, &uuid);
    match stream_body_to_file(body, &file_path).await {
        Ok(total_size) => {
            // Update DB
            let _ = rg_db::ops::oci_ops::update_upload_progress(&state.db, &uuid, total_size).await;

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
        Err(e) => return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &e.to_string()),
    };

    // Finalize: stream-read upload file, verify digest, move to blob storage
    match state
        .oci_storage
        .finalize_upload(&owner, &repo, &uuid, &expected_digest)
        .await
    {
        Ok((digest, size, storage_path)) => {
            // Record blob in DB
            let _ = rg_db::ops::oci_ops::insert_blob(
                &state.db,
                oci_repo.id,
                &digest,
                "application/octet-stream",
                size,
                &storage_path,
            )
            .await;

            // Clean up upload session
            let _ = rg_db::ops::oci_ops::delete_upload(&state.db, &uuid).await;

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
async fn stream_body_to_file(body: Body, file_path: &std::path::Path) -> anyhow::Result<i64> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)
        .await?;

    use futures::StreamExt;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let data = chunk.map_err(|e| anyhow::anyhow!("body stream error: {}", e))?;
        file.write_all(&data).await?;
    }

    let size = file.metadata().await?.len() as i64;
    Ok(size)
}

// ── DB helpers ────────────────────────────────────────────────

/// Find an OCI repository, auto-creating if it doesn't exist.
/// Uses the IronForge repo as the owner.
async fn find_oci_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
) -> anyhow::Result<Option<rg_db::entities::oci_repository::Model>> {
    // Look up the IronForge repository
    let ironforge_repo = rg_core::repo::service::find_repo_by_owner_name(db, owner, repo).await?;
    match ironforge_repo {
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
    let ironforge_repo = if let Some(id) = owner_id.filter(|&id| id > 0) {
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
        ironforge_repo.id,
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
