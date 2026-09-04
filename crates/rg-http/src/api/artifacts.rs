//! REST API handlers for CI Artifacts.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path as FsPath, PathBuf};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::api::ci::pipeline_in_repo;
use crate::api::repo_access::{AnchoredRead, AnchoredWrite, RepoAnchor, RepoRead};
use crate::error::AppError;
use crate::AppState;
use utoipa::ToSchema;

/// Artifact uploads carry metadata only. The artifact bytes are staged by the
/// runner under this job's private storage directory and never cross the HTTP
/// request boundary.
pub(crate) const ARTIFACT_METADATA_MAX_BYTES: usize = 64 * 1024;

/// The declared ceiling for one staged artifact archive.
///
/// Not a second number that happens to agree: the staging route mounts the very
/// wrapper the CI cache upload does — `routes.rs` says so in as many words —
/// so the handler's own backstop is derived from that layer's value instead of
/// being a literal beside it. A body over it is refused with `413`, the status
/// the route has always declared.
const ARTIFACT_ARCHIVE_MAX_BYTES: usize = crate::api::runners::CACHE_ARCHIVE_MAX_BYTES;

// ── Access ─────────────────────────────────────────────

/// The artifact routes' anchor: `/artifacts/{id}` names its repository only
/// through the artifact itself.
///
/// The walk — artifact → job → stage → pipeline → repository — is CI knowledge
/// and lives here; the decision taken on the repository it arrives at is
/// [`AnchoredRead`] / [`AnchoredWrite`]'s, and therefore `api::repo_access`'s.
/// Before this existed the two were written together in this file, and the
/// write half re-derived the caller from `extract_user_id` while the route
/// table declared `RepoWrite` (card_1ec383429aea).
pub struct Artifact;

impl RepoAnchor for Artifact {
    type Row = rg_db::entities::artifact::Model;

    const PARAM: &'static str = "id";

    fn masked() -> AppError {
        AppError::not_found("artifact not found")
    }

    async fn resolve(
        state: &AppState,
        artifact_id: i64,
    ) -> Result<(Self::Row, rg_db::entities::repository::Model), AppError> {
        let artifact = rg_db::ops::artifact_ops::get_by_id(&state.db, artifact_id)
            .await
            .map_err(AppError::from)?
            .ok_or_else(Self::masked)?;
        let job = rg_db::ops::pipeline_ops::get_job(&state.db, artifact.job_id)
            .await
            .map_err(AppError::from)?
            .ok_or_else(|| broken_chain(artifact_id, "job", artifact.job_id))?;
        let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id)
            .await
            .map_err(AppError::from)?
            .ok_or_else(|| broken_chain(artifact_id, "stage", job.stage_id))?;
        let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
            .await
            .map_err(AppError::from)?
            .ok_or_else(|| broken_chain(artifact_id, "pipeline", stage.pipeline_id))?;
        let repo = rg_db::entities::repository::Entity::find_by_id(pipeline.repo_id)
            .one(&state.db)
            .await
            .map_err(AppError::from)?
            .ok_or_else(|| broken_chain(artifact_id, "repository", pipeline.repo_id))?;
        Ok((artifact, repo))
    }

    /// Retention has a date on the row and a sweep that gets to it later;
    /// between the two the artifact is already past its policy, so it is
    /// answered as gone rather than served for however long the sweep lags.
    ///
    /// Judged here rather than in `resolve` so the answer lands *after* the
    /// gate: `artifact expired` and `artifact not found` are distinguishable,
    /// and an outsider who can tell them apart enumerates every artifact that
    /// ever existed in a private repository, which is the whole of what the
    /// masking is for.
    fn admit(artifact: &Self::Row) -> Result<(), AppError> {
        if artifact
            .expires_at
            .is_some_and(|expires| expires <= chrono::Utc::now())
        {
            return Err(AppError::not_found("artifact expired"));
        }
        Ok(())
    }
}

/// A link of artifact → job → stage → pipeline → repository that does not
/// resolve.
///
/// The caller is answered exactly as if the artifact were absent, because until
/// the walk reaches a repository there is no gate to run and therefore no one
/// this row may be admitted to — and the four texts this replaces
/// (`job not found` and friends) each confirmed to an outsider that the artifact
/// id itself was real. The detail an operator needs does not go to the caller;
/// it goes to the log, where a chain that cannot be walked belongs.
fn broken_chain(artifact_id: i64, link: &str, link_id: i64) -> AppError {
    tracing::warn!(
        artifact_id,
        link,
        link_id,
        "artifact is orphaned: its {link} row is missing, so no repository can be resolved to \
         authorize against — answering as if the artifact were absent"
    );
    Artifact::masked()
}

/// Read access to the repository the artifact belongs to — the metadata and
/// download routes.
pub type ArtifactRead = AnchoredRead<Artifact>;

/// Write access to it — the delete route.
pub type ArtifactWrite = AnchoredWrite<Artifact>;

// ── Response types ─────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct ArtifactResponse {
    id: i64,
    job_id: i64,
    name: String,
    file_path: String,
    size: i64,
    created_at: String,
    expires_at: Option<String>,
    /// Hex-encoded SHA-256 of the artifact bytes, recorded at upload. `None` for
    /// legacy artifacts uploaded before digest tracking existed.
    sha256: Option<String>,
}

/// Where a staged archive landed, in the spelling the publish route's
/// `file_path` takes.
#[derive(Serialize, ToSchema)]
pub struct StageArtifactResponse {
    file_path: String,
}

#[derive(Serialize, ToSchema)]
pub struct UploadArtifactResponse {
    id: i64,
    message: String,
}

// ── Handlers ───────────────────────────────────────────

/// PUT /api/v1/runners/:id/jobs/:job_id/artifacts/staging
/// Stage an artifact archive in this job's private storage directory.
///
/// The publish route below takes metadata only and names a file that must
/// already exist server-side, which a runner on its own machine has no way of
/// producing. This is that way: the bytes are streamed to disk here, and the
/// path they landed on is what the runner then publishes. Splitting it in two
/// keeps the artifact out of the JSON body — the reason the publish route
/// refuses raw bodies in the first place.
///
/// Auth handled by `authenticate_runner` middleware; the archive size is capped
/// by the route's body-limit layer, and by the handler's own backstop for a
/// chunked body that never declared a length for that layer to refuse.
#[utoipa::path(
    put,
    path = "/runners/{id}/jobs/{job_id}/artifacts/staging",
    tag = "Artifacts",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID, which must be assigned to this runner"),
    ),
    request_body(
        content = String,
        description = "Artifact archive, 1 byte to 1 GiB",
        content_type = "application/x-tar",
    ),
    responses(
        (status = 201, description = "Artifact staged", body = StageArtifactResponse),
        (status = 400, description = "Empty archive", body = serde_json::Value),
        (status = 404, description = "Job not found", body = serde_json::Value),
        (status = 413, description = "Artifact archive exceeds 1 GiB"),
    ),
)]
pub async fn stage_artifact(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    body: axum::body::Body,
) -> impl IntoResponse {
    if let Err(error) = crate::api::runners::assigned_job(&state, runner_id, job_id).await {
        return error.into_response();
    }

    let directory = artifact_root(&state).join("jobs").join(job_id.to_string());

    match stage_archive_file(body, &directory, ARTIFACT_ARCHIVE_MAX_BYTES).await {
        Ok(path) => {
            let file_path = path.to_string_lossy().into_owned();
            (
                StatusCode::CREATED,
                Json(StageArtifactResponse { file_path }),
            )
                .into_response()
        }
        Err(error) => error.into_response(),
    }
}

/// Receive one archive into `directory` and answer with the file it landed on.
///
/// Every way out but the successful one clears the file again: the half-written
/// archive is this request's alone and nothing names it, so a refused transfer
/// must not leave an artifact-sized fragment behind that the publish call could
/// still pick up. That is why the whole staging lifecycle sits in one function
/// rather than in the handler — the cleanup is part of what an over-ceiling
/// body is answered with, and is tested as such.
async fn stage_archive_file(
    body: axum::body::Body,
    directory: &FsPath,
    max_bytes: usize,
) -> Result<PathBuf, AppError> {
    // A job stages one archive, so anything already here is a previous attempt
    // of this same job that never reached the publish call. Nothing points at
    // it and no retention sweep walks it, so this is the only moment it can be
    // reclaimed.
    discard_stale_staging(directory).await;
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|error| {
            AppError::internal(artifact_path_error(
                "CI artifact staging directory",
                directory,
                &error,
            ))
        })?;
    let path = directory.join(format!("{}.tar", Uuid::new_v4()));

    match stream_to_file(body, &path, max_bytes).await {
        Ok(0) => {
            discard_staged_file(&path).await;
            Err(AppError::bad_request(
                "artifact archive must contain at least 1 byte",
            ))
        }
        Ok(_) => Ok(path),
        Err(error) => {
            discard_staged_file(&path).await;
            Err(error)
        }
    }
}

/// Write a request body to `path` without buffering it, returning the byte
/// count.
///
/// Over-ceiling bodies answer `413`, which is what the route declares. A body
/// that announced its length is refused by the transport layer before this is
/// called; a chunked one is not, and arrives here two ways — as the layer's
/// `LengthLimitError` surfacing as a stream error, and as bytes that simply
/// keep coming, which `max_bytes` stops. Both are the same answer, and before
/// this they were `400`: "you sent nonsense" for a caller who sent too much.
async fn stream_to_file(
    body: axum::body::Body,
    path: &FsPath,
    max_bytes: usize,
) -> Result<u64, AppError> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let over_ceiling = || {
        AppError::payload_too_large(format!(
            "artifact archive exceeds the configured {max_bytes}-byte request limit"
        ))
    };
    let mut file = tokio::fs::File::create(path).await.map_err(|error| {
        AppError::internal(artifact_path_error("CI artifact archive", path, &error))
    })?;
    let mut stream = body.into_data_stream();
    let mut written = 0_usize;
    while let Some(chunk) = stream.next().await {
        let data = match chunk {
            Ok(data) => data,
            Err(error) => {
                let inner = error.into_inner();
                if crate::body_limit::is_length_limit_error(&*inner) {
                    return Err(over_ceiling());
                }
                return Err(AppError::bad_request(format!(
                    "artifact archive transfer failed: {inner}"
                )));
            }
        };
        written = written
            .checked_add(data.len())
            .filter(|size| *size <= max_bytes)
            .ok_or_else(over_ceiling)?;
        file.write_all(&data).await.map_err(|error| {
            AppError::internal(artifact_path_error("CI artifact archive", path, &error))
        })?;
    }
    file.flush().await.map_err(|error| {
        AppError::internal(artifact_path_error("CI artifact archive", path, &error))
    })?;
    Ok(written as u64)
}

/// Remove a staged archive nothing points at, reporting a failure rather than
/// leaving an artifact-sized file on disk without a word.
async fn discard_staged_file(path: &FsPath) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            path = %path.display(),
            error = %error,
            "failed to remove a staged CI artifact archive; it stays on disk with nothing pointing at it"
        ),
    }
}

/// Drop everything a job staged and never published.
///
/// Called when the job settles: publication happens before the runner reports
/// completion, so anything still here belongs to an attempt that did not get
/// that far.
pub(crate) async fn discard_job_staging(state: &AppState, job_id: i64) {
    let directory = artifact_root(state).join("jobs").join(job_id.to_string());
    discard_stale_staging(&directory).await;
    // The directory itself only ever holds staged archives, so an empty one is
    // nothing to keep. `NotFound` is the normal answer for a job that staged
    // nothing, and a directory that is not empty is left alone rather than
    // forced — the files in it are the sweep above's answer to give, not this
    // call's.
    match tokio::fs::remove_dir(&directory).await {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => tracing::warn!(
            job_id,
            path = %directory.display(),
            error = %error,
            "failed to remove a settled job's artifact staging directory"
        ),
    }
}

/// Clear a job's staging directory of archives left by an earlier attempt.
async fn discard_stale_staging(directory: &FsPath) {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        // A job staging for the first time has no directory yet, which is not
        // a failure to report. Anything else is: an unreadable directory means
        // whatever is in it stays there, and nothing else will come back for it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(
                path = %directory.display(),
                error = %error,
                "failed to read a job's artifact staging directory; anything staged in it stays on disk"
            );
            return;
        }
    };
    loop {
        match entries.next_entry().await {
            Ok(None) => break,
            Ok(Some(entry)) => {
                if entry.file_type().await.is_ok_and(|kind| kind.is_file()) {
                    discard_staged_file(&entry.path()).await;
                }
            }
            Err(error) => {
                tracing::warn!(
                    path = %directory.display(),
                    error = %error,
                    "stopped walking a job's artifact staging directory; anything left in it stays on disk"
                );
                break;
            }
        }
    }
}

/// POST /api/v1/runners/:id/jobs/:job_id/artifacts
/// Publish an artifact already staged in this job's private storage directory.
/// Raw artifact bodies are intentionally unsupported: accepting them would
/// either inherit Axum's hidden 2 MiB extractor limit or buffer an artifact-sized
/// allocation in the HTTP process.
/// Auth handled by `authenticate_runner` middleware.
#[utoipa::path(
    post,
    path = "/runners/{id}/jobs/{job_id}/artifacts",
    tag = "Artifacts",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID"),
    ),
    request_body(
        content = UploadArtifactRequest,
        description = "Artifact metadata (maximum 64 KiB); file_path must name a file already staged under this job's private artifact directory"
    ),
    responses(
        (status = 201, description = "Artifact created", body = UploadArtifactResponse),
        (status = 400, description = "Invalid artifact metadata", body = serde_json::Value),
        (status = 413, description = "Artifact metadata exceeds 64 KiB"),
        (status = 404, description = "Job not found", body = serde_json::Value),
    ),
)]
pub async fn upload_artifact(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    Json(request): Json<UploadArtifactRequest>,
) -> impl IntoResponse {
    // Verify job belongs to this runner. The same helper the runner routes use,
    // so a job that is not this runner's is answered exactly as an unknown id is
    // — see `api::runners::assigned_job` for why the two must not be tellable
    // apart.
    let job = match crate::api::runners::assigned_job(&state, runner_id, job_id).await {
        Ok(job) => job,
        Err(error) => return error.into_response(),
    };

    let repo_id = match repo_id_for_job(&state, &job).await {
        Ok(repo_id) => repo_id,
        Err(error) => return error.into_response(),
    };
    let policy = match rg_db::ops::ci_retention_ops::get_policy(&state.db, repo_id).await {
        Ok(policy) => policy,
        Err(error) => return AppError::from(error).into_response(),
    };

    let upload = match persist_artifact_upload(&state, job_id, request).await {
        Ok(upload) => upload,
        Err(e) => return e.into_response(),
    };

    match rg_db::ops::artifact_ops::create_artifact(
        &state.db,
        job_id,
        &upload.name,
        &upload.storage_path,
        upload.size,
        upload.sha256,
        Some(rg_db::ops::ci_retention_ops::expires_after(
            policy.artifact_retention_days,
        )),
    )
    .await
    {
        Ok(artifact) => (
            StatusCode::CREATED,
            Json(UploadArtifactResponse {
                id: artifact.id,
                message: "Artifact created successfully".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            // Compensation on the error path: the client must still get the DB
            // failure, so a failed rollback can only be reported here.
            let cleanup = match rg_core::blob_storage::BlobKey::new(&upload.storage_path) {
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
                    job_id,
                    artifact = %upload.name,
                    storage_path = %upload.storage_path,
                    error = %reason,
                    "orphaned CI artifact blob: the artifact row was not created and the rollback delete failed too — the blob stays in storage with no row pointing at it"
                );
            }
            AppError::from(e).into_response()
        }
    }
}

/// GET /api/v1/repos/:owner/:name/pipelines/:id/artifacts
/// List all artifacts for a pipeline.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pipelines/{id}/artifacts",
    tag = "Artifacts",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_pipeline_artifacts(
    State(state): State<AppState>,
    RepoRead { repo }: RepoRead,
    Path((_owner, _name, pipeline_id)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let pipeline = match pipeline_in_repo(&state, &repo, pipeline_id).await {
        Ok(pipeline) => pipeline,
        Err(e) => return e.into_response(),
    };

    match rg_db::ops::artifact_ops::list_by_pipeline(&state.db, pipeline.id).await {
        Ok(artifacts) => {
            let resp: Vec<ArtifactResponse> = artifacts
                .into_iter()
                .map(|a| ArtifactResponse {
                    id: a.id,
                    job_id: a.job_id,
                    name: a.name,
                    file_path: a.file_path,
                    size: a.size,
                    created_at: a.created_at.to_string(),
                    expires_at: a.expires_at.map(|t| t.to_string()),
                    sha256: a.sha256,
                })
                .collect();
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/artifacts/:id
/// Get artifact metadata.
#[utoipa::path(
    get,
    path = "/artifacts/{id}",
    tag = "Artifacts",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 404, description = "No such artifact, or none this caller may see — \
                                     the two are one answer on purpose", body = serde_json::Value),
    ),
)]
pub async fn get_artifact(
    ArtifactRead {
        row: artifact,
        repo: _,
    }: ArtifactRead,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(ArtifactResponse {
            id: artifact.id,
            job_id: artifact.job_id,
            name: artifact.name,
            file_path: artifact.file_path,
            size: artifact.size,
            created_at: artifact.created_at.to_string(),
            expires_at: artifact.expires_at.map(|t| t.to_string()),
            sha256: artifact.sha256,
        }),
    )
        .into_response()
}

/// GET /api/v1/artifacts/:id/download
/// Download artifact file bytes.
#[utoipa::path(
    get,
    path = "/artifacts/{id}/download",
    tag = "Artifacts",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Artifact binary stream", content_type = "application/octet-stream"),
        (status = 404, description = "No such artifact, none this caller may see, or its bytes \
                                     are gone — one answer on purpose", body = serde_json::Value),
    ),
)]
pub async fn download_artifact(
    State(state): State<AppState>,
    ArtifactRead {
        row: artifact,
        repo: _,
    }: ArtifactRead,
) -> impl IntoResponse {
    let bytes = match read_artifact_bytes(&state, &artifact.file_path).await {
        Ok(bytes) => bytes,
        Err(error) => return error.into_response(),
    };

    // Integrity check: the stored bytes must still hash to the digest recorded at
    // upload. Legacy artifacts (uploaded before digest tracking) carry no recorded
    // hash and are served without this guard.
    if let Some(expected) = artifact.sha256.as_deref() {
        let actual = hex::encode(Sha256::digest(&bytes));
        if actual != expected {
            return AppError::internal(anyhow::anyhow!(
                "artifact integrity check failed: expected sha256 {expected}, got {actual}"
            ))
            .into_response();
        }
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    // The whole artifact is already buffered (the integrity check above needs
    // it), so its exact length is known — advertise it so clients can detect a
    // truncated download. An idle abort ends the stream short of this length,
    // which the client sees as a broken transfer rather than a silent short read.
    if let Ok(value) = HeaderValue::from_str(&bytes.len().to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    // The `.replace('"', "")` this used to carry was half of RFC 6266 done by
    // hand: it kept the quoting honest and still lost the header outright for a
    // non-ASCII artifact name. Both halves live in one builder now.
    headers.insert(
        header::CONTENT_DISPOSITION,
        crate::content_disposition::attachment(&artifact.name),
    );
    // Expose the upload-time digest so clients can verify the payload end-to-end.
    if let Some(sha) = artifact.sha256.as_deref() {
        if let Ok(value) = HeaderValue::from_str(sha) {
            headers.insert(header::HeaderName::from_static("x-checksum-sha256"), value);
        }
    }
    // Hand the verified buffer to the socket as a backpressure-sensitive,
    // idle-guarded stream instead of a single in-memory frame: a slow or stalled
    // client would otherwise pin this artifact-sized `Vec` in server memory until
    // the kernel eventually resets the dead TCP connection (card_16003d99e502).
    // Reuses the git-streaming idle budget (same HTTP slow-drip download class).
    (
        StatusCode::OK,
        headers,
        crate::http_stream::buffered_body_with_idle(bytes, state.git_idle_timeout_secs),
    )
        .into_response()
}

/// DELETE /api/v1/artifacts/:id
/// Delete an artifact.
#[utoipa::path(
    delete,
    path = "/artifacts/{id}",
    tag = "Artifacts",
    params(
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "No session — answered before the artifact is looked up, \
                                     so it says nothing about the id", body = serde_json::Value),
        (status = 403, description = "The caller can see the repository but may not write to it",
         body = serde_json::Value),
        (status = 404, description = "No such artifact, or none this caller may see — \
                                     the two are one answer on purpose", body = serde_json::Value),
    ),
)]
pub async fn delete_artifact(
    State(state): State<AppState>,
    ArtifactWrite {
        row: artifact,
        repo: _,
        actor_id: _,
    }: ArtifactWrite,
) -> impl IntoResponse {
    let staging =
        match ArtifactDeletionStaging::prepare(&state, artifact.id, &artifact.file_path).await {
            Ok(staging) => staging,
            Err(error) => return AppError::from(error).into_response(),
        };

    match rg_db::ops::artifact_ops::delete_by_id(&state.db, artifact.id).await {
        Ok(true) => {}
        // The lookup and this statement are separate, so a concurrent delete can
        // take the row in between. That request owns the deletion, so this one
        // must not report a success it did not perform — but the staged bytes
        // are still retired rather than restored: the row is gone either way,
        // and putting them back would leave them with nothing pointing at them.
        Ok(false) => {
            if let Err(error) = staging.retire(&state).await {
                return AppError::from(error).into_response();
            }
            return AppError::not_found("artifact not found").into_response();
        }
        Err(error) => {
            staging.restore(&state).await;
            return AppError::from(error).into_response();
        }
    }

    match staging.retire(&state).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

// ── Request types ───────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct UploadArtifactRequest {
    pub name: String,
    pub file_path: String,
    pub size: Option<i64>,
}

struct ParsedArtifactUpload {
    name: String,
    storage_path: String,
    size: i64,
    sha256: Option<String>,
}

async fn persist_artifact_upload(
    state: &AppState,
    job_id: i64,
    request: UploadArtifactRequest,
) -> Result<ParsedArtifactUpload, AppError> {
    let file_path = PathBuf::from(request.file_path);
    let job_root = artifact_root(state).join("jobs").join(job_id.to_string());
    if !is_path_under(&file_path, &job_root) {
        return Err(AppError::bad_request(
            "artifact metadata path must reference an existing file in this job's storage",
        ));
    }
    // Reporting every errno as "does not exist" sends the runner after the
    // wrong problem when the file is there but unreadable.
    let size = tokio::fs::metadata(&file_path)
        .await
        .map_err(|error| artifact_metadata_file_error(&file_path, &error))?
        .len() as i64;
    let name = sanitize_artifact_name(&request.name);
    // Stream the digest over the referenced file instead of buffering it in
    // memory — the metadata path exists precisely to avoid loading the whole
    // artifact into the request body.
    let sha256 = hash_file(&file_path)
        .await
        .map_err(|error| artifact_metadata_file_error(&file_path, &error))?;
    let key = artifact_key(job_id, &name).map_err(AppError::bad_request)?;
    state
        .blob_storage
        .put_file(&key, &file_path)
        .await
        .map_err(AppError::internal)?;
    // Storage owns the bytes from here. The staged copy is a second full copy
    // of the artifact that no row names and no retention sweep walks, so the
    // moment it stops being needed is the only moment it can be reclaimed.
    discard_staged_file(&file_path).await;

    Ok(ParsedArtifactUpload {
        name,
        storage_path: key.to_string(),
        size,
        sha256: Some(sha256),
    })
}

/// Compute the hex-encoded SHA-256 of a file by streaming it in bounded chunks,
/// so a large artifact is never held in application memory just to be hashed.
async fn hash_file(path: &FsPath) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn artifact_metadata_file_error(path: &FsPath, error: &std::io::Error) -> AppError {
    if error.kind() == std::io::ErrorKind::NotFound {
        AppError::bad_request("artifact metadata file does not exist in this job's storage")
    } else {
        AppError::internal(artifact_path_error("artifact metadata file", path, error))
    }
}

fn artifact_key(job_id: i64, name: &str) -> Result<rg_core::blob_storage::BlobKey, String> {
    let job_id = job_id.to_string();
    let object = format!("{}-{name}", Uuid::new_v4());
    rg_core::blob_storage::BlobKey::from_segments(["artifacts", "jobs", &job_id, &object])
        .map_err(|error| error.to_string())
}

/// One actionable line for a filesystem failure on a legacy artifact file.
///
/// Artifacts uploaded before the blob-storage migration keep an absolute path
/// in `artifacts.file_path`, so these branches bypass the storage backend and
/// touch the file directly. The path comes out of the database and is never
/// echoed back, so a bare `io::Error` reaching the download response — or the
/// retention loop's `refused to clean expired artifact` log line — is an errno
/// against a file under `_artifacts/` that only the server can name.
fn artifact_path_error(what: &str, path: &FsPath, error: &std::io::Error) -> String {
    rg_core::platform::fs::describe_path_error(
        what,
        path,
        error,
        rg_core::platform::fs::BLOB_STORAGE_HINT,
    )
}

/// True when `path` is a legacy artifact whose file is already gone but whose
/// directory is still inside the artifact root.
///
/// [`is_path_under`] canonicalizes both sides, so it answers `false` for a file
/// that no longer exists — indistinguishable from a genuine traversal attempt.
/// That turned a deleted legacy artifact into "outside artifact storage": a 403
/// on download, and a permanently undeletable row in the retention sweep, which
/// re-reported the same refusal on every pass. Resolving containment against
/// the parent directory keeps the check authoritative — a foreign path is still
/// refused — while letting a vanished file be reported as missing.
fn legacy_artifact_is_gone(path: &FsPath, root: &FsPath) -> bool {
    path.parent()
        .is_some_and(|parent| is_path_under(parent, root))
        && matches!(path.try_exists(), Ok(false))
}

async fn read_artifact_bytes(state: &AppState, storage_path: &str) -> Result<Vec<u8>, AppError> {
    match rg_core::blob_storage::BlobKey::new(storage_path) {
        Ok(key) => state.blob_storage.get(&key).await.map_err(|error| {
            if matches!(error, rg_core::blob_storage::BlobStorageError::NotFound(_)) {
                AppError::not_found("artifact file not found")
            } else {
                AppError::internal(error)
            }
        }),
        Err(_) => {
            let file_path = PathBuf::from(storage_path);
            if !is_path_under(&file_path, &artifact_root(state)) {
                if legacy_artifact_is_gone(&file_path, &artifact_root(state)) {
                    return Err(AppError::not_found("artifact file not found"));
                }
                return Err(AppError::forbidden(
                    "artifact path is outside artifact storage",
                ));
            }
            tokio::fs::read(&file_path).await.map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    AppError::not_found("artifact file not found")
                } else {
                    AppError::internal(artifact_path_error(
                        "legacy CI artifact",
                        &file_path,
                        &error,
                    ))
                }
            })
        }
    }
}

/// A portable artifact object, parked under a private key.
#[derive(Debug)]
struct StagedArtifactBlob {
    live: rg_core::blob_storage::BlobKey,
    staged: rg_core::blob_storage::BlobKey,
}

/// A pre-migration artifact file, renamed beside itself.
#[derive(Debug)]
struct StagedLegacyArtifact {
    live: PathBuf,
    staged: PathBuf,
}

/// An artifact's bytes, taken out of the live namespace but not yet destroyed.
///
/// Deleting the object first and the row second is not compensable: a database
/// failure after a successful unlink leaves a live row advertising a download
/// whose bytes are gone for good, and the `5xx` the caller sees says nothing
/// about which of the two halves already happened. The reversible order is the
/// one repository, release and OCI deletion already use — move the bytes to a
/// request-private name, delete the metadata, and only then destroy the
/// tombstone. Every stage is recoverable up to the commit, and after it the
/// remaining debt is physical cleanup, which is reported rather than assumed.
#[derive(Debug)]
pub(crate) struct ArtifactDeletionStaging {
    artifact_id: i64,
    blob: Option<StagedArtifactBlob>,
    legacy: Option<StagedLegacyArtifact>,
}

impl ArtifactDeletionStaging {
    /// Move the artifact's bytes out of the live namespace.
    ///
    /// Bytes that are already gone are the end state this call is asked for, so
    /// they stage nothing and succeed — otherwise a row whose file an operator
    /// removed by hand could never be deleted at all. Anything else fails here,
    /// before the metadata is touched.
    pub(crate) async fn prepare(
        state: &AppState,
        artifact_id: i64,
        storage_path: &str,
    ) -> anyhow::Result<Self> {
        let deletion_id = Uuid::new_v4().simple().to_string();
        let mut staging = Self {
            artifact_id,
            blob: None,
            legacy: None,
        };

        match rg_core::blob_storage::BlobKey::new(storage_path) {
            Ok(live) => {
                let staged = rg_core::blob_storage::BlobKey::from_segments([
                    "_deleted",
                    "artifact-deletions",
                    artifact_id.to_string().as_str(),
                    deletion_id.as_str(),
                ])?;
                match state.blob_storage.move_prefix(&live, &staged).await {
                    Ok(true) => staging.blob = Some(StagedArtifactBlob { live, staged }),
                    Ok(false) => {}
                    Err(error) => {
                        return Err(anyhow::Error::new(error).context(format!(
                            "failed to stage CI artifact blob {live} at {staged}"
                        )));
                    }
                }
            }
            Err(_) => {
                let live = PathBuf::from(storage_path);
                if !is_path_under(&live, &artifact_root(state)) {
                    // Nothing to unlink is a success, not a refusal — otherwise
                    // the row outlives its file and every retention sweep fails
                    // on it.
                    if legacy_artifact_is_gone(&live, &artifact_root(state)) {
                        return Ok(staging);
                    }
                    anyhow::bail!(
                        "stored path {} is outside managed artifact storage",
                        live.display()
                    );
                }
                let staged = match live.file_name() {
                    Some(name) => live.with_file_name(format!(
                        "{}.deleted-{deletion_id}",
                        name.to_string_lossy()
                    )),
                    None => anyhow::bail!(
                        "stored path {} does not name a legacy artifact file",
                        live.display()
                    ),
                };
                match tokio::fs::rename(&live, &staged).await {
                    Ok(()) => staging.legacy = Some(StagedLegacyArtifact { live, staged }),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(anyhow::anyhow!(artifact_path_error(
                            "legacy CI artifact",
                            &live,
                            &error
                        ))
                        .context(format!(
                            "failed to stage legacy CI artifact at {}",
                            staged.display()
                        )));
                    }
                }
            }
        }

        Ok(staging)
    }

    /// Put the bytes back where the surviving row expects them.
    ///
    /// Compensation on an error path: the caller must still see the original
    /// failure, so a failed restore can only be reported. When it fails the row
    /// is live again while its bytes sit under a name nothing else records —
    /// which is exactly what the log line has to say.
    pub(crate) async fn restore(&self, state: &AppState) {
        if let Some(legacy) = &self.legacy {
            if let Err(error) = tokio::fs::rename(&legacy.staged, &legacy.live).await {
                tracing::warn!(
                    artifact_id = self.artifact_id,
                    staged_at = %legacy.staged.display(),
                    belongs_at = %legacy.live.display(),
                    %error,
                    "failed to restore a legacy CI artifact after deletion aborted — the surviving row now points at missing bytes until the file is moved back by hand"
                );
            }
        }
        if let Some(blob) = &self.blob {
            match state
                .blob_storage
                .move_prefix(&blob.staged, &blob.live)
                .await
            {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    artifact_id = self.artifact_id,
                    staged_key = %blob.staged,
                    live_key = %blob.live,
                    "failed to restore a CI artifact blob after deletion aborted — the staged object disappeared and the surviving row now points at missing bytes"
                ),
                Err(error) => tracing::warn!(
                    artifact_id = self.artifact_id,
                    staged_key = %blob.staged,
                    live_key = %blob.live,
                    %error,
                    "failed to restore a CI artifact blob after deletion aborted — the surviving row cannot reach it until the object is moved back by hand"
                ),
            }
        }
    }

    /// Destroy the tombstone, once the metadata row is gone.
    ///
    /// After the commit there is nothing left to roll back — the live name is
    /// already free — so a failure here is cleanup debt, not a lost deletion. It
    /// is still returned: answering `204` while bytes remain parked under a
    /// private key is the silent half of the same failure.
    pub(crate) async fn retire(self, state: &AppState) -> anyhow::Result<()> {
        let mut cleanup_error = None;

        if let Some(legacy) = self.legacy {
            match tokio::fs::remove_file(&legacy.staged).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(
                        artifact_id = self.artifact_id,
                        staged_at = %legacy.staged.display(),
                        %error,
                        "CI artifact metadata is deleted, but its staged legacy file remains and must be removed by hand"
                    );
                    cleanup_error = Some(
                        anyhow::anyhow!(artifact_path_error(
                            "staged legacy CI artifact",
                            &legacy.staged,
                            &error
                        ))
                        .context("failed to retire a deleted legacy CI artifact"),
                    );
                }
            }
        }

        if let Some(blob) = self.blob {
            if let Err(error) = state.blob_storage.delete_prefix(&blob.staged).await {
                tracing::warn!(
                    artifact_id = self.artifact_id,
                    staged_key = %blob.staged,
                    live_key = %blob.live,
                    %error,
                    "CI artifact metadata is deleted and its live key is free, but the staged object remains and must be removed by hand"
                );
                if cleanup_error.is_none() {
                    cleanup_error = Some(anyhow::Error::new(error).context(format!(
                        "failed to retire a staged CI artifact blob at {}",
                        blob.staged
                    )));
                }
            }
        }

        cleanup_error.map_or(Ok(()), Err)
    }
}

fn artifact_root(state: &AppState) -> PathBuf {
    state.repo_root.join("_artifacts")
}

fn sanitize_artifact_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if sanitized.is_empty() {
        "artifact.bin".to_string()
    } else {
        sanitized
    }
}

fn is_path_under(path: &FsPath, root: &FsPath) -> bool {
    match (path.canonicalize(), root.canonicalize()) {
        (Ok(path), Ok(root)) => path.starts_with(root),
        _ => false,
    }
}

async fn repo_id_for_job(
    state: &AppState,
    job: &rg_db::entities::pipeline_job::Model,
) -> Result<i64, AppError> {
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("stage not found"))?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("pipeline not found"))?;
    Ok(pipeline.repo_id)
}

#[cfg(test)]
mod legacy_artifact_path_tests {
    use super::*;

    /// A legacy artifact path is built from `repo_root` and never handed back,
    /// so the failure has to name the file itself. The failure is staged as
    /// ENOTDIR (a regular file used as a directory) rather than a permission
    /// error: that reproduces under any uid, including root, and keeps the
    /// remedy in the message — `describe_path_error` swaps the remedy for an
    /// ownership diagnostic on `PermissionDenied`.
    #[test]
    fn legacy_read_failure_names_the_file_and_the_remedy() {
        let temp = tempfile::tempdir().unwrap();
        let blocker = temp.path().join("artifact.bin");
        std::fs::write(&blocker, "not a directory").unwrap();
        let file_path = blocker.join("nested.bin");
        let error = std::fs::read(&file_path).unwrap_err();

        let rendered = artifact_path_error("legacy CI artifact", &file_path, &error);

        assert!(
            rendered.contains(&file_path.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("legacy CI artifact"), "{rendered}");
        assert!(rendered.contains("[server].repo_root"), "{rendered}");
    }

    /// The file of a legacy artifact can be gone — retention removed it, or an
    /// operator cleaned the directory by hand. `is_path_under` cannot
    /// canonicalize it and answers `false`, which used to be reported as a
    /// path-traversal refusal and left the row undeletable.
    #[test]
    fn a_vanished_artifact_inside_the_root_is_missing_not_foreign() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("_artifacts");
        std::fs::create_dir_all(root.join("jobs").join("7")).unwrap();
        let file_path = root.join("jobs").join("7").join("report.txt");

        assert!(!is_path_under(&file_path, &root));
        assert!(legacy_artifact_is_gone(&file_path, &root));
    }

    /// Containment stays authoritative: forgiving a missing file must not
    /// forgive a path that was never inside the artifact root.
    #[test]
    fn a_path_outside_the_root_is_still_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("_artifacts");
        std::fs::create_dir_all(&root).unwrap();
        let outside = temp.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();

        let existing = outside.join("passwd");
        std::fs::write(&existing, "secret").unwrap();
        assert!(!legacy_artifact_is_gone(&existing, &root));
        assert!(!legacy_artifact_is_gone(&outside.join("gone.bin"), &root));
    }

    /// A file that is still on disk inside the root is not "gone" — the caller
    /// must go on to read or unlink it.
    #[test]
    fn a_present_artifact_is_not_reported_as_gone() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("_artifacts");
        std::fs::create_dir_all(&root).unwrap();
        let file_path = root.join("report.txt");
        std::fs::write(&file_path, "bytes").unwrap();

        assert!(is_path_under(&file_path, &root));
        assert!(!legacy_artifact_is_gone(&file_path, &root));
    }
}

#[cfg(test)]
mod artifact_staging_tests {
    use super::*;
    use axum::body::{Body, Bytes};
    use std::convert::Infallible;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// Every file left under the staging directory after a call — the fragment
    /// a refused transfer must not hand to the publish route.
    fn staged_files(directory: &FsPath) -> Vec<PathBuf> {
        match std::fs::read_dir(directory) {
            Ok(entries) => entries.map(|entry| entry.unwrap().path()).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("cannot read the staging directory: {error}"),
        }
    }

    fn chunked(chunks: &'static [&'static [u8]]) -> Body {
        Body::from_stream(futures::stream::iter(
            chunks
                .iter()
                .map(|chunk| Ok::<_, Infallible>(Bytes::from_static(chunk))),
        ))
    }

    /// The transport ceiling reaches a chunked upload as a stream error rather
    /// than as a rejection, and the route declares `413` for it. Before this it
    /// was collapsed into `400` together with every other body failure, so a
    /// runner that sent too much was told it had sent nonsense.
    #[tokio::test]
    async fn chunked_transport_overflow_is_413_and_leaves_no_staged_file() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("jobs").join("7");
        let limited = Body::new(http_body_util::Limited::new(chunked(&[b"123", b"45"]), 4));

        let error = stage_archive_file(limited, &directory, 10)
            .await
            .unwrap_err();

        assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            error
                .to_string()
                .contains("configured 10-byte request limit"),
            "{error}"
        );
        assert_eq!(
            staged_files(&directory),
            Vec::<PathBuf>::new(),
            "a refused chunked upload left an archive the publish call could name"
        );
    }

    /// The handler's own backstop, for a chunked body the transport layer never
    /// got a `Content-Length` to refuse in advance.
    #[tokio::test]
    async fn a_body_over_the_handler_ceiling_is_413_and_leaves_no_staged_file() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("jobs").join("7");

        let error = stage_archive_file(chunked(&[b"1234", b"5678"]), &directory, 6)
            .await
            .unwrap_err();

        assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            error
                .to_string()
                .contains("configured 6-byte request limit"),
            "{error}"
        );
        assert_eq!(
            staged_files(&directory),
            Vec::<PathBuf>::new(),
            "a body refused by the backstop left an archive behind"
        );
    }

    /// A body that fails for any other reason is still the caller's fault in
    /// the `400` sense — mapping the ceiling to `413` must not swallow that.
    #[tokio::test]
    async fn a_broken_transfer_is_still_400_and_leaves_no_staged_file() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("jobs").join("7");
        let body = Body::from_stream(futures::stream::iter([
            Ok(Bytes::from_static(b"half")),
            Err(std::io::Error::other("connection reset")),
        ]));

        let error = stage_archive_file(body, &directory, 1024)
            .await
            .unwrap_err();

        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("connection reset"), "{error}");
        assert_eq!(
            staged_files(&directory),
            Vec::<PathBuf>::new(),
            "a broken transfer left its half-written archive behind"
        );
    }

    /// An empty archive keeps its own refusal: it is a malformed upload, not an
    /// oversized one.
    #[tokio::test]
    async fn an_empty_archive_is_400_and_leaves_no_staged_file() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("jobs").join("7");

        let error = stage_archive_file(Body::empty(), &directory, 1024)
            .await
            .unwrap_err();

        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("at least 1 byte"), "{error}");
        assert_eq!(staged_files(&directory), Vec::<PathBuf>::new());
    }

    /// The accepted path: the bytes land on the file whose name the publish
    /// route is handed, and an earlier attempt's archive is cleared first.
    #[tokio::test]
    async fn an_accepted_archive_lands_on_disk_and_replaces_an_earlier_attempt() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("jobs").join("7");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("stale.tar"), "previous attempt").unwrap();

        let path = stage_archive_file(chunked(&[b"first-", b"second"]), &directory, 1024)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"first-second");
        assert_eq!(
            staged_files(&directory),
            vec![path],
            "the staging directory must hold this attempt's archive and nothing else"
        );
    }

    /// The backstop is only the transport ceiling for as long as the route
    /// keeps mounting the wrapper that carries it. Moved to a wrapper without a
    /// raised limit, the handler would answer `413` at a gigabyte while the
    /// layer refused at Axum's unrelated 2 MiB — so the mount is asserted, not
    /// assumed.
    #[test]
    fn the_staging_route_mounts_the_wrapper_the_backstop_is_derived_from() {
        let routes = rust_source::production_rust_code_only(include_str!("../routes.rs"));
        let mount = routes
            .split_once("api::artifacts::stage_artifact")
            .expect("routes.rs no longer mounts the artifact staging handler")
            .1;
        let (mounted_with, _) = mount.split_once(')').expect("unterminated route entry");
        assert!(
            mounted_with.contains("&runner_auth_1gb"),
            "artifact staging must keep the wrapper whose limit \
             ARTIFACT_ARCHIVE_MAX_BYTES is derived from, got: {mounted_with}"
        );
        assert!(
            routes.contains(
                "Wrap::runner_auth_with_body_limit(state, api::runners::CACHE_ARCHIVE_MAX_BYTES)"
            ),
            "that wrapper must layer the constant this handler backstops with"
        );
    }
}
