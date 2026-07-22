//! REST API handlers for CI Artifacts.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};
use std::path::{Path as FsPath, PathBuf};
use uuid::Uuid;

use crate::api::auth::extract_user_id;
use crate::error::AppError;
use crate::AppState;
use utoipa::ToSchema;

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
}

#[derive(Serialize, ToSchema)]
pub struct UploadArtifactResponse {
    id: i64,
    message: String,
}

// ── Handlers ───────────────────────────────────────────

/// POST /api/v1/runners/:id/jobs/:job_id/artifacts
/// Upload an artifact for a job.
/// Auth handled by `authenticate_runner` middleware.
#[utoipa::path(
    post,
    path = "/runners/{id}/jobs/{job_id}/artifacts",
    tag = "Artifacts",
    params(
        ("id" = i64, Path, description = "Runner ID"),
        ("job_id" = i64, Path, description = "Job ID"),
    ),
    request_body(content = UploadArtifactRequest, description = "Artifact metadata"),
    responses(
        (status = 201, description = "Artifact created", body = UploadArtifactResponse),
        (status = 403, description = "Forbidden - job not assigned to this runner", body = serde_json::Value),
        (status = 404, description = "Job not found", body = serde_json::Value),
    ),
)]
pub async fn upload_artifact(
    State(state): State<AppState>,
    Path((runner_id, job_id)): Path<(i64, i64)>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Verify job belongs to this runner
    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => {
            return AppError::not_found("job not found").into_response();
        }
        Err(e) => {
            return AppError::internal(e.to_string()).into_response();
        }
    };

    if job.runner_id != Some(runner_id) {
        return AppError::forbidden("job not assigned to this runner").into_response();
    }

    let repo_id = match repo_id_for_job(&state, &job).await {
        Ok(repo_id) => repo_id,
        Err(error) => return error.into_response(),
    };
    let policy = match rg_db::ops::ci_retention_ops::get_policy(&state.db, repo_id).await {
        Ok(policy) => policy,
        Err(error) => return AppError::internal(error).into_response(),
    };

    let upload = match parse_artifact_upload(&state, job_id, &headers, &body).await {
        Ok(upload) => upload,
        Err(e) => return e.into_response(),
    };

    match rg_db::ops::artifact_ops::create_artifact(
        &state.db,
        job_id,
        &upload.name,
        &upload.storage_path,
        upload.size,
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
            if let Ok(key) = rg_core::blob_storage::BlobKey::new(&upload.storage_path) {
                let _ = state.blob_storage.delete(&key).await;
            }
            AppError::internal(e.to_string()).into_response()
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
    headers: HeaderMap,
    Path((owner, name, pipeline_id)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    if let Err(e) = require_pipeline_read(&state, &headers, &owner, &name, pipeline_id).await {
        return e.into_response();
    }

    match rg_db::ops::artifact_ops::list_by_pipeline(&state.db, pipeline_id).await {
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
                })
                .collect();
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => AppError::internal(e.to_string()).into_response(),
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(artifact_id): Path<i64>,
) -> impl IntoResponse {
    let artifact = match require_artifact_read(&state, &headers, artifact_id).await {
        Ok(artifact) => artifact,
        Err(e) => return e.into_response(),
    };

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
        }),
    )
        .into_response()
}

async fn require_artifact_write(
    state: &AppState,
    headers: &HeaderMap,
    artifact_id: i64,
) -> Result<rg_db::entities::artifact::Model, AppError> {
    let artifact = require_artifact_read(state, headers, artifact_id).await?;
    let job = rg_db::ops::pipeline_ops::get_job(&state.db, artifact.job_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("job not found"))?;
    let repo_id = repo_id_for_job(state, &job).await?;
    let repo = rg_db::entities::repository::Entity::find_by_id(repo_id)
        .one(&state.db)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("repository not found"))?;
    let actor_id = extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    match rg_core::repo::service::can_write_repo(&state.db, &repo, Some(actor_id)).await {
        Ok(true) => Ok(artifact),
        Ok(false) => Err(AppError::forbidden("write access denied")),
        Err(error) => Err(AppError::internal(error)),
    }
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "Artifact file not found", body = serde_json::Value),
    ),
)]
pub async fn download_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(artifact_id): Path<i64>,
) -> impl IntoResponse {
    let artifact = match require_artifact_read(&state, &headers, artifact_id).await {
        Ok(artifact) => artifact,
        Err(e) => return e.into_response(),
    };

    let bytes = match read_artifact_bytes(&state, &artifact.file_path).await {
        Ok(bytes) => bytes,
        Err(error) => return error.into_response(),
    };

    let disposition = format!(
        "attachment; filename=\"{}\"",
        artifact.name.replace('"', "")
    );
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CONTENT_DISPOSITION, disposition.as_str()),
        ],
        bytes,
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
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(artifact_id): Path<i64>,
) -> impl IntoResponse {
    let artifact = match require_artifact_write(&state, &headers, artifact_id).await {
        Ok(artifact) => artifact,
        Err(e) => return e.into_response(),
    };

    if let Err(error) = delete_artifact_blob(&state, &artifact.file_path).await {
        return AppError::internal(error).into_response();
    }
    match rg_db::ops::artifact_ops::delete_by_id(&state.db, artifact_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => AppError::not_found("artifact not found").into_response(),
        Err(e) => AppError::internal(e.to_string()).into_response(),
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
}

async fn parse_artifact_upload(
    state: &AppState,
    job_id: i64,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<ParsedArtifactUpload, AppError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if content_type.starts_with("application/json") {
        let req: UploadArtifactRequest =
            serde_json::from_slice(body).map_err(|e| AppError::bad_request(e.to_string()))?;
        let file_path = PathBuf::from(req.file_path);
        let job_root = artifact_root(state).join("jobs").join(job_id.to_string());
        if !is_path_under(&file_path, &job_root) {
            return Err(AppError::bad_request(
                "artifact metadata path must reference an existing file in this job's storage",
            ));
        }
        let size = tokio::fs::metadata(&file_path)
            .await
            .map_err(|_| AppError::bad_request("artifact metadata file does not exist"))?
            .len() as i64;
        let name = sanitize_artifact_name(&req.name);
        let key = artifact_key(job_id, &name).map_err(AppError::bad_request)?;
        state
            .blob_storage
            .put_file(&key, &file_path)
            .await
            .map_err(AppError::internal)?;
        return Ok(ParsedArtifactUpload {
            name,
            storage_path: key.to_string(),
            size,
        });
    }

    if body.is_empty() {
        return Err(AppError::bad_request("artifact upload body is empty"));
    }

    let name = artifact_name_from_headers(headers).unwrap_or_else(|| "artifact.bin".to_string());
    let safe_name = sanitize_artifact_name(&name);
    let key = artifact_key(job_id, &safe_name).map_err(AppError::bad_request)?;
    let stored = state
        .blob_storage
        .put(&key, body)
        .await
        .map_err(AppError::internal)?;

    Ok(ParsedArtifactUpload {
        name: safe_name,
        storage_path: key.to_string(),
        size: stored.size as i64,
    })
}

fn artifact_key(job_id: i64, name: &str) -> Result<rg_core::blob_storage::BlobKey, String> {
    let job_id = job_id.to_string();
    let object = format!("{}-{name}", Uuid::new_v4());
    rg_core::blob_storage::BlobKey::from_segments(["artifacts", "jobs", &job_id, &object])
        .map_err(|error| error.to_string())
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
                return Err(AppError::forbidden(
                    "artifact path is outside artifact storage",
                ));
            }
            tokio::fs::read(file_path).await.map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    AppError::not_found("artifact file not found")
                } else {
                    AppError::internal(error)
                }
            })
        }
    }
}

pub(crate) async fn delete_artifact_blob(
    state: &AppState,
    storage_path: &str,
) -> anyhow::Result<()> {
    match rg_core::blob_storage::BlobKey::new(storage_path) {
        Ok(key) => {
            state.blob_storage.delete(&key).await?;
        }
        Err(_) => {
            let file_path = PathBuf::from(storage_path);
            if !is_path_under(&file_path, &artifact_root(state)) {
                anyhow::bail!("stored path is outside managed artifact storage");
            }
            if tokio::fs::try_exists(&file_path).await? {
                tokio::fs::remove_file(file_path).await?;
            }
        }
    }
    Ok(())
}

fn artifact_root(state: &AppState) -> PathBuf {
    state.repo_root.join("_artifacts")
}

fn artifact_name_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(name) = headers
        .get("x-artifact-name")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(name.to_string());
    }

    headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_filename_from_disposition)
}

fn parse_filename_from_disposition(value: &str) -> Option<String> {
    value.split(';').find_map(|part| {
        let part = part.trim();
        let filename = part.strip_prefix("filename=")?;
        Some(filename.trim_matches('"').to_string())
    })
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

async fn require_pipeline_read(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
    pipeline_id: i64,
) -> Result<rg_db::entities::pipeline::Model, AppError> {
    let repo = rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, name)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("repository not found"))?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, pipeline_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("pipeline not found"))?;
    if pipeline.repo_id != repo.id {
        return Err(AppError::not_found("pipeline not found"));
    }
    require_repo_read(state, headers, &repo).await?;
    Ok(pipeline)
}

async fn require_artifact_read(
    state: &AppState,
    headers: &HeaderMap,
    artifact_id: i64,
) -> Result<rg_db::entities::artifact::Model, AppError> {
    let artifact = rg_db::ops::artifact_ops::get_by_id(&state.db, artifact_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("artifact not found"))?;
    if artifact
        .expires_at
        .is_some_and(|expires| expires <= chrono::Utc::now())
    {
        return Err(AppError::not_found("artifact expired"));
    }
    let job = rg_db::ops::pipeline_ops::get_job(&state.db, artifact.job_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("job not found"))?;
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("stage not found"))?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("pipeline not found"))?;
    let repo = rg_db::entities::repository::Entity::find_by_id(pipeline.repo_id)
        .one(&state.db)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("repository not found"))?;
    require_repo_read(state, headers, &repo).await?;
    Ok(artifact)
}

async fn require_repo_read(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
) -> Result<(), AppError> {
    let actor_id = extract_user_id(headers, &state.jwt_secret);
    match rg_core::repo::service::can_read_repo(&state.db, repo, actor_id).await {
        Ok(true) => Ok(()),
        Ok(false) if repo.is_private && actor_id.is_none() => {
            Err(AppError::unauthorized("authentication required"))
        }
        Ok(false) => Err(AppError::forbidden("access denied")),
        Err(e) => Err(AppError::internal(e)),
    }
}

async fn repo_id_for_job(
    state: &AppState,
    job: &rg_db::entities::pipeline_job::Model,
) -> Result<i64, AppError> {
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("stage not found"))?;
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&state.db, stage.pipeline_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("pipeline not found"))?;
    Ok(pipeline.repo_id)
}
