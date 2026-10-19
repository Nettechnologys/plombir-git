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
//! GET    /api/v1/repos/{owner}/{repo}/packages/nuget/registration/{id}/{version}

use crate::error::AppError;
use axum::{
    body::Body,
    extract::{FromRequest, Multipart, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use base64::Engine as _;
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256, Sha512};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead as _, Read as _, Seek as _, Write as _},
    path::Path as FsPath,
};
use tokio::io::AsyncWriteExt as _;
use utoipa::ToSchema;

use crate::api::repo_access::{CiRead, Packages, RepoWrite};
use crate::AppState;
use rg_core::package_registry::{encode_path_segment, PackageArtifact};
mod cargo;
mod composer;
mod helm;
mod maven;
mod npm;
mod nuget;
mod oci;
mod pypi;
mod rubygems;

pub(crate) use cargo::*;
pub(crate) use composer::*;
pub(crate) use helm::*;
pub(crate) use maven::*;
pub(crate) use npm::*;
pub(crate) use nuget::*;
pub(crate) use oci::*;
pub(crate) use pypi::*;
pub(crate) use rubygems::*;

/// Metadata and encoding headroom above the decoded artifact ceiling.
///
/// npm expands a tarball to `ceil(n * 4 / 3)` before JSON framing; multipart
/// and Cargo add smaller envelopes. Keeping one bounded request allowance for
/// all enveloped package protocols means the configured value remains the
/// artifact ceiling rather than an npm-only 25% haircut.
const PACKAGE_UPLOAD_ENVELOPE_HEADROOM: usize = 1024 * 1024;

pub(crate) fn package_upload_envelope_limit(artifact_limit: usize) -> usize {
    artifact_limit
        .saturating_mul(4)
        .div_ceil(3)
        .saturating_add(PACKAGE_UPLOAD_ENVELOPE_HEADROOM)
}

/// A request-private spool an upload is written into as it arrives.
///
/// The spool is the memory bound. Every publish route used to end its ingress
/// with a `Vec<u8>` the size of the configured artifact ceiling — half a
/// gigabyte by default, on all four protocols, with nothing counting concurrent
/// requests. Handing the rest of the registry a [`PackageArtifact`] backed by
/// this file instead means the ceiling is a disk number.
struct UploadSpool {
    file: tokio::fs::File,
    path: tempfile::TempPath,
    len: u64,
    max_bytes: usize,
}

impl UploadSpool {
    async fn create(repo_root: &FsPath, max_bytes: usize) -> Result<Self, AppError> {
        let staging_dir = rg_core::staging::StagingArea::PackageUploads.path_in(repo_root);
        tokio::fs::create_dir_all(&staging_dir)
            .await
            .map_err(|error| {
                AppError::internal(rg_core::platform::fs::describe_path_error(
                    "package upload staging directory",
                    &staging_dir,
                    &error,
                    rg_core::platform::fs::BLOB_STORAGE_HINT,
                ))
            })?;
        let staged = tempfile::Builder::new()
            .prefix("package-")
            .suffix(".upload")
            .tempfile_in(&staging_dir)
            .map_err(|error| {
                AppError::internal(rg_core::platform::fs::describe_path_error(
                    "package upload staging file",
                    &staging_dir,
                    &error,
                    rg_core::platform::fs::BLOB_STORAGE_HINT,
                ))
            })?;
        let (file, path) = staged.into_parts();
        Ok(Self {
            file: tokio::fs::File::from_std(file),
            path,
            len: 0,
            max_bytes,
        })
    }

    /// Append one chunk, refusing the first that would cross the ceiling.
    ///
    /// The refusal happens before the write, so an over-ceiling upload never
    /// reaches the disk either.
    async fn write(&mut self, chunk: &[u8]) -> Result<(), AppError> {
        let max_bytes = self.max_bytes;
        self.len = self
            .len
            .checked_add(chunk.len() as u64)
            .filter(|size| *size <= max_bytes as u64)
            .ok_or_else(|| {
                AppError::payload_too_large(format!(
                    "package upload exceeds the configured {max_bytes}-byte request limit"
                ))
            })?;
        self.file.write_all(chunk).await.map_err(|error| {
            AppError::internal(rg_core::platform::fs::describe_path_error(
                "package upload staging file",
                &self.path,
                &error,
                rg_core::platform::fs::BLOB_STORAGE_HINT,
            ))
        })
    }

    async fn finish(mut self) -> Result<PackageArtifact, AppError> {
        let staging_error = |error: &std::io::Error| {
            AppError::internal(rg_core::platform::fs::describe_path_error(
                "package upload staging file",
                &self.path,
                error,
                rg_core::platform::fs::BLOB_STORAGE_HINT,
            ))
        };
        self.file
            .flush()
            .await
            .map_err(|error| staging_error(&error))?;
        drop(self.file);

        // The counted length is what every later limit check and the version's
        // `total_size` are computed from, while the digests and the published
        // object come from the file. Those are two answers to one question, and
        // the collect this replaced compared them — so it is compared here too,
        // rather than quietly dropped along with the collect.
        let written = tokio::fs::metadata(&self.path)
            .await
            .map_err(|error| staging_error(&error))?
            .len();
        if written != self.len {
            return Err(AppError::internal(format!(
                "package upload staging file changed size before validation: expected {}, got {written}",
                self.len
            )));
        }
        Ok(PackageArtifact::spooled(self.path, self.len))
    }
}

/// Stream one request body to a request-private temporary file, refusing the
/// first chunk that would cross `max_bytes`.
///
/// This deliberately takes `Body`, not `Bytes`: the latter invokes Axum's
/// hidden 2 MiB extractor and buffers the complete request before the handler
/// can enforce Plombir Git's configured boundary.
async fn stage_package_upload(
    body: Body,
    repo_root: &FsPath,
    max_bytes: usize,
) -> Result<PackageArtifact, AppError> {
    let mut spool = UploadSpool::create(repo_root, max_bytes).await?;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let inner = error.into_inner();
                if crate::body_limit::is_length_limit_error(&*inner) {
                    return Err(AppError::payload_too_large(
                        "package upload exceeds the configured request-body limit",
                    ));
                }
                return Err(AppError::bad_request(format!(
                    "failed to read package upload body: {inner}"
                )));
            }
        };
        spool.write(&chunk).await?;
    }
    spool.finish().await
}

/// Stream one multipart field to a request-private temporary file.
///
/// The Twine and NuGet publish routes carry their artifact in a multipart field
/// rather than in the body, and `Field::bytes` collects it whole — so those two
/// routes had no spool at all, only the multipart envelope's ceiling standing
/// between a publisher and the same allocation. This is the same bounded
/// ingress the `Body` routes get.
async fn stage_multipart_artifact(
    mut field: axum::extract::multipart::Field<'_>,
    repo_root: &FsPath,
    max_bytes: usize,
    context: &str,
) -> Result<PackageArtifact, AppError> {
    let mut spool = UploadSpool::create(repo_root, max_bytes).await?;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|error| package_multipart_error(error, context))?
    {
        spool.write(&chunk).await?;
    }
    spool.finish().await
}

/// Stage a route's request body as the artifact it is about to publish.
///
/// Named for what it does now: nothing on this path collects the upload, and a
/// helper still called `collect_…` would invite the next handler to expect a
/// `Vec` back.
async fn stage_request_artifact(
    state: &AppState,
    body: Body,
    max_request_bytes: usize,
) -> Result<PackageArtifact, axum::response::Response> {
    stage_package_upload(body, &state.repo_root, max_request_bytes)
        .await
        .map_err(IntoResponse::into_response)
}

/// The artifact's SHA-256, read through a fixed window rather than a copy.
///
/// Twine states the digest of what it uploaded and the upload is refused when
/// the two disagree, so this runs on every PyPI publish — hashing a collected
/// `Vec` there would reintroduce exactly the allocation the spool removed.
fn artifact_sha256(artifact: &PackageArtifact) -> Result<String, AppError> {
    use std::io::Read as _;

    let unreadable = |error: std::io::Error| {
        AppError::internal(format!("cannot read the staged package upload: {error}"))
    };
    let mut reader = artifact.reader().map_err(unreadable)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(unreadable)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

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

fn attachment_range_reader<'a>(
    body: &'a PackageArtifact,
    range: NpmAttachmentDataRange,
) -> Result<
    JsonStringRangeReader<io::Take<rg_core::package_registry::ArtifactReader<'a>>>,
    NpmPackumentError,
> {
    let mut reader = body.reader().map_err(|error| {
        NpmPackumentError::Internal(format!("cannot read the staged npm packument: {error}"))
    })?;
    reader
        .seek(io::SeekFrom::Start(range.start))
        .map_err(|error| {
            NpmPackumentError::Internal(format!("cannot seek in the staged npm packument: {error}"))
        })?;
    Ok(JsonStringRangeReader::new(
        reader.take(range.end - range.start),
        range.end - range.start,
    ))
}

fn read_attachment_string(
    body: &PackageArtifact,
    range: NpmAttachmentDataRange,
    filename: &str,
    limit: usize,
) -> Result<Vec<u8>, NpmPackumentError> {
    let mut reader = attachment_range_reader(body, range)?;
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            NpmPackumentError::Invalid(format!(
                "npm attachment '{filename}' is not a valid JSON string: {error}"
            ))
        })?;
    if bytes.len() > limit {
        return Err(NpmPackumentError::TooLarge(format!(
            "npm attachment '{filename}' exceeds the configured {limit}-byte metadata limit"
        )));
    }
    Ok(bytes)
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
///
/// Whether the artifact stated its own identity is answered in two parts, and
/// both have to hold before a disagreement is refused:
///
/// * `manifest_is_authoritative` — the adapter's answer for the *format*: may a
///   manifest of this kind be trusted over the request at all?
/// * `meta.coordinates_from_manifest` — the extraction's answer for *this
///   upload*: were these coordinates read out of a manifest, or guessed from
///   the filename?
///
/// Where both hold, the caller's coordinates are a *claim about* the artifact
/// rather than a substitute for it. Overriding them silently is how
/// `POST /packages/nuget/publish?name=evil&version=9.9.9` published a real
/// `serde.nupkg` as `evil 9.9.9`, nuspec and all, and the registration index
/// then described the result as if it were consistent (card_f3e0fd84d056).
///
/// The second part is what lets Maven be in the set at all. Asking only the
/// adapter forces one answer per format, and Maven has both kinds of artifact
/// in one version: the `.pom` declares `groupId:artifactId:version`, while
/// `matrix-1.0.0-sources.jar` carries no manifest and can only get coordinates
/// from the request. The format was therefore left permissive, and a POM
/// uploaded to somebody else's path published under that path — which is
/// precisely what breaks a Maven client, since it fetches the POM by path and
/// then reads different coordinates inside it (card_13cadc8a9d7a).
///
/// Where neither holds — `generic`, a classifier jar — the override is the
/// mechanism by which an artifact that cannot state its coordinates gets them
/// at all, and it is left alone.
fn resolve_publish_info(
    query: &PublishPackageQuery,
    adapter_meta: Option<rg_core::package_registry::ExtractedMetadata>,
    package_type: &str,
    manifest_is_authoritative: bool,
) -> Result<ResolvedPublishInfo, String> {
    // If adapter extracted metadata, use it as base; query params override.
    if let Some(meta) = adapter_meta {
        if manifest_is_authoritative && meta.coordinates_from_manifest {
            let disagreement = [
                ("name", query.name.as_deref(), meta.name.as_str()),
                ("version", query.version.as_deref(), meta.version.as_str()),
            ]
            .into_iter()
            .find(|(_, claimed, declared)| claimed.is_some_and(|claimed| claimed != *declared));
            if let Some((field, claimed, declared)) = disagreement {
                return Err(format!(
                    "the uploaded {package_type} artifact declares {field} `{declared}`, but the \
                     request asks to publish it as `{}`. The manifest inside the artifact is its \
                     own identity — publish it under the name and version it states, or upload \
                     the artifact that carries the ones you asked for.",
                    claimed.unwrap_or_default(),
                ));
            }
        }
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
        (status = 413, description = "Package artifact exceeds the configured limit", body = serde_json::Value),
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
    body: Body,
) -> axum::response::Response {
    let filename = filename_from_disposition(&headers);
    let body = match stage_request_artifact(&state, body, state.package_upload_max_bytes).await {
        Ok(body) => body,
        Err(response) => return response,
    };
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
        .and_then(crate::content_disposition::filename_from_disposition)
        .unwrap_or_else(|| "package".to_string())
}

fn package_multipart_error(
    error: axum::extract::multipart::MultipartError,
    context: &str,
) -> AppError {
    if crate::body_limit::is_length_limit_error(&error) {
        AppError::payload_too_large(format!(
            "{context}: package upload exceeds the configured request-body limit"
        ))
    } else {
        AppError::bad_request(format!("{context}: {error}"))
    }
}

/// POST /api/v1/repos/{owner}/{name}/packages/pypi/legacy[/]
///
/// The upload side of the legacy PyPI API used by Twine. The simple index is
/// the read side; without this protocol endpoint a package could be installed
/// from Plombir Git but no standard Python client could publish it there.
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
        description = "Twine legacy upload form with package metadata, a content file, and an \
                       optional PEP 740 `attestations` array; detached GPG signatures are rejected",
    ),
    responses(
        (status = 200, description = "Package uploaded", body = PublishResponse),
        (status = 400, description = "Malformed form, package, digest, or attestations, or an unsupported GPG signature", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "Distribution already exists", body = serde_json::Value),
        (status = 413, description = "Package artifact exceeds the configured limit", body = serde_json::Value),
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
    let upload = match decode_twine_upload(
        multipart,
        &state.repo_root,
        state.package_upload_max_bytes,
    )
    .await
    {
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
    let (content, actual_digest) = match content.run_blocking(artifact_sha256).await {
        Ok((content, Ok(digest))) => (content, digest),
        Ok((_content, Err(error))) => return error.into_response(),
        Err(error) => {
            return AppError::internal(format!("package sha256 task did not complete: {error}"))
                .into_response()
        }
    };
    if !claimed_digest.eq_ignore_ascii_case(&actual_digest) {
        return AppError::bad_request(format!(
            "Twine `sha256_digest` mismatch: claimed {claimed_digest}, calculated {actual_digest}"
        ))
        .into_response();
    }

    // PEP 740: "if the index fails to verify any attestation in `attestations`,
    // it MUST reject the upload". Verifying before anything is written is what
    // makes that refusal total — the alternative leaves a distribution file
    // published without the evidence its publisher meant to publish it with.
    let provenance = match upload.attestations.as_deref() {
        Some(raw) => {
            match rg_core::package_registry::build_pypi_provenance(
                raw,
                &filename,
                &actual_digest,
                rg_core::package_registry::pypi_upload_token_publisher(&owner, &name),
            ) {
                Ok(document) => Some((
                    rg_core::package_registry::pypi_provenance_filename(&filename),
                    PackageArtifact::from_bytes(document),
                )),
                Err(message) => return AppError::bad_request(message).into_response(),
            }
        }
        None => None,
    };

    let query = PublishPackageQuery {
        name: Some(package_name),
        version: Some(version),
        description: None,
        homepage: None,
        repository_url: None,
        semver: None,
    };
    let mut response = publish_package_with_extra_files(
        state,
        user_id,
        owner,
        name,
        "pypi".to_string(),
        query,
        filename,
        content,
        provenance.into_iter().collect(),
    )
    .await;
    // Twine's legacy upload contract uses 200 for a successful POST. Keep the
    // generic service's body, but do not expose its REST-specific 201 here.
    if response.status().is_success() {
        *response.status_mut() = StatusCode::OK;
    }
    response
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
    body: PackageArtifact,
) -> axum::response::Response {
    publish_package_with_extra_files(
        state,
        user_id,
        owner,
        name,
        pkg_type,
        query,
        filename,
        body,
        Vec::new(),
    )
    .await
}

/// Publish one artifact together with the sidecar files that belong to it.
///
/// `extra_files` travels into the same `publish` transaction as the artifact,
/// which is the whole point: a PyPI distribution and its PEP 740 provenance are
/// either both stored or neither is, so the Simple page can never advertise
/// evidence that a half-failed publish left unwritten. They bypass the adapter
/// checks below on purpose — a provenance document is not a distribution and
/// would fail `validate` — so every caller owes its own validation of them
/// *before* this call.
#[allow(clippy::too_many_arguments)]
async fn publish_package_with_extra_files(
    state: AppState,
    user_id: i64,
    owner: String,
    name: String,
    pkg_type: String,
    query: PublishPackageQuery,
    filename: String,
    body: PackageArtifact,
    extra_files: Vec<(String, PackageArtifact)>,
) -> axum::response::Response {
    if body.len() > state.package_upload_max_bytes as u64 {
        return AppError::payload_too_large(format!(
            "package artifact exceeds the configured {}-byte limit",
            state.package_upload_max_bytes
        ))
        .into_response();
    }
    if !rg_core::package_registry::package_types::is_valid(&pkg_type) {
        return err(
            StatusCode::BAD_REQUEST,
            &format!("unsupported package type: {}", pkg_type),
        );
    }

    // Try to auto-extract metadata via the adapter
    let adapter = rg_core::package_registry::get_adapter(&pkg_type);
    let (body, adapter_meta) = if let Some(adapter) = adapter {
        let (body, inspection) = match body.inspect_with_adapter(adapter, filename.clone()).await {
            Ok(result) => result,
            Err(error) => {
                return AppError::internal(format!(
                    "package artifact inspection task did not complete: {error}"
                ))
                .into_response()
            }
        };
        let adapter_meta = match inspection {
            Err(error) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    &format!("invalid package payload: {error:#}"),
                )
            }
            Ok(metadata) => metadata,
        };
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
        let adapter_meta = match adapter_meta {
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
        };
        (body, adapter_meta)
    } else {
        (body, None)
    };

    let mut files = vec![(filename, body)];
    files.extend(extra_files);

    persist_package(
        state,
        user_id,
        owner,
        name,
        pkg_type,
        query,
        files,
        adapter_meta,
        None,
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
    files: Vec<(String, PackageArtifact)>,
    adapter_meta: Option<rg_core::package_registry::ExtractedMetadata>,
    npm_dist_tag: Option<String>,
) -> axum::response::Response {
    // Asked of the adapter rather than of a list kept here: a second census of
    // which formats carry an authoritative manifest is a copy that can drift
    // away from the adapters it describes.
    let manifest_is_authoritative = rg_core::package_registry::get_adapter(&pkg_type)
        .is_some_and(|adapter| adapter.manifest_is_authoritative());
    let resolved =
        match resolve_publish_info(&query, adapter_meta, &pkg_type, manifest_is_authoritative) {
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
        npm_dist_tag,
        author_id: user_id,
        files,
    };

    match rg_core::package_registry::service::publish(
        &state.db,
        &storage,
        info,
        &state.storage_limits,
    )
    .await
    {
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
        (status = 413, description = "Package artifact exceeds the configured limit", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn publish_npm(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id, ..
    }: RepoWrite,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<PublishPackageQuery>,
    headers: axum::http::HeaderMap,
    body: Body,
) -> axum::response::Response {
    let body = match stage_request_artifact(&state, body, state.package_upload_max_bytes).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let filename = filename_from_disposition(&headers);
    publish_package(
        state,
        user_id,
        owner,
        name,
        "npm".to_string(),
        query,
        filename,
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
        description = "npm publish packument with a streamed base64 tarball attachment; non-attachment metadata and provenance are limited to 4 MiB each",
    ),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Malformed or unsupported packument", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "Package version already exists", body = serde_json::Value),
        (status = 413, description = "Package artifact or packument metadata exceeds its configured limit", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn publish_npm_packument(
    State(state): State<AppState>,
    RepoWrite {
        actor_id: user_id,
        repo: repository,
    }: RepoWrite,
    Path((owner, repo, pkg_name)): Path<(String, String, String)>,
    body: Body,
) -> axum::response::Response {
    let body = match stage_request_artifact(
        &state,
        body,
        package_upload_envelope_limit(state.package_upload_max_bytes),
    )
    .await
    {
        Ok(body) => body,
        Err(response) => return response,
    };
    let staged = match tokio::task::spawn_blocking(move || parse_staged_npm_packument(body)).await {
        Ok(Ok(staged)) => staged,
        Ok(Err(error)) => return npm_packument_error_response(error),
        Err(error) => {
            return AppError::internal(format!(
                "npm packument parser task did not complete: {error}"
            ))
            .into_response()
        }
    };
    // Before anything is decoded or stored: an access instruction this registry
    // cannot carry out has to be refused, not absorbed (card_b5db648768e4).
    if let Err(message) =
        npm_access_matches_repository(staged.packument.access.as_deref(), repository.is_private)
    {
        return err(StatusCode::BAD_REQUEST, &message);
    }
    if staged
        .packument
        .attachments
        .values()
        .any(|attachment| attachment.length > state.package_upload_max_bytes)
    {
        return AppError::payload_too_large(format!(
            "npm attachment exceeds the configured {}-byte artifact limit",
            state.package_upload_max_bytes
        ))
        .into_response();
    }
    if staged
        .packument
        .attachments
        .iter()
        .any(|(filename, attachment)| {
            filename.ends_with(".sigstore") && attachment.length > MAX_NPM_PROVENANCE_BYTES
        })
    {
        return AppError::payload_too_large(format!(
            "npm provenance attachment exceeds the configured {MAX_NPM_PROVENANCE_BYTES}-byte metadata limit"
        ))
        .into_response();
    }
    let decode_name = pkg_name.clone();
    let repo_root = state.repo_root.clone();
    let artifact_limit = state.package_upload_max_bytes;
    let decoded = match tokio::task::spawn_blocking(move || {
        decode_staged_npm_publish_packument(staged, &decode_name, artifact_limit, &repo_root)
    })
    .await
    {
        Ok(Ok(decoded)) => decoded,
        Ok(Err(error)) => return npm_packument_error_response(error),
        Err(error) => {
            return AppError::internal(format!(
                "npm attachment decoder task did not complete: {error}"
            ))
            .into_response()
        }
    };

    // The envelope and the URL are claims; package.json inside the tarball is
    // the artifact's own identity. Refuse disagreement rather than relying on
    // the generic query-parameter override used by multi-artifact formats.
    let adapter =
        rg_core::package_registry::get_adapter("npm").expect("npm is a built-in package adapter");
    let (tarball, inspection) = match decoded
        .tarball
        .inspect_with_adapter(adapter, decoded.filename.clone())
        .await
    {
        Ok(result) => result,
        Err(error) => {
            return AppError::internal(format!(
                "npm package inspection task did not complete: {error}"
            ))
            .into_response()
        }
    };
    let mut metadata = match inspection {
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("invalid package payload: {error:#}"),
            )
        }
        Ok(Ok(metadata)) => metadata,
        Ok(Err(error)) => return err(StatusCode::BAD_REQUEST, &format!("{error:#}")),
    };
    if metadata.name != pkg_name || metadata.version != decoded.version {
        return err(
            StatusCode::BAD_REQUEST,
            "npm tarball package.json does not match the publish URL and packument version",
        );
    }
    if let Some(provenance) = decoded.provenance.as_ref() {
        if let Err(error) = rg_core::package_registry::record_npm_provenance(
            &mut metadata,
            &provenance.predicate_type,
        ) {
            return package_error_response(error);
        }
    }

    let mut files = vec![(decoded.filename, tarball)];
    if let Some(provenance) = decoded.provenance {
        files.push((
            provenance.filename,
            PackageArtifact::from_bytes(provenance.bundle),
        ));
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
        files,
        Some(metadata),
        Some(decoded.dist_tag),
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

/// Tagged OCI images are published outside the generic package tables.
/// Expose their real registry metadata on the Packages API as well.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/packages/oci/images",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("last" = Option<String>, Query, description = "Tag marker from the previous page"),
    ),
    responses((status = 200, body = OciImageListResponse))
)]
pub async fn list_oci_images(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    Query(query): Query<OciImageListQuery>,
    CiRead::<Packages> { repo, .. }: CiRead<Packages>,
) -> axum::response::Response {
    let oci_repo = match rg_db::ops::oci_ops::find_repo_by_id(&state.db, repo.id).await {
        Ok(row) => row,
        Err(error) => return AppError::from(error).into_response(),
    };
    let Some(oci_repo) = oci_repo else {
        return Json(OciImageListResponse {
            images: Vec::new(),
            next: None,
        })
        .into_response();
    };
    let mut images = Vec::new();
    {
        let tags = match rg_db::ops::oci_ops::list_tagged_manifests(
            &state.db,
            oci_repo.id,
            query.last.as_deref(),
            101,
        )
        .await
        {
            Ok(rows) => rows,
            Err(error) => return AppError::from(error).into_response(),
        };
        let has_more = tags.len() > 100;
        for (tag, manifest) in tags.into_iter().take(100) {
            let Some(manifest) = manifest else {
                return AppError::internal("OCI tag names a missing manifest").into_response();
            };
            let size_bytes = match oci_image_size(&state.db, oci_repo.id, manifest.clone()).await {
                Ok(size) => size,
                Err(error) => {
                    return AppError::internal(format!(
                        "OCI manifest {} cannot be read: {error:#}",
                        manifest.digest
                    ))
                    .into_response()
                }
            };
            images.push(OciImageSummary {
                namespace: oci_repo.namespace.clone(),
                tag: tag.tag,
                digest: manifest.digest,
                size_bytes,
                pushed_at: manifest.created_at.to_rfc3339(),
            });
        }
        let next = if has_more {
            images.last().map(|image| image.tag.clone())
        } else {
            None
        };
        Json(OciImageListResponse { images, next }).into_response()
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
                source,
                content_type,
                sha256,
                ..
            } = file;
            let mut response = match source {
                rg_core::package_registry::storage::PackageFileSource::LocalFile { path, size } => {
                    let (file, actual_size) =
                        match crate::http_stream::open_local_file_for_stream(&path).await {
                            Ok(pair) => pair,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                return AppError::not_found("package file not found")
                                    .into_response()
                            }
                            Err(error) => {
                                return AppError::internal(anyhow::anyhow!(
                                    "failed to open package file {}: {error}",
                                    path.display()
                                ))
                                .into_response()
                            }
                        };
                    if actual_size != size {
                        return AppError::internal(anyhow::anyhow!(
                            "package file size changed while preparing the download: expected {size}, got {actual_size}"
                        ))
                        .into_response();
                    }
                    (
                        StatusCode::OK,
                        [(header::CONTENT_LENGTH, size.to_string())],
                        crate::http_stream::file_body_with_idle(file, state.git_idle_timeout_secs),
                    )
                        .into_response()
                }
                rg_core::package_registry::storage::PackageFileSource::Buffered(data) => {
                    let len = data.len();
                    (
                        StatusCode::OK,
                        [(header::CONTENT_LENGTH, len.to_string())],
                        crate::http_stream::buffered_body_with_idle(
                            data,
                            state.git_idle_timeout_secs,
                        ),
                    )
                        .into_response()
                }
            };
            // Built rather than formatted: a package whose file name is not
            // ASCII — `пакет-1.0.tgz` — used to produce a value `HeaderValue`
            // refuses, and an array turns that into a `500`. The package
            // published fine and then could never be downloaded. The type the
            // registry guessed from the name is passed through the same filter
            // as an uploader's: a package file is user content on this origin
            // like any other download (security audit finding #8).
            crate::content_disposition::apply_download_headers(
                response.headers_mut(),
                crate::content_disposition::served_upload_type(&content_type),
                Some(filename),
            );
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
/// request (or Plombir Git's own UI) may carry the manifest's spelling, and a
/// case mismatch is not a different crate.
fn matches_index_prefix(prefix: &[String], name: &str) -> bool {
    let expected = rg_core::package_registry::cargo_index_prefix(name);
    prefix.len() == expected.len()
        && prefix
            .iter()
            .zip(&expected)
            .all(|(got, want)| got.eq_ignore_ascii_case(want))
}

/// GET the Sigstore bundle npm/pacote discovers through `dist.attestations`.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/packages/npm/-/npm/v1/attestations/{package_spec}",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("package_spec" = String, Path, description = "npm package name and version"),
    ),
    responses(
        (status = 200, description = "npm attestation collection", body = serde_json::Value),
        (status = 400, description = "Malformed package specification", body = serde_json::Value),
        (status = 404, description = "Package or provenance attachment not found", body = serde_json::Value),
        (status = 500, description = "Stored provenance is unreadable", body = serde_json::Value),
    ),
)]
pub async fn npm_attestations(
    State(state): State<AppState>,
    Path((owner, repo, package_spec)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let Some((package_name, version)) = package_spec.rsplit_once('@') else {
        return err(
            StatusCode::BAD_REQUEST,
            "npm attestation package specification must be '<name>@<version>'",
        );
    };
    if package_name.is_empty() || version.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "npm attestation package name and version must not be empty",
        );
    }

    let filename = format!("{package_name}-{version}.sigstore");
    let storage =
        rg_core::package_registry::PackageStorage::from_backend(state.blob_storage.clone());
    let downloaded = match rg_core::package_registry::service::download_file(
        &state.db,
        &storage,
        &owner,
        &repo,
        "npm",
        package_name,
        version,
        &filename,
    )
    .await
    {
        Ok(downloaded) => downloaded,
        Err(error) => return package_error_response(error),
    };

    let data = match downloaded.source {
        rg_core::package_registry::storage::PackageFileSource::LocalFile { path, .. } => {
            match tokio::fs::read(&path).await {
                Ok(data) => data,
                Err(error) => {
                    return package_file_error_response(anyhow::anyhow!(
                        "failed to read npm provenance attachment {}: {error}",
                        path.display()
                    ))
                }
            }
        }
        rg_core::package_registry::storage::PackageFileSource::Buffered(data) => data,
    };
    let raw_bundle = match String::from_utf8(data) {
        Ok(bundle) => bundle,
        Err(error) => {
            return package_error_response(anyhow::anyhow!(
                "stored npm provenance attachment is not UTF-8 JSON: {error}"
            ))
        }
    };
    let media_type = match serde_json::from_str::<serde_json::Value>(&raw_bundle)
        .ok()
        .and_then(|bundle| bundle.get("mediaType")?.as_str().map(String::from))
    {
        Some(media_type) => media_type,
        None => {
            return package_error_response(anyhow::anyhow!(
                "stored npm provenance attachment has no mediaType"
            ))
        }
    };
    let attachment = NpmPublishAttachment {
        length: raw_bundle.encode_utf16().count(),
        data: raw_bundle,
        content_type: Some(media_type),
    };
    let inspected =
        match inspect_npm_provenance_bytes(attachment.data.as_bytes().to_vec(), &attachment) {
            Ok(inspected) => inspected,
            Err(error) => {
                return package_error_response(anyhow::anyhow!(
                    "stored npm provenance attachment is unreadable: {error}"
                ))
            }
        };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "attestations": [{
                "predicateType": inspected.predicate_type,
                "bundle": inspected.bundle
            }]
        })),
    )
        .into_response()
}

/// GET the mutable selectors managed by `npm dist-tag ls`.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_name" = String, Path, description = "npm package name"),
    ),
    responses(
        (status = 200, description = "Current npm dist-tags", body = serde_json::Value),
        (status = 404, description = "Package not found", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn npm_dist_tags(
    State(state): State<AppState>,
    Path((owner, repo, pkg_name)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    match rg_core::package_registry::service::list_npm_dist_tags(
        &state.db, &owner, &repo, &pkg_name,
    )
    .await
    {
        Ok(tags) => (StatusCode::OK, Json(tags)).into_response(),
        Err(error) => package_error_response(error),
    }
}

/// PUT one selector, as sent by `npm dist-tag add`.
#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_name" = String, Path, description = "npm package name"),
        ("tag" = String, Path, description = "mutable npm dist-tag"),
    ),
    request_body(
        content = String,
        content_type = "application/json",
        description = "Existing package version the tag should name",
    ),
    responses(
        (status = 200, description = "Dist-tag set", body = serde_json::Value),
        (status = 400, description = "Invalid dist-tag", body = serde_json::Value),
        (status = 404, description = "Package or version not found", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn set_npm_dist_tag(
    State(state): State<AppState>,
    RepoWrite { actor_id, .. }: RepoWrite,
    Path((owner, repo, pkg_name, tag)): Path<(String, String, String, String)>,
    headers: axum::http::HeaderMap,
    Json(version): Json<String>,
) -> axum::response::Response {
    // A dist-tag decides what `npm install <pkg>` resolves to without any
    // version changing, so the version row's `author_id` cannot answer who
    // pointed `latest` at an old release. The OCI twin — re-tagging an image —
    // journals as `oci.manifest.push`; this one wrote nothing (card_44ce692c5b9b).
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::package_registry::service::set_npm_dist_tag(
        &state.db, &owner, &repo, &pkg_name, &tag, &version,
    )
    .await
    {
        Ok(()) => {
            record_npm_dist_tag_event(
                &state,
                &audit_actor,
                "package.dist_tag.set",
                (&owner, &repo, &pkg_name, &tag),
                Some(&version),
                &headers,
            )
            .await;
            (StatusCode::OK, Json(serde_json::json!({}))).into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

/// DELETE one selector, as sent by `npm dist-tag rm`.
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}",
    tag = "Packages",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "repo name"),
        ("pkg_name" = String, Path, description = "npm package name"),
        ("tag" = String, Path, description = "mutable npm dist-tag"),
    ),
    responses(
        (status = 200, description = "Dist-tag removed", body = serde_json::Value),
        (status = 400, description = "Invalid dist-tag", body = serde_json::Value),
        (status = 404, description = "Package or dist-tag not found", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn delete_npm_dist_tag(
    State(state): State<AppState>,
    RepoWrite { actor_id, .. }: RepoWrite,
    Path((owner, repo, pkg_name, tag)): Path<(String, String, String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let audit_actor = match rg_core::audit::AuditActor::resolve(&state.db, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return AppError::from(error).into_response(),
    };

    match rg_core::package_registry::service::remove_npm_dist_tag(
        &state.db, &owner, &repo, &pkg_name, &tag,
    )
    .await
    {
        Ok(()) => {
            record_npm_dist_tag_event(
                &state,
                &audit_actor,
                "package.dist_tag.remove",
                (&owner, &repo, &pkg_name, &tag),
                None,
                &headers,
            )
            .await;
            (StatusCode::OK, Json(serde_json::json!({}))).into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

/// The journal row for a dist-tag that was set or removed.
///
/// Written after the tag already moved and never turned into an error: the
/// client sees the tag it asked for either way, and an audit failure that made
/// it retry would only move the tag again.
async fn record_npm_dist_tag_event(
    state: &AppState,
    actor: &rg_core::audit::AuditActor,
    action: &str,
    (owner, repo, pkg_name, tag): (&str, &str, &str, &str),
    version: Option<&str>,
    headers: &axum::http::HeaderMap,
) {
    let mut details = serde_json::json!({
        "repo": format!("{owner}/{repo}"),
        "pkg_type": "npm",
        "pkg_name": pkg_name,
        "tag": tag,
    });
    if let Some(version) = version {
        details["version"] = serde_json::Value::from(version);
    }
    rg_core::audit::record(
        &state.db,
        actor,
        action,
        Some("npm_dist_tag"),
        None,
        Some(&format!("{owner}/{repo}/{pkg_name}:{tag}")),
        Some(headers),
        Some(details),
    )
    .await;
}

/// A Maven digest of one stored file, without collecting a local artifact.
async fn hash_package_file(
    state: &AppState,
    owner: &str,
    repo: &str,
    pkg_name: &str,
    version: &str,
    filename: &str,
    algorithm: rg_core::package_registry::MavenChecksum,
) -> Result<String, axum::response::Response> {
    let storage =
        rg_core::package_registry::PackageStorage::from_backend(state.blob_storage.clone());
    match rg_core::package_registry::service::download_file(
        &state.db, &storage, owner, repo, "maven", pkg_name, version, filename,
    )
    .await
    {
        Ok(file) => match file.source {
            rg_core::package_registry::storage::PackageFileSource::LocalFile { path, .. } => {
                algorithm.hex_file(&path).await.map_err(|error| {
                    package_file_error_response(anyhow::anyhow!(
                        "failed to hash Maven package file {}: {error}",
                        path.display()
                    ))
                })
            }
            rg_core::package_registry::storage::PackageFileSource::Buffered(data) => {
                Ok(algorithm.hex(&data))
            }
        },
        Err(error) => Err(package_file_error_response(error)),
    }
}

#[derive(Deserialize)]
pub struct NuGetSearchParams {
    #[serde(default)]
    pub q: Option<String>,
    /// The second request form of `SearchAutocompleteService`: enumerate the
    /// versions of one package id instead of matching ids by prefix.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub skip: Option<usize>,
    #[serde(default)]
    pub take: Option<usize>,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default, rename = "semVerLevel")]
    pub semver_level: Option<String>,
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

/// Answer one of the compact index files.
fn text_index(body: String) -> axum::response::Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

// ── helpers ───────────────────────────────────────────────

/// The base of every link a package index hands its client. The client
/// downloads from these links as given, so they come from the one rule in
/// [`crate::public_url`] — never from a scheme guessed off the host name
/// (card_f78054e9e98f).
fn package_base_url(state: &AppState, headers: &axum::http::HeaderMap) -> Result<String, AppError> {
    crate::public_url::require_public_base_url(state, headers)
}

/// What a NuGet version's stored `protocol_metadata` says, in the shape the
/// registration index publishes it.
#[derive(Default)]
pub(crate) struct NuGetVersionMetadata {
    description: Option<String>,
    homepage: Option<String>,
    license: Option<String>,
    tags: Option<String>,
    /// The `<dependencies>` block of the nuspec, read back. Empty when the
    /// nuspec declared none — which is not the same as "we did not look", and
    /// is why the key is absent from the leaf rather than sent empty.
    dependency_groups: Vec<rg_core::package_registry::NuGetDependencyGroup>,
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

#[cfg(test)]
mod package_upload_staging_tests {
    use super::*;
    use std::convert::Infallible;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// The expensive adapter entry points belong behind the owned-artifact
    /// helper. Keeping this assertion beside the handlers makes a direct-call
    /// mutation fail even though all format and status tests still pass.
    #[test]
    fn publish_handlers_cannot_call_package_adapters_on_async_workers() {
        // The handlers live in this module's files, so the guard reads all of them.
        let source = rust_source::production_rust_code_only(
            &[
                include_str!("mod.rs"),
                include_str!("cargo.rs"),
                include_str!("composer.rs"),
                include_str!("helm.rs"),
                include_str!("maven.rs"),
                include_str!("npm.rs"),
                include_str!("nuget.rs"),
                include_str!("oci.rs"),
                include_str!("pypi.rs"),
                include_str!("rubygems.rs"),
            ]
            .concat(),
        );
        let validate = [".vali", "date("].concat();
        let extract = [".extract", "_metadata("].concat();
        let inspect = [".inspect", "_with_adapter("].concat();
        let hash = [".run", "_blocking(artifact_sha256)"].concat();

        assert_eq!(source.matches(&validate).count(), 0);
        assert_eq!(source.matches(&extract).count(), 0);
        assert_eq!(source.matches(&inspect).count(), 4);
        assert_eq!(source.matches(&hash).count(), 1);
    }

    #[tokio::test]
    async fn request_chunks_are_spooled_and_the_temporary_file_is_retired() {
        let root = tempfile::tempdir().unwrap();
        let body = Body::from_stream(futures::stream::iter([
            Ok::<_, Infallible>(axum::body::Bytes::from_static(b"first-")),
            Ok::<_, Infallible>(axum::body::Bytes::from_static(b"second")),
        ]));

        let staged = stage_package_upload(body, root.path(), 12).await.unwrap();
        let path = staged
            .spool_path()
            .expect("the bounded ingress must be a real spool file")
            .to_path_buf();
        assert!(path.exists());
        assert_eq!(staged.len(), 12);
        assert_eq!(staged.to_bytes().unwrap(), b"first-second");
        drop(staged);
        assert!(!path.exists(), "TempPath must retire the spool after use");
    }

    #[tokio::test]
    async fn crossing_the_ceiling_returns_413_and_leaves_no_spool() {
        let root = tempfile::tempdir().unwrap();
        let error = stage_package_upload(Body::from("12345"), root.path(), 4)
            .await
            .unwrap_err();
        assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(error
            .to_string()
            .contains("configured 4-byte request limit"));

        let entries = std::fs::read_dir(root.path().join(".tmp/package-uploads"))
            .unwrap()
            .count();
        assert_eq!(entries, 0, "a refused upload left a staging file behind");
    }

    #[tokio::test]
    async fn chunked_transport_overflow_is_413_and_leaves_no_spool() {
        let root = tempfile::tempdir().unwrap();
        let chunks = futures::stream::iter([
            Ok::<_, Infallible>(axum::body::Bytes::from_static(b"123")),
            Ok::<_, Infallible>(axum::body::Bytes::from_static(b"45")),
        ]);
        let body = Body::from_stream(chunks);
        let limited = Body::new(http_body_util::Limited::new(body, 4));

        let error = stage_package_upload(limited, root.path(), 10)
            .await
            .unwrap_err();
        assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(error.to_string().contains("configured request-body limit"));
        assert_eq!(
            std::fs::read_dir(root.path().join(".tmp/package-uploads"))
                .unwrap()
                .count(),
            0,
            "a refused chunked upload left a staging file behind"
        );
    }

    #[tokio::test]
    async fn ordinary_body_failure_stays_400_and_leaves_no_spool() {
        let root = tempfile::tempdir().unwrap();
        let body = Body::from_stream(futures::stream::once(async {
            Err::<axum::body::Bytes, _>(std::io::Error::other("connection reset"))
        }));

        let error = stage_package_upload(body, root.path(), 10)
            .await
            .unwrap_err();
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("connection reset"));
        assert_eq!(
            std::fs::read_dir(root.path().join(".tmp/package-uploads"))
                .unwrap()
                .count(),
            0,
            "a failed body read left a staging file behind"
        );
    }
}

#[cfg(test)]
mod package_multipart_error_tests {
    use super::*;
    use axum::{
        extract::DefaultBodyLimit,
        http::{Method, Request},
        routing::post,
    };
    use std::convert::Infallible;
    use tower::ServiceExt as _;
    use tower_http::limit::RequestBodyLimitLayer;

    const BOUNDARY: &str = "plombir-git-package-boundary";

    /// Where these handlers spool their fields.
    ///
    /// The decoders now stage the artifact field instead of collecting it, so
    /// they need a repository root; one shared directory for the module keeps
    /// the fixtures the bare `Multipart` handlers they were.
    fn staging_root() -> &'static std::path::Path {
        static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| tempfile::tempdir().expect("multipart staging root"))
            .path()
    }

    async fn twine_status(multipart: Multipart) -> axum::response::Response {
        match decode_twine_upload(multipart, staging_root(), 1024 * 1024).await {
            Ok(_) => StatusCode::OK.into_response(),
            Err(error) => error.into_response(),
        }
    }

    async fn nuget_status(multipart: Multipart) -> axum::response::Response {
        match decode_nuget_push(multipart, staging_root(), 1024 * 1024).await {
            Ok(_) => StatusCode::OK.into_response(),
            Err(error) => error.into_response(),
        }
    }

    fn multipart_part(name: &str, value: &[u8], filename: Option<&str>) -> Vec<u8> {
        let mut body =
            format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"").into_bytes();
        if let Some(filename) = filename {
            body.extend_from_slice(format!("; filename=\"{filename}\"").as_bytes());
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(value);
        body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        body
    }

    /// One multipart body whose field value is `frames` copies of a shared 1 MiB
    /// frame, delivered as a stream.
    ///
    /// The frame is cloned, and cloning `Bytes` is a refcount, so the *test*
    /// holds one megabyte however large the field is. That is the point: a
    /// measurement taken around this cannot be the test's own allocation.
    fn streamed_multipart_field(name: &str, filename: Option<&str>, frames: usize) -> Body {
        let mut head =
            format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"").into_bytes();
        if let Some(filename) = filename {
            head.extend_from_slice(format!("; filename=\"{filename}\"").as_bytes());
        }
        head.extend_from_slice(b"\r\n\r\n");
        let frame = axum::body::Bytes::from(vec![b'p'; FRAME_BYTES]);
        let tail = axum::body::Bytes::from(format!("\r\n--{BOUNDARY}--\r\n").into_bytes());

        let parts = std::iter::once(axum::body::Bytes::from(head))
            .chain(std::iter::repeat_n(frame, frames))
            .chain(std::iter::once(tail));
        // Yield between frames. A stream that is never `Pending` is not a
        // socket: `multer` keeps pulling while the source stays ready, so it
        // would swallow the whole field before the decoder saw a byte of it and
        // the reading would be the multipart parser's buffer rather than
        // anything this module does with the field.
        Body::from_stream(futures::stream::iter(parts).then(|part| async move {
            tokio::task::yield_now().await;
            Ok::<_, Infallible>(part)
        }))
    }

    const FRAME_BYTES: usize = 1024 * 1024;
    /// 64 MiB: far above anything either decoder legitimately keeps, and far
    /// enough above the process' own footprint that a collect cannot hide in it.
    const FRAMES: usize = 64;

    /// Peak resident set size of this process so far, in bytes.
    ///
    /// The *peak*, because a field that was collected and then dropped is back
    /// off the books by the time the handler answers. See the same helper in
    /// `api::runners`.
    fn peak_resident_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status
            .lines()
            .find(|line| line.starts_with("VmHWM:"))?
            .strip_prefix("VmHWM:")?;
        let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
        Some(kib * 1024)
    }

    /// Report what the decoder produced, so the assertion can see whether the
    /// field was staged or collected.
    fn describe(artifact: &PackageArtifact) -> String {
        format!("{} {}", artifact.spool_path().is_some(), artifact.len())
    }

    async fn twine_probe(multipart: Multipart) -> axum::response::Response {
        match decode_twine_upload(multipart, staging_root(), FRAME_BYTES * FRAMES).await {
            Ok(upload) => (
                StatusCode::OK,
                describe(&upload.content.expect("the `content` field")),
            )
                .into_response(),
            Err(error) => error.into_response(),
        }
    }

    async fn nuget_probe(multipart: Multipart) -> axum::response::Response {
        match decode_nuget_push(multipart, staging_root(), FRAME_BYTES * FRAMES).await {
            Ok(artifact) => (StatusCode::OK, describe(&artifact)).into_response(),
            Err(error) => error.into_response(),
        }
    }

    /// A form field is not the artifact, and it is not spooled either — so the
    /// bound on it is its own, and the refusal has to be the same `413` an
    /// oversized artifact gets rather than a truncated field nobody notices.
    #[tokio::test]
    async fn an_oversized_twine_text_field_is_413() {
        let body = streamed_multipart_field("attestations", None, 2);
        let (status, message) =
            request_outcome(post(twine_probe), body, FRAME_BYTES * (FRAMES + 2)).await;

        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{message}");
        assert!(
            message.contains("`attestations` field exceeds"),
            "the refusal must name the field: {message}"
        );
    }

    /// The multipart half of the same defect.
    ///
    /// These two routes never had a spool at all: `Field::bytes` collects the
    /// whole field, so the only thing standing between a publisher and an
    /// artifact-sized allocation was the multipart envelope's ceiling. The
    /// assertion is both halves of the answer — the artifact has to *be* a
    /// spool, and the process must not have grown as if it were not.
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "reads VmHWM from /proc/self/status"
    )]
    #[tokio::test]
    async fn a_large_multipart_artifact_is_spooled_rather_than_collected() {
        for (label, handler, field, filename) in [
            (
                "twine",
                post(twine_probe),
                "content",
                Some("sample-1.0.0.tar.gz"),
            ),
            ("nuget", post(nuget_probe), "package", Some("package.nupkg")),
        ] {
            let body = streamed_multipart_field(field, filename, FRAMES);
            let before = peak_resident_bytes().expect("no /proc/self/status to measure against");
            let (status, reported) =
                request_outcome(handler, body, FRAME_BYTES * (FRAMES + 2)).await;
            let after = peak_resident_bytes().expect("no /proc/self/status to measure against");

            assert_eq!(status, StatusCode::OK, "{label}: {reported}");
            assert_eq!(
                reported,
                format!("true {}", FRAME_BYTES * FRAMES),
                "{label}: the artifact field must arrive as a spool of the right size"
            );

            let grew = after.saturating_sub(before);
            let ceiling = (FRAME_BYTES * FRAMES / 4) as u64;
            assert!(
                grew < ceiling,
                "{label}: a {} MiB field grew the process by {} MiB — it is being collected, \
                 not spooled",
                FRAME_BYTES * FRAMES / (1024 * 1024),
                grew / (1024 * 1024)
            );
        }
    }

    fn unknown_length_body(body: Vec<u8>, first_chunk_len: usize) -> Body {
        assert!(first_chunk_len < body.len());
        let first = axum::body::Bytes::copy_from_slice(&body[..first_chunk_len]);
        let second = axum::body::Bytes::copy_from_slice(&body[first_chunk_len..]);
        Body::from_stream(futures::stream::iter([
            Ok::<_, Infallible>(first),
            Ok::<_, Infallible>(second),
        ]))
    }

    async fn request_outcome(
        handler: axum::routing::MethodRouter,
        body: Body,
        transport_limit: usize,
    ) -> (StatusCode, String) {
        request_outcome_with_content_type(
            handler,
            body,
            transport_limit,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .await
    }

    async fn request_outcome_with_content_type(
        handler: axum::routing::MethodRouter,
        body: Body,
        transport_limit: usize,
        content_type: String,
    ) -> (StatusCode, String) {
        let app = handler
            .layer::<_, Infallible>(DefaultBodyLimit::max(transport_limit))
            .layer::<_, Infallible>(RequestBodyLimitLayer::new(transport_limit));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/")
                    .header(header::CONTENT_TYPE, content_type)
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    fn split_inside_value(body: &[u8]) -> usize {
        body.windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("multipart field header terminator")
            + 5
    }

    #[tokio::test]
    async fn unknown_length_twine_text_overflow_is_413() {
        let body = multipart_part(":action", b"file_upload", None);
        let transport_limit = split_inside_value(&body);

        let (status, response_body) = request_outcome(
            post(twine_status),
            unknown_length_body(body, transport_limit),
            transport_limit,
        )
        .await;

        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{response_body}");
        assert!(response_body.contains("configured request-body limit"));
    }

    #[tokio::test]
    async fn unknown_length_nuget_package_overflow_is_413() {
        let body = multipart_part("package", b"not-a-complete-nupkg", Some("package.nupkg"));
        let transport_limit = split_inside_value(&body);

        let (status, response_body) = request_outcome(
            post(nuget_status),
            unknown_length_body(body, transport_limit),
            transport_limit,
        )
        .await;

        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{response_body}");
        assert!(response_body.contains("configured request-body limit"));
    }

    #[tokio::test]
    async fn unknown_length_next_field_overflow_is_413_for_both_protocols() {
        let head = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"ignored\"\r\n\r\npartial"
        )
        .into_bytes();
        let tail = format!("-field\r\n--{BOUNDARY}--\r\n").into_bytes();
        let transport_limit = head.len() + 1;
        let mut body = head;
        body.extend_from_slice(&tail);

        let twine = request_outcome(
            post(twine_status),
            unknown_length_body(body.clone(), transport_limit),
            transport_limit,
        )
        .await;
        let nuget = request_outcome(
            post(nuget_status),
            unknown_length_body(body, transport_limit),
            transport_limit,
        )
        .await;

        assert_eq!(twine.0, StatusCode::PAYLOAD_TOO_LARGE, "{}", twine.1);
        assert_eq!(nuget.0, StatusCode::PAYLOAD_TOO_LARGE, "{}", nuget.1);
        assert!(twine.1.contains("configured request-body limit"));
        assert!(nuget.1.contains("configured request-body limit"));
    }

    #[tokio::test]
    async fn malformed_utf8_and_ordinary_io_failures_stay_400() {
        assert_eq!(
            request_outcome_with_content_type(
                post(twine_status),
                Body::empty(),
                1024,
                "multipart/form-data".to_string(),
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );

        let invalid_utf8 = multipart_part(":action", &[0xff], None);
        assert_eq!(
            request_outcome(post(twine_status), Body::from(invalid_utf8), 1024)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );

        let partial = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"package\"; \
             filename=\"package.nupkg\"\r\n\r\npartial"
        );
        let failed = Body::from_stream(futures::stream::iter([
            Ok(axum::body::Bytes::from(partial)),
            Err(std::io::Error::other("connection reset")),
        ]));
        assert_eq!(
            request_outcome(post(nuget_status), failed, 1024).await.0,
            StatusCode::BAD_REQUEST
        );
    }
}
