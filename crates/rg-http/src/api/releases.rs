//! Release REST API.
//!
//! GET    /api/v1/repos/:owner/:name/releases                      — list releases
//! POST   /api/v1/repos/:owner/:name/releases                      — create release
//! GET    /api/v1/repos/:owner/:name/releases/:id                 — get release
//! PATCH  /api/v1/repos/:owner/:name/releases/:id                 — update release
//! DELETE /api/v1/repos/:owner/:name/releases/:id                 — delete release
//! GET    /api/v1/repos/:owner/:name/releases/:release_id/assets   — list assets
//! POST   /api/v1/repos/:owner/:name/releases/:release_id/assets   — upload asset
//! GET    /api/v1/repos/:owner/:name/releases/assets/:asset_id     — get asset
//! GET    /api/v1/repos/:owner/:name/releases/assets/:asset_id/download — download asset
//! DELETE /api/v1/repos/:owner/:name/releases/assets/:asset_id     — delete asset

use axum::body::Body;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use futures::StreamExt;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use utoipa::ToSchema;

use crate::api::repo_access::{RepoRead, RepoWrite};
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::AppState;

/// Maximum size of one release asset (512 MiB), enforced both by the route's
/// transport wrapper and by the streaming spool as a defense in depth.
pub(crate) const RELEASE_ASSET_UPLOAD_MAX_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug)]
struct StagedReleaseUpload {
    path: tempfile::TempPath,
    len: u64,
}

/// Stream one release asset into a request-private file with bounded memory.
async fn stage_release_upload(
    body: Body,
    repo_root: &std::path::Path,
    max_bytes: usize,
) -> Result<StagedReleaseUpload, AppError> {
    let staging_dir = repo_root.join(".tmp").join("release-uploads");
    tokio::fs::create_dir_all(&staging_dir)
        .await
        .map_err(|error| {
            AppError::internal(rg_core::platform::fs::describe_path_error(
                "release asset staging directory",
                &staging_dir,
                &error,
                rg_core::platform::fs::BLOB_STORAGE_HINT,
            ))
        })?;
    let staged = tempfile::Builder::new()
        .prefix("release-")
        .suffix(".upload")
        .tempfile_in(&staging_dir)
        .map_err(|error| {
            AppError::internal(rg_core::platform::fs::describe_path_error(
                "release asset staging file",
                &staging_dir,
                &error,
                rg_core::platform::fs::BLOB_STORAGE_HINT,
            ))
        })?;
    let (file, path) = staged.into_parts();
    let mut file = tokio::fs::File::from_std(file);
    let mut stream = body.into_data_stream();
    let mut len = 0_usize;

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                let inner = error.into_inner();
                if crate::body_limit::is_length_limit_error(&*inner) {
                    return Err(AppError::payload_too_large(format!(
                        "release asset exceeds the configured {max_bytes}-byte request limit"
                    )));
                }
                return Err(AppError::bad_request(format!(
                    "failed to read release asset body: {inner}"
                )));
            }
        };
        len = len
            .checked_add(chunk.len())
            .filter(|size| *size <= max_bytes)
            .ok_or_else(|| {
                AppError::payload_too_large(format!(
                    "release asset exceeds the configured {max_bytes}-byte request limit"
                ))
            })?;
        file.write_all(&chunk).await.map_err(|error| {
            AppError::internal(rg_core::platform::fs::describe_path_error(
                "release asset staging file",
                &path,
                &error,
                rg_core::platform::fs::BLOB_STORAGE_HINT,
            ))
        })?;
    }
    file.flush().await.map_err(|error| {
        AppError::internal(rg_core::platform::fs::describe_path_error(
            "release asset staging file",
            &path,
            &error,
            rg_core::platform::fs::BLOB_STORAGE_HINT,
        ))
    })?;
    drop(file);

    Ok(StagedReleaseUpload {
        path,
        len: len as u64,
    })
}

// ── Access gates ──────────────────────────────────────────────────────
//
// These routes carry the repository in the path but address the release (or
// asset) by a *global* id, so the permission check and the object it guards are
// two different things. Checking only the repository proves the caller can open
// (or write to) some repo — it says nothing about where the id points, and a
// public repo of their own is enough to reach a private repo's releases. Every
// helper below therefore re-anchors the object to the repository that was
// checked, and hands the resolved model back so handlers act on the id that was
// verified rather than the one from the path.
//
// A mismatch answers 404, not 403: a 403 would still confirm that the id
// exists, which is most of what an id-walking caller wants to learn.

/// Anchor a release to an already-authorized repository.
async fn release_in_repo(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    release_id: i64,
) -> Result<rg_db::entities::release::Model, AppError> {
    let release = rg_core::release::service::get_release(&state.db, release_id).await?;
    if release.repo_id != repo.id {
        return Err(AppError::not_found("release not found"));
    }

    Ok(release)
}

/// Anchor an asset to an already-authorized repository, through its release.
async fn asset_in_repo(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    asset_id: i64,
) -> Result<rg_db::entities::release_asset::Model, AppError> {
    let asset = rg_core::release::service::get_asset(&state.db, asset_id).await?;
    let release = rg_core::release::service::get_release(&state.db, asset.release_id).await?;
    if release.repo_id != repo.id {
        return Err(AppError::not_found("asset not found"));
    }

    Ok(asset)
}

/// Request body for creating a release.
#[derive(Deserialize, ToSchema)]
pub struct CreateReleaseRequest {
    pub tag_name: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_commitish: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_prerelease: Option<bool>,
}

/// Request body for updating a release.
#[derive(Deserialize, ToSchema)]
pub struct UpdateReleaseRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_prerelease: Option<bool>,
}

/// GET /api/v1/repos/:owner/:name/releases
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/releases",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        PaginationParams,
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_releases(
    State(state): State<AppState>,
    RepoRead { repo }: RepoRead,
    Path((_, _)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
) -> impl IntoResponse {
    let pagination = params.clamp();
    let offset = pagination.offset();
    let limit = pagination.limit();

    match rg_core::release::service::list_releases(&state.db, repo.id, offset, limit).await {
        Ok((releases, total)) => (
            StatusCode::OK,
            Json(PaginatedResponse::new(releases, &pagination, total as u64)),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// POST /api/v1/repos/:owner/:name/releases
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/releases",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "A release with that tag already exists", body = serde_json::Value),
    ),
)]
pub async fn create_release(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    RepoWrite {
        repo,
        actor_id: user_id,
    }: RepoWrite,
    Json(body): Json<CreateReleaseRequest>,
) -> impl IntoResponse {
    // The release is created *in* the resolved repository, so this route carries
    // no foreign id — the shared gate is enough, and it keeps one copy of the
    // resolve-then-check sequence instead of a second hand-rolled one.

    // H-02: Validate owner/name before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&name) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, name));

    match rg_core::release::service::create_release(
        &state.db,
        repo.id,
        user_id,
        &body.tag_name,
        &body.title,
        body.body.as_deref(),
        body.target_commitish.as_deref().unwrap_or("main"),
        body.is_draft.unwrap_or(false),
        body.is_prerelease.unwrap_or(false),
        &repo_path,
    )
    .await
    {
        Ok(release) => (StatusCode::CREATED, Json(serde_json::json!(release))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/releases/:id
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/releases/{id}",
    tag = "Releases",
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
pub async fn get_release(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    // The service marks a genuine miss with `rg_core::error::NotFound`, so this
    // still answers 404 for a deleted release — but a database outage surfaces
    // as 503 instead of telling the client the release is gone.
    match release_in_repo(&state, &repo, id).await {
        Ok(release) => (StatusCode::OK, Json(serde_json::json!(release))).into_response(),
        Err(e) => e.into_response(),
    }
}

/// PATCH /api/v1/repos/:owner/:name/releases/:id
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/releases/{id}",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_release(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<UpdateReleaseRequest>,
) -> impl IntoResponse {
    // Write access to `owner/name` says nothing about where `id` points: the
    // release has to live in the repository the permission was checked against.
    let release = match release_in_repo(&state, &repo, id).await {
        Ok(release) => release,
        Err(e) => return e.into_response(),
    };

    match rg_core::release::service::update_release(
        &state.db,
        release.id,
        body.title.as_deref(),
        body.body.as_deref(),
        body.is_draft,
        body.is_prerelease,
    )
    .await
    {
        Ok(release) => (StatusCode::OK, Json(serde_json::json!(release))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/repos/:owner/:name/releases/:id
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/releases/{id}",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_release(
    State(state): State<AppState>,
    Path((owner, name, id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    let release = match release_in_repo(&state, &repo, id).await {
        Ok(release) => release,
        Err(e) => return e.into_response(),
    };

    match rg_core::release::service::delete_release(
        &state.db,
        release.id,
        state.blob_storage.as_ref(),
        &state.repo_root,
        &owner,
        &name,
    )
    .await
    {
        Ok(()) => (
            StatusCode::NO_CONTENT,
            Json(serde_json::json!({ "deleted": true })),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ─── Release Assets ────────────────────────────────────────────────────────

/// GET /api/v1/repos/:owner/:name/releases/:release_id/assets
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/releases/{release_id}/assets",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("release_id" = i64, Path, description = "release_id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_assets(
    State(state): State<AppState>,
    Path((_, _, release_id)): Path<(String, String, i64)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    if let Err(e) = release_in_repo(&state, &repo, release_id).await {
        return e.into_response();
    }

    match rg_core::release::service::list_assets(&state.db, release_id).await {
        Ok(assets) => (StatusCode::OK, Json(serde_json::json!(assets))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// POST /api/v1/repos/:owner/:name/releases/:release_id/assets
///
/// Upload a release asset. The request body is the raw file content.
/// Required headers:
///   - `Content-Disposition`: `attachment; filename*=UTF-8''...`
///   - `Content-Type`: MIME type of the file (used as asset content_type)
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/releases/{release_id}/assets",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("release_id" = i64, Path, description = "release_id"),
    ),
    request_body(content_type = "application/octet-stream"),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 413, description = "Release asset exceeds the configured limit", body = serde_json::Value),
        (status = 500, description = "Server error", body = serde_json::Value),
    ),
)]
pub async fn upload_asset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, release_id)): Path<(String, String, i64)>,
    RepoWrite {
        repo,
        actor_id: user_id,
    }: RepoWrite,
    body: Body,
) -> impl IntoResponse {
    // Gate before reading the body: the release must belong to the repository
    // whose write permission was just checked, otherwise the asset lands in
    // someone else's release.
    let release = match release_in_repo(&state, &repo, release_id).await {
        Ok(release) => release,
        Err(e) => return e.into_response(),
    };

    let filename = headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(crate::content_disposition::filename_from_disposition)
        .or_else(|| {
            headers
                .get("x-asset-filename")
                .and_then(|v| v.to_str().ok())
                .map(String::from)
        });

    let filename = match filename {
        Some(f) if !f.is_empty() => f,
        _ => {
            return AppError::bad_request("missing required asset filename").into_response();
        }
    };

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let staged =
        match stage_release_upload(body, &state.repo_root, RELEASE_ASSET_UPLOAD_MAX_BYTES).await {
            Ok(staged) => staged,
            Err(error) => return error.into_response(),
        };

    match rg_core::release::service::upload_asset_from_file(
        &state.db,
        release.id,
        state.blob_storage.as_ref(),
        &owner,
        &name,
        &filename,
        &content_type,
        user_id,
        &staged.path,
        staged.len,
    )
    .await
    {
        Ok(asset) => (StatusCode::CREATED, Json(serde_json::json!(asset))).into_response(),
        // Everything the service can fail on lands here: a missing release, a
        // dropped database connection, and the blob-store write under
        // `repo_root`. Only the first is about the request, so classify on the
        // typed error instead of blaming the uploader for all three — a `400`
        // is the one answer a client will never retry.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/releases/assets/:asset_id
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/releases/assets/{asset_id}",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("asset_id" = i64, Path, description = "asset_id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_asset(
    State(state): State<AppState>,
    Path((_, _, asset_id)): Path<(String, String, i64)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    match asset_in_repo(&state, &repo, asset_id).await {
        Ok(asset) => (StatusCode::OK, Json(serde_json::json!(asset))).into_response(),
        Err(e) => e.into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/releases/assets/:asset_id/download
///
/// Downloads the release asset file and increments the download count.
///
/// Answers with both `Content-Disposition` forms — `filename="…"` for a client
/// that never learned RFC 5987, and `filename*=UTF-8''…` carrying the name the
/// asset was actually uploaded under.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/releases/assets/{asset_id}/download",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("asset_id" = i64, Path, description = "asset_id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn download_asset(
    State(state): State<AppState>,
    Path((owner, name, asset_id)): Path<(String, String, i64)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    // Read access to the repository is only half of it — the asset id is
    // global, so it also has to belong to this repository or the check guards
    // the wrong object.
    if let Err(e) = asset_in_repo(&state, &repo, asset_id).await {
        return e.into_response();
    }

    match rg_core::release::service::download_asset(
        &state.db,
        asset_id,
        state.blob_storage.as_ref(),
        &state.repo_root,
        &owner,
        &name,
    )
    .await
    {
        Ok((asset, data)) => {
            let mut resp_headers = HeaderMap::new();
            if let Ok(v) = header::HeaderValue::from_str(&asset.content_type) {
                resp_headers.insert(header::CONTENT_TYPE, v);
            }
            // Unconditional: the old `if let Ok(..)` around a plain
            // `filename="…"` dropped the header entirely for an asset whose name
            // is not ASCII, and the browser then saved the file under whatever
            // the URL suggested — a `200` that quietly did not do what the
            // endpoint documents.
            resp_headers.insert(
                header::CONTENT_DISPOSITION,
                crate::content_disposition::attachment(&asset.filename),
            );
            if let Ok(v) = header::HeaderValue::from_str(&asset.size.to_string()) {
                resp_headers.insert(header::CONTENT_LENGTH, v);
            }
            // Let clients verify the payload against the digest recorded at upload.
            if let Some(sha) = asset.sha256.as_deref() {
                if let Ok(v) = header::HeaderValue::from_str(sha) {
                    resp_headers.insert(header::HeaderName::from_static("x-checksum-sha256"), v);
                }
            }
            // The whole asset is already buffered because the service verifies
            // its sha256 over the complete bytes before returning them. Handing
            // that finished `Vec` to `Body::from` would make it a single frame a
            // slow-drip / stalled client can pin in server memory until the
            // kernel resets the dead connection — the same download-side slow-drip
            // class as the artifact/cache handlers (card_9cd96bafd879). Serve it
            // as a backpressure-sensitive, idle-guarded stream instead; reuses the
            // git-streaming idle budget. `Content-Length` above lets clients spot
            // an idle-aborted short read.
            (
                StatusCode::OK,
                resp_headers,
                crate::http_stream::buffered_body_with_idle(data, state.git_idle_timeout_secs),
            )
                .into_response()
        }
        // The read half of the same split: a missing asset row is a 404, but an
        // unreadable blob store is not — and reporting it as one both hides the
        // outage and (since a 404 body is not sanitized) hands the client the
        // storage path from the error text.
        Err(e) => AppError::from(e).into_response(),
    }
}

// ─── Release Asset Attestations (opt-in) ───────────────────────────────────

/// Identifier of this instance as the provenance builder.
fn attestation_builder_id(state: &AppState) -> String {
    state
        .external_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_string())
        .unwrap_or_else(|| "urn:forgekeep:instance".to_string())
}

/// POST /api/v1/repos/:owner/:name/releases/assets/:asset_id/attestation
///
/// Sign a detached Ed25519 provenance attestation for the asset with the
/// instance key and store it. Requires write permission. Opt-in: returns 404
/// when attestation is disabled on the instance.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/releases/assets/{asset_id}/attestation",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("asset_id" = i64, Path, description = "asset_id"),
    ),
    responses(
        (status = 201, description = "Attestation created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "Not found / feature disabled", body = serde_json::Value),
    ),
)]
pub async fn sign_asset_attestation(
    State(state): State<AppState>,
    Path((_, _, asset_id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    if !state.attestation_enabled {
        return AppError::not_found("attestation is not enabled").into_response();
    }

    let asset = match asset_in_repo(&state, &repo, asset_id).await {
        Ok(asset) => asset,
        Err(e) => return e.into_response(),
    };

    let builder_id = attestation_builder_id(&state);
    match rg_core::release::service::sign_asset_attestation(
        &state.db,
        asset.id,
        &state.instance_key,
        &builder_id,
    )
    .await
    {
        Ok((_asset, envelope)) => {
            (StatusCode::CREATED, Json(serde_json::json!(envelope))).into_response()
        }
        // A legacy asset with no recorded digest is the only client-fixable
        // failure here (typed `InvalidRequest`); a missing asset is a 404 and a
        // dropped connection a 503.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/releases/assets/:asset_id/attestation
///
/// Return the stored detached attestation envelope for the asset. Read
/// permission. Opt-in: 404 when disabled or when the asset has no attestation.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/releases/assets/{asset_id}/attestation",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("asset_id" = i64, Path, description = "asset_id"),
    ),
    responses(
        (status = 200, description = "Attestation envelope", body = serde_json::Value),
        (status = 404, description = "Not found / feature disabled", body = serde_json::Value),
    ),
)]
pub async fn get_asset_attestation(
    State(state): State<AppState>,
    Path((_, _, asset_id)): Path<(String, String, i64)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    if !state.attestation_enabled {
        return AppError::not_found("attestation is not enabled").into_response();
    }

    // Read access to the repository in the path is only half of it — the
    // envelope belongs to a globally addressed asset, and it carries the
    // filename and digest of a release that may live in a private repository
    // the caller cannot open.
    let asset = match asset_in_repo(&state, &repo, asset_id).await {
        Ok(asset) => asset,
        Err(e) => return e.into_response(),
    };

    match rg_core::release::service::get_asset_attestation(&state.db, asset.id).await {
        Ok(Some(envelope)) => (StatusCode::OK, Json(serde_json::json!(envelope))).into_response(),
        Ok(None) => AppError::not_found("asset has no attestation").into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// POST /api/v1/repos/:owner/:name/releases/assets/:asset_id/attestation/verify
///
/// Verify the stored attestation against the instance key and the asset's
/// current bytes. Read permission. Returns `{ verified, reason, ... }`.
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/releases/assets/{asset_id}/attestation/verify",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("asset_id" = i64, Path, description = "asset_id"),
    ),
    responses(
        (status = 200, description = "Verification report", body = serde_json::Value),
        (status = 404, description = "Not found / feature disabled", body = serde_json::Value),
    ),
)]
pub async fn verify_asset_attestation(
    State(state): State<AppState>,
    Path((owner, name, asset_id)): Path<(String, String, i64)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    if !state.attestation_enabled {
        return AppError::not_found("attestation is not enabled").into_response();
    }

    let asset = match asset_in_repo(&state, &repo, asset_id).await {
        Ok(asset) => asset,
        Err(e) => return e.into_response(),
    };

    match rg_core::release::service::verify_asset_attestation(
        &state.db,
        asset.id,
        state.blob_storage.as_ref(),
        &state.repo_root,
        &owner,
        &name,
        &state.instance_key,
    )
    .await
    {
        Ok(report) => (StatusCode::OK, Json(serde_json::json!(report))).into_response(),
        // Verification reads the asset's current bytes, so an unreadable blob
        // store used to be reported as "no such attestation" — with the storage
        // path in the (unsanitized) 404 body.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// DELETE /api/v1/repos/:owner/:name/releases/assets/:asset_id
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/releases/assets/{asset_id}",
    tag = "Releases",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("asset_id" = i64, Path, description = "asset_id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_asset(
    State(state): State<AppState>,
    Path((owner, name, asset_id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    // `owner`/`name` below only build the storage path — on their own they never
    // constrain which asset row is deleted, so the id has to be anchored first.
    let asset = match asset_in_repo(&state, &repo, asset_id).await {
        Ok(asset) => asset,
        Err(e) => return e.into_response(),
    };

    match rg_core::release::service::delete_asset(
        &state.db,
        asset.id,
        state.blob_storage.as_ref(),
        &state.repo_root,
        &owner,
        &name,
    )
    .await
    {
        Ok(()) => (
            StatusCode::NO_CONTENT,
            Json(serde_json::json!({ "deleted": true })),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

#[cfg(test)]
mod release_upload_staging_tests {
    use super::*;
    use axum::body::Bytes;
    use std::convert::Infallible;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    #[test]
    fn production_upload_path_keeps_the_body_out_of_one_heap_buffer() {
        let source = include_str!("releases.rs");
        let production = rust_source::production_rust_code_with_doc_comments(source);
        let handler = production
            .split_once("pub async fn upload_asset(")
            .expect("release upload handler")
            .1
            .split_once("/// GET /api/v1/repos/:owner/:name/releases/assets/:asset_id")
            .expect("handler end marker")
            .0;

        assert!(handler.contains("stage_release_upload("));
        assert!(handler.contains("upload_asset_from_file("));
        assert!(
            !production.contains("to_bytes("),
            "release production code must not reintroduce full-body heap collection"
        );
    }

    #[tokio::test]
    async fn request_chunks_are_spooled_and_the_temporary_file_is_retired() {
        let root = tempfile::tempdir().unwrap();
        let body = Body::from_stream(futures::stream::iter([
            Ok::<_, Infallible>(Bytes::from_static(b"first-")),
            Ok::<_, Infallible>(Bytes::from_static(b"second")),
        ]));

        let staged = stage_release_upload(body, root.path(), 12).await.unwrap();
        let path = staged.path.to_path_buf();
        assert_eq!(staged.len, 12);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"first-second");
        drop(staged);
        assert!(!path.exists(), "TempPath must retire the release spool");
    }

    #[tokio::test]
    async fn chunked_transport_overflow_is_413_and_leaves_no_spool() {
        let root = tempfile::tempdir().unwrap();
        let chunks = futures::stream::iter([
            Ok::<_, Infallible>(Bytes::from_static(b"123")),
            Ok::<_, Infallible>(Bytes::from_static(b"45")),
        ]);
        let body = Body::from_stream(chunks);
        let limited = Body::new(http_body_util::Limited::new(body, 4));

        let error = stage_release_upload(limited, root.path(), 10)
            .await
            .unwrap_err();
        assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(error
            .to_string()
            .contains("configured 10-byte request limit"));
        assert_eq!(
            std::fs::read_dir(root.path().join(".tmp/release-uploads"))
                .unwrap()
                .count(),
            0,
            "a refused chunked upload left a spool behind"
        );
    }
}
