//! Issue, pull-request and comment attachment APIs.

use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{body::Body, Json};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::api::auth::extract_user_id;
use crate::api::repo_access;
use crate::error::AppError;
use crate::AppState;
use rg_core::attachment::AttachmentTarget;

#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AttachmentResponse {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub size: i64,
    pub content_type: String,
    pub download_count: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub browser_download_url: String,
    /// Hex-encoded SHA-256 of the bytes, recorded at upload. `None` for legacy
    /// attachments uploaded before digest tracking existed.
    pub sha256: Option<String>,
}

#[derive(Clone, Copy)]
enum TargetKind {
    Issue,
    PullRequest,
    IssueComment,
    ReviewComment,
}

struct ResolvedTarget {
    target: AttachmentTarget,
    author_id: i64,
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/issues/{number}/assets", tag = "Attachments", responses((status = 200, body = serde_json::Value)))]
pub async fn list_issue_attachments(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: HeaderMap,
) -> Response {
    list(&state, &headers, &owner, &repo, TargetKind::Issue, number).await
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/issues/{number}/assets", tag = "Attachments", responses((status = 201, body = serde_json::Value)))]
pub async fn create_issue_attachment(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    create(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::Issue,
        number,
        query,
        multipart,
    )
    .await
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}", tag = "Attachments", responses((status = 200, body = Vec<u8>)))]
pub async fn get_issue_attachment(
    State(state): State<AppState>,
    Path((owner, repo, number, attachment_id)): Path<(String, String, i64, i64)>,
    headers: HeaderMap,
) -> Response {
    download(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::Issue,
        number,
        attachment_id,
    )
    .await
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}", tag = "Attachments", responses((status = 204)))]
pub async fn delete_issue_attachment(
    State(state): State<AppState>,
    Path((owner, repo, number, attachment_id)): Path<(String, String, i64, i64)>,
    headers: HeaderMap,
) -> Response {
    delete(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::Issue,
        number,
        attachment_id,
    )
    .await
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/pulls/{number}/assets", tag = "Attachments", responses((status = 200, body = serde_json::Value)))]
pub async fn list_pull_request_attachments(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: HeaderMap,
) -> Response {
    list(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::PullRequest,
        number,
    )
    .await
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/pulls/{number}/assets", tag = "Attachments", responses((status = 201, body = serde_json::Value)))]
pub async fn create_pull_request_attachment(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    create(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::PullRequest,
        number,
        query,
        multipart,
    )
    .await
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}", tag = "Attachments", responses((status = 200, body = Vec<u8>)))]
pub async fn get_pull_request_attachment(
    State(state): State<AppState>,
    Path((owner, repo, number, attachment_id)): Path<(String, String, i64, i64)>,
    headers: HeaderMap,
) -> Response {
    download(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::PullRequest,
        number,
        attachment_id,
    )
    .await
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}", tag = "Attachments", responses((status = 204)))]
pub async fn delete_pull_request_attachment(
    State(state): State<AppState>,
    Path((owner, repo, number, attachment_id)): Path<(String, String, i64, i64)>,
    headers: HeaderMap,
) -> Response {
    delete(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::PullRequest,
        number,
        attachment_id,
    )
    .await
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/issues/comments/{comment_id}/assets", tag = "Attachments", responses((status = 200, body = serde_json::Value)))]
pub async fn list_issue_comment_attachments(
    State(state): State<AppState>,
    Path((owner, repo, comment_id)): Path<(String, String, i64)>,
    headers: HeaderMap,
) -> Response {
    list(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::IssueComment,
        comment_id,
    )
    .await
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/issues/comments/{comment_id}/assets", tag = "Attachments", responses((status = 201, body = serde_json::Value)))]
pub async fn create_issue_comment_attachment(
    State(state): State<AppState>,
    Path((owner, repo, comment_id)): Path<(String, String, i64)>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    create(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::IssueComment,
        comment_id,
        query,
        multipart,
    )
    .await
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}", tag = "Attachments", responses((status = 200, body = Vec<u8>)))]
pub async fn get_issue_comment_attachment(
    State(state): State<AppState>,
    Path((owner, repo, comment_id, attachment_id)): Path<(String, String, i64, i64)>,
    headers: HeaderMap,
) -> Response {
    download(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::IssueComment,
        comment_id,
        attachment_id,
    )
    .await
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}", tag = "Attachments", responses((status = 204)))]
pub async fn delete_issue_comment_attachment(
    State(state): State<AppState>,
    Path((owner, repo, comment_id, attachment_id)): Path<(String, String, i64, i64)>,
    headers: HeaderMap,
) -> Response {
    delete(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::IssueComment,
        comment_id,
        attachment_id,
    )
    .await
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets", tag = "Attachments", responses((status = 200, body = serde_json::Value)))]
pub async fn list_review_comment_attachments(
    State(state): State<AppState>,
    Path((owner, repo, comment_id)): Path<(String, String, i64)>,
    headers: HeaderMap,
) -> Response {
    list(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::ReviewComment,
        comment_id,
    )
    .await
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets", tag = "Attachments", responses((status = 201, body = serde_json::Value)))]
pub async fn create_review_comment_attachment(
    State(state): State<AppState>,
    Path((owner, repo, comment_id)): Path<(String, String, i64)>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Response {
    create(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::ReviewComment,
        comment_id,
        query,
        multipart,
    )
    .await
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}", tag = "Attachments", responses((status = 200, body = Vec<u8>)))]
pub async fn get_review_comment_attachment(
    State(state): State<AppState>,
    Path((owner, repo, comment_id, attachment_id)): Path<(String, String, i64, i64)>,
    headers: HeaderMap,
) -> Response {
    download(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::ReviewComment,
        comment_id,
        attachment_id,
    )
    .await
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}", tag = "Attachments", responses((status = 204)))]
pub async fn delete_review_comment_attachment(
    State(state): State<AppState>,
    Path((owner, repo, comment_id, attachment_id)): Path<(String, String, i64, i64)>,
    headers: HeaderMap,
) -> Response {
    delete(
        &state,
        &headers,
        &owner,
        &repo,
        TargetKind::ReviewComment,
        comment_id,
        attachment_id,
    )
    .await
}

async fn list(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo_name: &str,
    kind: TargetKind,
    target_id: i64,
) -> Response {
    let (repo, target) = match resolve(state, headers, owner, repo_name, kind, target_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match rg_core::attachment::list_attachments(&state.db, repo.id, target.target).await {
        Ok(attachments) => Json(
            attachments
                .into_iter()
                .map(|attachment| response(owner, repo_name, kind, target_id, attachment))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// Where an in-flight attachment is staged and what has to be true about it.
const ATTACHMENT_STAGING_HINT: &str =
    "attachment uploads are staged in `.tmp/attachments/` under the `[server].repo_root` \
     directory; that directory must be writable by the user running forgekeep";

/// One actionable error for a filesystem failure while staging an upload.
///
/// The staging path is `repo_root/.tmp/attachments/<uuid>.upload` — built inside
/// the handler from a freshly generated UUID, so it appears in neither the
/// request nor the response. A bare `io::Error` leaves the operator with an
/// errno and no directory to act on, which is precisely what a repo root
/// bind-mounted from a host uid other than the container's produces.
fn upload_path_error(what: &str, path: &std::path::Path, error: &std::io::Error) -> AppError {
    AppError::internal(rg_core::platform::fs::describe_path_error(
        what,
        path,
        error,
        ATTACHMENT_STAGING_HINT,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn create(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo_name: &str,
    kind: TargetKind,
    target_id: i64,
    query: UploadQuery,
    mut multipart: Multipart,
) -> Response {
    let user_id = match extract_user_id(headers, &state.jwt_secret) {
        Some(id) => id,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let (repo, target) = match resolve(state, headers, owner, repo_name, kind, target_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    // A check that could not run is not a check that said "no": while the
    // database was down `unwrap_or(false)` answered 403 to the repository's own
    // writers, so the failure read as the caller's fault.
    let can_write =
        match rg_core::repo::service::can_write_repo(&state.db, &repo, Some(user_id)).await {
            Ok(allowed) => allowed,
            Err(error) => return AppError::from(error).into_response(),
        };
    if user_id != target.author_id && !can_write {
        return AppError::forbidden("write access denied").into_response();
    }

    let mut field = loop {
        match multipart.next_field().await {
            Ok(Some(field)) if field.name() == Some("attachment") => break field,
            Ok(Some(_)) => continue,
            Ok(None) => return AppError::bad_request("missing attachment field").into_response(),
            Err(error) => return AppError::bad_request(error).into_response(),
        }
    };
    let filename = query
        .name
        .or_else(|| field.file_name().map(str::to_string))
        .unwrap_or_default();
    let content_type = field
        .content_type()
        .map(str::to_string)
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let upload_dir = state.repo_root.join(".tmp").join("attachments");
    if let Err(error) = tokio::fs::create_dir_all(&upload_dir).await {
        return upload_path_error("attachment staging directory", &upload_dir, &error)
            .into_response();
    }
    let upload_path = upload_dir.join(format!("{}.upload", uuid::Uuid::new_v4()));
    let mut upload = match tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&upload_path)
        .await
    {
        Ok(upload) => upload,
        Err(error) => {
            return upload_path_error("attachment staging file", &upload_path, &error)
                .into_response();
        }
    };
    let mut size = 0_u64;
    loop {
        let chunk = match field.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                drop(upload);
                let _ = tokio::fs::remove_file(&upload_path).await;
                return AppError::bad_request(error).into_response();
            }
        };
        size = match size.checked_add(chunk.len() as u64) {
            Some(size) if size <= rg_core::attachment::MAX_ATTACHMENT_SIZE as u64 => size,
            _ => {
                drop(upload);
                let _ = tokio::fs::remove_file(&upload_path).await;
                return AppError::bad_request("attachment exceeds the 100 MiB file limit")
                    .into_response();
            }
        };
        if let Err(error) = upload.write_all(&chunk).await {
            drop(upload);
            let _ = tokio::fs::remove_file(&upload_path).await;
            return upload_path_error("attachment staging file", &upload_path, &error)
                .into_response();
        }
    }
    if let Err(error) = upload.flush().await {
        drop(upload);
        let _ = tokio::fs::remove_file(&upload_path).await;
        return upload_path_error("attachment staging file", &upload_path, &error).into_response();
    }
    drop(upload);

    let result = rg_core::attachment::create_attachment_from_file(
        &state.db,
        state.blob_storage.as_ref(),
        repo.id,
        user_id,
        target.target,
        &filename,
        &content_type,
        &upload_path,
        size,
    )
    .await;
    let _ = tokio::fs::remove_file(&upload_path).await;
    match result {
        Ok(attachment) => (
            StatusCode::CREATED,
            Json(response(owner, repo_name, kind, target_id, attachment)),
        )
            .into_response(),
        // Publishing the staged file is where the blob store, the quota query
        // and the filename rules all fail, and only the last of those is the
        // uploader's fault. `AppError::from` classifies on the typed
        // `InvalidRequest` / `NotFound` the service attaches, so an unwritable
        // `repo_root` is a retryable 500 instead of a 400 nobody retries — the
        // staging half of this handler already made that distinction.
        Err(error) => AppError::from(error).into_response(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn download(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo_name: &str,
    kind: TargetKind,
    target_id: i64,
    attachment_id: i64,
) -> Response {
    let (repo, target) = match resolve(state, headers, owner, repo_name, kind, target_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match rg_core::attachment::get_attachment(&state.db, repo.id, target.target, attachment_id)
        .await
    {
        Ok(attachment) => match stream_attachment(state, &attachment).await {
            Ok(response) => response,
            Err(error) => AppError::from(error).into_response(),
        },
        // `get_attachment` carries `rg_core::error::NotFound` for a row that
        // genuinely is not there, so `AppError::from` produces the same 404
        // without matching on the rendered message.
        Err(error) => AppError::from(error).into_response(),
    }
}

/// One actionable error for a filesystem failure on a stored attachment.
///
/// The download half is the mirror of [`upload_path_error`]: `local_path`
/// resolves the file from the row's `blob_key` inside the storage backend, so
/// the path exists nowhere in the request or the response. A bare `?` on the
/// open handed the operator an errno against a file only the server can name —
/// exactly what a repo_root bind-mounted from a foreign uid produces.
fn download_path_error(path: &std::path::Path, error: &std::io::Error) -> anyhow::Error {
    rg_core::platform::fs::path_error(
        "attachment file",
        path,
        error,
        rg_core::platform::fs::BLOB_STORAGE_HINT,
    )
}

async fn stream_attachment(
    state: &AppState,
    attachment: &rg_db::entities::attachment::Model,
) -> anyhow::Result<Response> {
    let key = rg_core::blob_storage::BlobKey::new(attachment.blob_key.clone())?;
    let mut response = if let Some(path) = state.blob_storage.local_path(&key) {
        let file = tokio::fs::File::open(&path)
            .await
            .map_err(|error| download_path_error(&path, &error))?;
        let stream = ReaderStream::new(file);
        Response::new(Body::from_stream(stream))
    } else {
        // Remote (non-local) blob backend returns the whole attachment as a
        // `Vec`; the local-path branch above streams from disk. Serve the buffer
        // as a backpressure-sensitive, idle-guarded stream so a slow/stalled
        // client can't pin the attachment-sized `Vec` in server memory until the
        // kernel resets the dead connection (card_444e03f1ca15).
        let data = state.blob_storage.get(&key).await?;
        Response::new(crate::http_stream::buffered_body_with_idle(
            data,
            state.git_idle_timeout_secs,
        ))
    };
    rg_db::ops::attachment_ops::increment_download_count(&state.db, attachment.id).await?;

    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&attachment.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(value) = HeaderValue::from_str(&attachment.size.to_string()) {
        response.headers_mut().insert(header::CONTENT_LENGTH, value);
    }
    let safe_name = attachment.filename.replace(['"', '\\'], "_");
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{safe_name}\"")) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    // Expose the upload-time digest so clients can verify the payload end-to-end.
    // The body is streamed (never fully buffered here), so verification is the
    // client's job; the digest recorded at upload is the trust anchor.
    if let Some(sha) = attachment.sha256.as_deref() {
        if let Ok(value) = HeaderValue::from_str(sha) {
            response
                .headers_mut()
                .insert(header::HeaderName::from_static("x-checksum-sha256"), value);
        }
    }
    Ok(response)
}

#[allow(clippy::too_many_arguments)]
async fn delete(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo_name: &str,
    kind: TargetKind,
    target_id: i64,
    attachment_id: i64,
) -> Response {
    let user_id = match extract_user_id(headers, &state.jwt_secret) {
        Some(id) => id,
        None => return AppError::unauthorized("authentication required").into_response(),
    };
    let (repo, target) = match resolve(state, headers, owner, repo_name, kind, target_id).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    // A check that could not run is not a check that said "no": while the
    // database was down `unwrap_or(false)` answered 403 to the repository's own
    // writers, so the failure read as the caller's fault.
    let can_write =
        match rg_core::repo::service::can_write_repo(&state.db, &repo, Some(user_id)).await {
            Ok(allowed) => allowed,
            Err(error) => return AppError::from(error).into_response(),
        };
    if user_id != target.author_id && !can_write {
        return AppError::forbidden("write access denied").into_response();
    }
    match rg_core::attachment::delete_attachment(
        &state.db,
        state.blob_storage.as_ref(),
        repo.id,
        target.target,
        attachment_id,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

async fn resolve(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo_name: &str,
    kind: TargetKind,
    target_id: i64,
) -> Result<(rg_db::entities::repository::Model, ResolvedTarget), AppError> {
    let repo = repo_access::require_read(state, headers, owner, repo_name).await?;

    let target = match kind {
        TargetKind::Issue => {
            let issue =
                rg_db::ops::issue_ops::find_by_repo_and_number(&state.db, repo.id, target_id)
                    .await
                    .map_err(AppError::internal)?
                    .ok_or_else(|| AppError::not_found("issue not found"))?;
            ResolvedTarget {
                target: AttachmentTarget::Issue(issue.id),
                author_id: issue.author_id,
            }
        }
        TargetKind::PullRequest => {
            let pull = rg_db::ops::pull_request_ops::find_by_repo_and_number(
                &state.db, repo.id, target_id,
            )
            .await
            .map_err(AppError::internal)?
            .ok_or_else(|| AppError::not_found("pull request not found"))?;
            ResolvedTarget {
                target: AttachmentTarget::PullRequest(pull.id),
                author_id: pull.author_id,
            }
        }
        TargetKind::IssueComment => {
            let comment = rg_db::ops::issue_comment_ops::find_by_id(&state.db, target_id)
                .await
                .map_err(AppError::internal)?
                .ok_or_else(|| AppError::not_found("comment not found"))?;
            let issue = rg_db::ops::issue_ops::find_by_id(&state.db, comment.issue_id)
                .await
                .map_err(AppError::internal)?
                .ok_or_else(|| AppError::not_found("comment not found"))?;
            if issue.repo_id != repo.id {
                return Err(AppError::not_found("comment not found"));
            }
            ResolvedTarget {
                target: AttachmentTarget::IssueComment(comment.id),
                author_id: comment.author_id,
            }
        }
        TargetKind::ReviewComment => {
            let comment = rg_db::ops::review_comment_ops::find_by_id(&state.db, target_id)
                .await
                .map_err(AppError::internal)?
                .ok_or_else(|| AppError::not_found("comment not found"))?;
            let pull = rg_db::ops::pull_request_ops::find_by_id(&state.db, comment.pr_id)
                .await
                .map_err(AppError::internal)?
                .ok_or_else(|| AppError::not_found("comment not found"))?;
            if pull.repo_id != repo.id {
                return Err(AppError::not_found("comment not found"));
            }
            ResolvedTarget {
                target: AttachmentTarget::ReviewComment(comment.id),
                author_id: comment.author_id,
            }
        }
    };
    Ok((repo, target))
}

fn response(
    owner: &str,
    repo: &str,
    kind: TargetKind,
    target_id: i64,
    attachment: rg_db::entities::attachment::Model,
) -> AttachmentResponse {
    let base = match kind {
        TargetKind::Issue => format!("/api/v1/repos/{owner}/{repo}/issues/{target_id}/assets"),
        TargetKind::PullRequest => format!("/api/v1/repos/{owner}/{repo}/pulls/{target_id}/assets"),
        TargetKind::IssueComment => {
            format!("/api/v1/repos/{owner}/{repo}/issues/comments/{target_id}/assets")
        }
        TargetKind::ReviewComment => {
            format!("/api/v1/repos/{owner}/{repo}/pulls/comments/{target_id}/assets")
        }
    };
    AttachmentResponse {
        id: attachment.id,
        uuid: attachment.uuid,
        name: attachment.filename,
        size: attachment.size,
        content_type: attachment.content_type,
        download_count: attachment.download_count,
        created_at: attachment.created_at,
        browser_download_url: format!("{base}/{}", attachment.id),
        sha256: attachment.sha256,
    }
}

#[cfg(test)]
mod upload_path_error_tests {
    use super::*;

    /// The staging path is `repo_root/.tmp/attachments/<uuid>.upload`, built
    /// from a freshly generated UUID inside the handler: it appears in neither
    /// the request nor the response, so an errno on its own is unactionable.
    #[test]
    fn staging_failure_names_the_directory_and_the_remedy() {
        let temp = tempfile::tempdir().unwrap();
        // A regular file where `.tmp/` belongs fails directory creation
        // deterministically, independent of the uid the tests run as.
        let blocker = temp.path().join(".tmp");
        std::fs::write(&blocker, "not a directory").unwrap();
        let upload_dir = blocker.join("attachments");
        let error = std::fs::create_dir_all(&upload_dir).unwrap_err();

        let AppError::InternalError(rendered) =
            upload_path_error("attachment staging directory", &upload_dir, &error)
        else {
            panic!("a filesystem failure while staging an upload must stay a 500");
        };

        assert!(
            rendered.contains(&upload_dir.display().to_string()),
            "{rendered}"
        );
        assert!(
            rendered.contains("attachment staging directory"),
            "{rendered}"
        );
        assert!(rendered.contains(".tmp/attachments/"), "{rendered}");
    }

    /// The download half: the stored file is resolved from the row's
    /// `blob_key`, so a bare `?` on the open left the operator with an errno
    /// and no file — the same blind spot the upload half already fixed.
    #[test]
    fn download_failure_names_the_file_and_the_remedy() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("attachments").join("gone.bin");
        let error = std::fs::File::open(&missing).unwrap_err();

        let rendered = format!("{:#}", download_path_error(&missing, &error));

        assert!(
            rendered.contains(&missing.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("attachment file"), "{rendered}");
        assert!(rendered.contains("[server].repo_root"), "{rendered}");
    }
}
