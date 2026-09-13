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

/// Metadata and encoding headroom above the decoded artifact ceiling.
///
/// npm expands a tarball to `ceil(n * 4 / 3)` before JSON framing; multipart
/// and Cargo add smaller envelopes. Keeping one bounded request allowance for
/// all enveloped package protocols means the configured value remains the
/// artifact ceiling rather than an npm-only 25% haircut.
const PACKAGE_UPLOAD_ENVELOPE_HEADROOM: usize = 1024 * 1024;

/// How much `cargo publish` frame metadata is read as JSON.
///
/// The first block of the frame is the index entry cargo expects back out of
/// the sparse index — dependencies, features, a checksum — and it is kilobytes
/// by construction. It is the one block that has to be parsed in memory, so the
/// length prefix that says how much to read is bounded rather than believed: a
/// prefix claiming the whole envelope would otherwise be a way to spend the
/// request ceiling of heap on a route whose artifact is spooled.
const MAX_CARGO_PUBLISH_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// Heap budget for the part of an npm publish document that is not attachment data.
///
/// The tarball may be hundreds of megabytes, but package coordinates, dist-tags and
/// the one version manifest are ordinary metadata. Attachment strings are removed
/// while the staged request is scanned, so this is a real heap ceiling rather than
/// another spelling of the request-body limit.
const MAX_NPM_PACKUMENT_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// A provenance bundle is metadata, not a second package artifact.
///
/// It has to be parsed as JSON to validate its in-toto subject, so unlike the
/// tarball it cannot remain entirely opaque. Keeping a separate small ceiling
/// prevents a malicious `.sigstore` attachment from reopening the same heap bug.
const MAX_NPM_PROVENANCE_BYTES: usize = 4 * 1024 * 1024;

/// Match serde_json's default recursion boundary and fail before recursive descent
/// can turn a hostile packument into an unbounded call stack.
const MAX_NPM_PACKUMENT_DEPTH: usize = 128;

/// Object keys are metadata too. This is generous for scoped npm names while
/// keeping the small temporary allocation used to unescape a key honest.
const MAX_NPM_PACKUMENT_KEY_BYTES: usize = 64 * 1024;

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
/// can enforce ForgeKeep's configured boundary.
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
        let source = rust_source::production_rust_code_only(include_str!("packages.rs"));
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

#[derive(Clone, Copy, Debug)]
struct NpmAttachmentDataRange {
    start: u64,
    end: u64,
}

struct StagedNpmPackument {
    body: PackageArtifact,
    packument: NpmPublishPackument,
    data_ranges: BTreeMap<String, NpmAttachmentDataRange>,
}

#[derive(Debug)]
enum NpmPackumentError {
    Invalid(String),
    TooLarge(String),
    Internal(String),
}

impl std::fmt::Display for NpmPackumentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::TooLarge(message) | Self::Internal(message) => {
                formatter.write_str(message)
            }
        }
    }
}

#[derive(Clone, Debug)]
enum NpmJsonObject {
    Root,
    Attachments,
    Attachment(String),
    Other,
}

/// Copy an npm publish document while replacing attachment strings with `""`.
///
/// The request is already on disk. This scanner visits it once, remembers the
/// byte ranges occupied by `_attachments.<filename>.data`, and builds only the
/// small JSON document serde needs for coordinates and declared lengths. The
/// large strings are decoded from their original ranges in a second pass.
struct NpmPackumentScanner<R> {
    reader: io::BufReader<R>,
    position: u64,
    metadata: Vec<u8>,
    data_ranges: BTreeMap<String, NpmAttachmentDataRange>,
    attachment_names: BTreeSet<String>,
    saw_attachments: bool,
}

impl<R: io::Read> NpmPackumentScanner<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: io::BufReader::with_capacity(64 * 1024, reader),
            position: 0,
            metadata: Vec::new(),
            data_ranges: BTreeMap::new(),
            attachment_names: BTreeSet::new(),
            saw_attachments: false,
        }
    }

    fn scan(
        mut self,
    ) -> Result<(Vec<u8>, BTreeMap<String, NpmAttachmentDataRange>), NpmPackumentError> {
        self.copy_whitespace()?;
        self.scan_value(0, NpmJsonObject::Root)?;
        self.copy_whitespace()?;
        if self.peek_byte()?.is_some() {
            return Err(NpmPackumentError::Invalid(
                "npm publish packument contains trailing JSON data".into(),
            ));
        }
        Ok((self.metadata, self.data_ranges))
    }

    fn read_byte(&mut self) -> Result<Option<u8>, NpmPackumentError> {
        let byte = match self.reader.fill_buf() {
            Ok([]) => return Ok(None),
            Ok(buffer) => buffer[0],
            Err(error) => Err(NpmPackumentError::Internal(format!(
                "cannot read the staged npm packument: {error}"
            )))?,
        };
        self.reader.consume(1);
        self.position += 1;
        Ok(Some(byte))
    }

    fn peek_byte(&mut self) -> Result<Option<u8>, NpmPackumentError> {
        match self.reader.fill_buf() {
            Ok([]) => Ok(None),
            Ok(buffer) => Ok(Some(buffer[0])),
            Err(error) => Err(NpmPackumentError::Internal(format!(
                "cannot read the staged npm packument: {error}"
            ))),
        }
    }

    fn required_byte(&mut self, what: &str) -> Result<u8, NpmPackumentError> {
        self.read_byte()?.ok_or_else(|| {
            NpmPackumentError::Invalid(format!(
                "invalid npm publish packument: unexpected end while reading {what}"
            ))
        })
    }

    fn emit(&mut self, byte: u8) -> Result<(), NpmPackumentError> {
        if self.metadata.len() == MAX_NPM_PACKUMENT_METADATA_BYTES {
            return Err(NpmPackumentError::TooLarge(format!(
                "npm packument metadata exceeds the configured {MAX_NPM_PACKUMENT_METADATA_BYTES}-byte limit"
            )));
        }
        self.metadata.push(byte);
        Ok(())
    }

    fn copy_whitespace(&mut self) -> Result<(), NpmPackumentError> {
        while self
            .peek_byte()?
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            let byte = self.required_byte("whitespace")?;
            self.emit(byte)?;
        }
        Ok(())
    }

    fn scan_value(&mut self, depth: usize, object: NpmJsonObject) -> Result<(), NpmPackumentError> {
        self.copy_whitespace()?;
        match self.peek_byte()? {
            Some(b'{') => self.scan_object(depth, object),
            Some(b'[') => self.scan_array(depth),
            Some(b'"') => {
                self.scan_copied_string(false)?;
                Ok(())
            }
            Some(_) => self.scan_primitive(),
            None => Err(NpmPackumentError::Invalid(
                "invalid npm publish packument: missing JSON value".into(),
            )),
        }
    }

    fn scan_object(
        &mut self,
        depth: usize,
        object: NpmJsonObject,
    ) -> Result<(), NpmPackumentError> {
        if depth >= MAX_NPM_PACKUMENT_DEPTH {
            return Err(NpmPackumentError::Invalid(format!(
                "npm publish packument exceeds the {MAX_NPM_PACKUMENT_DEPTH}-level JSON nesting limit"
            )));
        }
        let opening = self.required_byte("object opening")?;
        self.emit(opening)?;
        self.copy_whitespace()?;
        if self.peek_byte()? == Some(b'}') {
            let closing = self.required_byte("object closing")?;
            self.emit(closing)?;
            return Ok(());
        }

        loop {
            let capture_key = !matches!(object, NpmJsonObject::Other);
            let key = self.scan_copied_string(capture_key)?;
            self.copy_whitespace()?;
            if self.required_byte("object colon")? != b':' {
                return Err(NpmPackumentError::Invalid(
                    "invalid npm publish packument: expected ':' after object key".into(),
                ));
            }
            self.emit(b':')?;
            self.copy_whitespace()?;

            match (&object, key.as_deref()) {
                (NpmJsonObject::Root, Some("_attachments")) => {
                    if self.saw_attachments {
                        return Err(NpmPackumentError::Invalid(
                            "npm publish packument contains duplicate `_attachments` keys".into(),
                        ));
                    }
                    self.saw_attachments = true;
                    self.scan_value(depth + 1, NpmJsonObject::Attachments)?;
                }
                (NpmJsonObject::Attachments, Some(filename)) => {
                    if !self.attachment_names.insert(filename.to_string()) {
                        return Err(NpmPackumentError::Invalid(format!(
                            "npm publish packument contains duplicate attachment '{filename}'"
                        )));
                    }
                    self.scan_value(depth + 1, NpmJsonObject::Attachment(filename.to_string()))?;
                }
                (NpmJsonObject::Attachment(filename), Some("data")) => {
                    self.scan_attachment_data(filename.clone())?;
                }
                _ => self.scan_value(depth + 1, NpmJsonObject::Other)?,
            }

            self.copy_whitespace()?;
            match self.required_byte("object delimiter")? {
                b',' => {
                    self.emit(b',')?;
                    self.copy_whitespace()?;
                }
                b'}' => {
                    self.emit(b'}')?;
                    return Ok(());
                }
                _ => {
                    return Err(NpmPackumentError::Invalid(
                        "invalid npm publish packument: expected ',' or '}'".into(),
                    ))
                }
            }
        }
    }

    fn scan_array(&mut self, depth: usize) -> Result<(), NpmPackumentError> {
        if depth >= MAX_NPM_PACKUMENT_DEPTH {
            return Err(NpmPackumentError::Invalid(format!(
                "npm publish packument exceeds the {MAX_NPM_PACKUMENT_DEPTH}-level JSON nesting limit"
            )));
        }
        let opening = self.required_byte("array opening")?;
        self.emit(opening)?;
        self.copy_whitespace()?;
        if self.peek_byte()? == Some(b']') {
            let closing = self.required_byte("array closing")?;
            self.emit(closing)?;
            return Ok(());
        }
        loop {
            self.scan_value(depth + 1, NpmJsonObject::Other)?;
            self.copy_whitespace()?;
            match self.required_byte("array delimiter")? {
                b',' => {
                    self.emit(b',')?;
                    self.copy_whitespace()?;
                }
                b']' => {
                    self.emit(b']')?;
                    return Ok(());
                }
                _ => {
                    return Err(NpmPackumentError::Invalid(
                        "invalid npm publish packument: expected ',' or ']'".into(),
                    ))
                }
            }
        }
    }

    fn scan_copied_string(&mut self, capture: bool) -> Result<Option<String>, NpmPackumentError> {
        if self.required_byte("JSON string")? != b'"' {
            return Err(NpmPackumentError::Invalid(
                "invalid npm publish packument: object key is not a string".into(),
            ));
        }
        self.emit(b'"')?;
        let mut raw = capture.then(|| vec![b'"']);
        loop {
            let byte = self.required_byte("JSON string")?;
            self.emit(byte)?;
            if let Some(raw) = raw.as_mut() {
                if raw.len() == MAX_NPM_PACKUMENT_KEY_BYTES {
                    return Err(NpmPackumentError::TooLarge(format!(
                        "npm packument object key exceeds the configured {MAX_NPM_PACKUMENT_KEY_BYTES}-byte limit"
                    )));
                }
                raw.push(byte);
            }
            match byte {
                b'"' => break,
                b'\\' => {
                    let escaped = self.required_byte("JSON string escape")?;
                    self.emit(escaped)?;
                    if let Some(raw) = raw.as_mut() {
                        raw.push(escaped);
                    }
                    if escaped == b'u' {
                        for _ in 0..4 {
                            let digit = self.required_byte("JSON unicode escape")?;
                            self.emit(digit)?;
                            if let Some(raw) = raw.as_mut() {
                                raw.push(digit);
                            }
                        }
                    }
                }
                0x00..=0x1f => {
                    return Err(NpmPackumentError::Invalid(
                        "invalid npm publish packument: control byte in JSON string".into(),
                    ))
                }
                _ => {}
            }
        }
        raw.map(|raw| {
            serde_json::from_slice::<String>(&raw).map_err(|error| {
                NpmPackumentError::Invalid(format!(
                    "invalid npm publish packument object key: {error}"
                ))
            })
        })
        .transpose()
    }

    fn scan_attachment_data(&mut self, filename: String) -> Result<(), NpmPackumentError> {
        if self.required_byte("attachment data")? != b'"' {
            return Err(NpmPackumentError::Invalid(format!(
                "npm attachment '{filename}' data is not a JSON string"
            )));
        }
        self.emit(b'"')?;
        let start = self.position;
        loop {
            let plain = {
                let buffer = self.reader.fill_buf().map_err(|error| {
                    NpmPackumentError::Internal(format!(
                        "cannot read the staged npm packument: {error}"
                    ))
                })?;
                buffer
                    .iter()
                    .position(|byte| matches!(*byte, b'"' | b'\\' | 0x00..=0x1f))
                    .unwrap_or(buffer.len())
            };
            if plain > 0 {
                self.reader.consume(plain);
                self.position += plain as u64;
                continue;
            }
            let byte = self.required_byte("attachment data")?;
            match byte {
                b'"' => {
                    let end = self.position - 1;
                    self.emit(b'"')?;
                    if self
                        .data_ranges
                        .insert(filename.clone(), NpmAttachmentDataRange { start, end })
                        .is_some()
                    {
                        return Err(NpmPackumentError::Invalid(format!(
                            "npm attachment '{filename}' contains duplicate `data` keys"
                        )));
                    }
                    return Ok(());
                }
                b'\\' => {
                    let escaped = self.required_byte("attachment data escape")?;
                    if escaped == b'u' {
                        for _ in 0..4 {
                            self.required_byte("attachment data unicode escape")?;
                        }
                    }
                }
                0x00..=0x1f => {
                    return Err(NpmPackumentError::Invalid(format!(
                        "npm attachment '{filename}' contains a control byte"
                    )))
                }
                _ => {}
            }
        }
    }

    fn scan_primitive(&mut self) -> Result<(), NpmPackumentError> {
        let mut read = 0_usize;
        while let Some(byte) = self.peek_byte()? {
            if byte.is_ascii_whitespace() || matches!(byte, b',' | b']' | b'}') {
                break;
            }
            let byte = self.required_byte("JSON value")?;
            self.emit(byte)?;
            read += 1;
        }
        if read == 0 {
            return Err(NpmPackumentError::Invalid(
                "invalid npm publish packument: missing JSON value".into(),
            ));
        }
        Ok(())
    }
}

/// A bounded view of a JSON string's raw contents that yields its unescaped
/// bytes. It lets `base64::read::DecoderReader` consume a tarball incrementally
/// without ever constructing the encoded string.
struct JsonStringRangeReader<R> {
    inner: io::BufReader<R>,
    remaining: u64,
    pending: [u8; 4],
    pending_start: usize,
    pending_end: usize,
}

impl<R: io::Read> JsonStringRangeReader<R> {
    fn new(inner: R, remaining: u64) -> Self {
        Self {
            inner: io::BufReader::with_capacity(64 * 1024, inner),
            remaining,
            pending: [0; 4],
            pending_start: 0,
            pending_end: 0,
        }
    }

    fn invalid(message: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message.into())
    }

    fn raw_byte(&mut self) -> io::Result<u8> {
        if self.remaining == 0 {
            return Err(Self::invalid("truncated JSON string escape"));
        }
        let byte = match self.inner.fill_buf()? {
            [] => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated JSON string",
                ))
            }
            buffer => buffer[0],
        };
        self.inner.consume(1);
        self.remaining -= 1;
        Ok(byte)
    }

    fn unicode_unit(&mut self) -> io::Result<u16> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let digit = self.raw_byte()?;
            let digit = (digit as char)
                .to_digit(16)
                .ok_or_else(|| Self::invalid("invalid JSON unicode escape"))?;
            value = (value << 4) | digit as u16;
        }
        Ok(value)
    }

    fn fill_pending(&mut self) -> io::Result<bool> {
        if self.remaining == 0 {
            return Ok(false);
        }
        let byte = self.raw_byte()?;
        let character = if byte != b'\\' {
            if byte < 0x20 {
                return Err(Self::invalid("control byte in JSON string"));
            }
            self.pending[0] = byte;
            self.pending_start = 0;
            self.pending_end = 1;
            return Ok(true);
        } else {
            match self.raw_byte()? {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'b' => '\u{0008}',
                b'f' => '\u{000c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let high = self.unicode_unit()?;
                    let codepoint = if (0xd800..=0xdbff).contains(&high) {
                        if self.raw_byte()? != b'\\' || self.raw_byte()? != b'u' {
                            return Err(Self::invalid("unpaired high surrogate in JSON string"));
                        }
                        let low = self.unicode_unit()?;
                        if !(0xdc00..=0xdfff).contains(&low) {
                            return Err(Self::invalid("unpaired high surrogate in JSON string"));
                        }
                        0x1_0000 + (((high as u32 - 0xd800) << 10) | (low as u32 - 0xdc00))
                    } else {
                        if (0xdc00..=0xdfff).contains(&high) {
                            return Err(Self::invalid("unpaired low surrogate in JSON string"));
                        }
                        high as u32
                    };
                    char::from_u32(codepoint)
                        .ok_or_else(|| Self::invalid("invalid unicode scalar in JSON string"))?
                }
                _ => return Err(Self::invalid("invalid JSON string escape")),
            }
        };

        let encoded = character.encode_utf8(&mut self.pending);
        self.pending_start = 0;
        self.pending_end = encoded.len();
        Ok(true)
    }
}

impl<R: io::Read> io::Read for JsonStringRangeReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let mut written = 0;
        while written < output.len() {
            if self.pending_start != self.pending_end {
                let available = self.pending_end - self.pending_start;
                let copy = available.min(output.len() - written);
                output[written..written + copy]
                    .copy_from_slice(&self.pending[self.pending_start..self.pending_start + copy]);
                self.pending_start += copy;
                written += copy;
                continue;
            }
            if self.remaining == 0 {
                break;
            }

            let plain = {
                let buffer = self.inner.fill_buf()?;
                if buffer.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated JSON string",
                    ));
                }
                let available = buffer.len().min(self.remaining as usize);
                buffer[..available]
                    .iter()
                    .position(|byte| *byte == b'\\' || *byte < 0x20)
                    .unwrap_or(available)
                    .min(output.len() - written)
            };
            if plain > 0 {
                let buffer = self.inner.fill_buf()?;
                output[written..written + plain].copy_from_slice(&buffer[..plain]);
                self.inner.consume(plain);
                self.remaining -= plain as u64;
                written += plain;
                continue;
            }
            if !self.fill_pending()? {
                break;
            }
        }
        Ok(written)
    }
}

fn parse_staged_npm_packument(
    body: PackageArtifact,
) -> Result<StagedNpmPackument, NpmPackumentError> {
    let reader = body.reader().map_err(|error| {
        NpmPackumentError::Internal(format!("cannot read the staged npm packument: {error}"))
    })?;
    let (metadata, data_ranges) = NpmPackumentScanner::new(reader).scan()?;
    let packument = serde_json::from_slice(&metadata).map_err(|error| {
        NpmPackumentError::Invalid(format!("invalid npm publish packument: {error}"))
    })?;
    Ok(StagedNpmPackument {
        body,
        packument,
        data_ranges,
    })
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

struct DecodedNpmTarball {
    artifact: PackageArtifact,
    sha512: String,
}

fn decode_npm_tarball_range(
    body: &PackageArtifact,
    range: NpmAttachmentDataRange,
    filename: &str,
    artifact_limit: usize,
    repo_root: &FsPath,
) -> Result<DecodedNpmTarball, NpmPackumentError> {
    let reader = attachment_range_reader(body, range)?;
    let mut decoder =
        base64::read::DecoderReader::new(reader, &base64::engine::general_purpose::STANDARD);
    let staging_dir = rg_core::staging::StagingArea::PackageUploads.path_in(repo_root);
    let mut staged = tempfile::Builder::new()
        .prefix("npm-attachment-")
        .suffix(".upload")
        .tempfile_in(&staging_dir)
        .map_err(|error| {
            NpmPackumentError::Internal(format!(
                "cannot create npm attachment staging file: {error}"
            ))
        })?;
    let mut hasher = Sha512::new();
    let mut buffer = vec![0_u8; 128 * 1024];
    let mut len = 0_u64;
    loop {
        let read = decoder.read(&mut buffer).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                NpmPackumentError::Invalid(format!(
                    "npm tarball attachment '{filename}' is not valid base64: {error}"
                ))
            } else {
                NpmPackumentError::Internal(format!("cannot read npm tarball attachment: {error}"))
            }
        })?;
        if read == 0 {
            break;
        }
        len = len.checked_add(read as u64).ok_or_else(|| {
            NpmPackumentError::TooLarge(format!(
                "npm tarball attachment exceeds the configured {artifact_limit}-byte artifact limit"
            ))
        })?;
        if len > artifact_limit as u64 {
            return Err(NpmPackumentError::TooLarge(format!(
                "npm tarball attachment exceeds the configured {artifact_limit}-byte artifact limit"
            )));
        }
        staged.write_all(&buffer[..read]).map_err(|error| {
            NpmPackumentError::Internal(format!(
                "cannot write npm attachment staging file: {error}"
            ))
        })?;
        hasher.update(&buffer[..read]);
    }
    staged.flush().map_err(|error| {
        NpmPackumentError::Internal(format!("cannot flush npm attachment staging file: {error}"))
    })?;
    let written = staged.as_file().metadata().map_err(|error| {
        NpmPackumentError::Internal(format!("cannot stat npm attachment staging file: {error}"))
    })?;
    if written.len() != len {
        return Err(NpmPackumentError::Internal(format!(
            "npm attachment staging file changed size before validation: expected {len}, got {}",
            written.len()
        )));
    }
    Ok(DecodedNpmTarball {
        artifact: PackageArtifact::spooled(staged.into_temp_path(), len),
        sha512: hex::encode(hasher.finalize()),
    })
}

/// The CouchDB-shaped document `npm publish` PUTs to the package URL.
///
/// npm sends one new version and its tarball per request. Unknown top-level and
/// version fields are intentionally ignored: the tarball's `package.json` is
/// the artifact authority, while this envelope only identifies and carries it.
///
/// `access` is the exception, and card_b5db648768e4 is why. It is not a
/// description of the artifact but an instruction about *who may read it*, and
/// serde's silent handling of unknown keys turned `npm publish --access
/// restricted` into a `201` for a package left as readable as its repository.
/// See [`npm_access_matches_repository`].
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
    /// `null` when the publisher expressed no preference, which npm's own CLI
    /// sends whenever `--access` is absent.
    #[serde(default)]
    pub access: Option<String>,
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
    #[serde(default)]
    pub content_type: Option<String>,
}

#[derive(Debug)]
struct DecodedNpmProvenance {
    filename: String,
    bundle: Vec<u8>,
    predicate_type: String,
}

#[derive(Debug)]
struct DecodedNpmPublish {
    filename: String,
    version: String,
    dist_tag: String,
    tarball: PackageArtifact,
    provenance: Option<DecodedNpmProvenance>,
}

struct PlannedNpmPublish {
    filename: String,
    version: String,
    dist_tag: String,
    tarball: NpmPublishAttachment,
    provenance: Option<(String, NpmPublishAttachment)>,
}

#[derive(Debug)]
struct InspectedNpmProvenance {
    bundle: serde_json::Value,
    bytes: Vec<u8>,
    predicate_type: String,
    subject_name: String,
    subject_sha512: String,
}

/// Whether the access the packument asks for is the access the registry gives.
///
/// ForgeKeep has no per-package visibility: a package is exactly as readable as
/// the repository holding it. So `access` cannot be *applied*, only agreed with
/// or refused — and refusing is the only honest answer, in both directions. A
/// `restricted` package in a public repository is world-readable while its
/// publisher believes it is not; a `public` one in a private repository is
/// invisible to the audience it was published for. Silence used to answer both
/// with `201`.
///
/// `None` is the absence of a claim (npm sends it whenever `--access` is not
/// given) and passes; an unrecognised value is refused, because a registry that
/// waves through an instruction it cannot read is back where it started.
fn npm_access_matches_repository(access: Option<&str>, is_private: bool) -> Result<(), String> {
    let Some(access) = access else {
        return Ok(());
    };
    let wanted_private = match access {
        "restricted" => true,
        "public" => false,
        other => {
            return Err(format!(
                "unsupported npm access '{other}': this registry understands only \
                 'public' and 'restricted'"
            ))
        }
    };
    if wanted_private == is_private {
        return Ok(());
    }
    let repository = if is_private { "private" } else { "public" };
    Err(format!(
        "npm access '{access}' does not match the {repository} repository this package is \
         published to. ForgeKeep has no per-package visibility — a package is readable by \
         exactly whoever can read its repository — so change the repository's visibility \
         instead of the publish's --access"
    ))
}

fn npm_package_purl(name: &str, version: &str) -> String {
    match name.strip_prefix('@').and_then(|name| name.split_once('/')) {
        Some((scope, package)) => format!("pkg:npm/%40{scope}/{package}@{version}"),
        None => format!("pkg:npm/{name}@{version}"),
    }
}

/// Parse the Sigstore bundle shape emitted by npm and the in-toto statement it
/// signs. Trust-chain verification remains npm/Sigstore's job; ForgeKeep's
/// publish boundary enforces the registry-specific invariant: the one subject
/// in that signed payload must name and hash the tarball in the same request.
fn inspect_npm_provenance_bytes(
    bytes: Vec<u8>,
    attachment: &NpmPublishAttachment,
) -> Result<InspectedNpmProvenance, String> {
    let data = std::str::from_utf8(&bytes)
        .map_err(|error| format!("npm provenance attachment is not valid UTF-8: {error}"))?;
    // npm writes JavaScript's `serializedBundle.length`, which counts UTF-16
    // code units rather than UTF-8 bytes. Real certificate material can contain
    // non-ASCII identity text, so comparing this field with `str::len()` rejects
    // a bundle npm itself just verified and sent.
    let npm_length = data.encode_utf16().count();
    if npm_length != attachment.length {
        return Err(format!(
            "npm provenance attachment length mismatch: declared {}, received {} UTF-16 code units",
            attachment.length, npm_length
        ));
    }

    let bundle: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("npm provenance attachment is not valid JSON: {error}"))?;
    let media_type = bundle
        .get("mediaType")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.starts_with("application/vnd.dev.sigstore.bundle."))
        .ok_or_else(|| {
            "npm provenance attachment has no supported Sigstore mediaType".to_string()
        })?;
    if attachment.content_type.as_deref() != Some(media_type) {
        return Err(
            "npm provenance attachment content_type does not match its Sigstore mediaType".into(),
        );
    }
    if !bundle
        .get("verificationMaterial")
        .is_some_and(serde_json::Value::is_object)
    {
        return Err("npm provenance Sigstore bundle is missing verificationMaterial".into());
    }

    let envelope = bundle
        .get("dsseEnvelope")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "npm provenance Sigstore bundle is missing dsseEnvelope".to_string())?;
    if envelope
        .get("payloadType")
        .and_then(serde_json::Value::as_str)
        != Some("application/vnd.in-toto+json")
    {
        return Err("npm provenance DSSE envelope has an unsupported payloadType".into());
    }
    let signatures = envelope
        .get("signatures")
        .and_then(serde_json::Value::as_array)
        .filter(|signatures| {
            !signatures.is_empty()
                && signatures.iter().all(|signature| {
                    signature
                        .get("sig")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|sig| !sig.is_empty())
                })
        })
        .ok_or_else(|| "npm provenance DSSE envelope has no signature".to_string())?;
    debug_assert!(!signatures.is_empty());

    let encoded_payload = envelope
        .get("payload")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "npm provenance DSSE envelope is missing its payload".to_string())?;
    let payload = base64::engine::general_purpose::STANDARD
        .decode(encoded_payload)
        .map_err(|error| format!("npm provenance DSSE payload is not valid base64: {error}"))?;
    let statement: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|error| format!("npm provenance DSSE payload is not valid JSON: {error}"))?;
    if statement.get("_type").and_then(serde_json::Value::as_str)
        != Some("https://in-toto.io/Statement/v1")
    {
        return Err("npm provenance payload is not an in-toto Statement v1".into());
    }
    let predicate_type = statement
        .get("predicateType")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "npm provenance statement has no predicateType".to_string())?
        .to_string();
    let subjects = statement
        .get("subject")
        .and_then(serde_json::Value::as_array)
        .filter(|subjects| subjects.len() == 1)
        .ok_or_else(|| "npm provenance statement must contain exactly one subject".to_string())?;
    let subject_name = subjects[0]
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "npm provenance subject has no name".to_string())?
        .to_string();
    let subject_sha512 = subjects[0]
        .pointer("/digest/sha512")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() == 128 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "npm provenance subject has no valid SHA-512 digest".to_string())?
        .to_ascii_lowercase();

    Ok(InspectedNpmProvenance {
        bundle,
        bytes,
        predicate_type,
        subject_name,
        subject_sha512,
    })
}

/// Turn npm's JSON envelope into the one tarball the package service owns.
///
/// Current npm builds the attachment key as exactly
/// `<manifest.name>-<manifest.version>.tgz`; retaining that spelling is also
/// what makes a second publish of the same version hit the package-file unique
/// constraint instead of being mistaken for an additional Maven-style file.
fn plan_npm_publish_packument(
    path_name: &str,
    packument: NpmPublishPackument,
    artifact_limit: usize,
) -> Result<PlannedNpmPublish, String> {
    if packument.id != path_name || packument.name != path_name {
        return Err(format!(
            "npm package name mismatch: URL names '{path_name}', packument names '{}'",
            packument.name
        ));
    }

    // "exactly one" is asserted by the pattern that takes it: a `len() != 1`
    // check followed by `.expect()` on `next()` were two statements agreeing
    // about a client-supplied packument, and only the first one was load
    // bearing for the process staying alive.
    let mut versions = packument.versions.into_iter();
    let (Some((version, manifest)), None) = (versions.next(), versions.next()) else {
        return Err("npm publish packument must contain exactly one version".into());
    };
    if manifest.name != path_name || manifest.version != version {
        return Err(format!(
            "npm version coordinates do not match URL and versions key '{version}'"
        ));
    }

    let Some((dist_tag, tagged_version)) = packument.dist_tags.iter().next() else {
        return Err("npm publish packument must contain exactly one dist-tag".into());
    };
    if packument.dist_tags.len() != 1 || tagged_version != &version {
        return Err("npm publish dist-tag must name the version being published".into());
    }
    rg_core::package_registry::service::validate_npm_dist_tag(dist_tag)
        .map_err(|error| format!("{error:#}"))?;
    let dist_tag = dist_tag.clone();

    let filename = format!("{path_name}-{version}.tgz");
    let provenance_filename = format!("{path_name}-{version}.sigstore");
    let mut attachments = packument.attachments;
    let tarball = attachments
        .remove(&filename)
        .ok_or_else(|| format!("npm publish packument is missing attachment '{filename}'"))?;
    if tarball.length > artifact_limit {
        return Err(format!(
            "npm tarball attachment exceeds the configured {artifact_limit}-byte artifact limit"
        ));
    }
    let provenance = match attachments.remove(&provenance_filename) {
        Some(attachment) => {
            if attachment.length > MAX_NPM_PROVENANCE_BYTES {
                return Err(format!(
                    "npm provenance attachment exceeds the configured {MAX_NPM_PROVENANCE_BYTES}-byte metadata limit"
                ));
            }
            Some((provenance_filename, attachment))
        }
        None => None,
    };
    if let Some(unexpected) = attachments.keys().next() {
        return Err(format!(
            "npm publish packument contains unsupported attachment '{unexpected}'"
        ));
    }

    Ok(PlannedNpmPublish {
        filename,
        version,
        dist_tag,
        tarball,
        provenance,
    })
}

fn finish_npm_publish_packument(
    path_name: &str,
    plan: PlannedNpmPublish,
    tarball: PackageArtifact,
    tarball_sha512: String,
    provenance_bytes: Option<Vec<u8>>,
) -> Result<DecodedNpmPublish, String> {
    let provenance = match (plan.provenance, provenance_bytes) {
        (Some((filename, attachment)), Some(bytes)) => {
            let inspected = inspect_npm_provenance_bytes(bytes, &attachment)?;
            let expected_name = npm_package_purl(path_name, &plan.version);
            if inspected.subject_name != expected_name {
                return Err(format!(
                    "npm provenance subject names '{}', expected '{expected_name}'",
                    inspected.subject_name
                ));
            }
            if inspected.subject_sha512 != tarball_sha512 {
                return Err(
                    "npm provenance subject SHA-512 does not match the tarball attachment".into(),
                );
            }
            Some(DecodedNpmProvenance {
                filename,
                bundle: inspected.bytes,
                predicate_type: inspected.predicate_type,
            })
        }
        (None, None) => None,
        _ => {
            return Err("npm publish packument attachment data does not match its metadata".into())
        }
    };

    Ok(DecodedNpmPublish {
        filename: plan.filename,
        version: plan.version,
        dist_tag: plan.dist_tag,
        tarball,
        provenance,
    })
}

#[cfg(test)]
fn decode_npm_publish_packument(
    path_name: &str,
    packument: NpmPublishPackument,
    artifact_limit: usize,
) -> Result<DecodedNpmPublish, String> {
    let plan = plan_npm_publish_packument(path_name, packument, artifact_limit)?;
    let tarball = base64::engine::general_purpose::STANDARD
        .decode(&plan.tarball.data)
        .map_err(|error| format!("npm tarball attachment is not valid base64: {error}"))?;
    if tarball.len() != plan.tarball.length {
        return Err(format!(
            "npm tarball attachment length mismatch: declared {}, decoded {}",
            plan.tarball.length,
            tarball.len()
        ));
    }
    let tarball_sha512 = hex::encode(Sha512::digest(&tarball));
    let provenance_bytes = plan
        .provenance
        .as_ref()
        .map(|(_, attachment)| attachment.data.as_bytes().to_vec());
    finish_npm_publish_packument(
        path_name,
        plan,
        PackageArtifact::from_bytes(tarball),
        tarball_sha512,
        provenance_bytes,
    )
}

fn decode_staged_npm_publish_packument(
    staged: StagedNpmPackument,
    path_name: &str,
    artifact_limit: usize,
    repo_root: &FsPath,
) -> Result<DecodedNpmPublish, NpmPackumentError> {
    let StagedNpmPackument {
        body,
        packument,
        mut data_ranges,
    } = staged;
    let plan = plan_npm_publish_packument(path_name, packument, artifact_limit)
        .map_err(NpmPackumentError::Invalid)?;
    let tarball_range = data_ranges.remove(&plan.filename).ok_or_else(|| {
        NpmPackumentError::Invalid(format!(
            "npm tarball attachment '{}' is missing its data string",
            plan.filename
        ))
    })?;
    let decoded = decode_npm_tarball_range(
        &body,
        tarball_range,
        &plan.filename,
        artifact_limit,
        repo_root,
    )?;
    if decoded.artifact.len() != plan.tarball.length as u64 {
        return Err(NpmPackumentError::Invalid(format!(
            "npm tarball attachment length mismatch: declared {}, decoded {}",
            plan.tarball.length,
            decoded.artifact.len()
        )));
    }

    let provenance_bytes = match plan.provenance.as_ref() {
        Some((filename, _)) => {
            let range = data_ranges.remove(filename).ok_or_else(|| {
                NpmPackumentError::Invalid(format!(
                    "npm provenance attachment '{filename}' is missing its data string"
                ))
            })?;
            Some(read_attachment_string(
                &body,
                range,
                filename,
                MAX_NPM_PROVENANCE_BYTES,
            )?)
        }
        None => None,
    };
    if let Some(filename) = data_ranges.keys().next() {
        return Err(NpmPackumentError::Invalid(format!(
            "npm publish packument contains unsupported attachment data '{filename}'"
        )));
    }

    finish_npm_publish_packument(
        path_name,
        plan,
        decoded.artifact,
        decoded.sha512,
        provenance_bytes,
    )
    .map_err(NpmPackumentError::Invalid)
}

fn npm_packument_error_response(error: NpmPackumentError) -> axum::response::Response {
    match error {
        NpmPackumentError::Invalid(message) => err(StatusCode::BAD_REQUEST, &message),
        NpmPackumentError::TooLarge(message) => {
            AppError::payload_too_large(message).into_response()
        }
        NpmPackumentError::Internal(message) => AppError::internal(message).into_response(),
    }
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
        decode_npm_publish_packument(NAME, serde_json::from_value(document).unwrap(), usize::MAX)
    }

    #[test]
    fn staged_packument_decodes_attachment_to_a_spool_and_honours_json_escapes() {
        let root = tempfile::tempdir().unwrap();
        let staging = rg_core::staging::StagingArea::PackageUploads.path_in(root.path());
        std::fs::create_dir_all(&staging).unwrap();
        let document = format!(
            r#"{{
                "_id":"{NAME}",
                "name":"{NAME}",
                "dist-tags":{{"latest":"{VERSION}"}},
                "versions":{{"{VERSION}":{{"name":"{NAME}","version":"{VERSION}"}}}},
                "_attachments":{{"{FILENAME}":{{
                    "data":"dGFy\u0059mFsbA==",
                    "length":7
                }}}}
            }}"#
        );
        let mut envelope = tempfile::NamedTempFile::new_in(&staging).unwrap();
        envelope.write_all(document.as_bytes()).unwrap();
        envelope.flush().unwrap();
        let body = PackageArtifact::spooled(
            envelope.into_temp_path(),
            document.len().try_into().unwrap(),
        );

        let staged = parse_staged_npm_packument(body).unwrap();
        assert_eq!(staged.packument.attachments[FILENAME].data, "");
        let decoded =
            decode_staged_npm_publish_packument(staged, NAME, usize::MAX, root.path()).unwrap();

        assert!(
            decoded.tarball.spool_path().is_some(),
            "the decoded tarball must remain file-backed"
        );
        assert_eq!(decoded.tarball.to_bytes().unwrap(), b"tarball");
    }

    #[test]
    fn duplicate_attachment_keys_are_refused_before_serde_can_overwrite_one() {
        let document = format!(
            r#"{{
                "_id":"{NAME}","name":"{NAME}",
                "dist-tags":{{"latest":"{VERSION}"}},
                "versions":{{"{VERSION}":{{"name":"{NAME}","version":"{VERSION}"}}}},
                "_attachments":{{
                    "{FILENAME}":{{"data":"dGFyYmFsbA==","length":7}},
                    "{FILENAME}":{{"data":"b3RoZXI=","length":5}}
                }}
            }}"#
        );
        let error = NpmPackumentScanner::new(document.as_bytes())
            .scan()
            .unwrap_err();
        assert!(
            error.to_string().contains("duplicate attachment"),
            "{error}"
        );
    }

    /// card_9903905d92a3: "exactly one version" used to be a `len() != 1` check
    /// followed by `.expect("one version checked above")` on the iterator, two
    /// statements agreeing about a document the publishing client wrote.
    #[test]
    fn a_packument_that_is_not_exactly_one_version_is_refused() {
        for (case, versions) in [
            ("no versions at all", serde_json::json!({})),
            (
                "two versions in one publish",
                serde_json::json!({
                    VERSION: { "name": NAME, "version": VERSION },
                    "9.9.9": { "name": NAME, "version": "9.9.9" }
                }),
            ),
        ] {
            let mut document = document();
            document["versions"] = versions;
            let error = decode(document).expect_err(case);
            assert_eq!(
                error, "npm publish packument must contain exactly one version",
                "{case} must keep the typed refusal"
            );
        }
    }

    fn provenance_attachment(subject_name: &str, subject_sha512: &str) -> serde_json::Value {
        let statement = serde_json::json!({
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [{
                "name": subject_name,
                "digest": { "sha512": subject_sha512 }
            }],
            "predicateType": "https://slsa.dev/provenance/v1",
            "predicate": {}
        });
        let bundle = serde_json::json!({
            "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
            "verificationMaterial": {},
            "dsseEnvelope": {
                "payloadType": "application/vnd.in-toto+json",
                "payload": base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_vec(&statement).unwrap()),
                "signatures": [{ "sig": "structurally-present-signature" }]
            }
        });
        let data = bundle.to_string();
        let length = data.len();
        serde_json::json!({
            "data": data,
            "length": length,
            "content_type": "application/vnd.dev.sigstore.bundle.v0.3+json"
        })
    }

    fn add_matching_provenance(document: &mut serde_json::Value) {
        let sha512 = hex::encode(Sha512::digest(b"tarball"));
        document["_attachments"]["@scope/matrix-1.2.3.sigstore"] =
            provenance_attachment("pkg:npm/%40scope/matrix@1.2.3", &sha512);
    }

    #[test]
    fn scoped_packument_keeps_the_client_attachment_name_and_bytes() {
        let decoded = decode(document()).unwrap();
        assert_eq!(decoded.filename, FILENAME);
        assert_eq!(decoded.version, VERSION);
        assert_eq!(decoded.dist_tag, "latest");
        assert_eq!(decoded.tarball.to_bytes().unwrap(), b"tarball");
        assert!(decoded.provenance.is_none());
    }

    #[test]
    fn matching_sigstore_attachment_is_kept_with_its_predicate_type() {
        let mut value = document();
        add_matching_provenance(&mut value);

        let decoded = decode(value).unwrap();
        let provenance = decoded.provenance.expect("provenance was discarded");
        assert_eq!(provenance.filename, "@scope/matrix-1.2.3.sigstore");
        assert_eq!(provenance.predicate_type, "https://slsa.dev/provenance/v1");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&provenance.bundle).unwrap()["mediaType"],
            "application/vnd.dev.sigstore.bundle.v0.3+json"
        );
    }

    #[test]
    fn provenance_length_uses_the_javascript_utf16_contract() {
        let mut value = document();
        let sha512 = hex::encode(Sha512::digest(b"tarball"));
        let mut attachment = provenance_attachment("pkg:npm/%40scope/matrix@1.2.3", &sha512);
        let mut bundle: serde_json::Value =
            serde_json::from_str(attachment["data"].as_str().unwrap()).unwrap();
        bundle["verificationMaterial"]["certificateIdentity"] = serde_json::json!("München");
        let data = bundle.to_string();
        let npm_length = data.encode_utf16().count();
        assert!(
            data.len() > npm_length,
            "fixture must distinguish UTF-8 from JS length"
        );
        attachment["data"] = serde_json::json!(data);
        attachment["length"] = serde_json::json!(npm_length);
        value["_attachments"]["@scope/matrix-1.2.3.sigstore"] = attachment;

        assert!(decode(value).unwrap().provenance.is_some());
    }

    #[test]
    fn a_named_dist_tag_is_kept_as_part_of_the_publish() {
        let mut custom_tag = document();
        custom_tag["dist-tags"] = serde_json::json!({ "beta": VERSION });
        let decoded = decode(custom_tag).unwrap();
        assert_eq!(decoded.dist_tag, "beta");
        assert_eq!(decoded.version, VERSION);
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

        let mut wrong_tag_target = document();
        wrong_tag_target["dist-tags"] = serde_json::json!({ "beta": "9.9.9" });
        assert!(decode(wrong_tag_target)
            .unwrap_err()
            .contains("must name the version being published"));

        let mut multiple_tags = document();
        multiple_tags["dist-tags"] = serde_json::json!({ "latest": VERSION, "beta": VERSION });
        assert!(decode(multiple_tags)
            .unwrap_err()
            .contains("must name the version being published"));

        let mut unexpected = document();
        unexpected["_attachments"]["README.txt"] = serde_json::json!({
            "data": "readme",
            "length": 6
        });
        assert!(decode(unexpected)
            .unwrap_err()
            .contains("unsupported attachment"));
    }

    #[test]
    fn malformed_or_foreign_provenance_fails_closed() {
        let matching_sha512 = hex::encode(Sha512::digest(b"tarball"));

        let mut foreign_name = document();
        foreign_name["_attachments"]["@scope/matrix-1.2.3.sigstore"] =
            provenance_attachment("pkg:npm/other@1.2.3", &matching_sha512);
        assert!(decode(foreign_name).unwrap_err().contains("subject names"));

        let mut foreign_tarball = document();
        foreign_tarball["_attachments"]["@scope/matrix-1.2.3.sigstore"] = provenance_attachment(
            "pkg:npm/%40scope/matrix@1.2.3",
            &hex::encode(Sha512::digest(b"other tarball")),
        );
        assert!(decode(foreign_tarball)
            .unwrap_err()
            .contains("SHA-512 does not match"));

        let mut malformed = document();
        malformed["_attachments"]["@scope/matrix-1.2.3.sigstore"] = serde_json::json!({
            "data": "not json",
            "length": 8,
            "content_type": "application/vnd.dev.sigstore.bundle.v0.3+json"
        });
        assert!(decode(malformed).unwrap_err().contains("not valid JSON"));
    }

    #[test]
    fn declared_tarball_size_is_rejected_before_base64_decode() {
        let mut value = document();
        value["_attachments"][FILENAME]["length"] = serde_json::json!(8);
        value["_attachments"][FILENAME]["data"] = serde_json::json!("not base64");
        let error = decode_npm_publish_packument(NAME, serde_json::from_value(value).unwrap(), 7)
            .unwrap_err();
        assert!(error.contains("7-byte artifact limit"), "{error}");
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

#[derive(Default)]
struct TwineUpload {
    action: Option<String>,
    protocol_version: Option<String>,
    name: Option<String>,
    version: Option<String>,
    sha256_digest: Option<String>,
    filename: Option<String>,
    content: Option<PackageArtifact>,
    /// The PEP 740 `attestations` field, verbatim: a JSON array of attestation
    /// objects describing the file in the same form. `None` means the publisher
    /// sent none, which is a different answer from "sent some and we lost them".
    attestations: Option<String>,
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

/// How much of a Twine text field is read.
///
/// The coordinates and the digest are tens of bytes; `attestations` is a PEP
/// 740 array of sigstore bundles, which is tens of kilobytes. None of them is
/// the artifact, and none of them is spooled — so unlike the `content` field
/// they are read into memory, and the bound is what stops that being a way to
/// spend the multipart envelope's whole allowance on a form field.
const MAX_TWINE_TEXT_FIELD_BYTES: usize = 1024 * 1024;

async fn decode_twine_text_field(
    mut field: axum::extract::multipart::Field<'_>,
    field_name: &str,
) -> Result<String, AppError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|error| {
        package_multipart_error(error, &format!("cannot read Twine `{field_name}` field"))
    })? {
        if bytes.len() + chunk.len() > MAX_TWINE_TEXT_FIELD_BYTES {
            return Err(AppError::payload_too_large(format!(
                "Twine `{field_name}` field exceeds the {MAX_TWINE_TEXT_FIELD_BYTES}-byte limit \
                 a form field is read under"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|error| {
        AppError::bad_request(format!(
            "Twine `{field_name}` field is not valid UTF-8: {error}"
        ))
    })
}

/// Decode the multipart form emitted by `twine upload`.
///
/// Twine sends many descriptive metadata fields as well; the package adapter
/// reads the authoritative metadata from the wheel/sdist itself, so this
/// boundary only consumes the protocol controls, coordinates, digest and file.
///
/// `attestations` is the exception to that rule: it is supply-chain evidence
/// the publisher deliberately attached and that no other part of the request
/// carries, so dropping it into the ignored-field arm loses it for good while
/// the upload still answers 200 (`card_b25bd1cbc60c`).
///
/// A detached `gpg_signature` is evidence for the same reason, but ForgeKeep
/// does not currently verify, store, or serve GPG sidecars. Reject it before
/// publication instead of claiming that a `twine upload --sign` succeeded.
async fn decode_twine_upload(
    mut multipart: Multipart,
    repo_root: &FsPath,
    max_bytes: usize,
) -> Result<TwineUpload, AppError> {
    let mut upload = TwineUpload::default();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| package_multipart_error(error, "invalid Twine multipart body"))?
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
                let content = stage_multipart_artifact(
                    field,
                    repo_root,
                    max_bytes,
                    "cannot read Twine `content` field",
                )
                .await?;
                set_twine_field(&mut upload.filename, "content", filename)?;
                set_twine_field(&mut upload.content, "content", content)?;
            }
            "gpg_signature" => {
                return Err(AppError::bad_request(
                    "Twine GPG signatures are not supported; upload again without `--sign`",
                ));
            }
            ":action" | "protocol_version" | "name" | "version" | "sha256_digest"
            | "attestations" => {
                let value = decode_twine_text_field(field, &field_name).await?;
                let slot = match field_name.as_str() {
                    ":action" => &mut upload.action,
                    "protocol_version" => &mut upload.protocol_version,
                    "name" => &mut upload.name,
                    "version" => &mut upload.version,
                    "sha256_digest" => &mut upload.sha256_digest,
                    "attestations" => &mut upload.attestations,
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

async fn decode_nuget_push(
    mut multipart: Multipart,
    repo_root: &FsPath,
    max_bytes: usize,
) -> Result<PackageArtifact, AppError> {
    let mut package = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| package_multipart_error(error, "invalid NuGet multipart body"))?
    {
        if field.name() != Some("package") {
            continue;
        }
        if package.is_some() {
            return Err(AppError::bad_request(
                "NuGet upload repeats the `package` field",
            ));
        }
        package = Some(
            stage_multipart_artifact(
                field,
                repo_root,
                max_bytes,
                "cannot read NuGet `package` field",
            )
            .await?,
        );
    }

    package.ok_or_else(|| AppError::bad_request("NuGet upload is missing `package`"))
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

    const BOUNDARY: &str = "forgekeep-package-boundary";

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
        let body =
            match decode_nuget_push(multipart, &state.repo_root, state.package_upload_max_bytes)
                .await
            {
                Ok(body) => body,
                Err(error) => return error.into_response(),
            };
        // NuGet.Client deliberately sends a random filename; using the same
        // stable spelling also makes a repeated file hit the uniqueness guard.
        ("package.nupkg".to_string(), body)
    } else {
        let filename = filename_from_disposition(&headers);
        let body = match stage_request_artifact(
            &state,
            request.into_body(),
            state.package_upload_max_bytes,
        )
        .await
        {
            Ok(body) => body,
            Err(response) => return response,
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
                        [
                            (header::CONTENT_TYPE, content_type),
                            (header::CONTENT_LENGTH, size.to_string()),
                        ],
                        crate::http_stream::file_body_with_idle(file, state.git_idle_timeout_secs),
                    )
                        .into_response()
                }
                rg_core::package_registry::storage::PackageFileSource::Buffered(data) => {
                    let len = data.len();
                    (
                        StatusCode::OK,
                        [
                            (header::CONTENT_TYPE, content_type),
                            (header::CONTENT_LENGTH, len.to_string()),
                        ],
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
            // refuses, and this array turns that into a `500`. The package
            // published fine and then could never be downloaded.
            response.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                crate::content_disposition::attachment(filename),
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

    let dist_tags = match rg_core::package_registry::service::list_npm_dist_tags(
        &state.db, &owner, &name, &pkg_name,
    )
    .await
    {
        Ok(tags) => tags,
        Err(e) => return package_error_response(e),
    };

    // Same as the cargo index above: an unreadable row must not be served as a
    // packument saying the version depends on nothing.
    let metadata = match rg_core::package_registry::build_npm_metadata_with_dist_tags(
        &pkg_name,
        &npm_versions,
        &dist_tags,
        &base_url,
        &owner,
        &name,
    ) {
        Ok(metadata) => metadata,
        Err(e) => return package_error_response(e),
    };

    (StatusCode::OK, Json(metadata)).into_response()
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
    RepoWrite { .. }: RepoWrite,
    Path((owner, repo, pkg_name, tag)): Path<(String, String, String, String)>,
    Json(version): Json<String>,
) -> axum::response::Response {
    match rg_core::package_registry::service::set_npm_dist_tag(
        &state.db, &owner, &repo, &pkg_name, &tag, &version,
    )
    .await
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({}))).into_response(),
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
    RepoWrite { .. }: RepoWrite,
    Path((owner, repo, pkg_name, tag)): Path<(String, String, String, String)>,
) -> axum::response::Response {
    match rg_core::package_registry::service::remove_npm_dist_tag(
        &state.db, &owner, &repo, &pkg_name, &tag,
    )
    .await
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({}))).into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
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

            // PEP 740's `data-provenance` may only name a URL this server
            // answers, so it is derived from the stored file list rather than
            // from a naming convention: the attribute appears exactly when the
            // publish that wrote the distribution also wrote its provenance.
            let stored: std::collections::HashSet<&str> =
                v.files.iter().map(|f| f.filename.as_str()).collect();

            let files: Vec<rg_core::package_registry::PyPIVersionEntry> = v
                .files
                .iter()
                .filter(|f| is_pypi_distribution(&f.filename))
                .map(|f| {
                    let provenance =
                        rg_core::package_registry::pypi_provenance_filename(&f.filename);
                    rg_core::package_registry::PyPIVersionEntry {
                        version: v.version.clone(),
                        filename: f.filename.clone(),
                        sha256: v.sha256_of(f),
                        download_url: link_to(&v.version, &f.filename),
                        requires_python: requires_python.clone(),
                        yanked,
                        provenance_url: stored
                            .contains(provenance.as_str())
                            .then(|| link_to(&v.version, &provenance)),
                    }
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
                // This entry names a file the version does not actually hold,
                // so there is nothing whose provenance could be advertised.
                provenance_url: None,
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
        return match hash_package_file(
            &state,
            &request.owner,
            &request.repo,
            &pkg_name,
            version,
            target,
            algorithm,
        )
        .await
        {
            Ok(digest) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                digest,
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
///
/// The two blocks are separated *without* the envelope ever being a `Vec`: the
/// metadata is JSON small enough to bound outright, and the `.crate` is copied
/// straight from the envelope's spool into one of its own, so the rest of the
/// publish path treats it exactly like a body upload. Slicing them out of a
/// collected buffer instead would put both the envelope and the archive in
/// heap, which is what the ingress spool exists to avoid.
async fn split_cargo_publish_frame(
    envelope: &PackageArtifact,
    repo_root: &FsPath,
    max_archive_bytes: usize,
) -> Result<(Vec<u8>, PackageArtifact), String> {
    use tokio::io::{AsyncReadExt as _, BufReader};

    let file = match envelope.spool_path() {
        Some(path) => tokio::fs::File::open(path)
            .await
            .map_err(|error| format!("cannot read the staged cargo publish body: {error}"))?,
        None => return Err("the cargo publish body was not staged".to_string()),
    };
    let mut reader = BufReader::with_capacity(64 * 1024, file);

    async fn read_len(
        reader: &mut (impl tokio::io::AsyncRead + Unpin),
        what: &str,
    ) -> Result<usize, String> {
        let mut prefix = [0_u8; 4];
        reader
            .read_exact(&mut prefix)
            .await
            .map_err(|_| format!("body ends before the {what} length prefix"))?;
        Ok(u32::from_le_bytes(prefix) as usize)
    }

    let metadata_len = read_len(&mut reader, "metadata").await?;
    if metadata_len > MAX_CARGO_PUBLISH_METADATA_BYTES {
        return Err(format!(
            "the metadata length prefix claims {metadata_len} bytes, more than the \
             {MAX_CARGO_PUBLISH_METADATA_BYTES}-byte limit cargo publish metadata is read under"
        ));
    }
    let mut metadata = vec![0_u8; metadata_len];
    reader.read_exact(&mut metadata).await.map_err(|_| {
        format!("the metadata length prefix claims {metadata_len} bytes, the body has fewer")
    })?;

    let archive_len = read_len(&mut reader, "crate").await?;
    if archive_len > max_archive_bytes {
        return Err(format!(
            "the crate length prefix claims {archive_len} bytes, more than the configured \
             {max_archive_bytes}-byte artifact limit"
        ));
    }
    let mut spool = UploadSpool::create(repo_root, archive_len)
        .await
        .map_err(|error| format!("cannot stage the uploaded .crate: {error}"))?;
    let mut remaining = archive_len;
    let mut buffer = vec![0_u8; 128 * 1024];
    while remaining > 0 {
        let want = remaining.min(buffer.len());
        let read = reader
            .read(&mut buffer[..want])
            .await
            .map_err(|error| format!("cannot read the staged cargo publish body: {error}"))?;
        if read == 0 {
            return Err(format!(
                "the crate length prefix claims {archive_len} bytes, the body has fewer"
            ));
        }
        spool
            .write(&buffer[..read])
            .await
            .map_err(|error| format!("cannot stage the uploaded .crate: {error}"))?;
        remaining -= read;
    }

    let trailing = reader
        .read(&mut buffer[..1])
        .await
        .map_err(|error| format!("cannot read the staged cargo publish body: {error}"))?;
    if trailing != 0 {
        let counted = envelope.len() as usize - (8 + metadata_len + archive_len);
        return Err(format!("{counted} trailing byte(s) after the crate"));
    }

    let archive = spool
        .finish()
        .await
        .map_err(|error| format!("cannot stage the uploaded .crate: {error}"))?;
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
    let (metadata, archive) =
        match split_cargo_publish_frame(&body, &state.repo_root, state.package_upload_max_bytes)
            .await
        {
            Ok(split) => split,
            Err(reason) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    &format!("malformed cargo publish body: {reason}"),
                )
            }
        };
    // The envelope's spool has served its purpose; retire it before the publish
    // rather than holding two copies of the upload on disk through it.
    drop(body);

    let metadata: serde_json::Value = match serde_json::from_slice(&metadata) {
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

    // card_c10c3f4d28e5: the envelope is a claim, `Cargo.toml` inside the
    // `.crate` is the artifact's own identity — the same reasoning
    // `npm_publish` states, and cargo used to take the generic
    // query-parameter override that comment warns about instead. The override
    // exists for multi-artifact formats: Maven publishes a version as a `.pom`
    // plus classifier `.jar`s that carry no manifest of their own. A `.crate`
    // always has exactly one manifest, so there is nothing here for it to
    // stand in for, and letting it win published a version row, a stored
    // `{name}-{version}.crate` and a sparse-index entry — deps, features,
    // `links`, `rust_version` — describing a crate the archive is not. The
    // index looks self-consistent, `cargo` verifies only the `cksum`, and the
    // failure lands at unpacking, where the tarball unfolds into the real
    // crate's directory.
    let cargo_adapter = rg_core::package_registry::get_adapter("cargo")
        .expect("cargo is a built-in package adapter");
    let filename = format!("{crate_name}-{version}.crate");
    let (archive, inspection) = match archive
        .inspect_with_adapter(cargo_adapter, filename.clone())
        .await
    {
        Ok(result) => result,
        Err(error) => {
            return AppError::internal(format!(
                "cargo package inspection task did not complete: {error}"
            ))
            .into_response()
        }
    };
    let metadata = match inspection {
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("invalid package payload: {error:#}"),
            )
        }
        Ok(Ok(metadata)) => metadata,
        Ok(Err(error)) => return err(StatusCode::BAD_REQUEST, &format!("{error:#}")),
    };
    if metadata.name != crate_name || metadata.version != version {
        return err(
            StatusCode::BAD_REQUEST,
            &format!(
                "cargo publish metadata names `{crate_name} {version}`, but the uploaded \
                 .crate carries `{} {}`",
                metadata.name, metadata.version
            ),
        );
    }

    // `publish_package` is bypassed because the identity check above already
    // ran the adapter, but its artifact limit is not: the frame was collected
    // under the larger envelope budget, so the `.crate` inside it still has to
    // be measured against the artifact one.
    if archive.len() > state.package_upload_max_bytes as u64 {
        return AppError::payload_too_large(format!(
            "package artifact exceeds the configured {}-byte limit",
            state.package_upload_max_bytes
        ))
        .into_response();
    }

    let query = PublishPackageQuery {
        name: Some(crate_name.to_string()),
        version: Some(version.to_string()),
        description: None,
        homepage: None,
        repository_url: None,
        semver: None,
    };

    let published = persist_package(
        state,
        user_id,
        owner,
        name,
        "cargo".to_string(),
        query,
        vec![(filename, archive)],
        Some(metadata),
        None,
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
    State(state): State<AppState>,
    _write: RepoWrite,
    _params: axum::extract::RawPathParams,
    body: Body,
) -> axum::response::Response {
    if let Err(response) =
        stage_request_artifact(&state, body, state.package_upload_max_bytes).await
    {
        return response;
    }
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
    body: Body,
) -> axum::response::Response {
    let body = match stage_request_artifact(&state, body, state.package_upload_max_bytes).await {
        Ok(body) => body,
        Err(response) => return response,
    };
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
        let expected = match hash_package_file(
            &state,
            &request.owner,
            &request.repo,
            &pkg_name,
            version,
            target,
            algorithm,
        )
        .await
        {
            Ok(digest) => digest,
            Err(response) => return response,
        };
        // A checksum sidecar is one hex digest and at most a filename after it.
        // Bounded rather than collected: the route's ceiling is the artifact
        // one, and nothing says a client cannot PUT half a gigabyte named
        // `x.jar.sha1`.
        let claimed = match body.head(256) {
            Ok(head) => head,
            Err(error) => {
                return AppError::internal(format!(
                    "cannot read the staged checksum upload: {error}"
                ))
                .into_response()
            }
        };
        let claimed = String::from_utf8_lossy(&claimed);
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
    let registration_url = nuget_registration_url(&base_url, &owner, &name, &pkg_name);
    let entries = versions
        .iter()
        .map(|version| nuget_registration_entry(&base_url, &owner, &name, &pkg_name, version))
        .collect::<anyhow::Result<Vec<_>>>();
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) => return package_error_response(error),
    };

    let json =
        rg_core::package_registry::build_registration_index(&pkg_name, &registration_url, &entries);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

/// GET/HEAD /api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/{version}
///
/// A registration index advertises this document through every inline leaf's
/// `@id`. The URL uses NuGet's normalized version spelling, while historical
/// rows are resolved with the same equality rules as the flat container.
pub async fn nuget_registration_leaf(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_name, version)): Path<(String, String, String, String)>,
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
        .find(|stored| rg_core::package_registry::nuget_versions_match(&stored.version, &version))
    else {
        return err_text(
            StatusCode::NOT_FOUND,
            &format!("package '{pkg_name}' has no version '{version}'"),
        );
    };

    let base_url = build_base_url(&headers);
    let registration_url = nuget_registration_url(&base_url, &owner, &name, &pkg_name);
    let entry = match nuget_registration_entry(&base_url, &owner, &name, &pkg_name, found) {
        Ok(entry) => entry,
        Err(error) => return package_error_response(error),
    };
    let json =
        rg_core::package_registry::build_registration_leaf(&pkg_name, &registration_url, &entry);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

fn nuget_registration_url(base_url: &str, owner: &str, repo: &str, package_name: &str) -> String {
    format!(
        "{}/api/v1/repos/{}/{}/packages/nuget/registration/{}/index.json",
        base_url.trim_end_matches('/'),
        encode_path_segment(owner),
        encode_path_segment(repo),
        encode_path_segment(&package_name.to_lowercase()),
    )
}

fn nuget_registration_entry(
    base_url: &str,
    owner: &str,
    repo: &str,
    package_name: &str,
    version: &rg_core::package_registry::VersionDetail,
) -> anyhow::Result<rg_core::package_registry::NuGetRegistrationEntry> {
    let metadata =
        parse_nuget_metadata(version.metadata.as_deref(), package_name, &version.version)?;
    let filename = version
        .files
        .iter()
        .find(|file| file.filename.to_lowercase().ends_with(".nupkg"))
        .map(|file| file.filename.clone())
        .unwrap_or_else(|| format!("{}.{}.nupkg", package_name, version.version));
    let download_url = format!(
        "{}/api/v1/repos/{}/{}/packages/nuget/{}/{}/{}",
        base_url.trim_end_matches('/'),
        encode_path_segment(owner),
        encode_path_segment(repo),
        encode_path_segment(package_name),
        encode_path_segment(&version.version),
        encode_path_segment(&filename),
    );

    Ok(rg_core::package_registry::NuGetRegistrationEntry {
        version: version.version.clone(),
        description: metadata.description,
        homepage: metadata.homepage,
        license: metadata.license,
        tags: metadata.tags,
        download_url,
        dependency_groups: metadata.dependency_groups,
        // A yanked version stays in the registration so a consumer that
        // already resolved it keeps restoring; `listed` is what keeps it out
        // of a fresh resolution.
        listed: version.is_install_candidate(),
    })
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
        let name_lower = pkg.summary.name.to_lowercase();
        // Simple substring match
        if query.is_empty() || name_lower.contains(&query_lower) {
            let Some(version) = pkg.summary.latest_version.clone() else {
                continue;
            };
            let registration_url =
                nuget_registration_url(&base_url, &owner, &name, &pkg.summary.name);

            results.push(rg_core::package_registry::NuGetSearchResult {
                name: pkg.summary.name.clone(),
                version,
                versions: pkg.versions.clone(),
                description: pkg.summary.description.clone(),
                tags: pkg.summary.keywords.clone(),
                registration_url,
            });
        }
    }

    // `totalHits` counts the whole match set; `data` carries the one window the
    // client paged to. Both are cut from this same filtered sequence, which is
    // ordered by package name — a total order, so walking the pages visits every
    // hit exactly once.
    let total_hits = results.len();
    let page = rg_core::package_registry::nuget_page(results, params.skip, params.take);
    let json = rg_core::package_registry::build_search_results(&page, total_hits);

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

async fn nuget_search_packages(
    state: &AppState,
    owner: &str,
    repo: &str,
    include_prerelease: bool,
    semver_level: Option<&str>,
) -> Result<Vec<rg_core::package_registry::NuGetSearchPackage>, axum::response::Response> {
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

/// The `?id=` form of autocomplete: every version of one package, under the
/// same capability filter that builds `versions` in SearchQueryService.
///
/// An id nobody published answers an empty list rather than a 404 — this
/// endpoint feeds a picker while the user is still typing, so absence is a
/// normal state of the request, not a diagnosis.
async fn nuget_autocomplete_versions(
    state: &AppState,
    owner: &str,
    repo: &str,
    requested: &str,
    params: &NuGetSearchParams,
) -> Result<Vec<String>, axum::response::Response> {
    let resolved = resolve_nuget_id(state, owner, repo, requested).await?;
    let packages = nuget_search_packages(
        state,
        owner,
        repo,
        params.prerelease,
        params.semver_level.as_deref(),
    )
    .await?;

    Ok(packages
        .into_iter()
        .find(|pkg| pkg.summary.name == resolved)
        .map(|pkg| {
            pkg.versions
                .into_iter()
                .map(|version| version.version)
                .collect()
        })
        .unwrap_or_default())
}

/// GET /api/v1/repos/{owner}/{name}/packages/nuget/autocomplete?q=... | ?id=...
///
/// NuGet `SearchAutocompleteService/3.5.0`. The service index used to advertise
/// this resource at the bare `nuget/` root, which is not an endpoint of anything
/// (card_dba77cceec56).
///
/// One URL, two questions: `?q=` completes package ids, `?id=` completes the
/// versions of one package. Serving only the first answered a version picker
/// with the repository's package ids — a 200 of the right *shape*, which is
/// worse than a 404, because the ids land in the client's version dropdown
/// (card_11227138d44e).
pub async fn nuget_autocomplete(
    State(state): State<AppState>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<NuGetSearchParams>,
) -> axum::response::Response {
    // `id` wins over `q` when a client sends both: the version enumeration is
    // the more specific request, and the two answers are not interchangeable.
    let data = match params.id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => match nuget_autocomplete_versions(&state, &owner, &name, id, &params).await {
            Ok(versions) => versions,
            Err(response) => return response,
        },
        None => {
            let query = params.q.as_deref().unwrap_or("").to_lowercase();
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

            packages
                .into_iter()
                .filter(|pkg| query.is_empty() || pkg.summary.name.to_lowercase().contains(&query))
                .map(|pkg| pkg.summary.name)
                .collect()
        }
    };

    // One paging step for both request forms: whichever question was asked, the
    // count describes the whole answer and `data` carries the window of it the
    // client paged to. Ids arrive ordered by name and versions in version order,
    // so either sequence can be walked page by page without repeats.
    let total_hits = data.len();
    let page = rg_core::package_registry::nuget_page(data, params.skip, params.take);
    let json = rg_core::package_registry::build_autocomplete_results(&page, total_hits);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Json(json),
    )
        .into_response()
}

// ── RubyGems Protocol Endpoints ───────────────────────────

/// GET /api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/dependencies.json?gems={name}
///
/// RubyGems dependencies JSON API. The extensionless endpoint uses Ruby
/// Marshal; ForgeKeep leaves it unregistered rather than returning JSON bytes
/// that legacy Bundler will try to pass to `Marshal.load`.
pub async fn rubygems_dependencies_json(
    State(state): State<AppState>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
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
        // `rubygems_dependencies_json`, which resolves against the same rows.
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
    body: Body,
) -> axum::response::Response {
    let body = match stage_request_artifact(&state, body, state.package_upload_max_bytes).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let Some(adapter) = rg_core::package_registry::get_adapter("rubygems") else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the rubygems adapter is not registered",
        );
    };

    // Read the gemspec here rather than letting `publish_package` do it,
    // because the name and version are what the filename is built from and the
    // filename has to be settled before the upload is described at all.
    let (body, inspection) = match body
        .inspect_with_adapter(adapter, "package.gem".to_string())
        .await
    {
        Ok(result) => result,
        Err(error) => {
            return AppError::internal(format!(
                "RubyGems package inspection task did not complete: {error}"
            ))
            .into_response()
        }
    };
    let meta = match inspection {
        Err(error) => {
            return err(
                StatusCode::BAD_REQUEST,
                &format!("invalid package payload: {error:#}"),
            )
        }
        Ok(Ok(meta)) => meta,
        Ok(Err(error)) => return err(StatusCode::BAD_REQUEST, &format!("{error:#}")),
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

    let published = persist_package(
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
        vec![(filename, body)],
        Some(meta),
        None,
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

    // card_33f83c325515: `parse_stored_metadata_document` already refuses a
    // stored blob that is not a JSON object, but inside it every field used to
    // fall back to a default — so a `dependencies` that is not an array served
    // `index.yaml` with `200` and no subcharts, and a `deprecated` that is not
    // a boolean served the chart as current. `chart_protocol_metadata` writes
    // each of these with one shape and refuses anything it could not carry, so
    // a value of another shape here is this registry's own row having rotted,
    // not something a `Chart.yaml` could have said. Untyped on purpose: that
    // reaches the client as a 5xx, because the request was fine.
    let damaged = |key: &str, must_be: &str| {
        anyhow::anyhow!(
            "stored helm metadata for package '{package_name}' version '{version}' is \
             damaged: `{key}` is not {must_be}"
        )
    };
    // An absent key and an explicit null are the same claim — a chart that
    // declared none of this — and neither is damage.
    let declared = |key: &str| doc.get(key).filter(|value| !value.is_null());

    let string_list = |key: &str| -> anyhow::Result<Vec<String>> {
        let Some(value) = declared(key) else {
            return Ok(Vec::new());
        };
        let array = value.as_array().ok_or_else(|| damaged(key, "an array"))?;
        array
            .iter()
            .enumerate()
            .map(|(position, entry)| {
                entry
                    .as_str()
                    .map(String::from)
                    .ok_or_else(|| damaged(&format!("{key}[{position}]"), "a string"))
            })
            .collect()
    };

    let string_field = |key: &str| -> anyhow::Result<Option<String>> {
        declared(key)
            .map(|value| {
                value
                    .as_str()
                    .map(String::from)
                    .ok_or_else(|| damaged(key, "a string"))
            })
            .transpose()
    };

    Ok(HelmChartMetadata {
        app_version: string_field("appVersion")?,
        api_version: string_field("apiVersion")?,
        kube_version: string_field("kubeVersion")?,
        chart_type: string_field("type")?,
        deprecated: match declared("deprecated") {
            Some(value) => value
                .as_bool()
                .ok_or_else(|| damaged("deprecated", "a boolean"))?,
            None => false,
        },
        dependencies: match declared("dependencies") {
            Some(value) => value
                .as_array()
                .ok_or_else(|| damaged("dependencies", "an array"))?
                .clone(),
            None => Vec::new(),
        },
        keywords: string_list("keywords")?,
        sources: string_list("sources")?,
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
        // Read through rg-core, which also writes these keys and classifies the
        // same graph for the search SemVer-level filter. A second reader here
        // would be free to drift from both — including about what counts as a
        // damaged element, which is why the refusal comes from there too.
        dependency_groups: rg_core::package_registry::stored_dependency_groups(
            package_name,
            version,
            &doc,
        )?,
    })
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
///
/// card_69cfa8de4fd1: the adapter refuses to publish a runtime dependency the
/// stored shape cannot carry, so an element here without a usable `name` is
/// damage to the row rather than something a gemspec could have said. Skipping
/// it answers `200` with a dependency graph the gem never declared — and a
/// resolver cannot tell that apart from an honest answer, because a shorter
/// list still resolves. This is the same rule the whole document is already
/// read under (card_a4be92713930), one level down.
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
    let Some(declared) = doc.get("dependencies").filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let deps = declared.as_array().ok_or_else(|| {
        damaged_rubygems_metadata(package_name, version, "`dependencies` is not a list")
    })?;

    deps.iter()
        .enumerate()
        .map(|(index, dep)| {
            let name = dep
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| {
                    damaged_rubygems_metadata(
                        package_name,
                        version,
                        &format!("`dependencies[{index}].name` is not a non-empty string"),
                    )
                })?
                .to_string();
            // The adapter always writes a requirement string; absent is how a
            // version published before it did reads, and `>= 0` is what the
            // gemspec would have meant.
            let requirements = match dep.get("requirements").filter(|v| !v.is_null()) {
                Some(value) => value
                    .as_str()
                    .ok_or_else(|| {
                        damaged_rubygems_metadata(
                            package_name,
                            version,
                            &format!("`dependencies[{index}].requirements` is not a string"),
                        )
                    })?
                    .to_string(),
                None => ">= 0".to_string(),
            };
            Ok(rg_core::package_registry::RubyGemsDep { name, requirements })
        })
        .collect()
}

/// Stored gemspec metadata no publish could have produced. The message names
/// the coordinate and the offending element, never the stored value.
fn damaged_rubygems_metadata(package_name: &str, version: &str, detail: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "stored rubygems metadata for package '{package_name}' version '{version}' is damaged: {detail}"
    )
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
