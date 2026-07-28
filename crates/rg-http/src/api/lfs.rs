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

use crate::api::repo_access;
use crate::error::AppError;
use crate::AppState;

/// Where LFS objects live and what has to be true about that directory. Shared
/// with the background compressor in `rg_core::lfs::service`, so a handler
/// failure and a maintenance-pass failure name the same directory the same way.
use rg_core::platform::fs::LFS_STORAGE_HINT;

#[derive(Debug, Default, Deserialize)]
pub struct LfsActionQuery {
    expires: Option<i64>,
    signature: Option<String>,
}

/// One actionable error for a filesystem failure on an LFS object path.
///
/// `lfs_root(repo_root, owner, repo)` builds the directory from the request but
/// never echoes it back, so a bare `io::Error` reaching the client is an errno
/// against a path only the server knows.
fn lfs_path_error(what: &str, path: &std::path::Path, error: &std::io::Error) -> AppError {
    AppError::internal(rg_core::platform::fs::describe_path_error(
        what,
        path,
        error,
        LFS_STORAGE_HINT,
    ))
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

/// The user behind this request's credentials, if any.
///
/// LFS answers "who is calling"; whether that caller may read or write the
/// repository is [`repo_access::check_read_for`] / [`check_write_for`]'s
/// decision, never this file's. An unauthenticated caller is `None` rather than
/// an early `401`: on a public repository a download needs no credentials at
/// all, and only the gate knows that.
fn actor_id(headers: &HeaderMap, state: &AppState) -> Option<i64> {
    crate::api::auth::extract_user_id(headers, &state.jwt_secret)
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
            Err(e) => return AppError::from(e).into_response(),
        };

    // Upload actions always require repository write access, including for
    // public repositories; a download requires whatever reading the repository
    // requires. Both answers come from the shared gate: an anonymous caller on
    // a private repository used to be turned away by the *credential* helper
    // rather than by the gate, which happened to produce the same 401 without
    // anything guaranteeing it would.
    let actor_id = actor_id(&headers, &state);
    let decision = if req.operation == "upload" {
        repo_access::check_write_for(&state, &repo_model, actor_id).await
    } else {
        repo_access::check_read_for(&state, &repo_model, actor_id).await
    };
    if let Err(error) = decision {
        return error.into_response();
    }

    let repo_id = repo_model.id;
    let lfs_root = rg_core::lfs::service::lfs_root(&state.repo_root, &owner, &repo);

    // Prefer the configured public URL so signed actions retain HTTPS and the
    // externally visible host when ForgeKeep runs behind a reverse proxy.
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
        Err(e) => AppError::from(e).into_response(),
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
            Err(e) => return AppError::from(e).into_response(),
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
        let actor_id = actor_id(&headers, &state);
        if let Err(error) = repo_access::check_write_for(&state, &repo_model, actor_id).await {
            return error.into_response();
        }
    }

    let repo_id = repo_model.id;
    let lfs_root = rg_core::lfs::service::lfs_root(&state.repo_root, &owner, &repo);

    // Stream body to temp file. The directory creation used to be discarded
    // with `let _ =`, so an unwritable LFS root surfaced later as a failure to
    // *open* the temp file — pointing the operator at the file instead of at
    // the directory that actually has to be fixed.
    let temp_path = lfs_root.join(format!(".tmp_{}", oid));
    if let Some(parent) = temp_path.parent() {
        if let Err(error) = tokio::fs::create_dir_all(parent).await {
            return lfs_path_error("LFS object directory", parent, &error).into_response();
        }
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
                Err(e) => AppError::from(e).into_response(),
            }
        }
        Err(e) => AppError::from(e).into_response(),
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
            Err(e) => return AppError::from(e).into_response(),
        };

    if let Err(response) = authorize_lfs_download(&state, &repo_model, &oid, &query, &headers).await
    {
        return response;
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
                stream_compressed_lfs_object(file_path).await
            } else {
                stream_uncompressed_lfs_object(&file_path).await
            }
        }
        Ok(rg_core::lfs::service::LfsObjectSource::Bytes { data, compressed }) => {
            respond_with_lfs_bytes(data, compressed, state.git_idle_timeout_secs)
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            [(axum::http::header::CONTENT_TYPE, "text/plain")],
            e.to_string().into_bytes(),
        )
            .into_response(),
    }
}

/// Enforce download authorization: a valid signed action URL bypasses auth,
/// otherwise the repository's own read gate decides.
async fn authorize_lfs_download(
    state: &AppState,
    repo_model: &rg_db::entities::repository::Model,
    oid: &str,
    query: &LfsActionQuery,
    headers: &HeaderMap,
) -> Result<(), axum::response::Response> {
    let signed = match verify_signed_action(
        state,
        repo_model.id,
        oid,
        rg_core::lfs::service::LfsActionKind::Download,
        query,
    ) {
        Ok(signed) => signed,
        Err(error) => return Err(error.into_response()),
    };
    if !signed {
        let actor_id = actor_id(headers, state);
        if let Err(error) = repo_access::check_read_for(state, repo_model, actor_id).await {
            return Err(error.into_response());
        }
    }
    Ok(())
}

/// Stream a zstd-compressed LFS object, decompressing on a blocking thread and
/// piping decoded chunks through a channel into the response body.
///
/// The file is opened *before* the response head is built. Opening it inside
/// the blocking task meant the `200` was already on the wire, so the only way
/// left to report the failure was an aborted body: `git lfs pull` saw a
/// truncated transfer and the operator saw nothing at all — no path, no errno.
/// A failure that is still reportable is now a 500 naming the file; the errors
/// that genuinely can only happen mid-stream are logged with the path before
/// they go into the body channel.
async fn stream_compressed_lfs_object(file_path: std::path::PathBuf) -> axum::response::Response {
    let file = match tokio::fs::File::open(&file_path).await {
        Ok(file) => file.into_std().await,
        Err(error) => {
            return lfs_path_error("LFS object file", &file_path, &error).into_response();
        }
    };

    // Stream-decompress via channel: spawn_blocking reads zstd chunks → channel → response body
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<axum::body::Bytes>>(8);

    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        // A body-stream error is logged by nobody: the client sees a truncated
        // transfer and hyper drops the cause, so this is the only place the
        // file can still be named.
        let aborted = |error: &std::io::Error| {
            let message = rg_core::platform::fs::describe_path_error(
                "LFS object file",
                &file_path,
                error,
                LFS_STORAGE_HINT,
            );
            tracing::error!(error = %message, "LFS object stream aborted");
            std::io::Error::other(message)
        };

        let decoder = match zstd::stream::Decoder::new(file) {
            Ok(d) => d,
            Err(error) => {
                if tx.blocking_send(Err(aborted(&error))).is_err() {
                    // Client disconnected before the stream error could be delivered.
                }
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
                Err(error) => {
                    if tx.blocking_send(Err(aborted(&error))).is_err() {
                        // Client disconnected before the stream error could be delivered.
                    }
                    break;
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let frame_stream = futures::StreamExt::map(stream, |item| item.map(http_body::Frame::data));
    let stream_body = http_body_util::StreamBody::new(frame_stream);
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        Body::new(stream_body),
    )
        .into_response()
}

/// Stream an uncompressed LFS object file directly from disk.
///
/// `read_object_source` already confirmed the object exists, so a failure here
/// is the storage under the server misbehaving — a stale handle, an unreadable
/// bind-mount, a file yanked between the check and the open. It stays a 500,
/// but it now names the file instead of handing `git lfs pull` a bare
/// "failed to open LFS object file" with the path and the errno both dropped.
async fn stream_uncompressed_lfs_object(file_path: &std::path::Path) -> axum::response::Response {
    match tokio::fs::File::open(file_path).await {
        Ok(file) => {
            // Size off the open handle: the second, *blocking* `std::fs::metadata`
            // this used to do sat on the async runtime and re-resolved a path
            // that could already have changed under it.
            let estimated_size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
            let stream = tokio_util::io::ReaderStream::new(file);
            let frame_stream =
                futures::StreamExt::map(stream, |item| item.map(http_body::Frame::data));
            let stream_body = http_body_util::StreamBody::new(frame_stream);
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
        Err(error) => lfs_path_error("LFS object file", file_path, &error).into_response(),
    }
}

/// Build the response for an in-memory LFS object, decompressing if needed.
///
/// This is the **remote (non-local) blob-backend** branch: `read_object_source`
/// hands back the whole object as a `Vec` (the local-path branch above streams
/// from disk instead). LFS objects can be very large, so the finished buffer is
/// served as a backpressure-sensitive, idle-guarded stream rather than a single
/// in-memory frame a slow/stalled client can pin until the kernel resets the
/// dead connection (card_444e03f1ca15). `Content-Length` lets clients spot an
/// idle-aborted short read.
fn respond_with_lfs_bytes(
    data: Vec<u8>,
    compressed: bool,
    idle_secs: u64,
) -> axum::response::Response {
    let body = if compressed {
        match zstd::stream::decode_all(std::io::Cursor::new(data)) {
            Ok(decoded) => decoded,
            Err(error) => return AppError::internal(error).into_response(),
        }
    } else {
        data
    };
    let len = body.len();
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
            (axum::http::header::CONTENT_LENGTH, len.to_string().as_str()),
        ],
        crate::http_stream::buffered_body_with_idle(body, idle_secs),
    )
        .into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Stream an Axum `Body` to a file. Returns the number of bytes written.
///
/// This is the write path of every `git lfs push`: the staging path is derived
/// from `repo_root` plus the object id, so a bare `?` on the io error hands the
/// client an errno and nothing else. Every failure names the file.
async fn write_body_to_file(body: Body, path: &std::path::Path) -> anyhow::Result<usize> {
    let staged = |error: &std::io::Error| {
        rg_core::platform::fs::path_error("LFS staging file", path, error, LFS_STORAGE_HINT)
    };

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|error| staged(&error))?;

    use futures::StreamExt;
    let mut written: usize = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let data = chunk.map_err(|e| anyhow::anyhow!("body stream error: {}", e))?;
        file.write_all(&data)
            .await
            .map_err(|error| staged(&error))?;
        written += data.len();
    }

    Ok(written)
}

#[cfg(test)]
mod staging_path_tests {
    use super::*;

    /// Every `git lfs push` stages its object at
    /// `<owner>.lfs/<repo>/.tmp_<oid>`. A bare `?` on the io error left the
    /// client and the log with an errno against a path only the server computes.
    #[tokio::test]
    async fn staging_failure_names_the_file_and_the_remedy() {
        let temp = tempfile::tempdir().unwrap();
        // A regular file where `<owner>.lfs/` belongs fails the open
        // deterministically, independent of the uid the tests run as.
        let blocker = temp.path().join("owner.lfs");
        std::fs::write(&blocker, "not a directory").unwrap();
        let staged = blocker.join("repo").join(".tmp_abc");

        let error = write_body_to_file(Body::from("payload"), &staged)
            .await
            .expect_err("staging must fail when the LFS root is not a directory");
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&staged.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("LFS staging file"), "{rendered}");
        assert!(rendered.contains("<owner>.lfs/<repo>/"), "{rendered}");
    }

    /// The upload handler used to discard the result of creating this
    /// directory, so an unwritable LFS root was reported one step later against
    /// the temp *file* — sending the operator after the wrong path.
    #[test]
    fn directory_failure_names_the_directory_not_the_object() {
        let temp = tempfile::tempdir().unwrap();
        let blocker = temp.path().join("owner.lfs");
        std::fs::write(&blocker, "not a directory").unwrap();
        let directory = blocker.join("repo");
        let error = std::fs::create_dir_all(&directory).unwrap_err();

        let AppError::InternalError(rendered) =
            lfs_path_error("LFS object directory", &directory, &error)
        else {
            panic!("a filesystem failure on the LFS root must stay a 500");
        };

        assert!(
            rendered.contains(&directory.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("LFS object directory"), "{rendered}");
        assert!(rendered.contains("<owner>.lfs/<repo>/"), "{rendered}");
    }

    /// `read_object_source` confirms the object exists before handing back a
    /// local path, so an open that still fails is the storage misbehaving —
    /// a 500, not a 404, and it has to survive as a *status* rather than as a
    /// truncated body.
    #[tokio::test]
    async fn a_compressed_object_that_cannot_be_opened_is_a_500_not_a_broken_200() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("owner.lfs").join("repo").join("abc.zst");

        let response = stream_compressed_lfs_object(missing).await;

        // Opening inside the blocking task committed the `200` first, leaving
        // an aborted body as the only channel for the failure.
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn an_uncompressed_object_that_cannot_be_opened_stays_a_500() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("owner.lfs").join("repo").join("abc");

        let response = stream_uncompressed_lfs_object(&missing).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
