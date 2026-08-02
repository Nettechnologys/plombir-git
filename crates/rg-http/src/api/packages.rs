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
//! GET    /api/v1/repos/{owner}/{repo}/packages/maven/{group…}/{artifact}/maven-metadata.xml
//! GET    /api/v1/repos/{owner}/{repo}/packages/maven/{group…}/{artifact}/{version}/{file}
//! GET    /api/v1/repos/{owner}/{repo}/packages/rubygems/versions      — compact index
//! GET    /api/v1/repos/{owner}/{repo}/packages/rubygems/info/{gem}    — compact index
//! GET    /api/v1/repos/{owner}/{repo}/packages/rubygems/names         — compact index
//! GET    /api/v1/repos/{owner}/{repo}/packages/rubygems/gems/{file}   — `.gem` download

use crate::error::AppError;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::repo_access::{CiRead, Packages, RepoWrite};
use crate::AppState;

// ── Request / Response types ─────────────────────────────

#[derive(Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
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

/// Classify one package-service failure through the shared HTTP error funnel.
///
/// The service layer carries genuine absence as `rg_core::error::NotFound` and
/// leaves database/blob failures as their original typed sources. Rebuilding a
/// status locally loses that distinction and is how an outage became a 404 or
/// an empty protocol index in the first place.
fn package_error_response(error: anyhow::Error) -> axum::response::Response {
    AppError::from(error).into_response()
}

/// Whether a failed lookup proved absence rather than failing to check it.
fn package_is_absent(error: &anyhow::Error) -> bool {
    error.downcast_ref::<rg_core::error::NotFound>().is_some()
}

/// Package rows can outlive their blob. That is still a missing downloadable
/// file, while every other storage failure is an internal server error.
fn package_file_error_response(error: anyhow::Error) -> axum::response::Response {
    if matches!(
        error.downcast_ref::<rg_core::blob_storage::BlobStorageError>(),
        Some(rg_core::blob_storage::BlobStorageError::NotFound(_))
    ) {
        AppError::not_found("package file not found").into_response()
    } else {
        package_error_response(error)
    }
}

/// What a publish request resolved to, once the adapter's reading of the file
/// and the caller's query params have been merged.
struct ResolvedPublishInfo {
    name: String,
    version: String,
    description: Option<String>,
    homepage: Option<String>,
    repository_url: Option<String>,
    semver: Option<String>,
    /// The protocol-specific JSON the adapter read out of the file — gemspec
    /// dependencies, nuspec tags, a chart's `apiVersion`. It is stored on the
    /// version row and parsed back by the endpoint that speaks that protocol;
    /// there is no query param for it, because it is not something a caller
    /// could restate by hand.
    protocol_metadata: Option<String>,
}

/// Resolve publish metadata: adapter-extracted fields take precedence, then
/// query-param overrides.
fn resolve_publish_info(
    query: &PublishPackageQuery,
    adapter_meta: Option<rg_core::package_registry::ExtractedMetadata>,
) -> Result<ResolvedPublishInfo, String> {
    // If adapter extracted metadata, use it as base; query params override.
    if let Some(meta) = adapter_meta {
        let name = query.name.clone().unwrap_or(meta.name);
        let version = query.version.clone().unwrap_or(meta.version);
        if name.is_empty() || version.is_empty() {
            return Err(
                "package name and version are required (could not be auto-extracted)".into(),
            );
        }
        return Ok(ResolvedPublishInfo {
            name,
            version,
            description: query.description.clone().or(meta.description),
            homepage: query.homepage.clone().or(meta.homepage),
            repository_url: query.repository_url.clone().or(meta.repository_url),
            semver: query.semver.clone().or(meta.semver),
            protocol_metadata: meta.protocol_metadata,
        });
    }

    // No adapter extraction — must be in query params.
    let name = query.name.clone().ok_or("package name is required")?;
    let version = query.version.clone().ok_or("package version is required")?;
    Ok(ResolvedPublishInfo {
        name,
        version,
        description: query.description.clone(),
        homepage: query.homepage.clone(),
        repository_url: query.repository_url.clone(),
        semver: query.semver.clone(),
        protocol_metadata: None,
    })
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
        PublishPackageQuery,
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
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    Path((owner, name, pkg_type)): Path<(String, String, String)>,
    Query(query): Query<PublishPackageQuery>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

    let resolved = match resolve_publish_info(&query, adapter_meta) {
        Ok(v) => v,
        Err(msg) => return err(StatusCode::BAD_REQUEST, &msg),
    };

    let storage =
        rg_core::package_registry::PackageStorage::from_backend(state.blob_storage.clone());

    let info = rg_core::package_registry::PublishInfo {
        owner,
        repo: name,
        package_type: pkg_type,
        name: resolved.name,
        version: resolved.version,
        semver: resolved.semver,
        metadata: resolved.protocol_metadata,
        description: resolved.description,
        homepage: resolved.homepage,
        repository_url: resolved.repository_url,
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
        Err(e) => AppError::from(e).into_response(),
    }
}

/// npm metadata uses `/packages/npm/{pkg_name}`, which otherwise captures the reserved
/// `publish` segment before Axum can select the generic `/{pkg_type}/publish` route.
/// Keep an exact npm publish route and delegate to the common implementation.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/packages/npm/publish",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        PublishPackageQuery,
    ),
    request_body(
        content = String,
        description = "npm tarball payload",
    ),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 200, description = "Updated existing package", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn publish_npm(
    State(state): State<AppState>,
    gate: RepoWrite,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<PublishPackageQuery>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    publish(
        State(state),
        gate,
        Path((owner, name, "npm".to_string())),
        Query(query),
        headers,
        body,
    )
    .await
}

/// The generic package listing, pinned to `npm` — `/packages/npm/{pkg_name}` is
/// the npm metadata route, so the listing needs a spelling of its own.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/packages/npm/list",
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
pub async fn list_npm_packages(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    gate: CiRead<Packages>,
) -> axum::response::Response {
    list_packages(State(state), Path((owner, name, "npm".to_string())), gate).await
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
    Path((_, _)): Path<(String, String)>,
    CiRead::<Packages> { repo, .. }: CiRead<Packages>,
) -> axum::response::Response {
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
        Err(e) => AppError::from(e).into_response(),
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
    Path((owner, name, pkg_type)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    match rg_core::package_registry::service::list_packages(&state.db, &owner, &name, &pkg_type)
        .await
    {
        Ok(packages) => Json(PackageListResponse { packages }).into_response(),
        Err(e) => AppError::from(e).into_response(),
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
    Path((owner, name, pkg_type, pkg_name)): Path<(String, String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    match rg_core::package_registry::service::get_package(
        &state.db, &owner, &name, &pkg_type, &pkg_name,
    )
    .await
    {
        Ok(detail) => Json(detail).into_response(),
        Err(e) => package_error_response(e),
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
    Path((owner, name, pkg_type, pkg_name)): Path<(String, String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, &pkg_type, &pkg_name,
    )
    .await
    {
        Ok(versions) => Json(VersionListResponse { versions }).into_response(),
        Err(e) => package_error_response(e),
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
    Path((owner, name, pkg_type, pkg_name, version)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    match rg_core::package_registry::service::get_version(
        &state.db, &owner, &name, &pkg_type, &pkg_name, &version,
    )
    .await
    {
        Ok(detail) => Json(detail).into_response(),
        Err(e) => package_error_response(e),
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
        (status = 404, description = "No such package version — including one a \
                                     concurrent request deleted first",
         body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn delete_version(
    State(state): State<AppState>,
    Path((owner, name, pkg_type, pkg_name, version)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    RepoWrite { .. }: RepoWrite,
) -> axum::response::Response {
    let storage =
        rg_core::package_registry::PackageStorage::from_backend(state.blob_storage.clone());

    match rg_core::package_registry::service::delete_version(
        &state.db, &storage, &owner, &name, &pkg_type, &pkg_name, &version,
    )
    .await
    {
        Ok(_) => (StatusCode::NO_CONTENT,).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// PATCH /api/v1/repos/:owner/:name/packages/:type/:pkg/:ver/yank
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/yank",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_type" = String, Path, description = "package type"),
        ("pkg_name" = String, Path, description = "package name"),
        ("version" = String, Path, description = "package version"),
    ),
    request_body = YankRequest,
    responses(
        (status = 200, description = "New yank state", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Package version not found", body = serde_json::Value),
    ),
)]
pub async fn yank_version(
    State(state): State<AppState>,
    Path((owner, name, pkg_type, pkg_name, version)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    RepoWrite { .. }: RepoWrite,
    Json(body): Json<YankRequest>,
) -> axum::response::Response {
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
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/packages/:type/:pkg/:ver/*file
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/{*file}",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_type" = String, Path, description = "package type"),
        ("pkg_name" = String, Path, description = "package name"),
        ("version" = String, Path, description = "package version"),
    ),
    responses(
        (status = 200, description = "Stored package file", content_type = "application/octet-stream"),
        (status = 404, description = "Package, version or file not found", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn download_file(
    State(state): State<AppState>,
    Path((owner, name, pkg_type, pkg_name, version, filename)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    serve_package_file(
        &state, &owner, &name, &pkg_type, &pkg_name, &version, &filename,
    )
    .await
}

/// Read one stored file of one version and answer it as a download.
///
/// Shared by the generic route above and by the protocol routes that address
/// the same file through their own layout — Maven's, for one, which spells the
/// package name out as a directory tree.
#[allow(clippy::too_many_arguments)]
async fn serve_package_file(
    state: &AppState,
    owner: &str,
    name: &str,
    pkg_type: &str,
    pkg_name: &str,
    version: &str,
    filename: &str,
) -> axum::response::Response {
    let storage =
        rg_core::package_registry::PackageStorage::from_backend(state.blob_storage.clone());

    match rg_core::package_registry::service::download_file(
        &state.db, &storage, owner, name, pkg_type, pkg_name, version, filename,
    )
    .await
    {
        Ok((data, content_type, _size)) => {
            // The whole package file is buffered in memory (`read_file` returns a
            // `Vec` — there is no local-path streaming branch, so unlike the
            // LFS/OCI/attachment handlers this fires even in the default on-disk
            // config). Serve it as a backpressure-sensitive, idle-guarded stream
            // instead of a single `Body::from` frame a slow/stalled client can pin
            // in server memory until the kernel resets the dead connection — the
            // same download-side slow-drip class as the artifact/cache/release
            // handlers (card_444e03f1ca15). `Content-Length` lets clients spot an
            // idle-aborted short read.
            let len = data.len();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, content_type),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{}\"", filename),
                    ),
                    (header::CONTENT_LENGTH, len.to_string()),
                ],
                crate::http_stream::buffered_body_with_idle(data, state.git_idle_timeout_secs),
            )
                .into_response()
        }
        Err(e) => package_file_error_response(e),
    }
}

// ── Protocol-specific endpoints ──────────────────────────

/// Split the captures of a layout route into the repository and the path
/// below it.
///
/// `{owner}` and `{name}` name the repository; every other capture on these
/// routes is one segment of the client's own layout, in path order. Maven and
/// Cargo both register one route per depth (`maven_layout_routes`,
/// `cargo_index_routes`), so the number of captures varies from request to
/// request and they cannot be read positionally.
fn layout_segments(params: &axum::extract::RawPathParams) -> (String, String, Vec<String>) {
    let mut owner = String::new();
    let mut repo = String::new();
    let mut segments = Vec::new();

    for (key, value) in params {
        match key {
            "owner" => owner = value.to_string(),
            "name" => repo = value.to_string(),
            _ => segments.push(value.to_string()),
        }
    }

    (owner, repo, segments)
}

/// Does `prefix` spell out `name` the way Cargo lays the index out?
///
/// Compared case-insensitively: Cargo lowercases the path, but a hand-written
/// request (or ForgeKeep's own UI) may carry the manifest's spelling, and a
/// case mismatch is not a different crate.
fn matches_index_prefix(prefix: &[String], name: &str) -> bool {
    let expected = rg_core::package_registry::cargo_index_prefix(name);
    prefix.len() == expected.len()
        && prefix
            .iter()
            .zip(&expected)
            .all(|(got, want)| got.eq_ignore_ascii_case(want))
}

/// GET /api/v1/repos/:owner/:name/packages/cargo/index/config.json
///
/// The first request Cargo makes against a sparse registry, and the one that
/// decides whether it will talk to it at all: without a `config.json` carrying
/// a `dl` URL the index is not a registry. Registered at the index root, so the
/// URL a user configures is
/// `sparse+{base}/api/v1/repos/{owner}/{name}/packages/cargo/index/`.
pub async fn cargo_index_config(
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let base_url = build_base_url(&headers);
    let json = rg_core::package_registry::build_cargo_index_config(&base_url, &owner, &name);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET /api/v1/repos/:owner/:name/packages/cargo/index/{prefix…}/{crate}
///
/// Cargo sparse index protocol (RFC 2789 / Cargo ≥ 1.68).
/// Returns line-delimited JSON, one line per version.
///
/// Cargo never asks for a crate by its bare name: the name is spelled out as
/// the index prefix — `1/a`, `2/ab`, `3/a/abc`, `se/rd/serde` — so the routes
/// capture two or three segments (see `cargo_index_routes` in `crate::routes`)
/// and the crate name is read off the end here. The prefix is checked against
/// the name rather than ignored: an unverified prefix would serve any crate
/// under any path, and the index would stop being addressable.
///
/// A single segment is ForgeKeep's own flat spelling, `index/{crate}`, which
/// its API and UI use. The layout never produces one segment, so the two cannot
/// be confused.
pub async fn cargo_sparse_index(
    State(state): State<AppState>,
    params: axum::extract::RawPathParams,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let (owner, name, segments) = layout_segments(&params);

    let Some((pkg_name, prefix)) = segments.split_last() else {
        return err_text(StatusCode::NOT_FOUND, "empty Cargo index path");
    };
    if !prefix.is_empty() && !matches_index_prefix(prefix, pkg_name) {
        return err_text(
            StatusCode::NOT_FOUND,
            &format!(
                "'{}' is not the index prefix of crate '{}'",
                prefix.join("/"),
                pkg_name
            ),
        );
    }

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "cargo", pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return package_error_response(e),
    };

    let entries: Vec<rg_core::package_registry::CargoIndexVersion<'_>> = versions
        .iter()
        .map(|v| rg_core::package_registry::CargoIndexVersion {
            version: v.version.as_str(),
            sha256: v.sha256.as_deref(),
            yanked: v.is_yanked,
            // Dependencies and features live only in the manifest inside the
            // `.crate`; the adapter lifted them here at publish, and this is
            // where cargo's resolver reads them back.
            metadata: v.metadata.as_deref(),
        })
        .collect();

    let body = rg_core::package_registry::build_sparse_index(pkg_name, &entries);

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
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "npm", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return package_error_response(e),
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
                // Digests of the tarball itself — the file the `dist` block
                // makes its promises about. The version-level `sha256` is only
                // the first file's and is used as a fallback for a version
                // stored before the per-file digests existed.
                sha256: tgz_file.and_then(|f| v.sha256_of(f)),
                sha1: tgz_file.and_then(|f| f.sha1.clone()),
                sha512: tgz_file.and_then(|f| f.sha512.clone()),
                filename: tgz_file.map(|f| f.filename.clone()),
                yanked: v.is_yanked,
                // Dependency tables live only in the `package.json` inside the
                // `.tgz`; the adapter lifted them here at publish, and this is
                // where npm's resolver reads them back.
                metadata: v.metadata.clone(),
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

/// Base of the PyPI Simple Repository API for one repository, trailing slash
/// excluded — `{base}/simple`, the prefix a client is pointed at.
fn pypi_simple_base(base_url: &str, owner: &str, repo: &str) -> String {
    format!(
        "{}/api/v1/repos/{}/{}/packages/pypi/simple",
        base_url.trim_end_matches('/'),
        owner,
        repo,
    )
}

/// Is this file one of the distributions a PyPI client installs from?
///
/// The Simple page lists installable artifacts; a checksum or signature stored
/// beside them is not one, and offering it as a download makes pip choose a
/// file it cannot install.
fn is_pypi_distribution(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    lower.ends_with(".whl")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".zip")
}

/// Resolve the project a client asked for to the name it was published under.
///
/// PEP 503 has the *client* normalize the name before putting it in the URL, so
/// `pip install Matrix_PyPI` asks for `matrix-pypi/`, while the registry stores
/// whatever `Name:` the wheel metadata carried. Only reached after a lookup on
/// the literal spelling missed, so the common case still costs one query.
///
/// `Ok(None)` is the answer "the scan ran and no published project normalizes to
/// this name" — the only outcome that may become a `404`. A failed scan is an
/// `Err` and stays one: swallowing it (`.ok()?`) told pip the project does not
/// exist, which is precisely the answer it will not retry.
async fn resolve_pypi_project(
    db: &sea_orm::DatabaseConnection,
    owner: &str,
    repo: &str,
    requested: &str,
) -> anyhow::Result<Option<String>> {
    let wanted = rg_core::package_registry::normalize_project_name(requested);

    Ok(
        rg_core::package_registry::service::list_packages(db, owner, repo, "pypi")
            .await?
            .into_iter()
            .find(|pkg| rg_core::package_registry::normalize_project_name(&pkg.name) == wanted)
            .map(|pkg| pkg.name),
    )
}

/// GET /api/v1/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}/
/// GET /api/v1/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}
///
/// PyPI Simple Repository API (PEP 503) — the project page.
/// Returns an HTML page with download links for all versions.
///
/// Both spellings are routed here because PEP 503 defines the project URL
/// *with* the trailing slash and that is what pip, poetry and uv send; the bare
/// one is kept for a hand-typed URL or a proxy that strips the slash.
pub async fn pypi_simple_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_name)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    // The spelling in the URL first — one query, and the common case. Only a
    // miss pays for the normalized scan.
    let (project, versions) = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "pypi", &pkg_name,
    )
    .await
    {
        Ok(versions) => (pkg_name.clone(), versions),
        // Only a genuine absence earns the second, more expensive lookup. Any
        // other failure — the database being down, above all — is ours, and
        // retrying it under a different spelling would just fail again and then
        // be reported as "no such project": an answer `pip`/`uv` cache and never
        // retry. `AppError::from` keeps the outage a 5xx and the detail in the
        // operator log rather than in the response body (H-05).
        Err(miss) if miss.downcast_ref::<rg_core::error::NotFound>().is_some() => {
            match resolve_pypi_project(&state.db, &owner, &name, &pkg_name).await {
                Ok(Some(stored)) => match rg_core::package_registry::service::list_versions(
                    &state.db, &owner, &name, "pypi", &stored,
                )
                .await
                {
                    Ok(versions) => (stored, versions),
                    Err(e) => return AppError::from(e).into_response(),
                },
                // The scan ran and nothing normalizes to this name: the client's
                // original miss was the truth, and it is a `NotFound`, so it
                // renders as the fixed "… not found" with no `db: …` chain.
                Ok(None) => return AppError::from(miss).into_response(),
                Err(e) => return AppError::from(e).into_response(),
            }
        }
        Err(e) => return AppError::from(e).into_response(),
    };

    let base_url = build_base_url(&headers);

    // The download route matches on the *stored* package name, so the link has
    // to carry that one and not the normalized spelling the client asked with.
    let link_to = |version: &str, filename: &str| {
        format!(
            "{}/api/v1/repos/{}/{}/packages/pypi/{}/{}/{}",
            base_url.trim_end_matches('/'),
            owner,
            name,
            project,
            version,
            filename,
        )
    };

    // One link per distribution *file*, not per version. A release routinely
    // carries a wheel and an sdist — `twine upload dist/*` publishes both — and
    // a page with one link per version hid every artifact but the first from
    // pip entirely. The digest beside each link is that file's own, because
    // that is what the client hashes after downloading it.
    let entries: Vec<rg_core::package_registry::PyPIVersionEntry> = versions
        .iter()
        .flat_map(|v| {
            let files: Vec<rg_core::package_registry::PyPIVersionEntry> = v
                .files
                .iter()
                .filter(|f| is_pypi_distribution(&f.filename))
                .map(|f| rg_core::package_registry::PyPIVersionEntry {
                    version: v.version.clone(),
                    filename: f.filename.clone(),
                    sha256: v.sha256_of(f),
                    download_url: link_to(&v.version, &f.filename),
                })
                .collect();

            if !files.is_empty() {
                return files;
            }

            // A version with no recognisable distribution file still has to
            // appear: the name is the conventional sdist one, which is what the
            // download route falls back to as well.
            let filename = format!("{}-{}.tar.gz", project, v.version);
            let download_url = link_to(&v.version, &filename);
            vec![rg_core::package_registry::PyPIVersionEntry {
                version: v.version.clone(),
                filename,
                sha256: v.sha256.clone(),
                download_url,
            }]
        })
        .collect();

    let html = rg_core::package_registry::build_simple_repository_html(&project, &entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// GET /api/v1/repos/{owner}/{name}/packages/pypi/simple/
/// GET /api/v1/repos/{owner}/{name}/packages/pypi/simple
///
/// PyPI Simple Repository API (PEP 503) — the root index: one link per project,
/// each pointing at that project's page.
///
/// This is the URL a user configures as `--index-url`, so it has to answer even
/// when the repository has no PyPI packages yet: an empty index is a valid
/// answer, a 404 (which in production falls through to the SPA and returns
/// HTML) is not.
pub async fn pypi_simple_root_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let packages =
        match rg_core::package_registry::service::list_packages(&state.db, &owner, &name, "pypi")
            .await
        {
            Ok(packages) => packages,
            Err(error) if package_is_absent(&error) => Vec::new(),
            Err(error) => return package_error_response(error),
        };

    let base = pypi_simple_base(&build_base_url(&headers), &owner, &name);

    let projects: Vec<rg_core::package_registry::PyPIProjectEntry> = packages
        .iter()
        .map(|pkg| rg_core::package_registry::PyPIProjectEntry {
            name: pkg.name.clone(),
            url: format!(
                "{}/{}/",
                base,
                rg_core::package_registry::normalize_project_name(&pkg.name)
            ),
        })
        .collect();

    let html = rg_core::package_registry::build_simple_root_html(&projects);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

// ── Maven Protocol Endpoints ──────────────────────────────

/// The repository and the layout segments of one Maven request.
///
/// A Maven client writes a `groupId` with one path segment per dot, so
/// `com.example:matrix-maven` is fetched from `com/example/matrix-maven/…` and
/// the number of segments depends on the group. The routes therefore capture
/// the layout as `{m1}`, `{m2}`, … (see `maven_layout_routes` in
/// `crate::routes`) and the split back into coordinates happens here, from the
/// end, where the shape is fixed.
struct MavenRequest {
    owner: String,
    repo: String,
    /// Everything below `.../packages/maven/`, in order.
    segments: Vec<String>,
}

impl MavenRequest {
    /// Read the captures by name — see [`layout_segments`], which Cargo's index
    /// routes share.
    fn from_params(params: &axum::extract::RawPathParams) -> Self {
        let (owner, repo, segments) = layout_segments(params);

        Self {
            owner,
            repo,
            segments,
        }
    }

    /// `<group…>/<artifact>` → the `groupId:artifactId` the registry stores.
    ///
    /// The group is everything before the artifact, joined back with the dots
    /// the client replaced by slashes — so the flat spelling ForgeKeep's own
    /// API uses (`com.example/matrix-maven`) resolves to the same name.
    fn coordinates(group_and_artifact: &[String]) -> Option<(String, String)> {
        let (artifact_id, group) = group_and_artifact.split_last()?;
        if group.is_empty() || artifact_id.is_empty() {
            return None;
        }
        Some((group.join("."), artifact_id.clone()))
    }
}

/// GET /api/v1/repos/{owner}/{name}/packages/maven/{group…}/{artifact}/maven-metadata.xml
///
/// Maven metadata XML endpoint — returns version list in Maven's standard format.
pub async fn maven_metadata(
    State(state): State<AppState>,
    params: axum::extract::RawPathParams,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let request = MavenRequest::from_params(&params);
    let Some((group_id, artifact_id)) = MavenRequest::coordinates(&request.segments) else {
        return err(
            StatusCode::NOT_FOUND,
            "Maven metadata path carries no groupId/artifactId",
        );
    };
    let (owner, name) = (request.owner, request.repo);

    // Maven package names are stored as "{groupId}:{artifactId}"
    let pkg_name = format!("{}:{}", group_id, artifact_id);

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "maven", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(error) if package_is_absent(&error) => {
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
        Err(error) => return package_error_response(error),
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

/// GET /api/v1/repos/{owner}/{name}/packages/maven/{group…}/{artifact}/{version}/{file}
///
/// The artifact itself, at the path `mvn` and Gradle build from the coordinate:
/// the group's dots are slashes, and the version is a directory. Everything
/// before the last three segments is the group.
pub async fn maven_download(
    State(state): State<AppState>,
    params: axum::extract::RawPathParams,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let request = MavenRequest::from_params(&params);

    // `<group…>/<artifact>/<version>/<file>` — at least four segments, and the
    // coordinate is read off the end so the group can be any length.
    let Some((filename, head)) = request.segments.split_last() else {
        return err(StatusCode::NOT_FOUND, "empty Maven path");
    };
    let Some((version, head)) = head.split_last() else {
        return err(StatusCode::NOT_FOUND, "Maven path carries no version");
    };
    let Some((group_id, artifact_id)) = MavenRequest::coordinates(head) else {
        return err(
            StatusCode::NOT_FOUND,
            "Maven path carries no groupId/artifactId",
        );
    };

    let pkg_name = format!("{}:{}", group_id, artifact_id);

    serve_package_file(
        &state,
        &request.owner,
        &request.repo,
        "maven",
        &pkg_name,
        version,
        filename,
    )
    .await
}

// ── NuGet Protocol Endpoints ──────────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/index.json
///
/// NuGet Service Index (v3) — returns the list of available API resources.
pub async fn nuget_service_index(
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
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
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "nuget", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return package_error_response(e),
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
    CiRead::<Packages> { .. }: CiRead<Packages>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<NuGetSearchParams>,
) -> axum::response::Response {
    let query = params.q.as_deref().unwrap_or("");
    let base_url = build_base_url(&headers);

    // List all nuget packages in the repo
    let packages =
        match rg_core::package_registry::service::list_packages(&state.db, &owner, &name, "nuget")
            .await
        {
            Ok(p) => p,
            Err(e) => return AppError::from(e).into_response(),
        };

    let mut results: Vec<rg_core::package_registry::NuGetSearchResult> = Vec::new();
    let query_lower = query.to_lowercase();

    for pkg in &packages {
        let name_lower = pkg.name.to_lowercase();
        // Simple substring match
        if query.is_empty() || name_lower.contains(&query_lower) {
            let Some(version) = pkg.latest_version.clone() else {
                continue;
            };
            let registration_url = format!(
                "{}/api/v1/repos/{}/{}/packages/nuget/registration/{}/index.json",
                base_url.trim_end_matches('/'),
                owner,
                name,
                pkg.name,
            );

            results.push(rg_core::package_registry::NuGetSearchResult {
                name: pkg.name.clone(),
                version,
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
    CiRead::<Packages> { .. }: CiRead<Packages>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<RubyGemsDepsParams>,
) -> axum::response::Response {
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
            Err(error) if package_is_absent(&error) => continue,
            Err(error) => return package_error_response(error),
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
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
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
        Err(e) => return package_error_response(e),
    };

    let base_url = build_base_url(&headers);
    let root = rubygems_root(&base_url, &owner, &name);

    let entries: Vec<rg_core::package_registry::RubyGemsVersionEntry> = versions
        .iter()
        .map(|v| {
            // The name the file was published under, not one rebuilt from the
            // coordinates: a platform gem is stored as `{name}-{ver}-{platform}.gem`.
            let file = gem_file(v);
            let filename = file
                .map(|f| f.filename.clone())
                .unwrap_or_else(|| format!("{}-{}.gem", gem_name, v.version));
            let download_url = format!("{root}/{gem_name}/{}/{filename}", v.version);
            // `gems/{file}` under the registry root, because that is the only
            // path a client will ever ask for: `Gem::RemoteFetcher#download`
            // glues it onto the source URL itself. Advertising anything else
            // here is advertising a URL nothing serves.
            let gem_uri = format!("{root}/gems/{filename}");

            let (summary, desc, hp, lic) = parse_rubygems_info(v.metadata.as_deref());

            rg_core::package_registry::RubyGemsVersionEntry {
                number: v.version.clone(),
                platform: "ruby".to_string(),
                summary,
                description: desc,
                homepage: hp,
                license: lic,
                // The digest of the `.gem` the two URLs above point at — not
                // the version's, which is a different file as soon as the
                // version carries more than one.
                sha256: file.and_then(|f| v.sha256_of(f)),
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

// ── RubyGems compact index ────────────────────────────────

/// The registry root a client is pointed at — `gem install --source <this>`,
/// or a `Gemfile`'s `source`. Every compact-index path hangs off it, and so
/// does the `gems/{file}` download the client builds on its own.
fn rubygems_root(base_url: &str, owner: &str, repo: &str) -> String {
    format!(
        "{}/api/v1/repos/{}/{}/packages/rubygems",
        base_url.trim_end_matches('/'),
        owner,
        repo,
    )
}

/// The `.gem` of a version — the file a client downloads, as opposed to any
/// checksum or signature published beside it.
fn gem_file(
    version: &rg_core::package_registry::VersionDetail,
) -> Option<&rg_core::package_registry::FileDetail> {
    version
        .files
        .iter()
        .find(|f| f.filename.ends_with(".gem"))
        .or_else(|| version.files.first())
}

/// Turn stored versions into compact-index lines.
///
/// Yanked versions are dropped rather than marked: the format has no spelling
/// for a yanked version inside an `info` file, it simply stops listing it.
fn compact_index_entries(
    gem_name: &str,
    versions: &[rg_core::package_registry::VersionDetail],
) -> Vec<rg_core::package_registry::CompactIndexVersion> {
    versions
        .iter()
        .filter(|v| !v.is_yanked)
        .map(|v| {
            let file = gem_file(v);
            rg_core::package_registry::CompactIndexVersion {
                number: v.version.clone(),
                platform: file.and_then(|f| gem_platform(&f.filename, gem_name, &v.version)),
                dependencies: parse_rubygems_deps(v.metadata.as_deref()),
                checksum: file.and_then(|f| v.sha256_of(f)),
            }
        })
        .collect()
}

/// The platform a stored `.gem` carries, when it is not the default `ruby`.
///
/// ForgeKeep keeps no platform column — but the client encodes it in the file
/// name it published (`nokogiri-1.16.0-x86_64-linux.gem`), and the compact
/// index has to spell the same `VERSION-PLATFORM` chunk back or the download
/// the client derives from it will not exist.
fn gem_platform(filename: &str, gem_name: &str, version: &str) -> Option<String> {
    let stem = filename.strip_suffix(".gem")?;
    let rest = stem.strip_prefix(&format!("{gem_name}-{version}"))?;
    let platform = rest.strip_prefix('-')?;

    (!platform.is_empty() && platform != "ruby").then(|| platform.to_string())
}

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/versions
///
/// The compact index's entry point, and the request that decides which protocol
/// the client speaks for the rest of the session: `Gem::Source` asks for this
/// file first, resolves through `info/{gem}` when it is served, and falls back
/// to the legacy Marshal index (which ForgeKeep does not serve) when it is not.
///
/// A repository with no gems answers an empty index rather than a 404, for that
/// reason: the 404 would not read as "nothing published yet", it would push the
/// client onto a protocol that then fails on its own missing files.
pub async fn rubygems_compact_versions(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let mut gems = Vec::new();
    let mut created_at = String::new();

    let packages = match rubygems_packages(&state, &owner, &name).await {
        Ok(packages) => packages,
        Err(error) => return package_error_response(error),
    };

    for pkg in packages {
        let versions = match rg_core::package_registry::service::list_versions(
            &state.db, &owner, &name, "rubygems", &pkg.name,
        )
        .await
        {
            Ok(v) => v,
            Err(error) if package_is_absent(&error) => continue,
            Err(error) => return package_error_response(error),
        };

        let entries = compact_index_entries(&pkg.name, &versions);
        if entries.is_empty() {
            continue;
        }

        if let Some(newest) = versions.iter().map(|v| &v.created_at).max() {
            if *newest > created_at {
                created_at.clone_from(newest);
            }
        }

        // The checksum has to be of the body this server will actually serve at
        // `info/{gem}`, so the info file is built here and hashed, not guessed.
        let info = rg_core::package_registry::build_compact_index_info(&entries);
        gems.push(rg_core::package_registry::CompactIndexGem {
            name: pkg.name.clone(),
            versions: entries.iter().map(|e| e.version_and_platform()).collect(),
            info_checksum: rg_core::package_registry::compact_index_info_checksum(&info),
        });
    }

    gems.sort_by(|a, b| a.name.cmp(&b.name));

    text_index(rg_core::package_registry::build_compact_index_versions(
        &created_at,
        &gems,
    ))
}

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/info/{gem_name}
///
/// One gem's versions, dependencies and checksums — what the client resolves
/// against once `versions` has told it this registry speaks the compact index.
pub async fn rubygems_compact_info(
    State(state): State<AppState>,
    Path((owner, name, gem_name)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "rubygems", &gem_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return package_error_response(e),
    };

    let entries = compact_index_entries(&gem_name, &versions);
    if entries.is_empty() {
        return err_text(
            StatusCode::NOT_FOUND,
            &format!("gem '{gem_name}' has no installable version"),
        );
    }

    text_index(rg_core::package_registry::build_compact_index_info(
        &entries,
    ))
}

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/names
///
/// Every gem name in the registry. No official RubyGems tool reads it, but it
/// is part of the index a mirroring client expects to find beside the other
/// two, and it costs one query.
pub async fn rubygems_compact_names(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let packages = match rubygems_packages(&state, &owner, &name).await {
        Ok(packages) => packages,
        Err(error) => return package_error_response(error),
    };
    let mut names: Vec<String> = packages.into_iter().map(|pkg| pkg.name).collect();
    names.sort();

    text_index(rg_core::package_registry::build_compact_index_names(&names))
}

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/gems/{filename}
///
/// The `.gem` download. The client does not read this path out of any index —
/// `Gem::RemoteFetcher#download` appends `gems/{file}` to the source URL — so
/// it is fixed, and the file is found by the name it was published under rather
/// than by splitting `{name}-{version}` back out of it (both halves may contain
/// dashes, and a platform gem carries a third).
pub async fn rubygems_gem_download(
    State(state): State<AppState>,
    Path((owner, name, filename)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let packages = match rubygems_packages(&state, &owner, &name).await {
        Ok(packages) => packages,
        Err(error) => return package_error_response(error),
    };

    for pkg in packages {
        // Every gem file starts with its gem's name, so most candidates are
        // ruled out without a query.
        if !filename.starts_with(&format!("{}-", pkg.name)) {
            continue;
        }

        let versions = match rg_core::package_registry::service::list_versions(
            &state.db, &owner, &name, "rubygems", &pkg.name,
        )
        .await
        {
            Ok(v) => v,
            Err(error) if package_is_absent(&error) => continue,
            Err(error) => return package_error_response(error),
        };

        if let Some(version) = versions
            .iter()
            .find(|v| v.files.iter().any(|f| f.filename == filename))
        {
            return serve_package_file(
                &state,
                &owner,
                &name,
                "rubygems",
                &pkg.name,
                &version.version,
                &filename,
            )
            .await;
        }
    }

    err_text(
        StatusCode::NOT_FOUND,
        &format!("gem '{filename}' not found"),
    )
}

/// The gems published to a repository, or nothing at all.
///
/// A repository that never enabled the registry is not an error on these
/// routes — an empty index is the honest answer, and the alternative pushes the
/// client onto the legacy protocol.
async fn rubygems_packages(
    state: &AppState,
    owner: &str,
    repo: &str,
) -> anyhow::Result<Vec<rg_core::package_registry::PackageSummary>> {
    match rg_core::package_registry::service::list_packages(&state.db, owner, repo, "rubygems")
        .await
    {
        Ok(packages) => Ok(packages),
        Err(error) if package_is_absent(&error) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Answer one of the compact index files.
fn text_index(body: String) -> axum::response::Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

// ── Helm Protocol Endpoints ───────────────────────────────

/// The chart archive of a version — the file `urls` points at and `digest`
/// makes its promise about, as opposed to anything stored beside it.
fn chart_file(
    version: &rg_core::package_registry::VersionDetail,
) -> Option<&rg_core::package_registry::FileDetail> {
    version
        .files
        .iter()
        .find(|f| f.filename.ends_with(".tgz"))
        .or_else(|| version.files.first())
}

/// GET /api/v1/repos/{owner}/{name}/packages/helm/index.yaml
///
/// Helm repository index — returns the index.yaml that `helm repo add` expects.
pub async fn helm_index(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let base_url = build_base_url(&headers);

    // List all helm packages in the repo
    let packages =
        match rg_core::package_registry::service::list_packages(&state.db, &owner, &name, "helm")
            .await
        {
            Ok(p) => p,
            Err(e) => return AppError::from(e).into_response(),
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
            Err(error) if package_is_absent(&error) => continue,
            Err(error) => return package_error_response(error),
        };

        for v in &versions {
            if v.is_yanked {
                continue;
            }

            // Build download URL
            let chart = chart_file(v);
            let filename = chart
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
            let meta = parse_helm_metadata(v.metadata.as_deref());

            entries.push(rg_core::package_registry::HelmIndexEntry {
                name: pkg.name.clone(),
                version: v.version.clone(),
                app_version: meta.app_version,
                description: pkg.description.clone(),
                api_version: meta.api_version,
                home: pkg.homepage.clone(),
                sources: meta.sources,
                keywords: meta.keywords,
                created: v.created_at.clone(),
                // `digest` is defined as the SHA-256 of the archive `urls`
                // points at, and `helm` verifies exactly that.
                digest: chart.and_then(|f| v.sha256_of(f)),
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
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let base_url = build_base_url(&headers);

    let packages = match rg_core::package_registry::service::list_packages(
        &state.db, &owner, &name, "composer",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => return package_error_response(e),
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
            Err(error) if package_is_absent(&error) => continue,
            Err(error) => return package_error_response(error),
        };

        let composer_versions: Vec<
            rg_core::package_registry::adapters::composer::ComposerVersionInfo,
        > = versions
            .iter()
            .map(|v| {
                let archive = v.files.first();
                let filename = archive
                    .map(|f| f.filename.clone())
                    .unwrap_or_else(|| format!("{}.zip", v.version));
                rg_core::package_registry::adapters::composer::ComposerVersionInfo {
                    version: v.version.clone(),
                    filename,
                    // Digests of the archive the `dist` block points at, not of
                    // whatever the version recorded first.
                    sha256: archive.and_then(|f| v.sha256_of(f)),
                    sha1: archive.and_then(|f| f.sha1.clone()),
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

/// The Chart.yaml fields `index.yaml` republishes, read back out of the stored
/// version metadata.
#[derive(Default)]
struct HelmChartMetadata {
    app_version: Option<String>,
    api_version: Option<String>,
    keywords: Vec<String>,
    sources: Vec<String>,
}

/// Parse Helm-specific metadata from version metadata JSON.
fn parse_helm_metadata(metadata_json: Option<&str>) -> HelmChartMetadata {
    let Some(md) = metadata_json else {
        return HelmChartMetadata::default();
    };
    let doc: serde_json::Value = match serde_json::from_str(md) {
        Ok(v) => v,
        Err(_) => return HelmChartMetadata::default(),
    };

    let string_list = |key: &str| {
        doc.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|k| k.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };

    HelmChartMetadata {
        app_version: doc
            .get("appVersion")
            .and_then(|v| v.as_str())
            .map(String::from),
        api_version: doc
            .get("apiVersion")
            .and_then(|v| v.as_str())
            .map(String::from),
        keywords: string_list("keywords"),
        sources: string_list("sources"),
    }
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
