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
    extract::{FromRequest, Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use utoipa::ToSchema;

use crate::api::repo_access::{CiRead, Packages, RepoWrite};
use crate::AppState;
use rg_core::package_registry::encode_path_segment;

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

/// The CouchDB-shaped document `npm publish` PUTs to the package URL.
///
/// npm sends one new version and its tarball per request. Unknown top-level and
/// version fields are intentionally ignored: the tarball's `package.json` is
/// the artifact authority, while this envelope only identifies and carries it.
#[derive(Deserialize, ToSchema)]
pub struct NpmPublishPackument {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    #[serde(rename = "dist-tags")]
    pub dist_tags: BTreeMap<String, String>,
    pub versions: BTreeMap<String, NpmPublishVersion>,
    #[serde(rename = "_attachments")]
    pub attachments: BTreeMap<String, NpmPublishAttachment>,
}

#[derive(Deserialize, ToSchema)]
pub struct NpmPublishVersion {
    pub name: String,
    pub version: String,
}

#[derive(Deserialize, ToSchema)]
pub struct NpmPublishAttachment {
    pub data: String,
    pub length: usize,
}

#[derive(Debug)]
struct DecodedNpmPublish {
    filename: String,
    version: String,
    tarball: Vec<u8>,
}

/// Turn npm's JSON envelope into the one tarball the package service owns.
///
/// Current npm builds the attachment key as exactly
/// `<manifest.name>-<manifest.version>.tgz`; retaining that spelling is also
/// what makes a second publish of the same version hit the package-file unique
/// constraint instead of being mistaken for an additional Maven-style file.
fn decode_npm_publish_packument(
    path_name: &str,
    packument: NpmPublishPackument,
) -> Result<DecodedNpmPublish, String> {
    if packument.id != path_name || packument.name != path_name {
        return Err(format!(
            "npm package name mismatch: URL names '{path_name}', packument names '{}'",
            packument.name
        ));
    }

    if packument.versions.len() != 1 {
        return Err("npm publish packument must contain exactly one version".into());
    }
    let (version, manifest) = packument
        .versions
        .into_iter()
        .next()
        .expect("one version checked above");
    if manifest.name != path_name || manifest.version != version {
        return Err(format!(
            "npm version coordinates do not match URL and versions key '{version}'"
        ));
    }

    if packument.dist_tags.len() != 1
        || packument.dist_tags.get("latest").map(String::as_str) != Some(version.as_str())
    {
        return Err("npm publish currently supports only the default 'latest' dist-tag".into());
    }

    let filename = format!("{path_name}-{version}.tgz");
    if packument.attachments.len() != 1 {
        return Err(
            "npm publish packument must contain exactly one tarball attachment; provenance attachments are not supported"
                .into(),
        );
    }
    let attachment = packument
        .attachments
        .get(&filename)
        .ok_or_else(|| format!("npm publish packument is missing attachment '{filename}'"))?;
    let tarball = base64::engine::general_purpose::STANDARD
        .decode(&attachment.data)
        .map_err(|error| format!("npm tarball attachment is not valid base64: {error}"))?;
    if tarball.len() != attachment.length {
        return Err(format!(
            "npm tarball attachment length mismatch: declared {}, decoded {}",
            attachment.length,
            tarball.len()
        ));
    }

    Ok(DecodedNpmPublish {
        filename,
        version,
        tarball,
    })
}

#[cfg(test)]
mod npm_publish_packument_tests {
    use super::*;

    const NAME: &str = "@scope/matrix";
    const VERSION: &str = "1.2.3";
    const FILENAME: &str = "@scope/matrix-1.2.3.tgz";

    fn document() -> serde_json::Value {
        serde_json::json!({
            "_id": NAME,
            "name": NAME,
            "dist-tags": { "latest": VERSION },
            "versions": {
                VERSION: { "name": NAME, "version": VERSION }
            },
            "_attachments": {
                FILENAME: {
                    "data": base64::engine::general_purpose::STANDARD.encode(b"tarball"),
                    "length": 7
                }
            }
        })
    }

    fn decode(document: serde_json::Value) -> Result<DecodedNpmPublish, String> {
        decode_npm_publish_packument(NAME, serde_json::from_value(document).unwrap())
    }

    #[test]
    fn scoped_packument_keeps_the_client_attachment_name_and_bytes() {
        let decoded = decode(document()).unwrap();
        assert_eq!(decoded.filename, FILENAME);
        assert_eq!(decoded.version, VERSION);
        assert_eq!(decoded.tarball, b"tarball");
    }

    #[test]
    fn inconsistent_or_unsupported_packuments_fail_closed() {
        let mut wrong_name = document();
        wrong_name["_id"] = serde_json::json!("other");
        assert!(decode(wrong_name)
            .unwrap_err()
            .contains("package name mismatch"));

        let mut wrong_length = document();
        wrong_length["_attachments"][FILENAME]["length"] = serde_json::json!(8);
        assert!(decode(wrong_length)
            .unwrap_err()
            .contains("length mismatch"));

        let mut custom_tag = document();
        custom_tag["dist-tags"] = serde_json::json!({ "beta": VERSION });
        assert!(decode(custom_tag)
            .unwrap_err()
            .contains("default 'latest' dist-tag"));

        let mut provenance = document();
        provenance["_attachments"]["@scope/matrix-1.2.3.sigstore"] = serde_json::json!({
            "data": "{}",
            "length": 2
        });
        assert!(decode(provenance)
            .unwrap_err()
            .contains("provenance attachments are not supported"));
    }
}

#[derive(Deserialize, ToSchema)]
pub struct YankRequest {
    pub yank: bool,
}

#[derive(Serialize, ToSchema)]
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
    let filename = filename_from_disposition(&headers);
    publish_package(state, user_id, owner, name, pkg_type, query, filename, body).await
}

/// The upload's filename as the `Content-Disposition` header spells it.
///
/// Only the routes that carry the payload in the body need this: a protocol
/// route whose URL *is* the layout (Maven's, for one) reads the filename off the
/// path instead, and must not be handed this fallback.
fn filename_from_disposition(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_filename_from_disposition)
        .unwrap_or_else(|| "package".to_string())
}

#[derive(Default)]
struct TwineUpload {
    action: Option<String>,
    protocol_version: Option<String>,
    name: Option<String>,
    version: Option<String>,
    sha256_digest: Option<String>,
    filename: Option<String>,
    content: Option<axum::body::Bytes>,
}

fn set_twine_field<T>(slot: &mut Option<T>, field: &str, value: T) -> Result<(), AppError> {
    if slot.replace(value).is_some() {
        return Err(AppError::bad_request(format!(
            "Twine upload repeats the `{field}` field"
        )));
    }
    Ok(())
}

fn required_twine_field(value: Option<String>, field: &str) -> Result<String, AppError> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::bad_request(format!("Twine upload is missing `{field}`")))
}

/// Decode the multipart form emitted by `twine upload`.
///
/// Twine sends many descriptive metadata fields as well; the package adapter
/// reads the authoritative metadata from the wheel/sdist itself, so this
/// boundary only consumes the protocol controls, coordinates, digest and file.
async fn decode_twine_upload(mut multipart: Multipart) -> Result<TwineUpload, AppError> {
    let mut upload = TwineUpload::default();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::bad_request(format!("invalid Twine multipart body: {error}")))?
    {
        let Some(field_name) = field.name().map(str::to_owned) else {
            continue;
        };
        match field_name.as_str() {
            "content" => {
                let filename = field
                    .file_name()
                    .map(str::to_owned)
                    .filter(|filename| !filename.is_empty())
                    .ok_or_else(|| {
                        AppError::bad_request("Twine `content` field has no filename")
                    })?;
                let content = field.bytes().await.map_err(|error| {
                    AppError::bad_request(format!("cannot read Twine `content` field: {error}"))
                })?;
                set_twine_field(&mut upload.filename, "content", filename)?;
                set_twine_field(&mut upload.content, "content", content)?;
            }
            ":action" | "protocol_version" | "name" | "version" | "sha256_digest" => {
                let value = field.text().await.map_err(|error| {
                    AppError::bad_request(format!(
                        "Twine `{field_name}` field is not valid text: {error}"
                    ))
                })?;
                let slot = match field_name.as_str() {
                    ":action" => &mut upload.action,
                    "protocol_version" => &mut upload.protocol_version,
                    "name" => &mut upload.name,
                    "version" => &mut upload.version,
                    "sha256_digest" => &mut upload.sha256_digest,
                    _ => unreachable!("matched above"),
                };
                set_twine_field(slot, &field_name, value)?;
            }
            _ => {}
        }
    }

    Ok(upload)
}

/// POST /api/v1/repos/{owner}/{name}/packages/pypi/legacy[/]
///
/// The upload side of the legacy PyPI API used by Twine. The simple index is
/// the read side; without this protocol endpoint a package could be installed
/// from ForgeKeep but no standard Python client could publish it there.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/packages/pypi/legacy/",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
    ),
    request_body(
        content_type = "multipart/form-data",
        description = "Twine legacy upload form with package metadata and a content file",
    ),
    responses(
        (status = 200, description = "Package uploaded", body = PublishResponse),
        (status = 400, description = "Malformed form, package, or digest", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "Distribution already exists", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn pypi_legacy_upload(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    Path((owner, name)): Path<(String, String)>,
    multipart: Multipart,
) -> axum::response::Response {
    let upload = match decode_twine_upload(multipart).await {
        Ok(upload) => upload,
        Err(error) => return error.into_response(),
    };

    let action = match required_twine_field(upload.action, ":action") {
        Ok(action) => action,
        Err(error) => return error.into_response(),
    };
    if action != "file_upload" {
        return AppError::bad_request("Twine `:action` must be `file_upload`").into_response();
    }

    let protocol_version = match required_twine_field(upload.protocol_version, "protocol_version") {
        Ok(version) => version,
        Err(error) => return error.into_response(),
    };
    if protocol_version != "1" {
        return AppError::bad_request("Twine `protocol_version` must be `1`").into_response();
    }

    let package_name = match required_twine_field(upload.name, "name") {
        Ok(name) => name,
        Err(error) => return error.into_response(),
    };
    let version = match required_twine_field(upload.version, "version") {
        Ok(version) => version,
        Err(error) => return error.into_response(),
    };
    let claimed_digest = match required_twine_field(upload.sha256_digest, "sha256_digest") {
        Ok(digest) => digest,
        Err(error) => return error.into_response(),
    };
    if claimed_digest.len() != 64 || !claimed_digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return AppError::bad_request("Twine `sha256_digest` must be 64 hexadecimal characters")
            .into_response();
    }
    let Some(filename) = upload.filename else {
        return AppError::bad_request("Twine upload is missing `content`").into_response();
    };
    let Some(content) = upload.content else {
        return AppError::bad_request("Twine upload is missing `content`").into_response();
    };
    let actual_digest = hex::encode(Sha256::digest(&content));
    if !claimed_digest.eq_ignore_ascii_case(&actual_digest) {
        return AppError::bad_request(format!(
            "Twine `sha256_digest` mismatch: claimed {claimed_digest}, calculated {actual_digest}"
        ))
        .into_response();
    }

    let query = PublishPackageQuery {
        name: Some(package_name),
        version: Some(version),
        description: None,
        homepage: None,
        repository_url: None,
        semver: None,
    };
    let mut response = publish_package(
        state,
        user_id,
        owner,
        name,
        "pypi".to_string(),
        query,
        filename,
        content,
    )
    .await;
    // Twine's legacy upload contract uses 200 for a successful POST. Keep the
    // generic service's body, but do not expose its REST-specific 201 here.
    if response.status().is_success() {
        *response.status_mut() = StatusCode::OK;
    }
    response
}

async fn decode_nuget_push(mut multipart: Multipart) -> Result<axum::body::Bytes, AppError> {
    let mut package = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::bad_request(format!("invalid NuGet multipart body: {error}")))?
    {
        if field.name() != Some("package") {
            continue;
        }
        if package.is_some() {
            return Err(AppError::bad_request(
                "NuGet upload repeats the `package` field",
            ));
        }
        package = Some(field.bytes().await.map_err(|error| {
            AppError::bad_request(format!("cannot read NuGet `package` field: {error}"))
        })?);
    }

    package.ok_or_else(|| AppError::bad_request("NuGet upload is missing `package`"))
}

/// POST/PUT /api/v1/repos/{owner}/{name}/packages/nuget/publish
///
/// `dotnet nuget push` sends PUT to the advertised `PackagePublish` resource,
/// with the nupkg in a multipart field named `package`. Its part filename is
/// always the generic `package.nupkg`, not the artifact's own name. Keep raw
/// bodies too: ForgeKeep exposed that contract before the native-client route
/// existed, and the explicit POST route still serves those publishers.
pub async fn nuget_publish(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<PublishPackageQuery>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let headers = request.headers().clone();
    let is_multipart = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("multipart/form-data"));

    let (filename, body) = if is_multipart {
        let multipart = match Multipart::from_request(request, &state).await {
            Ok(multipart) => multipart,
            Err(rejection) => return rejection.into_response(),
        };
        let body = match decode_nuget_push(multipart).await {
            Ok(body) => body,
            Err(error) => return error.into_response(),
        };
        // NuGet.Client deliberately sends a random filename; using the same
        // stable spelling also makes a repeated file hit the uniqueness guard.
        ("package.nupkg".to_string(), body)
    } else {
        let filename = filename_from_disposition(&headers);
        let body = match axum::body::Bytes::from_request(request, &state).await {
            Ok(body) => body,
            Err(rejection) => return rejection.into_response(),
        };
        (filename, body)
    };

    publish_package(
        state,
        user_id,
        owner,
        name,
        "nuget".to_string(),
        query,
        filename,
        body,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn publish_package(
    state: AppState,
    user_id: i64,
    owner: String,
    name: String,
    pkg_type: String,
    query: PublishPackageQuery,
    filename: String,
    body: axum::body::Bytes,
) -> axum::response::Response {
    if !rg_core::package_registry::package_types::is_valid(&pkg_type) {
        return err(
            StatusCode::BAD_REQUEST,
            &format!("unsupported package type: {}", pkg_type),
        );
    }

    // Try to auto-extract metadata via the adapter
    let adapter = rg_core::package_registry::get_adapter(&pkg_type);
    let adapter_meta = if let Some(ref adapter) = adapter {
        if let Err(e) = adapter.validate(&body) {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("invalid package payload: {e:#}"),
            );
        }
        // `validate` above is the verdict on the artifact itself, and it is
        // fatal. What is left here is reading a name and a version out of it,
        // which is a convenience the caller can also do by hand — so its
        // failure is fatal only when nothing else can supply them.
        //
        // Both halves of that used to be wrong, because `.ok()` dropped the
        // verdict and its reason together. Without query params the caller was
        // answered `package name is required` by the resolution below — a
        // message naming the wrong cause, since the name was not missing, it
        // was unreadable. With query params the failure left no trace at all,
        // not even a log line.
        //
        // The fall-through is not a loophole: a multi-artifact format reaches
        // it legitimately. Maven publishes one version as a `.pom` plus a
        // `.jar` plus `matrix-1.0.0-sources.jar`, and only the first carries a
        // manifest — the classifier suffix defeats the `{name}-{version}`
        // filename convention by design, and the caller names the coordinates
        // in the query precisely because the file cannot. That is why the
        // corrupt-manifest cases this guards against belong in `validate`,
        // where they are unconditional: `HelmAdapter` and `DockerAdapter` were
        // moved there rather than being caught by a blanket refusal here.
        //
        // The adapter's message is passed through verbatim — each already
        // names its own manifest and reason ("invalid Chart.yaml: … at line 3
        // column 5", ".nuspec missing <id> element"), and a wrapper sentence of
        // ours would only talk over it.
        match adapter.extract_metadata(&filename, &body) {
            Ok(meta) => Some(meta),
            Err(e) if query.name.is_some() && query.version.is_some() => {
                tracing::warn!(
                    package_type = %pkg_type,
                    filename = %filename,
                    error = %format!("{e:#}"),
                    "no metadata read from the uploaded file; publishing under the \
                     name and version given in the query"
                );
                None
            }
            Err(e) => return err(StatusCode::BAD_REQUEST, &format!("{e:#}")),
        }
    } else {
        None
    };

    persist_package(
        state,
        user_id,
        owner,
        name,
        pkg_type,
        query,
        filename,
        body,
        adapter_meta,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn persist_package(
    state: AppState,
    user_id: i64,
    owner: String,
    name: String,
    pkg_type: String,
    query: PublishPackageQuery,
    filename: String,
    body: axum::body::Bytes,
    adapter_meta: Option<rg_core::package_registry::ExtractedMetadata>,
) -> axum::response::Response {
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

/// PUT the packument envelope emitted by `npm publish` to the package URL.
#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/packages/npm/{pkg_name}",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_name" = String, Path, description = "npm package name"),
    ),
    request_body(
        content = NpmPublishPackument,
        content_type = "application/json",
        description = "npm publish packument with a base64 tarball attachment",
    ),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Malformed or unsupported packument", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "Package version already exists", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn publish_npm_packument(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    Path((owner, repo, pkg_name)): Path<(String, String, String)>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let packument = match serde_json::from_slice::<NpmPublishPackument>(&body) {
        Ok(packument) => packument,
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("invalid npm publish packument: {error}"),
            )
        }
    };
    let decoded = match decode_npm_publish_packument(&pkg_name, packument) {
        Ok(decoded) => decoded,
        Err(message) => return err(StatusCode::BAD_REQUEST, &message),
    };

    // The envelope and the URL are claims; package.json inside the tarball is
    // the artifact's own identity. Refuse disagreement rather than relying on
    // the generic query-parameter override used by multi-artifact formats.
    let adapter =
        rg_core::package_registry::get_adapter("npm").expect("npm is a built-in package adapter");
    if let Err(error) = adapter.validate(&decoded.tarball) {
        return err(
            StatusCode::BAD_REQUEST,
            &format!("invalid package payload: {error:#}"),
        );
    }
    let metadata = match adapter.extract_metadata(&decoded.filename, &decoded.tarball) {
        Ok(metadata) => metadata,
        Err(error) => return err(StatusCode::BAD_REQUEST, &format!("{error:#}")),
    };
    if metadata.name != pkg_name || metadata.version != decoded.version {
        return err(
            StatusCode::BAD_REQUEST,
            "npm tarball package.json does not match the publish URL and packument version",
        );
    }

    let query = PublishPackageQuery {
        name: Some(pkg_name),
        version: Some(decoded.version),
        description: None,
        homepage: None,
        repository_url: None,
        semver: None,
    };
    persist_package(
        state,
        user_id,
        owner,
        repo,
        "npm".to_string(),
        query,
        decoded.filename,
        decoded.tarball.into(),
        Some(metadata),
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
    RepoWrite { actor_id, .. }: RepoWrite,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let storage =
        rg_core::package_registry::PackageStorage::from_backend(state.blob_storage.clone());
    // Publishing is attributed forever — `PublishInfo` puts `author_id` on the
    // version row. Deleting removes that row and used to leave nothing in its
    // place, so the one registry operation that cannot be undone was the one
    // operation nobody could be identified for (card_6baa3e341bf3). The actor
    // comes from the gate that already admitted the request, not from headers.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::package_registry::service::delete_version(
        &state.db, &storage, &owner, &name, &pkg_type, &pkg_name, &version,
    )
    .await
    {
        Ok(_) => {
            // Fire-and-forget, after the fact: the bytes and the rows are
            // already gone and no audit failure may turn that into an error the
            // caller would retry.
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                "package.delete",
                Some("package_version"),
                None,
                Some(&format!("{owner}/{name}/{pkg_name}@{version}")),
                Some(&headers),
                Some(serde_json::json!({
                    "repo": format!("{owner}/{name}"),
                    "pkg_type": pkg_type,
                    "pkg_name": pkg_name,
                    "version": version,
                })),
            )
            .await;
            (StatusCode::NO_CONTENT,).into_response()
        }
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
    RepoWrite { actor_id, .. }: RepoWrite,
    headers: axum::http::HeaderMap,
    Json(body): Json<YankRequest>,
) -> axum::response::Response {
    // Publishing is attributed forever — `PublishInfo` puts `author_id` on the
    // version row. Deleting removes that row and used to leave nothing in its
    // place, so the one registry operation that cannot be undone was the one
    // operation nobody could be identified for (card_6baa3e341bf3). The actor
    // comes from the gate that already admitted the request, not from headers.
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::package_registry::service::yank_version(
        &state.db, &owner, &name, &pkg_type, &pkg_name, &version, body.yank,
    )
    .await
    {
        Ok(_) => {
            // Two actions, not one flag: a journal is read by filtering on
            // `action`, and "who yanked this" and "who put it back" are
            // different questions.
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                if body.yank {
                    "package.yank"
                } else {
                    "package.unyank"
                },
                Some("package_version"),
                None,
                Some(&format!("{owner}/{name}/{pkg_name}@{version}")),
                Some(&headers),
                Some(serde_json::json!({
                    "repo": format!("{owner}/{name}"),
                    "pkg_type": pkg_type,
                    "pkg_name": pkg_name,
                    "version": version,
                    "yanked": body.yank,
                })),
            )
            .await;
            (
                StatusCode::OK,
                Json(serde_json::json!({"yanked": body.yank})),
            )
                .into_response()
        }
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
        Ok(file) => {
            let rg_core::package_registry::service::DownloadedFile {
                data,
                content_type,
                sha256,
                ..
            } = file;
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
            let mut response = (
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
                .into_response();
            // The digest the bytes were just verified against, advertised the way
            // the release-asset, CI-artifact and CI-cache handlers advertise
            // theirs, so a client can check the same thing end to end. Reaching
            // here means the value equals `hex::encode(...)` output, so the
            // conversion cannot actually fail; dropping an unrepresentable one
            // would cost an advisory header, never the verification itself.
            if let Some(value) = sha256
                .as_deref()
                .and_then(|sha256| axum::http::HeaderValue::from_str(sha256).ok())
            {
                response.headers_mut().insert(
                    axum::http::HeaderName::from_static("x-checksum-sha256"),
                    value,
                );
            }
            response
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
            yanked: !v.is_install_candidate(),
            // Dependencies and features live only in the manifest inside the
            // `.crate`; the adapter lifted them here at publish, and this is
            // where cargo's resolver reads them back.
            metadata: v.metadata.as_deref(),
        })
        .collect();

    // A row this registry cannot read is this registry's failure. Serving the
    // entry anyway would tell cargo the crate has no dependencies — resolution
    // then succeeds on that and the build dies at `unresolved import`.
    let body = match rg_core::package_registry::build_sparse_index(pkg_name, &entries) {
        Ok(body) => body,
        Err(e) => return package_error_response(e),
    };

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
        // npm has no version-level yank marker. Leaving the object in the
        // packument makes an exact request install it as if it were live.
        .filter(|v| v.is_install_candidate())
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
                yanked: !v.is_install_candidate(),
                // Dependency tables live only in the `package.json` inside the
                // `.tgz`; the adapter lifted them here at publish, and this is
                // where npm's resolver reads them back.
                metadata: v.metadata.clone(),
            }
        })
        .collect();

    // Same as the cargo index above: an unreadable row must not be served as a
    // packument saying the version depends on nothing.
    let metadata = match rg_core::package_registry::build_npm_metadata(
        &pkg_name,
        &npm_versions,
        &base_url,
        &owner,
        &name,
    ) {
        Ok(metadata) => metadata,
        Err(e) => return package_error_response(e),
    };

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
            encode_path_segment(&owner),
            encode_path_segment(&name),
            encode_path_segment(&project),
            encode_path_segment(version),
            encode_path_segment(filename),
        )
    };

    // One link per distribution *file*, not per version. A release routinely
    // carries a wheel and an sdist — `twine upload dist/*` publishes both — and
    // a page with one link per version hid every artifact but the first from
    // pip entirely. The digest beside each link is that file's own, because
    // that is what the client hashes after downloading it.
    let requires_python_by_version = versions
        .iter()
        .map(|v| parse_pypi_requires_python(v.metadata.as_deref(), &project, &v.version))
        .collect::<anyhow::Result<Vec<_>>>();
    let requires_python_by_version = match requires_python_by_version {
        Ok(metadata) => metadata,
        Err(error) => return package_error_response(error),
    };
    let entries: Vec<rg_core::package_registry::PyPIVersionEntry> = versions
        .iter()
        .zip(requires_python_by_version)
        .flat_map(|(v, requires_python)| {
            // Both attributes describe the *version*, so every file of it
            // carries the same pair. A yanked version stays on the page — that
            // is what keeps an exact pin resolvable — and `data-yanked` is what
            // takes it out of every other resolution (PEP 592).
            let yanked = !v.is_install_candidate();

            let files: Vec<rg_core::package_registry::PyPIVersionEntry> = v
                .files
                .iter()
                .filter(|f| is_pypi_distribution(&f.filename))
                .map(|f| rg_core::package_registry::PyPIVersionEntry {
                    version: v.version.clone(),
                    filename: f.filename.clone(),
                    sha256: v.sha256_of(f),
                    download_url: link_to(&v.version, &f.filename),
                    requires_python: requires_python.clone(),
                    yanked,
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
                requires_python,
                yanked,
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

    let entries: Result<Vec<rg_core::package_registry::MavenVersionEntry>, anyhow::Error> =
        versions
            .iter()
            // Maven metadata has no spelling for a withdrawn release. Omitting it
            // removes it from both `<versions>` and the derived `<release>`.
            .filter(|v| v.is_install_candidate())
            .map(|v| {
                let updated = chrono::DateTime::parse_from_rfc3339(&v.created_at)
                    .map_err(|error| {
                        anyhow::anyhow!(
                        "stored creation time for Maven package version '{}' is invalid: {error}",
                        v.version
                    )
                    })?
                    .to_utc()
                    .format("%Y%m%d%H%M%S")
                    .to_string();
                Ok(rg_core::package_registry::MavenVersionEntry {
                    version: v.version.clone(),
                    is_snapshot: v.version.ends_with("-SNAPSHOT"),
                    updated,
                })
            })
            .collect();
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) => return package_error_response(error),
    };

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

    // A resolver fetches `<artifact>.jar.sha1` right after the artifact to check
    // it. The sidecars are not stored — see `maven_upload`, which verifies them
    // on the way in rather than keeping a second copy of a number the bytes
    // already determine — so they are computed here from the file itself. A
    // registry that 404s them makes every `mvn` build print a checksum warning
    // for artifacts that are in fact intact.
    if let Some((target, algorithm)) =
        rg_core::package_registry::MavenChecksum::split_sidecar(filename)
    {
        return match read_package_file(
            &state,
            &request.owner,
            &request.repo,
            &pkg_name,
            version,
            target,
        )
        .await
        {
            Ok(data) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                algorithm.hex(&data),
            )
                .into_response(),
            Err(response) => response,
        };
    }

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

/// The stored bytes of one Maven file, or the response that explains their absence.
async fn read_package_file(
    state: &AppState,
    owner: &str,
    repo: &str,
    pkg_name: &str,
    version: &str,
    filename: &str,
) -> Result<Vec<u8>, axum::response::Response> {
    let storage =
        rg_core::package_registry::PackageStorage::from_backend(state.blob_storage.clone());
    match rg_core::package_registry::service::download_file(
        &state.db, &storage, owner, repo, "maven", pkg_name, version, filename,
    )
    .await
    {
        Ok(file) => Ok(file.data),
        Err(error) => Err(package_file_error_response(error)),
    }
}

// ── Cargo write API ───────────────────────────────────────
//
// `cargo publish` / `cargo yank` derive these URLs from the `api` key of the
// sparse index's `config.json`. Reading the index was served long before any of
// this was, so a crate could be resolved from ForgeKeep but never put there by
// the tool that builds it (card_5a790cc6ac35).

/// Split cargo's publish body into its metadata and its `.crate`.
///
/// The frame is two length-prefixed blocks — `u32-LE len`, JSON, `u32-LE len`,
/// archive — and cargo sends nothing else, so a body this cannot read is a
/// protocol mismatch worth naming rather than a bad crate.
fn split_cargo_publish_frame(body: &[u8]) -> Result<(&[u8], &[u8]), String> {
    fn take<'a>(rest: &'a [u8], what: &str) -> Result<(&'a [u8], &'a [u8]), String> {
        let (len, rest) = rest
            .split_at_checked(4)
            .ok_or_else(|| format!("body ends before the {what} length prefix"))?;
        let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let (block, rest) = rest.split_at_checked(len).ok_or_else(|| {
            format!("the {what} length prefix claims {len} bytes, the body has fewer")
        })?;
        Ok((block, rest))
    }

    let (metadata, rest) = take(body, "metadata")?;
    let (archive, rest) = take(rest, "crate")?;
    if !rest.is_empty() {
        return Err(format!("{} trailing byte(s) after the crate", rest.len()));
    }
    Ok((metadata, archive))
}

/// PUT /api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/new
///
/// What `cargo publish` sends: one request whose body is the index metadata and
/// the `.crate` archive, each behind a `u32-LE` length. The name and version are
/// taken from that metadata rather than from the archive, because it is the
/// metadata cargo will expect back out of the index.
pub async fn cargo_publish_new(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    Path((owner, name)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let (metadata, archive) = match split_cargo_publish_frame(&body) {
        Ok(split) => split,
        Err(reason) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("malformed cargo publish body: {reason}"),
            )
        }
    };

    let metadata: serde_json::Value = match serde_json::from_slice(metadata) {
        Ok(value) => value,
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("cargo publish metadata is not JSON: {error}"),
            )
        }
    };
    let (Some(crate_name), Some(version)) = (
        metadata["name"].as_str().filter(|v| !v.is_empty()),
        metadata["vers"].as_str().filter(|v| !v.is_empty()),
    ) else {
        return err(
            StatusCode::BAD_REQUEST,
            "cargo publish metadata must carry a non-empty `name` and `vers`",
        );
    };

    let query = PublishPackageQuery {
        name: Some(crate_name.to_string()),
        version: Some(version.to_string()),
        description: None,
        homepage: None,
        repository_url: None,
        semver: None,
    };
    let filename = format!("{crate_name}-{version}.crate");

    let published = publish_package(
        state,
        user_id,
        owner,
        name,
        "cargo".to_string(),
        query,
        filename,
        axum::body::Bytes::copy_from_slice(archive),
    )
    .await;

    // cargo reads the body of a 2xx as its warnings document and reports
    // anything else as a publish failure, so the generic publish envelope
    // cannot be passed through.
    if !published.status().is_success() {
        return published;
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "warnings": { "invalid_categories": [], "invalid_badges": [], "other": [] }
        })),
    )
        .into_response()
}

/// DELETE /api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate}/{version}/yank
pub async fn cargo_yank(
    state: State<AppState>,
    write: RepoWrite,
    path: Path<(String, String, String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    cargo_set_yanked(state, write, path, headers, true).await
}

/// PUT /api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate}/{version}/unyank
pub async fn cargo_unyank(
    state: State<AppState>,
    write: RepoWrite,
    path: Path<(String, String, String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    cargo_set_yanked(state, write, path, headers, false).await
}

/// The shared body of [`cargo_yank`] / [`cargo_unyank`].
///
/// cargo separates the two by verb rather than by a flag in a body, and answers
/// `{"ok": true}` — the generic yank route's `{"yanked": …}` envelope is not
/// what it reads.
async fn cargo_set_yanked(
    State(state): State<AppState>,
    RepoWrite { actor_id, .. }: RepoWrite,
    Path((owner, name, crate_name, version)): Path<(String, String, String, String)>,
    headers: axum::http::HeaderMap,
    yanked: bool,
) -> axum::response::Response {
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::package_registry::service::yank_version(
        &state.db,
        &owner,
        &name,
        "cargo",
        &crate_name,
        &version,
        yanked,
    )
    .await
    {
        Ok(_) => {
            rg_core::audit::record(
                &state.db,
                &audit_actor,
                if yanked {
                    "package.yank"
                } else {
                    "package.unyank"
                },
                Some("package_version"),
                None,
                Some(&format!("{owner}/{name}/{crate_name}@{version}")),
                Some(&headers),
                Some(serde_json::json!({
                    "repo": format!("{owner}/{name}"),
                    "pkg_type": "cargo",
                    "pkg_name": crate_name,
                    "version": version,
                    "yanked": yanked,
                })),
            )
            .await;
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

/// PUT /api/v1/repos/{owner}/{name}/packages/maven/{group…}/{artifact}/maven-metadata.xml
///
/// Accepted, and deliberately not stored. `mvn deploy` finishes by uploading its
/// own copy of the version list, but the list this registry serves is derived
/// from its own rows (see [`maven_metadata`]) — keeping the client's copy would
/// create a second, immediately stale source of the same answer, and refusing it
/// would fail the whole deploy over a document nobody reads back.
///
/// Separate from [`maven_upload`] because on this route the filename is part of
/// the pattern rather than a capture: the shared handler would read `{m3}` as
/// the file and mistake the coordinate for one segment short.
pub async fn maven_upload_metadata(
    _state: State<AppState>,
    _write: RepoWrite,
    _params: axum::extract::RawPathParams,
    _body: axum::body::Bytes,
) -> axum::response::Response {
    (StatusCode::OK, "").into_response()
}

/// PUT /api/v1/repos/{owner}/{name}/packages/maven/{group…}/{artifact}/{version}/{file}
///
/// The way `mvn deploy` publishes: one `PUT` per file, addressed by the very
/// layout the resolver later reads. The registry only had `POST
/// .../packages/maven/publish`, a spelling no Maven client knows, so the
/// deploy half of the round trip could not be driven by the real tool at all
/// (card_11d8655a9cd8).
///
/// Three kinds of upload arrive on this path and each is answered differently:
///
///   * `maven-metadata.xml` is **accepted and not stored**. The version list
///     this registry serves is derived from its own rows (`maven_metadata`), so
///     storing the client's copy would create a second, immediately stale
///     source of the same answer. Refusing it instead would fail the deploy over
///     a document we do not need.
///   * `*.sha1` / `*.md5` are **verified, not stored**. A checksum is a claim
///     about bytes we already hold, so the useful thing to do with it is check
///     it — a mismatch fails the deploy loudly, which is the entire point of
///     sending one. `maven_download` computes them back on the way out.
///   * everything else is the artifact, published under the coordinate the URL
///     spells out. The URL wins over anything the POM says, because the URL is
///     where the resolver will come looking.
pub async fn maven_upload(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    params: axum::extract::RawPathParams,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let request = MavenRequest::from_params(&params);

    let Some((filename, head)) = request.segments.split_last() else {
        return err(StatusCode::NOT_FOUND, "empty Maven path");
    };

    // A SNAPSHOT deploy sends `<group…>/<artifact>/<version>/maven-metadata.xml`,
    // and its checksums arrive as ordinary files because the sidecar suffix
    // stops the static route from matching. Both are the derived document —
    // accepted, not stored, same as the version-less spelling that
    // `maven_upload_metadata` answers.
    let base = rg_core::package_registry::MavenChecksum::split_sidecar(filename)
        .map_or(filename.as_str(), |(target, _)| target);
    if base == "maven-metadata.xml" {
        return (StatusCode::OK, "").into_response();
    }

    let Some((version, head)) = head.split_last() else {
        return err(StatusCode::NOT_FOUND, "Maven path carries no version");
    };
    let Some((group_id, artifact_id)) = MavenRequest::coordinates(head) else {
        return err(
            StatusCode::NOT_FOUND,
            "Maven path carries no groupId/artifactId",
        );
    };
    let pkg_name = format!("{group_id}:{artifact_id}");

    if let Some((target, algorithm)) =
        rg_core::package_registry::MavenChecksum::split_sidecar(filename)
    {
        let stored = match read_package_file(
            &state,
            &request.owner,
            &request.repo,
            &pkg_name,
            version,
            target,
        )
        .await
        {
            Ok(data) => data,
            Err(response) => return response,
        };
        let expected = algorithm.hex(&stored);
        let claimed = String::from_utf8_lossy(&body);
        // Maven writes the bare hex digest, but some clients append a filename
        // the way `sha1sum` does.
        let claimed = claimed.split_whitespace().next().unwrap_or("");
        if !claimed.eq_ignore_ascii_case(&expected) {
            return err(
                StatusCode::BAD_REQUEST,
                &format!(
                    "checksum mismatch for {target}: the upload claims {claimed}, \
                     the stored file is {expected}"
                ),
            );
        }
        return (StatusCode::OK, "").into_response();
    }

    let query = PublishPackageQuery {
        name: Some(pkg_name),
        version: Some(version.clone()),
        description: None,
        homepage: None,
        repository_url: None,
        semver: None,
    };

    publish_package(
        state,
        user_id,
        request.owner,
        request.repo,
        "maven".to_string(),
        query,
        filename.clone(),
        body,
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
    let pkg_name = match resolve_nuget_id(&state, &owner, &name, &pkg_name).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "nuget", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return package_error_response(e),
    };

    let base_url = build_base_url(&headers);
    let registration_url = format!(
        "{}/api/v1/repos/{}/{}/packages/nuget/registration/{}/index.json",
        base_url.trim_end_matches('/'),
        encode_path_segment(&owner),
        encode_path_segment(&name),
        encode_path_segment(&pkg_name.to_lowercase()),
    );

    let version_metadata = versions
        .iter()
        .map(|v| parse_nuget_metadata(v.metadata.as_deref(), &pkg_name, &v.version))
        .collect::<anyhow::Result<Vec<_>>>();
    let version_metadata = match version_metadata {
        Ok(metadata) => metadata,
        Err(error) => return package_error_response(error),
    };
    let entries: Vec<rg_core::package_registry::NuGetRegistrationEntry> = versions
        .iter()
        .zip(version_metadata)
        .map(|(v, meta)| {
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
                encode_path_segment(&owner),
                encode_path_segment(&name),
                encode_path_segment(&pkg_name),
                encode_path_segment(&v.version),
                encode_path_segment(&filename),
            );

            rg_core::package_registry::NuGetRegistrationEntry {
                version: v.version.clone(),
                description: meta.description,
                homepage: meta.homepage,
                license: meta.license,
                tags: meta.tags,
                download_url,
                dependency_groups: meta.dependency_groups,
                // A yanked version stays in the registration so a consumer that
                // already resolved it keeps restoring; `listed` is what keeps it
                // out of a fresh resolution.
                listed: v.is_install_candidate(),
            }
        })
        .collect();

    let json =
        rg_core::package_registry::build_registration_index(&pkg_name, &registration_url, &entries);

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

    // List all nuget packages in the repo. A repository that never enabled the
    // registry answers an empty result set rather than a 404: the service index
    // advertises this endpoint for any repository, so it owes a readable answer.
    let packages = match nuget_search_packages(
        &state,
        &owner,
        &name,
        params.prerelease,
        params.semver_level.as_deref(),
    )
    .await
    {
        Ok(packages) => packages,
        Err(response) => return response,
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
                encode_path_segment(&owner),
                encode_path_segment(&name),
                encode_path_segment(&pkg.name),
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
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default, rename = "semVerLevel")]
    pub semver_level: Option<String>,
}

/// Resolve the package id a NuGet client asked for to the id it was published
/// under.
///
/// Ids are case-insensitive and every v3 URL carries the lowercase form, so
/// `dotnet restore` of `Matrix.NuGet` asks for `matrix.nuget` while the registry
/// stores what the nuspec said. Matching the path segment literally therefore
/// missed every id with a capital in it — the same defect PyPI already fixed
/// with [`resolve_pypi_project`] (card_dba77cceec56).
///
/// One query, and an exact spelling still wins over a case-folded one, so a
/// registry that somehow holds two ids differing only in case keeps answering
/// for the one that was actually asked for. An unresolvable id is handed back
/// unchanged: producing the 404 is the caller's job, not this helper's.
async fn resolve_nuget_id(
    state: &AppState,
    owner: &str,
    repo: &str,
    requested: &str,
) -> Result<String, axum::response::Response> {
    let packages =
        match rg_core::package_registry::service::list_packages(&state.db, owner, repo, "nuget")
            .await
        {
            Ok(packages) => packages,
            Err(error) if package_is_absent(&error) => return Ok(requested.to_string()),
            Err(error) => return Err(package_error_response(error)),
        };

    if packages.iter().any(|pkg| pkg.name == requested) {
        return Ok(requested.to_string());
    }

    let wanted = rg_core::package_registry::normalize_package_id(requested);
    Ok(packages
        .into_iter()
        .find(|pkg| rg_core::package_registry::normalize_package_id(&pkg.name) == wanted)
        .map_or_else(|| requested.to_string(), |pkg| pkg.name))
}

/// The nuget packages of a repository, or nothing at all.
///
/// A repository that never enabled the registry is not an error on the discovery
/// routes: the service index is served for any repository, so every resource it
/// advertises has to answer something a client can read. An empty answer is that
/// something — a 404 here would make the advertised endpoint look unrouted.
async fn nuget_packages(
    state: &AppState,
    owner: &str,
    repo: &str,
) -> Result<Vec<rg_core::package_registry::PackageSummary>, axum::response::Response> {
    match rg_core::package_registry::service::list_packages(&state.db, owner, repo, "nuget").await {
        Ok(packages) => Ok(packages),
        Err(error) if package_is_absent(&error) => Ok(Vec::new()),
        Err(error) => Err(package_error_response(error)),
    }
}

async fn nuget_search_packages(
    state: &AppState,
    owner: &str,
    repo: &str,
    include_prerelease: bool,
    semver_level: Option<&str>,
) -> Result<Vec<rg_core::package_registry::PackageSummary>, axum::response::Response> {
    match rg_core::package_registry::service::list_nuget_search_packages(
        &state.db,
        owner,
        repo,
        include_prerelease,
        semver_level,
    )
    .await
    {
        Ok(packages) => Ok(packages),
        Err(error) if package_is_absent(&error) => Ok(Vec::new()),
        Err(error) => Err(package_error_response(error)),
    }
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/package/{id}/index.json
///
/// NuGet flat container (`PackageBaseAddress/3.0.0`) — the versions of one
/// package. This is the first hop of `dotnet restore`, and it had no route at
/// all while the service index advertised the resource, so restore could not
/// download anything (card_dba77cceec56).
pub async fn nuget_flat_container_index(
    State(state): State<AppState>,
    Path((owner, name, pkg_name)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let pkg_name = match resolve_nuget_id(&state, &owner, &name, &pkg_name).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "nuget", &pkg_name,
    )
    .await
    {
        Ok(versions) => versions,
        Err(error) => return package_error_response(error),
    };

    let listed: Vec<String> = versions.into_iter().map(|v| v.version).collect();
    let json = rg_core::package_registry::build_flat_container_index(&listed);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/package/{id}/{version}/{file}
///
/// NuGet flat container package content — the `.nupkg` itself.
///
/// The client builds this URL out of *normalized* parts (`{id-lower}` and
/// `{id-lower}.{version}.nupkg`), which is almost never the filename the package
/// was published under, so the stored file is matched case-insensitively and
/// then by extension rather than by the client's spelling.
pub async fn nuget_flat_container_download(
    State(state): State<AppState>,
    Path((owner, name, pkg_name, version, filename)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let pkg_name = match resolve_nuget_id(&state, &owner, &name, &pkg_name).await {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };

    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "nuget", &pkg_name,
    )
    .await
    {
        Ok(versions) => versions,
        Err(error) => return package_error_response(error),
    };

    let Some(found) = versions
        .iter()
        .find(|v| rg_core::package_registry::nuget_versions_match(&v.version, &version))
    else {
        return err_text(
            StatusCode::NOT_FOUND,
            &format!("package '{pkg_name}' has no version '{version}'"),
        );
    };

    let wanted_file = filename.to_lowercase();
    let Some(file) = found
        .files
        .iter()
        .find(|f| f.filename.to_lowercase() == wanted_file)
        .or_else(|| {
            found
                .files
                .iter()
                .find(|f| f.filename.to_lowercase().ends_with(".nupkg"))
        })
    else {
        return err_text(
            StatusCode::NOT_FOUND,
            &format!("no package content stored for '{pkg_name}' {version}"),
        );
    };

    serve_package_file(
        &state,
        &owner,
        &name,
        "nuget",
        &pkg_name,
        &found.version,
        &file.filename,
    )
    .await
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/autocomplete?q=...
///
/// NuGet `SearchAutocompleteService/3.5.0`. The service index used to advertise
/// this resource at the bare `nuget/` root, which is not an endpoint of anything
/// (card_dba77cceec56).
pub async fn nuget_autocomplete(
    State(state): State<AppState>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<NuGetSearchParams>,
) -> axum::response::Response {
    let query = params.q.as_deref().unwrap_or("").to_lowercase();
    let packages = match nuget_packages(&state, &owner, &name).await {
        Ok(packages) => packages,
        Err(response) => return response,
    };

    let names: Vec<String> = packages
        .into_iter()
        .filter(|pkg| query.is_empty() || pkg.name.to_lowercase().contains(&query))
        .map(|pkg| pkg.name)
        .collect();
    let total_hits = names.len();
    let json = rg_core::package_registry::build_autocomplete_results(&names, total_hits);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
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
            // A withdrawn version is not a candidate. The compact index has
            // always dropped them; this endpoint is the other half of the same
            // resolution and used to offer them (card_0c9e858230b6).
            if !v.is_install_candidate() {
                continue;
            }

            // Parse dependencies from metadata JSON
            let deps = match parse_rubygems_deps(v.metadata.as_deref(), gem_name, &v.version) {
                Ok(deps) => deps,
                Err(error) => return package_error_response(error),
            };

            entries.push(rg_core::package_registry::RubyGemsDependencyEntry {
                name: gem_name.to_string(),
                number: v.version.clone(),
                // Bundler keys a candidate on `(number, platform)` and picks
                // between a pure-ruby gem and a native build with it. A flat
                // `ruby` for every gem locks a native build as
                // platform-independent, and the URL Bundler derives from the
                // pair does not exist.
                platform: gem_declared_platform(v, gem_name),
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

    let version_info = versions
        .iter()
        .filter(|v| v.is_install_candidate())
        .map(|v| parse_rubygems_info(v.metadata.as_deref(), &gem_name, &v.version))
        .collect::<anyhow::Result<Vec<_>>>();
    let version_info = match version_info {
        Ok(info) => info,
        Err(error) => return package_error_response(error),
    };
    let entries: Vec<rg_core::package_registry::RubyGemsVersionEntry> = versions
        .iter()
        // A withdrawn version is not on offer here either — see
        // `rubygems_dependencies`, which resolves against the same rows.
        .filter(|v| v.is_install_candidate())
        .zip(version_info)
        .map(|(v, info)| {
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

            rg_core::package_registry::RubyGemsVersionEntry {
                number: v.version.clone(),
                platform: gem_declared_platform(v, &gem_name),
                summary: info.summary,
                description: info.description,
                homepage: info.homepage,
                license: info.license,
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
) -> anyhow::Result<Vec<rg_core::package_registry::CompactIndexVersion>> {
    versions
        .iter()
        .filter(|v| v.is_install_candidate())
        .map(|v| {
            let file = gem_file(v);
            let dependencies = parse_rubygems_deps(v.metadata.as_deref(), gem_name, &v.version)?;
            let facts = parse_rubygems_facts(v.metadata.as_deref());
            Ok(rg_core::package_registry::CompactIndexVersion {
                number: v.version.clone(),
                platform: facts
                    .platform
                    .or_else(|| file.and_then(|f| gem_platform(&f.filename, gem_name, &v.version))),
                dependencies,
                checksum: file.and_then(|f| v.sha256_of(f)),
                ruby_version: facts.ruby_version,
                rubygems_version: facts.rubygems_version,
            })
        })
        .collect()
}

/// The gemspec facts every RubyGems endpoint needs beside the dependency list.
struct RubyGemsFacts {
    /// The declared platform, and only when it is not the default `ruby`.
    platform: Option<String>,
    ruby_version: Option<String>,
    rubygems_version: Option<String>,
}

/// Read them back out of a version's stored gemspec metadata.
///
/// `platform` is absent for a pure-ruby gem *and* for anything published before
/// the adapter recorded it; [`gem_platform`] is the fallback for the second
/// case, and it cannot tell the two apart — which is the whole reason the
/// declared value is now stored.
fn parse_rubygems_facts(metadata_json: Option<&str>) -> RubyGemsFacts {
    let field = |doc: &serde_json::Value, key: &str| {
        doc.get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(String::from)
    };

    match metadata_json.and_then(|md| serde_json::from_str::<serde_json::Value>(md).ok()) {
        Some(doc) => RubyGemsFacts {
            platform: field(&doc, "platform"),
            ruby_version: field(&doc, "required_ruby_version"),
            rubygems_version: field(&doc, "required_rubygems_version"),
        },
        None => RubyGemsFacts {
            platform: None,
            ruby_version: None,
            rubygems_version: None,
        },
    }
}

/// The platform a version declares, spelled the way the two JSON RubyGems APIs
/// publish it: `ruby` for a pure-ruby gem, and the real one otherwise.
fn gem_declared_platform(
    version: &rg_core::package_registry::VersionDetail,
    gem_name: &str,
) -> String {
    parse_rubygems_facts(version.metadata.as_deref())
        .platform
        .or_else(|| {
            gem_file(version).and_then(|f| gem_platform(&f.filename, gem_name, &version.version))
        })
        .unwrap_or_else(|| "ruby".to_string())
}

/// The platform a stored `.gem` carries, when it is not the default `ruby`.
///
/// The fallback for a version published before the gemspec's own `platform` was
/// recorded: the client encodes it in the file name it published
/// (`nokogiri-1.16.0-x86_64-linux.gem`), and the compact index has to spell the
/// same `VERSION-PLATFORM` chunk back or the download the client derives from it
/// will not exist.
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

        let entries = match compact_index_entries(&pkg.name, &versions) {
            Ok(entries) => entries,
            Err(error) => return package_error_response(error),
        };
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

    let entries = match compact_index_entries(&gem_name, &versions) {
        Ok(entries) => entries,
        Err(error) => return package_error_response(error),
    };
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

// ── RubyGems write API ────────────────────────────────────
//
// The read side was completed long before this existed (card_01ee27252197), so
// a gem could be resolved and installed from ForgeKeep but never put there by
// the tool that builds it (card_11a578ae1820).

/// POST /api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems
///
/// What `gem push <file> --host <source>` sends: the `.gem` as the entire body,
/// no `Content-Disposition` naming it, and the API key out of
/// `~/.gem/credentials` in a scheme-less `Authorization` header — the spelling
/// the PAT middleware learned for cargo (card_5a790cc6ac35), which is why this
/// route can sit behind the ordinary [`RepoWrite`] gate.
///
/// The stored filename is therefore ours to derive, and it is not cosmetic:
/// `Gem::RemoteFetcher#download` asks for `gems/{name}-{version}.gem`, and
/// [`rubygems_gem_download`] resolves a file by the name it was published
/// under. Routing this through the generic publish handler instead would have
/// stored the gem as `package` — the `Content-Disposition` fallback — leaving
/// it undownloadable by the very client that pushed it.
///
/// The answer is `text/plain` because `gem` prints the body of a 2xx verbatim
/// as the server's word on the push; the generic publish envelope would reach
/// the user as a line of JSON. A failure is passed through as it came, since
/// the client prints that body just as literally and the shared classifier's
/// message is the informative part.
pub async fn rubygems_push(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    Path((owner, name)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let Some(adapter) = rg_core::package_registry::get_adapter("rubygems") else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the rubygems adapter is not registered",
        );
    };

    // Read the gemspec here rather than letting `publish_package` do it,
    // because the name and version are what the filename is built from and the
    // filename has to be settled before the upload is described at all.
    let meta = match adapter.extract_metadata("package.gem", &body) {
        Ok(meta) => meta,
        Err(error) => return err(StatusCode::BAD_REQUEST, &format!("{error:#}")),
    };
    let (gem_name, version) = (meta.name.clone(), meta.version.clone());
    // `{name}-{version}[-{platform}].gem`, the name RubyGems itself builds. The
    // platform belongs in it because the compact index spells the same
    // `VERSION-PLATFORM` chunk back and the client turns that chunk into the
    // download URL — a native gem stored under the pure-ruby name would be
    // advertised at a path nothing serves (card_0c9e858230b6).
    let filename = match parse_rubygems_facts(meta.protocol_metadata.as_deref()).platform {
        Some(platform) => format!("{gem_name}-{version}-{platform}.gem"),
        None => format!("{gem_name}-{version}.gem"),
    };

    let published = publish_package(
        state,
        user_id,
        owner,
        name,
        "rubygems".to_string(),
        PublishPackageQuery {
            name: Some(gem_name.clone()),
            version: Some(version.clone()),
            description: None,
            homepage: None,
            repository_url: None,
            semver: None,
        },
        filename,
        body,
    )
    .await;

    if !published.status().is_success() {
        return published;
    }
    (
        published.status(),
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("Successfully registered gem: {gem_name} ({version})\n"),
    )
        .into_response()
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
            if !v.is_install_candidate() {
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
                encode_path_segment(&owner),
                encode_path_segment(&name),
                encode_path_segment(&pkg.name),
                encode_path_segment(&v.version),
                encode_path_segment(&filename),
            );

            // Parse Helm-specific metadata from version JSON
            let meta = match parse_helm_metadata(v.metadata.as_deref(), &pkg.name, &v.version) {
                Ok(meta) => meta,
                Err(error) => return package_error_response(error),
            };

            entries.push(rg_core::package_registry::HelmIndexEntry {
                name: pkg.name.clone(),
                version: v.version.clone(),
                app_version: meta.app_version,
                description: pkg.description.clone(),
                api_version: meta.api_version,
                kube_version: meta.kube_version,
                chart_type: meta.chart_type,
                deprecated: meta.deprecated,
                dependencies: meta.dependencies,
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
            // Composer's `abandoned` flag is package-wide, not a version yank.
            // A withdrawn version therefore has to disappear from this map.
            .filter(|v| v.is_install_candidate())
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
                    // `license` and `type` come out of the manifest sections the
                    // adapter lifted at publish, below — there is no license or
                    // type column on a version row to read them from, and the
                    // literal `None` that used to sit here is what made every
                    // package announce itself as a `library`.
                    license: None,
                    package_type: None,
                    metadata: v.metadata.clone(),
                }
            })
            .collect();
        let name_json = serde_json::json!(pkg.name).to_string();
        // Same as the npm packument and the cargo index: an unreadable row must
        // not be served as an entry saying the package requires nothing.
        let versions_json = match rg_core::package_registry::adapters::composer::build_packages_json(
            &pkg.name,
            &composer_versions,
            &base_url,
            &owner,
            &name,
        ) {
            Ok(versions_json) => versions_json,
            Err(error) => return package_error_response(error),
        };
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
    kube_version: Option<String>,
    chart_type: Option<String>,
    deprecated: bool,
    dependencies: Vec<serde_json::Value>,
    keywords: Vec<String>,
    sources: Vec<String>,
}

/// Parse Helm-specific metadata from version metadata JSON.
fn parse_helm_metadata(
    metadata_json: Option<&str>,
    package_name: &str,
    version: &str,
) -> anyhow::Result<HelmChartMetadata> {
    let Some(doc) = parse_stored_metadata_document("helm", package_name, version, metadata_json)?
    else {
        return Ok(HelmChartMetadata::default());
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

    let string_field = |key: &str| doc.get(key).and_then(|v| v.as_str()).map(String::from);

    Ok(HelmChartMetadata {
        app_version: string_field("appVersion"),
        api_version: string_field("apiVersion"),
        kube_version: string_field("kubeVersion"),
        chart_type: string_field("type"),
        deprecated: doc
            .get("deprecated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        dependencies: doc
            .get("dependencies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
        keywords: string_list("keywords"),
        sources: string_list("sources"),
    })
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

/// What a NuGet version's stored `protocol_metadata` says, in the shape the
/// registration index publishes it.
#[derive(Default)]
struct NuGetVersionMetadata {
    description: Option<String>,
    homepage: Option<String>,
    license: Option<String>,
    tags: Option<String>,
    /// The `<dependencies>` block of the nuspec, read back. Empty when the
    /// nuspec declared none — which is not the same as "we did not look", and
    /// is why the key is absent from the leaf rather than sent empty.
    dependency_groups: Vec<rg_core::package_registry::NuGetDependencyGroup>,
}

/// Parse NuGet-specific metadata from a JSON metadata string.
fn parse_nuget_metadata(
    metadata_json: Option<&str>,
    package_name: &str,
    version: &str,
) -> anyhow::Result<NuGetVersionMetadata> {
    let Some(doc) = parse_stored_metadata_document("nuget", package_name, version, metadata_json)?
    else {
        return Ok(NuGetVersionMetadata::default());
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

    Ok(NuGetVersionMetadata {
        description,
        homepage,
        license,
        tags,
        dependency_groups: parse_nuget_dependency_groups(&doc),
    })
}

/// The dependency groups stored beside a NuGet version.
///
/// A group whose `dependencies` array is missing is still a group — it declares
/// a supported framework that needs nothing — so an unreadable entry is dropped
/// only when it carries no framework *and* no dependencies at all.
fn parse_nuget_dependency_groups(
    doc: &serde_json::Value,
) -> Vec<rg_core::package_registry::NuGetDependencyGroup> {
    let Some(groups) = doc.get("dependencyGroups").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    groups
        .iter()
        .map(|group| rg_core::package_registry::NuGetDependencyGroup {
            target_framework: group
                .get("targetFramework")
                .and_then(|v| v.as_str())
                .filter(|f| !f.is_empty())
                .map(String::from),
            dependencies: group
                .get("dependencies")
                .and_then(|v| v.as_array())
                .map(|deps| {
                    deps.iter()
                        .filter_map(|dep| {
                            Some(rg_core::package_registry::NuGetDependency {
                                id: dep
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .filter(|id| !id.is_empty())?
                                    .to_string(),
                                range: dep
                                    .get("range")
                                    .and_then(|v| v.as_str())
                                    .filter(|r| !r.is_empty())
                                    .unwrap_or("(, )")
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .filter(|group| group.target_framework.is_some() || !group.dependencies.is_empty())
        .collect()
}

/// The `Requires-Python` a PyPI version recorded at publish.
///
/// Absent for anything published before the adapter read the field, and for a
/// distribution that declared none — both mean the same thing to the page: no
/// attribute, so no claim is made either way.
fn parse_pypi_requires_python(
    metadata_json: Option<&str>,
    package_name: &str,
    version: &str,
) -> anyhow::Result<Option<String>> {
    let Some(doc) = parse_stored_metadata_document("pypi", package_name, version, metadata_json)?
    else {
        return Ok(None);
    };
    Ok(doc
        .get("requires_python")
        .and_then(|value| value.as_str())
        .filter(|spec| !spec.is_empty())
        .map(String::from))
}

/// Parse RubyGems dependencies from version metadata JSON.
fn parse_rubygems_deps(
    metadata_json: Option<&str>,
    package_name: &str,
    version: &str,
) -> anyhow::Result<Vec<rg_core::package_registry::RubyGemsDep>> {
    let Some(doc) =
        parse_stored_metadata_document("rubygems", package_name, version, metadata_json)?
    else {
        return Ok(Vec::new());
    };
    let deps = match doc.get("dependencies").and_then(|v| v.as_array()) {
        Some(d) => d,
        None => return Ok(Vec::new()),
    };
    Ok(deps
        .iter()
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
        .collect())
}

/// Parse RubyGems gem info from version metadata JSON.
#[derive(Default)]
struct RubyGemsInfo {
    summary: Option<String>,
    description: Option<String>,
    homepage: Option<String>,
    license: Option<String>,
}

fn parse_rubygems_info(
    metadata_json: Option<&str>,
    package_name: &str,
    version: &str,
) -> anyhow::Result<RubyGemsInfo> {
    let Some(doc) =
        parse_stored_metadata_document("rubygems", package_name, version, metadata_json)?
    else {
        return Ok(RubyGemsInfo::default());
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
    Ok(RubyGemsInfo {
        summary,
        description,
        homepage,
        license,
    })
}

/// Decode protocol metadata stored on a package version without confusing a
/// legacy `NULL` with a damaged value. The error names the affected coordinate
/// for operators but never includes the stored blob itself.
fn parse_stored_metadata_document(
    package_type: &str,
    package_name: &str,
    version: &str,
    metadata_json: Option<&str>,
) -> anyhow::Result<Option<serde_json::Value>> {
    let Some(metadata_json) = metadata_json else {
        return Ok(None);
    };

    let document = serde_json::from_str::<serde_json::Value>(metadata_json).map_err(|error| {
        anyhow::anyhow!(
            "stored {package_type} metadata for package '{package_name}' version '{version}' is not valid JSON: {error}"
        )
    })?;
    if !document.is_object() {
        anyhow::bail!(
            "stored {package_type} metadata for package '{package_name}' version '{version}' is not a JSON object"
        );
    }

    Ok(Some(document))
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
