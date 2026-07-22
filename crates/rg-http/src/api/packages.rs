//! Package Registry REST API.
//!
//! == Generic REST endpoints ==
//! POST   /api/v1/repos/:owner/:name/packages/:type/publish   — upload package
//! GET    /api/v1/repos/:owner/:name/packages/:type/list       — list packages
//! GET    /api/v1/repos/:owner/:name/packages/:type/:pkg       — get package detail
//! GET    /api/v1/repos/:owner/:name/packages/:type/:pkg/versions — list versions
//! GET    /api/v1/repos/:owner/:name/packages/:type/:pkg/:ver  — get version
//! DELETE /api/v1/repos/:owner/:name/packages/:type/:pkg/:ver  — delete version
//! PATCH  /api/v1/repos/:owner/:name/packages/:type/:pkg/:ver/yank — yank/unyank
//! GET    /api/v1/repos/:owner/:name/packages/:type/:pkg/:ver/:file — download file
//! GET    /api/v1/repos/{owner}/{repo}/packages                — list registries
//!
//! == Protocol-specific endpoints ==
//! GET    /api/v1/repos/{owner}/{repo}/packages/cargo/index/{pkg}  — Cargo sparse index
//! GET    /api/v1/repos/{owner}/{repo}/packages/npm/{pkg}          — npm registry metadata

use crate::error::AppError;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::auth::{extract_ci_job_claims, extract_user_id};
use crate::AppState;

// ── Request / Response types ─────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct PublishPackageQuery {
    /// Package name (can be auto-extracted by adapter if the file is a known format).
    #[serde(default)]
    pub name: Option<String>,
    /// Package version (can be auto-extracted by adapter if the file is a known format).
    #[serde(default)]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semver: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct YankRequest {
    pub yank: bool,
}

#[derive(Serialize)]
pub struct PublishResponse {
    pub package_id: i64,
    pub version_id: i64,
    pub existing: bool,
}

#[derive(Serialize)]
pub struct PackageListResponse {
    pub packages: Vec<rg_core::package_registry::PackageSummary>,
}

#[derive(Serialize)]
pub struct VersionListResponse {
    pub versions: Vec<rg_core::package_registry::VersionDetail>,
}

#[derive(Serialize)]
pub struct RegistryListResponse {
    pub registries: Vec<RegistryEntry>,
}

#[derive(Serialize)]
pub struct RegistryEntry {
    pub package_type: String,
    pub enabled: bool,
}

/// Helper: generate a standardized JSON error response via AppError.
fn err(status: StatusCode, msg: &str) -> axum::response::Response {
    let error = match status {
        StatusCode::BAD_REQUEST => AppError::bad_request(msg),
        StatusCode::NOT_FOUND => AppError::not_found(msg),
        StatusCode::UNAUTHORIZED => AppError::unauthorized(msg),
        StatusCode::FORBIDDEN => AppError::forbidden(msg),
        StatusCode::CONFLICT => AppError::conflict(msg),
        StatusCode::TOO_MANY_REQUESTS => AppError::rate_limited(msg),
        StatusCode::INTERNAL_SERVER_ERROR => AppError::internal(msg),
        _ => AppError::internal(msg),
    };
    error.into_response()
}

/// Helper: plain-text error response.
fn err_text(status: StatusCode, msg: &str) -> axum::response::Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        msg.to_string(),
    )
        .into_response()
}

/// H-3: Thin wrapper over the unified `extract_user_id` from `auth.rs`.
/// New handlers should prefer the `AuthUser` extractor for compile-time auth.
/// This wrapper is retained because packages.rs returns `AppError` (not bare tuples).
fn auth(headers: &axum::http::HeaderMap, secret: &str) -> Result<i64, AppError> {
    extract_user_id(headers, secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))
}

async fn resolve_repo(
    state: &AppState,
    owner: &str,
    name: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, name)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("repository not found"))
}

async fn require_repo_read(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    let repo = resolve_repo(state, owner, name).await?;
    let actor_id = extract_user_id(headers, &state.jwt_secret);
    match rg_core::repo::service::can_read_repo(&state.db, &repo, actor_id).await {
        Ok(true) => Ok(repo),
        Ok(false)
            if actor_id.is_none()
                && extract_ci_job_claims(headers, &state.jwt_secret, repo.id, "packages:read")
                    .is_some() =>
        {
            Ok(repo)
        }
        Ok(false) if repo.is_private && actor_id.is_none() => {
            Err(AppError::unauthorized("authentication required"))
        }
        Ok(false) => Err(AppError::forbidden("access denied")),
        Err(e) => Err(AppError::internal(e)),
    }
}

async fn require_repo_write(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<(rg_db::entities::repository::Model, i64), AppError> {
    let user_id = auth(headers, &state.jwt_secret)?;
    let repo = resolve_repo(state, owner, name).await?;
    match rg_core::repo::service::can_write_repo(&state.db, &repo, Some(user_id)).await {
        Ok(true) => Ok((repo, user_id)),
        Ok(false) => Err(AppError::forbidden("write access denied")),
        Err(e) => Err(AppError::internal(e)),
    }
}

/// Resolve publish metadata: adapter-extracted fields take precedence, then
/// query-param overrides.
type PublishPackageTuple = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn resolve_publish_info(
    query: &PublishPackageQuery,
    adapter_meta: Option<rg_core::package_registry::ExtractedMetadata>,
) -> Result<PublishPackageTuple, String> {
    // If adapter extracted metadata, use it as base; query params override.
    if let Some(meta) = adapter_meta {
        let name = query.name.clone().unwrap_or(meta.name);
        let version = query.version.clone().unwrap_or(meta.version);
        let description = query.description.clone().or(meta.description);
        let homepage = query.homepage.clone().or(meta.homepage);
        let repository_url = query.repository_url.clone().or(meta.repository_url);
        let semver = query.semver.clone().or(meta.semver);
        if name.is_empty() || version.is_empty() {
            return Err(
                "package name and version are required (could not be auto-extracted)".into(),
            );
        }
        return Ok((name, version, description, homepage, repository_url, semver));
    }

    // No adapter extraction — must be in query params.
    let name = query.name.clone().ok_or("package name is required")?;
    let version = query.version.clone().ok_or("package version is required")?;
    Ok((
        name,
        version,
        query.description.clone(),
        query.homepage.clone(),
        query.repository_url.clone(),
        query.semver.clone(),
    ))
}

// ── Generic REST route handlers ──────────────────────────

/// POST /api/v1/repos/:owner/:name/packages/:type/publish
/// Upload a package file.  Name/version are auto-extracted from known
/// package formats (Cargo, npm) if not provided in query params.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/packages/{pkg_type}/publish",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_type" = String, Path, description = "package type"),
    ),
    request_body(
        content = String,
        description = "Package archive/binary payload",
    ),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 200, description = "Updated existing package", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn publish(
    State(state): State<AppState>,
    Path((owner, name, pkg_type)): Path<(String, String, String)>,
    Query(query): Query<PublishPackageQuery>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let (_repo, user_id) = match require_repo_write(&state, &headers, &owner, &name).await {
        Ok(access) => access,
        Err(e) => return e.into_response(),
    };

    if !rg_core::package_registry::package_types::is_valid(&pkg_type) {
        return err(
            StatusCode::BAD_REQUEST,
            &format!("unsupported package type: {}", pkg_type),
        );
    }

    let filename = headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_filename_from_disposition)
        .unwrap_or_else(|| "package".to_string());

    // Try to auto-extract metadata via the adapter
    let adapter = rg_core::package_registry::get_adapter(&pkg_type);
    let adapter_meta = if let Some(ref adapter) = adapter {
        if let Err(e) = adapter.validate(&body) {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("invalid package payload: {e:#}"),
            );
        }
        adapter.extract_metadata(&filename, &body).ok()
    } else {
        None
    };

    let (pkg_name, pkg_version, description, homepage, repository_url, semver) =
        match resolve_publish_info(&query, adapter_meta) {
            Ok(v) => v,
            Err(msg) => return err(StatusCode::BAD_REQUEST, &msg),
        };

    let storage =
        rg_core::package_registry::PackageStorage::from_backend(state.blob_storage.clone());

    let info = rg_core::package_registry::PublishInfo {
        owner,
        repo: name,
        package_type: pkg_type,
        name: pkg_name,
        version: pkg_version,
        semver,
        metadata: None,
        description,
        homepage,
        repository_url,
        author_id: user_id,
        files: vec![(filename, body.to_vec())],
    };

    match rg_core::package_registry::service::publish(&state.db, &storage, info).await {
        Ok(result) => {
            let status = if result.existing {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (
                status,
                Json(PublishResponse {
                    package_id: result.package_id,
                    version_id: result.version_id,
                    existing: result.existing,
                }),
            )
                .into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

/// npm metadata uses `/packages/npm/{pkg_name}`, which otherwise captures the reserved
/// `publish` segment before Axum can select the generic `/{pkg_type}/publish` route.
/// Keep an exact npm publish route and delegate to the common implementation.
pub async fn publish_npm(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<PublishPackageQuery>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    publish(
        State(state),
        Path((owner, name, "npm".to_string())),
        Query(query),
        headers,
        body,
    )
    .await
}

pub async fn list_npm_packages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> axum::response::Response {
    list_packages(
        State(state),
        headers,
        Path((owner, name, "npm".to_string())),
    )
    .await
}

/// GET /api/v1/repos/:owner/:name/packages
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/packages",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 404, description = "Repository not found", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn list_registries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> axum::response::Response {
    let repo = match require_repo_read(&state, &headers, &owner, &name).await {
        Ok(repo) => repo,
        Err(e) => return e.into_response(),
    };

    match rg_db::ops::package_registry_ops::list_by_repo(&state.db, repo.id).await {
        Ok(registries) => Json(RegistryListResponse {
            registries: registries
                .into_iter()
                .map(|r| RegistryEntry {
                    package_type: r.package_type,
                    enabled: r.enabled,
                })
                .collect(),
        })
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

/// GET /api/v1/repos/:owner/:name/packages/:type/list
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/packages/{pkg_type}/list",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_type" = String, Path, description = "package type"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn list_packages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, pkg_type)): Path<(String, String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    match rg_core::package_registry::service::list_packages(&state.db, &owner, &name, &pkg_type)
        .await
    {
        Ok(packages) => Json(PackageListResponse { packages }).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

/// GET /api/v1/repos/:owner/:name/packages/:type/:pkg
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_type" = String, Path, description = "package type"),
        ("pkg_name" = String, Path, description = "package name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 404, description = "Package not found", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn get_package(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, pkg_type, pkg_name)): Path<(String, String, String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    match rg_core::package_registry::service::get_package(
        &state.db, &owner, &name, &pkg_type, &pkg_name,
    )
    .await
    {
        Ok(detail) => Json(detail).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, &format!("{e:#}")),
    }
}

/// GET /api/v1/repos/:owner/:name/packages/:type/:pkg/versions
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/versions",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_type" = String, Path, description = "package type"),
        ("pkg_name" = String, Path, description = "package name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 404, description = "Package not found", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn list_versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, pkg_type, pkg_name)): Path<(String, String, String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, &pkg_type, &pkg_name,
    )
    .await
    {
        Ok(versions) => Json(VersionListResponse { versions }).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, &format!("{e:#}")),
    }
}

/// GET /api/v1/repos/:owner/:name/packages/:type/:pkg/:ver
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_type" = String, Path, description = "package type"),
        ("pkg_name" = String, Path, description = "package name"),
        ("version" = String, Path, description = "package version"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 404, description = "Package version not found", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn get_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, pkg_type, pkg_name, version)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    match rg_core::package_registry::service::get_version(
        &state.db, &owner, &name, &pkg_type, &pkg_name, &version,
    )
    .await
    {
        Ok(detail) => Json(detail).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, &format!("{e:#}")),
    }
}

/// DELETE /api/v1/repos/:owner/:name/packages/:type/:pkg/:ver
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_type" = String, Path, description = "package type"),
        ("pkg_name" = String, Path, description = "package name"),
        ("version" = String, Path, description = "package version"),
    ),
    responses(
        (status = 204, description = "Deleted", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn delete_version(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_type, pkg_name, version)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
) -> axum::response::Response {
    if let Err(e) = require_repo_write(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let _user_id = match auth(&headers, &state.jwt_secret) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };

    let storage =
        rg_core::package_registry::PackageStorage::from_backend(state.blob_storage.clone());

    match rg_core::package_registry::service::delete_version(
        &state.db, &storage, &owner, &name, &pkg_type, &pkg_name, &version,
    )
    .await
    {
        Ok(_) => (StatusCode::NO_CONTENT,).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

/// PATCH /api/v1/repos/:owner/:name/packages/:type/:pkg/:ver/yank
pub async fn yank_version(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_type, pkg_name, version)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    Json(body): Json<YankRequest>,
) -> axum::response::Response {
    if let Err(e) = require_repo_write(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    match rg_core::package_registry::service::yank_version(
        &state.db, &owner, &name, &pkg_type, &pkg_name, &version, body.yank,
    )
    .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"yanked": body.yank})),
        )
            .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

/// GET /api/v1/repos/:owner/:name/packages/:type/:pkg/:ver/*file
pub async fn download_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, pkg_type, pkg_name, version, filename)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let storage =
        rg_core::package_registry::PackageStorage::from_backend(state.blob_storage.clone());

    match rg_core::package_registry::service::download_file(
        &state.db, &storage, &owner, &name, &pkg_type, &pkg_name, &version, &filename,
    )
    .await
    {
        Ok((data, content_type, _size)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", filename),
                ),
            ],
            data,
        )
            .into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, &format!("{e:#}")),
    }
}

// ── Protocol-specific endpoints ──────────────────────────

/// GET /api/v1/repos/:owner/:name/packages/cargo/index/:pkg
///
/// Cargo sparse index protocol (RFC 2789 / Cargo ≥ 1.68).
/// Returns line-delimited JSON, one line per version.
pub async fn cargo_sparse_index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, pkg_name)): Path<(String, String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "cargo", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return err_text(StatusCode::NOT_FOUND, &format!("{e:#}")),
    };

    let entries: Vec<(&str, Option<&str>, bool)> = versions
        .iter()
        .map(|v| (v.version.as_str(), v.sha256.as_deref(), v.is_yanked))
        .collect();

    let body = rg_core::package_registry::build_sparse_index(&pkg_name, &entries);

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            ("x-cargo-registry-type".parse().unwrap(), "sparse"),
        ],
        body,
    )
        .into_response()
}

/// GET /api/v1/repos/:owner/:name/packages/npm/:pkg_name
///
/// npm registry "abbreviated" metadata protocol.
/// Returns JSON with dist-tags and versions.
pub async fn npm_registry_metadata(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_name)): Path<(String, String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "npm", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return err(StatusCode::NOT_FOUND, &format!("{e:#}")),
    };

    // Determine base URL from request host header
    let base_url = headers
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
        .unwrap_or_else(|| "http://localhost".into());

    let npm_versions: Vec<rg_core::package_registry::NpmVersionInfo> = versions
        .iter()
        .map(|v| {
            // Find the tgz file
            let tgz_file = v
                .files
                .iter()
                .find(|f| f.filename.ends_with(".tgz") || f.filename.ends_with(".tar.gz"));

            rg_core::package_registry::NpmVersionInfo {
                version: v.version.clone(),
                description: None, // Version-level descriptions come from package detail
                sha256: v.sha256.clone(),
                filename: tgz_file.map(|f| f.filename.clone()),
                yanked: v.is_yanked,
            }
        })
        .collect();

    let metadata = rg_core::package_registry::build_npm_metadata(
        &pkg_name,
        &npm_versions,
        &base_url,
        &owner,
        &name,
    );

    (StatusCode::OK, Json(metadata)).into_response()
}

// ── PyPI Protocol Endpoints ───────────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}
///
/// PyPI Simple Repository API (PEP 503).
/// Returns an HTML page with download links for all versions.
pub async fn pypi_simple_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_name)): Path<(String, String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "pypi", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return err(StatusCode::NOT_FOUND, &format!("{e:#}")),
    };

    let base_url = headers
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
        .unwrap_or_else(|| "http://localhost".into());

    let entries: Vec<rg_core::package_registry::PyPIVersionEntry> = versions
        .iter()
        .map(|v| {
            // Find the primary package file
            let primary_file = v.files.iter().find(|f| {
                let fl = f.filename.to_lowercase();
                fl.ends_with(".whl")
                    || fl.ends_with(".tar.gz")
                    || fl.ends_with(".tgz")
                    || fl.ends_with(".zip")
            });

            let filename = primary_file
                .map(|f| f.filename.clone())
                .unwrap_or_else(|| format!("{}-{}.tar.gz", pkg_name, v.version));

            let download_url = format!(
                "{}/api/v1/repos/{}/{}/packages/pypi/{}/{}/{}",
                base_url.trim_end_matches('/'),
                owner,
                name,
                pkg_name,
                v.version,
                filename,
            );

            rg_core::package_registry::PyPIVersionEntry {
                version: v.version.clone(),
                filename,
                sha256: v.sha256.clone(),
                download_url,
            }
        })
        .collect();

    let html = rg_core::package_registry::build_simple_repository_html(&pkg_name, &entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

// ── Maven Protocol Endpoints ──────────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/maven/{group_id}/{artifact_id}/maven-metadata.xml
///
/// Maven metadata XML endpoint — returns version list in Maven's standard format.
pub async fn maven_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, group_id, artifact_id)): Path<(String, String, String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    // Maven package names are stored as "{groupId}:{artifactId}"
    let pkg_name = format!("{}:{}", group_id, artifact_id);

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "maven", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(_e) => {
            // Return empty metadata rather than 404 — Maven/Gradle handle gracefully
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<metadata>\n  <groupId>{}</groupId>\n  <artifactId>{}</artifactId>\n  <versioning>\n    <versions/>\n  </versioning>\n</metadata>\n",
                    escape_xml(&group_id),
                    escape_xml(&artifact_id),
                ),
            ).into_response();
        }
    };

    let entries: Vec<rg_core::package_registry::MavenVersionEntry> = versions
        .iter()
        .map(|v| rg_core::package_registry::MavenVersionEntry {
            version: v.version.clone(),
            is_snapshot: v.version.ends_with("-SNAPSHOT"),
            updated: v.created_at.clone(),
        })
        .collect();

    let xml =
        rg_core::package_registry::build_maven_metadata_xml(&group_id, &artifact_id, &entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

// ── NuGet Protocol Endpoints ──────────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/index.json
///
/// NuGet Service Index (v3) — returns the list of available API resources.
pub async fn nuget_service_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let base_url = build_base_url(&headers);
    let json = rg_core::package_registry::build_service_index(&base_url, &owner, &name);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/index.json
///
/// NuGet Registration Index — returns the metadata for all versions of a package.
pub async fn nuget_registration_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_name)): Path<(String, String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "nuget", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return err(StatusCode::NOT_FOUND, &format!("{e:#}")),
    };

    let base_url = build_base_url(&headers);

    let entries: Vec<rg_core::package_registry::NuGetRegistrationEntry> = versions
        .iter()
        .map(|v| {
            let primary_file = v
                .files
                .iter()
                .find(|f| f.filename.to_lowercase().ends_with(".nupkg"));

            let filename = primary_file
                .map(|f| f.filename.clone())
                .unwrap_or_else(|| format!("{}.{}.nupkg", pkg_name, v.version));

            let download_url = format!(
                "{}/api/v1/repos/{}/{}/packages/nuget/{}/{}/{}",
                base_url.trim_end_matches('/'),
                owner,
                name,
                pkg_name,
                v.version,
                filename,
            );

            let nuspec_url = primary_file.map(|_| {
                format!(
                    "{}/api/v1/repos/{}/{}/packages/nuget/{}/{}/{}.nuspec",
                    base_url.trim_end_matches('/'),
                    owner,
                    name,
                    pkg_name,
                    v.version,
                    pkg_name,
                )
            });

            // Parse NuGet-specific metadata from version JSON if available
            let (desc, hp, lic, tags) = parse_nuget_metadata(v.metadata.as_deref());

            rg_core::package_registry::NuGetRegistrationEntry {
                version: v.version.clone(),
                description: desc,
                homepage: hp,
                license: lic,
                tags,
                download_url,
                nuspec_url,
            }
        })
        .collect();

    let json = rg_core::package_registry::build_registration_index(&pkg_name, &entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/query?q=...
///
/// NuGet Search Query API (3.5.0) — search packages by name.
pub async fn nuget_search(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<NuGetSearchParams>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let query = params.q.as_deref().unwrap_or("");
    let base_url = build_base_url(&headers);

    // List all nuget packages in the repo
    let packages =
        match rg_core::package_registry::service::list_packages(&state.db, &owner, &name, "nuget")
            .await
        {
            Ok(p) => p,
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
        };

    let mut results: Vec<rg_core::package_registry::NuGetSearchResult> = Vec::new();
    let query_lower = query.to_lowercase();

    for pkg in &packages {
        let name_lower = pkg.name.to_lowercase();
        // Simple substring match
        if query.is_empty() || name_lower.contains(&query_lower) {
            let registration_url = format!(
                "{}/api/v1/repos/{}/{}/packages/nuget/registration/{}/index.json",
                base_url.trim_end_matches('/'),
                owner,
                name,
                pkg.name,
            );

            results.push(rg_core::package_registry::NuGetSearchResult {
                name: pkg.name.clone(),
                version: pkg.latest_version.clone().unwrap_or_else(|| "0.0.0".into()),
                description: pkg.description.clone(),
                tags: pkg.keywords.clone(),
                registration_url,
            });
        }
    }

    let total_hits = results.len();
    let json = rg_core::package_registry::build_search_results(&results, total_hits);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct NuGetSearchParams {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub skip: Option<usize>,
    #[serde(default)]
    pub take: Option<usize>,
}

// ── RubyGems Protocol Endpoints ───────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/dependencies?gems={name}
///
/// RubyGems dependencies API — returns version info for dependency resolution.
pub async fn rubygems_dependencies(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<RubyGemsDepsParams>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let gem_list: Vec<&str> = params
        .gems
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .collect();
    let _base_url = build_base_url(&headers);

    let mut entries: Vec<rg_core::package_registry::RubyGemsDependencyEntry> = Vec::new();

    for gem_name in gem_list {
        let versions = match rg_core::package_registry::service::list_versions(
            &state.db, &owner, &name, "rubygems", gem_name,
        )
        .await
        {
            Ok(v) => v,
            Err(_e) => continue,
        };

        for v in &versions {
            // Parse dependencies from metadata JSON
            let deps = parse_rubygems_deps(v.metadata.as_deref());

            entries.push(rg_core::package_registry::RubyGemsDependencyEntry {
                name: gem_name.to_string(),
                number: v.version.clone(),
                platform: "ruby".to_string(),
                dependencies: deps,
            });
        }
    }

    // Return empty array instead of null for unknown gems
    let json = rg_core::package_registry::build_dependencies_json(&entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems/{gem_name}.json
///
/// RubyGems gem info API — returns detailed metadata for all versions.
pub async fn rubygems_gem_info(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, gem_name)): Path<(String, String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let gem_name = gem_name
        .strip_suffix(".json")
        .unwrap_or(&gem_name)
        .to_string();

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "rubygems", &gem_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return err(StatusCode::NOT_FOUND, &format!("{e:#}")),
    };

    let base_url = build_base_url(&headers);

    let entries: Vec<rg_core::package_registry::RubyGemsVersionEntry> = versions
        .iter()
        .map(|v| {
            let filename = format!("{}-{}.gem", gem_name, v.version);
            let download_url = format!(
                "{}/api/v1/repos/{}/{}/packages/rubygems/{}/{}/{}",
                base_url.trim_end_matches('/'),
                owner,
                name,
                gem_name,
                v.version,
                filename,
            );
            let gem_uri = format!(
                "{}/gems/{}-{}.gem",
                base_url.trim_end_matches('/'),
                gem_name,
                v.version,
            );

            let (summary, desc, hp, lic) = parse_rubygems_info(v.metadata.as_deref());

            rg_core::package_registry::RubyGemsVersionEntry {
                number: v.version.clone(),
                platform: "ruby".to_string(),
                summary,
                description: desc,
                homepage: hp,
                license: lic,
                sha256: v.sha256.clone(),
                download_url,
                gem_uri,
                created_at: v.created_at.clone(),
            }
        })
        .collect();

    let json = rg_core::package_registry::build_gem_info_json(&gem_name, &entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct RubyGemsDepsParams {
    #[serde(default)]
    pub gems: Option<String>,
}

// ── Helm Protocol Endpoints ───────────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/helm/index.yaml
///
/// Helm repository index — returns the index.yaml that `helm repo add` expects.
pub async fn helm_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let base_url = build_base_url(&headers);

    // List all helm packages in the repo
    let packages =
        match rg_core::package_registry::service::list_packages(&state.db, &owner, &name, "helm")
            .await
        {
            Ok(p) => p,
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
        };

    let mut entries: Vec<rg_core::package_registry::HelmIndexEntry> = Vec::new();

    for pkg in &packages {
        // Get all versions for this chart
        let versions = match rg_core::package_registry::service::list_versions(
            &state.db, &owner, &name, "helm", &pkg.name,
        )
        .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };

        for v in &versions {
            // Build download URL
            let filename = v
                .files
                .first()
                .map(|f| f.filename.clone())
                .unwrap_or_else(|| format!("{}-{}.tgz", pkg.name, v.version));

            let download_url = format!(
                "{}/api/v1/repos/{}/{}/packages/helm/{}/{}/{}",
                base_url.trim_end_matches('/'),
                owner,
                name,
                pkg.name,
                v.version,
                filename,
            );

            // Parse Helm-specific metadata from version JSON
            let (app_version, api_version, keywords_list) =
                parse_helm_metadata(v.metadata.as_deref());

            entries.push(rg_core::package_registry::HelmIndexEntry {
                name: pkg.name.clone(),
                version: v.version.clone(),
                app_version,
                description: pkg.description.clone(),
                api_version,
                home: pkg.homepage.clone(),
                sources: Vec::new(),
                keywords: keywords_list,
                created: v.created_at.clone(),
                digest: v.sha256.clone(),
                urls: vec![download_url],
            });
        }
    }

    let yaml = rg_core::package_registry::build_helm_index(&entries);

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/x-yaml; charset=utf-8"),
            // Some Helm clients also check for text/yaml
        ],
        yaml,
    )
        .into_response()
}

// ── Composer Protocol Endpoint ────────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/composer/packages.json
///
/// Returns a Composer repository `packages.json` compatible with
/// Composer 2.x SAT solver.
pub async fn composer_packages_json(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> axum::response::Response {
    if let Err(e) = require_repo_read(&state, &headers, &owner, &name).await {
        return e.into_response();
    }

    let base_url = build_base_url(&headers);

    let packages = match rg_core::package_registry::service::list_packages(
        &state.db, &owner, &name, "composer",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => return (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    };

    let mut json_output = String::new();
    let mut first = true;

    // Start building the combined JSON manually
    json_output.push_str("{\"packages\":{");

    for pkg in &packages {
        let versions = match rg_core::package_registry::service::list_versions(
            &state.db, &owner, &name, "composer", &pkg.name,
        )
        .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };

        let composer_versions: Vec<
            rg_core::package_registry::adapters::composer::ComposerVersionInfo,
        > = versions
            .iter()
            .map(|v| {
                let filename = v
                    .files
                    .first()
                    .map(|f| f.filename.clone())
                    .unwrap_or_else(|| format!("{}.zip", v.version));
                rg_core::package_registry::adapters::composer::ComposerVersionInfo {
                    version: v.version.clone(),
                    filename,
                    sha256: v.sha256.clone(),
                    description: pkg.description.clone(),
                    license: None, // Composer license is stored in metadata
                    package_type: None,
                }
            })
            .collect();
        let name_json = serde_json::json!(pkg.name).to_string();
        let versions_json = rg_core::package_registry::adapters::composer::build_packages_json(
            &pkg.name,
            &composer_versions,
            &base_url,
            &owner,
            &name,
        );
        // Extract just the inner version map from the full response
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&versions_json) {
            if let Some(pkgs) = val.get("packages") {
                if let Some(inner) = pkgs.get(&pkg.name) {
                    if !first {
                        json_output.push(',');
                    }
                    first = false;
                    json_output.push_str(&format!("{}:{}", name_json, inner));
                }
            }
        }
    }

    json_output.push_str("}}");

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        json_output,
    )
        .into_response()
}

/// Parse Helm-specific metadata from version metadata JSON.
/// Returns (app_version, api_version, keywords).
fn parse_helm_metadata(
    metadata_json: Option<&str>,
) -> (Option<String>, Option<String>, Vec<String>) {
    let md = match metadata_json {
        Some(s) => s,
        None => return (None, None, Vec::new()),
    };
    let doc: serde_json::Value = match serde_json::from_str(md) {
        Ok(v) => v,
        Err(_) => return (None, None, Vec::new()),
    };
    let app_version = doc
        .get("appVersion")
        .and_then(|v| v.as_str())
        .map(String::from);
    let api_version = doc
        .get("apiVersion")
        .and_then(|v| v.as_str())
        .map(String::from);
    let keywords = doc
        .get("keywords")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|k| k.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    (app_version, api_version, keywords)
}

// ── helpers ───────────────────────────────────────────────

fn build_base_url(headers: &axum::http::HeaderMap) -> String {
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

/// Parse NuGet-specific metadata from a JSON metadata string.
/// Returns (description, homepage, license, tags).
fn parse_nuget_metadata(
    metadata_json: Option<&str>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let md = match metadata_json {
        Some(s) => s,
        None => return (None, None, None, None),
    };

    let doc: serde_json::Value = match serde_json::from_str(md) {
        Ok(v) => v,
        Err(_) => return (None, None, None, None),
    };

    let description = doc
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    let homepage = doc
        .get("projectUrl")
        .and_then(|v| v.as_str())
        .or_else(|| doc.get("homepage").and_then(|v| v.as_str()))
        .map(String::from);
    let license = doc
        .get("licenseUrl")
        .and_then(|v| v.as_str())
        .or_else(|| doc.get("license").and_then(|v| v.as_str()))
        .map(String::from);
    let tags = doc.get("tags").and_then(|v| v.as_str()).map(String::from);

    (description, homepage, license, tags)
}

/// Parse RubyGems dependencies from version metadata JSON.
fn parse_rubygems_deps(metadata_json: Option<&str>) -> Vec<rg_core::package_registry::RubyGemsDep> {
    let md = match metadata_json {
        Some(s) => s,
        None => return Vec::new(),
    };
    let doc: serde_json::Value = match serde_json::from_str(md) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let deps = match doc.get("dependencies").and_then(|v| v.as_array()) {
        Some(d) => d,
        None => return Vec::new(),
    };
    deps.iter()
        .filter_map(|d| {
            let name = d.get("name").and_then(|v| v.as_str())?.to_string();
            let req = d
                .get("requirements")
                .and_then(|v| v.as_str())
                .unwrap_or(">= 0")
                .to_string();
            Some(rg_core::package_registry::RubyGemsDep {
                name,
                requirements: req,
            })
        })
        .collect()
}

/// Parse RubyGems gem info from version metadata JSON.
/// Returns (summary, description, homepage, license).
fn parse_rubygems_info(
    metadata_json: Option<&str>,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let md = match metadata_json {
        Some(s) => s,
        None => return (None, None, None, None),
    };
    let doc: serde_json::Value = match serde_json::from_str(md) {
        Ok(v) => v,
        Err(_) => return (None, None, None, None),
    };
    let summary = doc
        .get("summary")
        .and_then(|v| v.as_str())
        .map(String::from);
    let description = doc
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    let homepage = doc
        .get("homepage")
        .and_then(|v| v.as_str())
        .map(String::from);
    let license = doc
        .get("licenses")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .or_else(|| {
            doc.get("license")
                .and_then(|v| v.as_str())
                .map(String::from)
        });
    (summary, description, homepage, license)
}

/// Simple XML string escaping.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn parse_filename_from_disposition(disposition: &str) -> Option<String> {
    for part in disposition.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("filename=") {
            return Some(val.trim_matches('"').to_string());
        }
        if let Some(val) = part.strip_prefix("filename*=") {
            if let Some(idx) = val.find("''") {
                let encoded = &val[idx + 2..];
                if let Ok(decoded) = percent_decode(encoded) {
                    return Some(decoded);
                }
            }
        }
    }
    None
}

fn percent_decode(s: &str) -> Result<String, ()> {
    let mut result = Vec::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().ok_or(())?;
            let lo = chars.next().ok_or(())?;
            let hi = hex_val(hi)?;
            let lo = hex_val(lo)?;
            result.push((hi << 4) | lo);
        } else {
            result.push(b);
        }
    }
    String::from_utf8(result).map_err(|_| ())
}

fn hex_val(b: u8) -> Result<u8, ()> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        _ => Err(()),
    }
}
