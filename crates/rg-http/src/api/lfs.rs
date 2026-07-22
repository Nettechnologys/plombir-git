//! Git LFS REST API endpoints.
//!
//! Implements the LFS batch API and object upload/download endpoints.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::api::auth::extract_bearer_claims;
use crate::error::AppError;
use crate::AppState;

#[derive(Debug, Default, Deserialize)]
pub struct LfsActionQuery {
    expires: Option<i64>,
    signature: Option<String>,
}

fn verify_signed_action(
    state: &AppState,
    repo_id: i64,
    oid: &str,
    action: rg_core::lfs::service::LfsActionKind,
    query: &LfsActionQuery,
) -> Result<bool, AppError> {
    match (&query.expires, &query.signature) {
        (None, None) => Ok(false),
        (Some(expires), Some(signature)) => {
            match rg_core::lfs::service::verify_action_url(
                state.jwt_secret.as_bytes(),
                action,
                repo_id,
                oid,
                *expires,
                signature,
                chrono::Utc::now().timestamp(),
            ) {
                Ok(()) => Ok(true),
                Err(rg_core::lfs::service::LfsActionSignatureError::Expired) => {
                    Err(AppError::gone("LFS action URL has expired"))
                }
                Err(rg_core::lfs::service::LfsActionSignatureError::Invalid) => {
                    Err(AppError::forbidden("invalid LFS action URL signature"))
                }
            }
        }
        _ => Err(AppError::forbidden("incomplete LFS action URL signature")),
    }
}

fn authenticated_user_id(headers: &HeaderMap, state: &AppState) -> Result<i64, AppError> {
    let claims = extract_bearer_claims(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    claims
        .sub
        .parse::<i64>()
        .map_err(|_| AppError::unauthorized("invalid token subject"))
}

/// LFS batch API: POST /repos/:owner/:name/lfs/objects/batch
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/lfs/objects/batch",
    tag = "LFS",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn batch(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(req): Json<rg_core::lfs::service::LfsBatchRequest>,
) -> impl IntoResponse {
    // LFS client sends Accept: application/vnd.git-lfs+json
    let repo_model =
        match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &repo).await {
            Ok(Some(r)) => r,
            Ok(None) => return AppError::not_found("repository not found").into_response(),
            Err(e) => return AppError::internal(e).into_response(),
        };

    // Upload actions always require repository write access, including for
    // public repositories. Downloads only require auth for private repos.
    if req.operation == "upload" {
        let user_id = match authenticated_user_id(&headers, &state) {
            Ok(user_id) => user_id,
            Err(error) => return error.into_response(),
        };
        match rg_core::repo::service::can_write_repo(&state.db, &repo_model, Some(user_id)).await {
            Ok(true) => {}
            Ok(false) => return AppError::forbidden("write access denied").into_response(),
            Err(error) => return AppError::internal(error).into_response(),
        }
    } else if req.operation == "download" && repo_model.is_private {
        let user_id = match authenticated_user_id(&headers, &state) {
            Ok(user_id) => user_id,
            Err(error) => return error.into_response(),
        };
        match rg_core::repo::service::can_read_repo(&state.db, &repo_model, Some(user_id)).await {
            Ok(true) => {}
            Ok(false) => return AppError::forbidden("access denied").into_response(),
            Err(error) => return AppError::internal(error).into_response(),
        }
    }

    let repo_id = repo_model.id;
    let lfs_root = rg_core::lfs::service::lfs_root(&state.repo_root, &owner, &repo);

    // Prefer the configured public URL so signed actions retain HTTPS and the
    // externally visible host when IronForge runs behind a reverse proxy.
    let base_url = state
        .external_url
        .as_deref()
        .map(|url| url.trim_end_matches('/').to_string())
        .or_else(|| {
            headers
                .get("host")
                .and_then(|value| value.to_str().ok())
                .map(|host| format!("http://{host}"))
        })
        .unwrap_or_else(|| "http://localhost:8080".to_string());

    match rg_core::lfs::service::batch(
        &state.db,
        repo_id,
        state.blob_storage.as_ref(),
        &lfs_root,
        &base_url,
        &owner,
        &repo,
        &req,
        state.jwt_secret.as_bytes(),
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::json!(resp))).into_response(),
        Err(e) => AppError::internal(e).into_response(),
    }
}

/// Upload an LFS object: PUT /repos/:owner/:name/lfs/objects/:oid
/// Streams the request body directly to a temp file, then stream-compresses
/// it with zstd — never buffers the entire object in memory.
#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/lfs/objects/{oid}",
    tag = "LFS",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("oid" = String, Path, description = "oid"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn upload_object(
    State(state): State<AppState>,
    Path((owner, repo, oid)): Path<(String, String, String)>,
    Query(query): Query<LfsActionQuery>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    if !rg_core::lfs::service::is_valid_oid(&oid) {
        return AppError::bad_request("invalid LFS object identifier").into_response();
    }
    let repo_model =
        match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &repo).await {
            Ok(Some(r)) => r,
            Ok(None) => return AppError::not_found("repository not found").into_response(),
            Err(e) => return AppError::internal(e).into_response(),
        };

    let signed = match verify_signed_action(
        &state,
        repo_model.id,
        &oid,
        rg_core::lfs::service::LfsActionKind::Upload,
        &query,
    ) {
        Ok(signed) => signed,
        Err(error) => return error.into_response(),
    };
    if !signed {
        let user_id = match authenticated_user_id(&headers, &state) {
            Ok(user_id) => user_id,
            Err(error) => return error.into_response(),
        };
        match rg_core::repo::service::can_write_repo(&state.db, &repo_model, Some(user_id)).await {
            Ok(true) => {}
            Ok(false) => return AppError::forbidden("write access denied").into_response(),
            Err(error) => return AppError::internal(error).into_response(),
        }
    }

    let repo_id = repo_model.id;
    let lfs_root = rg_core::lfs::service::lfs_root(&state.repo_root, &owner, &repo);

    // Stream body to temp file
    let temp_path = lfs_root.join(format!(".tmp_{}", oid));
    if let Some(parent) = temp_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    match write_body_to_file(body, &temp_path).await {
        Ok(written) => {
            match rg_core::lfs::service::store_object_from_file(
                &state.db,
                repo_id,
                state.blob_storage.as_ref(),
                &owner,
                &repo,
                &oid,
                &temp_path,
                written as i64,
            )
            .await
            {
                Ok(()) => StatusCode::OK.into_response(),
                Err(e) => AppError::internal(e).into_response(),
            }
        }
        Err(e) => AppError::internal(e).into_response(),
    }
}

/// Download an LFS object: GET /repos/:owner/:name/lfs/objects/:oid
/// Streams the object, decompressing on the fly if compressed.
/// For compressed objects, uses spawn_blocking + channel for streaming
/// zstd decompression without blocking the async runtime.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/lfs/objects/{oid}",
    tag = "LFS",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("oid" = String, Path, description = "oid"),
    ),
    responses(
        (status = 200, description = "Success", content_type = "application/octet-stream"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn download_object(
    State(state): State<AppState>,
    Path((owner, repo, oid)): Path<(String, String, String)>,
    Query(query): Query<LfsActionQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !rg_core::lfs::service::is_valid_oid(&oid) {
        return AppError::bad_request("invalid LFS object identifier").into_response();
    }
    // H-01: Auth check for private repos
    let repo_model =
        match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &repo).await {
            Ok(Some(r)) => r,
            Ok(None) => return AppError::not_found("repository not found").into_response(),
            Err(e) => return AppError::internal(e).into_response(),
        };

    let signed = match verify_signed_action(
        &state,
        repo_model.id,
        &oid,
        rg_core::lfs::service::LfsActionKind::Download,
        &query,
    ) {
        Ok(signed) => signed,
        Err(error) => return error.into_response(),
    };
    if !signed && repo_model.is_private {
        let user_id = match authenticated_user_id(&headers, &state) {
            Ok(user_id) => user_id,
            Err(error) => return error.into_response(),
        };
        match rg_core::repo::service::can_read_repo(&state.db, &repo_model, Some(user_id)).await {
            Ok(true) => {}
            Ok(false) => return AppError::forbidden("access denied").into_response(),
            Err(error) => return AppError::internal(error).into_response(),
        }
    }

    let lfs_root = rg_core::lfs::service::lfs_root(&state.repo_root, &owner, &repo);

    match rg_core::lfs::service::read_object_source(
        state.blob_storage.as_ref(),
        &lfs_root,
        &owner,
        &repo,
        &oid,
    )
    .await
    {
        Ok(rg_core::lfs::service::LfsObjectSource::Local {
            path: file_path,
            compressed: is_compressed,
        }) => {
            if is_compressed {
                // Stream-decompress via channel: spawn_blocking reads zstd chunks → channel → response body
                let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<axum::body::Bytes>>(8);
                let path_for_thread = file_path.clone();

                tokio::task::spawn_blocking(move || {
                    use std::io::Read;
                    let file = match std::fs::File::open(&path_for_thread) {
                        Ok(f) => f,
                        Err(e) => {
                            let _ = tx.blocking_send(Err(e));
                            return;
                        }
                    };
                    let decoder = match zstd::stream::Decoder::new(file) {
                        Ok(d) => d,
                        Err(e) => {
                            let _ = tx.blocking_send(Err(std::io::Error::other(e.to_string())));
                            return;
                        }
                    };
                    let mut reader = std::io::BufReader::with_capacity(64 * 1024, decoder);
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if tx
                                    .blocking_send(Ok(axum::body::Bytes::from(buf[..n].to_vec())))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(e) => {
                                let _ = tx.blocking_send(Err(e));
                                break;
                            }
                        }
                    }
                });

                let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
                let frame_stream =
                    futures::StreamExt::map(stream, |item| item.map(http_body::Frame::data));
                let stream_body = http_body_util::StreamBody::new(frame_stream);
                (
                    StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                    Body::new(stream_body),
                )
                    .into_response()
            } else {
                // Uncompressed file — stream directly
                match tokio::fs::File::open(&file_path).await {
                    Ok(file) => {
                        let stream = tokio_util::io::ReaderStream::new(file);
                        let frame_stream = futures::StreamExt::map(stream, |item| {
                            item.map(http_body::Frame::data)
                        });
                        let stream_body = http_body_util::StreamBody::new(frame_stream);
                        let estimated_size =
                            std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);
                        (
                            StatusCode::OK,
                            [
                                (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
                                (
                                    axum::http::header::CONTENT_LENGTH,
                                    estimated_size.to_string().as_str(),
                                ),
                            ],
                            Body::new(stream_body),
                        )
                            .into_response()
                    }
                    Err(_) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "failed to open LFS object file",
                    )
                        .into_response(),
                }
            }
        }
        Ok(rg_core::lfs::service::LfsObjectSource::Bytes { data, compressed }) => {
            let body = if compressed {
                match zstd::stream::decode_all(std::io::Cursor::new(data)) {
                    Ok(decoded) => decoded,
                    Err(error) => return AppError::internal(error).into_response(),
                }
            } else {
                data
            };
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                body,
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            [(axum::http::header::CONTENT_TYPE, "text/plain")],
            e.to_string().into_bytes(),
        )
            .into_response(),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Stream an Axum `Body` to a file. Returns the number of bytes written.
async fn write_body_to_file(body: Body, path: &std::path::Path) -> anyhow::Result<usize> {
    let mut file = tokio::fs::File::create(path).await?;

    use futures::StreamExt;
    let mut written: usize = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let data = chunk.map_err(|e| anyhow::anyhow!("body stream error: {}", e))?;
        file.write_all(&data).await?;
        written += data.len();
    }

    Ok(written)
}
