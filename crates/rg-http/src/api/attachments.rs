//! Issue, pull-request and comment attachment APIs.

use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{body::Body, Json};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::api::auth::extract_user_id;
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
        Err(error) => AppError::internal(error).into_response(),
    }
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
    let can_write = rg_core::repo::service::can_write_repo(&state.db, &repo, Some(user_id))
        .await
        .unwrap_or(false);
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
        return AppError::internal(error).into_response();
    }
    let upload_path = upload_dir.join(format!("{}.upload", uuid::Uuid::new_v4()));
    let mut upload = match tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&upload_path)
        .await
    {
        Ok(upload) => upload,
        Err(error) => return AppError::internal(error).into_response(),
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
            return AppError::internal(error).into_response();
        }
    }
    if let Err(error) = upload.flush().await {
        drop(upload);
        let _ = tokio::fs::remove_file(&upload_path).await;
        return AppError::internal(error).into_response();
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
        Err(error) => AppError::bad_request(error).into_response(),
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
            Err(error) => AppError::internal(error).into_response(),
        },
        Err(error) if error.to_string().contains("not found") => {
            AppError::not_found("attachment not found").into_response()
        }
        Err(error) => AppError::internal(error).into_response(),
    }
}

async fn stream_attachment(
    state: &AppState,
    attachment: &rg_db::entities::attachment::Model,
) -> anyhow::Result<Response> {
    let key = rg_core::blob_storage::BlobKey::new(attachment.blob_key.clone())?;
    let mut response = if let Some(path) = state.blob_storage.local_path(&key) {
        let file = tokio::fs::File::open(path).await?;
        let stream = ReaderStream::new(file);
        Response::new(Body::from_stream(stream))
    } else {
        Response::new(Body::from(state.blob_storage.get(&key).await?))
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
    let can_write = rg_core::repo::service::can_write_repo(&state.db, &repo, Some(user_id))
        .await
        .unwrap_or(false);
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
        Err(error) if error.to_string().contains("not found") => {
            AppError::not_found("attachment not found").into_response()
        }
        Err(error) => AppError::internal(error).into_response(),
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
    let repo = rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, repo_name)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("repository not found"))?;
    let user_id = extract_user_id(headers, &state.jwt_secret);
    if !rg_core::repo::service::can_read_repo(&state.db, &repo, user_id)
        .await
        .unwrap_or(false)
    {
        return Err(if user_id.is_some() {
            AppError::forbidden("access denied")
        } else {
            AppError::unauthorized("authentication required")
        });
    }

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
    }
}
