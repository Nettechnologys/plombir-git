//! REST API handlers for Issues and Issue Comments.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::api::auth::extract_bearer_claims;
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::AppState;

// ── Request / Response types ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateIssueRequest {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub milestone_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct UpdateIssueRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub assignee_id: Option<Option<i64>>,
    #[serde(default)]
    pub milestone_id: Option<Option<i64>>,
}

#[derive(Deserialize)]
pub struct CreateCommentRequest {
    pub body: String,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub state: Option<String>,
    #[serde(default)]
    pub labels: Option<String>,
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

#[derive(Serialize)]
pub struct IssueResponse {
    #[serde(flatten)]
    pub issue: rg_db::entities::issue::Model,
    pub author: Option<String>,
}

/// List valid Markdown issue templates from the repository default branch.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issue_templates",
    tag = "Issues",
    responses(
        (status = 200, description = "Gitea-compatible issue templates", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = serde_json::Value),
    ),
)]
pub async fn list_issue_templates(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let repo_model = match resolve_and_check_read_access(&state, &headers, &owner, &repo).await {
        Ok(repo) => repo,
        Err(error) => return error.into_response(),
    };
    let path = state.repo_root.join(format!("{owner}/{repo}.git"));
    let default_branch = repo_model.default_branch;
    match tokio::task::spawn_blocking(move || {
        rg_core::issue_template::discover_issue_templates(&path, &default_branch)
    })
    .await
    {
        Ok(Ok(discovery)) => {
            for (file, error) in discovery.errors {
                tracing::warn!(%file, %error, "ignored invalid issue template");
            }
            (StatusCode::OK, Json(discovery.templates)).into_response()
        }
        Ok(Err(error)) => AppError::internal(error).into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

/// Read `.gitea`/`.github` issue chooser configuration.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issue_config",
    tag = "Issues",
    responses((status = 200, body = serde_json::Value)),
)]
pub async fn get_issue_config(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let repo_model = match resolve_and_check_read_access(&state, &headers, &owner, &repo).await {
        Ok(repo) => repo,
        Err(error) => return error.into_response(),
    };
    let path = state.repo_root.join(format!("{owner}/{repo}.git"));
    let default_branch = repo_model.default_branch;
    match tokio::task::spawn_blocking(move || {
        rg_core::issue_template::read_issue_config(&path, &default_branch)
    })
    .await
    {
        Ok(Ok(config)) => (StatusCode::OK, Json(config)).into_response(),
        Ok(Err(error)) => AppError::bad_request(error).into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[derive(Serialize)]
pub struct IssueConfigValidation {
    valid: bool,
    message: String,
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issue_config/validate",
    tag = "Issues",
    responses((status = 200, body = serde_json::Value)),
)]
pub async fn validate_issue_config(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let repo_model = match resolve_and_check_read_access(&state, &headers, &owner, &repo).await {
        Ok(repo) => repo,
        Err(error) => return error.into_response(),
    };
    let path = state.repo_root.join(format!("{owner}/{repo}.git"));
    let default_branch = repo_model.default_branch;
    match tokio::task::spawn_blocking(move || {
        rg_core::issue_template::read_issue_config(&path, &default_branch)
    })
    .await
    {
        Ok(Ok(_)) => Json(IssueConfigValidation {
            valid: true,
            message: String::new(),
        })
        .into_response(),
        Ok(Err(error)) => Json(IssueConfigValidation {
            valid: false,
            message: error.to_string(),
        })
        .into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pull_request_template",
    tag = "Pull Requests",
    responses(
        (status = 200, body = serde_json::Value),
        (status = 204, description = "No pull request template"),
    ),
)]
pub async fn get_pull_request_template(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let repo_model = match resolve_and_check_read_access(&state, &headers, &owner, &repo).await {
        Ok(repo) => repo,
        Err(error) => return error.into_response(),
    };
    let path = state.repo_root.join(format!("{owner}/{repo}.git"));
    let default_branch = repo_model.default_branch;
    match tokio::task::spawn_blocking(move || {
        rg_core::issue_template::read_pull_request_template(&path, &default_branch)
    })
    .await
    {
        Ok(Ok(Some(template))) => (StatusCode::OK, Json(template)).into_response(),
        Ok(Ok(None)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(error)) => AppError::internal(error).into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[derive(Serialize)]
pub struct CommentResponse {
    #[serde(flatten)]
    pub comment: rg_db::entities::issue_comment::Model,
    pub author: Option<String>,
}

// ── Issue handlers ──────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_issues(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Query(params): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = resolve_and_check_read_access(&state, &headers, &owner, &repo).await {
        return e.into_response();
    }

    let state_filter = params.state.as_deref();
    let pagination = params.pagination.clamp();

    // If labels filter is present, use filtered query
    if let Some(ref labels_str) = params.labels {
        let label_names: Vec<String> = labels_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !label_names.is_empty() {
            return match rg_core::issue::list_issues_filtered_by_labels(
                &state.db,
                &owner,
                &repo,
                state_filter,
                &label_names,
                pagination.offset(),
                pagination.limit(),
            )
            .await
            {
                Ok((data, total)) => {
                    let data = issues_with_authors(&state.db, data).await;
                    (
                        StatusCode::OK,
                        Json(PaginatedResponse::new(data, &pagination, total as u64)),
                    )
                        .into_response()
                }
                Err(e) => {
                    tracing::error!(%e, "handler error");
                    AppError::internal(e).into_response()
                }
            };
        }
    }

    match rg_core::issue::list_issues_paginated(
        &state.db,
        &owner,
        &repo,
        state_filter,
        pagination.offset(),
        pagination.limit(),
    )
    .await
    {
        Ok((data, total)) => {
            let data = issues_with_authors(&state.db, data).await;
            (
                StatusCode::OK,
                Json(PaginatedResponse::new(data, &pagination, total as u64)),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues/{number}",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_issue(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = resolve_and_check_read_access(&state, &headers, &owner, &repo).await {
        return e.into_response();
    }

    match rg_core::issue::get_issue(&state.db, &owner, &repo, number).await {
        Ok(issue) => {
            let issue = issue_with_author(&state.db, issue).await;
            (StatusCode::OK, Json(issue)).into_response()
        }
        Err(e) => AppError::not_found(e.to_string()).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/issues",
    tag = "Issues",
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
pub async fn create_issue(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(req): Json<CreateIssueRequest>,
) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required".to_string()).into_response()
        }
    };

    let repo_model = match resolve_and_check_read_access(&state, &headers, &owner, &repo).await {
        Ok(repo) => repo,
        Err(e) => return e.into_response(),
    };

    match rg_core::issue::create_issue(
        &state.db,
        repo_model.id,
        user_id,
        req.title,
        req.body,
        req.labels,
        req.milestone_id,
    )
    .await
    {
        Ok(issue) => {
            let issue = issue_with_author(&state.db, issue).await;
            (StatusCode::CREATED, Json(issue)).into_response()
        }
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/issues/{number}",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_issue(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: HeaderMap,
    Json(req): Json<UpdateIssueRequest>,
) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required".to_string()).into_response()
        }
    };

    let existing = match rg_core::issue::get_issue(&state.db, &owner, &repo, number).await {
        Ok(issue) => issue,
        Err(e) => return AppError::not_found(e.to_string()).into_response(),
    };

    let repo_model = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &repo)
        .await
    {
        Ok(Some(repo)) => repo,
        Ok(None) => return AppError::not_found("repository not found".to_string()).into_response(),
        Err(e) => return AppError::internal(e).into_response(),
    };

    let can_write = rg_core::repo::service::can_write_repo(&state.db, &repo_model, Some(user_id))
        .await
        .unwrap_or(false);
    let can_read = rg_core::repo::service::can_read_repo(&state.db, &repo_model, Some(user_id))
        .await
        .unwrap_or(false);
    let touches_management_fields =
        req.labels.is_some() || req.assignee_id.is_some() || req.milestone_id.is_some();

    if !can_read {
        return AppError::forbidden("access denied").into_response();
    }

    if !can_write && (existing.author_id != user_id || touches_management_fields) {
        return AppError::forbidden("write access required").into_response();
    }

    match rg_core::issue::update_issue(
        &state.db,
        &owner,
        &repo,
        number,
        req.title,
        req.body,
        req.state,
        req.labels,
        req.assignee_id,
        req.milestone_id,
    )
    .await
    {
        Ok(issue) => {
            let issue = issue_with_author(&state.db, issue).await;
            (StatusCode::OK, Json(issue)).into_response()
        }
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

// ── Comment handlers ────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues/{number}/comments",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_comments(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = resolve_and_check_read_access(&state, &headers, &owner, &repo).await {
        return e.into_response();
    }

    match rg_core::issue::list_comments(&state.db, &owner, &repo, number).await {
        Ok(comments) => {
            let comments = comments_with_authors(&state.db, comments).await;
            (StatusCode::OK, Json(comments)).into_response()
        }
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/issues/{number}/comments",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn add_comment(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: HeaderMap,
    Json(req): Json<CreateCommentRequest>,
) -> impl IntoResponse {
    let user_id = match super::auth::extract_user_id(&headers, &state.jwt_secret) {
        Some(id) => id,
        None => {
            return AppError::unauthorized("authentication required".to_string()).into_response()
        }
    };

    if let Err(e) = resolve_and_check_read_access(&state, &headers, &owner, &repo).await {
        return e.into_response();
    }

    match rg_core::issue::add_comment(&state.db, &owner, &repo, number, user_id, req.body).await {
        Ok(comment) => {
            let comment = comment_with_author(&state.db, comment).await;
            (StatusCode::CREATED, Json(comment)).into_response()
        }
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

async fn author_name(
    db: &sea_orm::DatabaseConnection,
    cache: &mut HashMap<i64, Option<String>>,
    user_id: i64,
) -> Option<String> {
    if let Some(cached) = cache.get(&user_id) {
        return cached.clone();
    }

    let name = rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .ok()
        .flatten()
        .map(|user| user.username);
    cache.insert(user_id, name.clone());
    name
}

async fn issue_with_author(
    db: &sea_orm::DatabaseConnection,
    issue: rg_db::entities::issue::Model,
) -> IssueResponse {
    let mut cache = HashMap::new();
    let author = author_name(db, &mut cache, issue.author_id).await;
    IssueResponse { issue, author }
}

async fn issues_with_authors(
    db: &sea_orm::DatabaseConnection,
    issues: Vec<rg_db::entities::issue::Model>,
) -> Vec<IssueResponse> {
    let mut cache = HashMap::new();
    let mut responses = Vec::with_capacity(issues.len());
    for issue in issues {
        let author = author_name(db, &mut cache, issue.author_id).await;
        responses.push(IssueResponse { issue, author });
    }
    responses
}

async fn comment_with_author(
    db: &sea_orm::DatabaseConnection,
    comment: rg_db::entities::issue_comment::Model,
) -> CommentResponse {
    let mut cache = HashMap::new();
    let author = author_name(db, &mut cache, comment.author_id).await;
    CommentResponse { comment, author }
}

async fn comments_with_authors(
    db: &sea_orm::DatabaseConnection,
    comments: Vec<rg_db::entities::issue_comment::Model>,
) -> Vec<CommentResponse> {
    let mut cache = HashMap::new();
    let mut responses = Vec::with_capacity(comments.len());
    for comment in comments {
        let author = author_name(db, &mut cache, comment.author_id).await;
        responses.push(CommentResponse { comment, author });
    }
    responses
}

async fn resolve_and_check_read_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo_name: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    let repo = rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, repo_name)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("repository not found"))?;

    if repo.is_private {
        let claims = extract_bearer_claims(headers, &state.jwt_secret)
            .ok_or_else(|| AppError::unauthorized("authentication required"))?;
        let user_id = claims
            .sub
            .parse::<i64>()
            .map_err(|_| AppError::unauthorized("invalid token subject".to_string()))?;

        if !rg_core::repo::service::can_read_repo(&state.db, &repo, Some(user_id))
            .await
            .unwrap_or(false)
        {
            return Err(AppError::forbidden("access denied"));
        }
    }

    Ok(repo)
}

// ── Milestone handlers ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ListMilestonesQuery {
    pub state: Option<String>,
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/milestones",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_milestones(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<ListMilestonesQuery>,
) -> impl IntoResponse {
    let repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name).await
    {
        Ok(Some(r)) => r,
        Ok(None) => return AppError::not_found("repository not found".to_string()).into_response(),
        Err(e) => {
            return {
                tracing::error!(%e, "handler error");
                AppError::internal(e).into_response()
            }
        }
    };
    match rg_db::ops::milestone_ops::list_by_repo(&state.db, repo.id, params.state.as_deref()).await
    {
        Ok(milestones) => (StatusCode::OK, Json(serde_json::json!(milestones))).into_response(),
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct CreateMilestoneRequest {
    pub title: String,
    pub description: Option<String>,
    pub due_date: Option<String>,
    pub state: Option<String>,
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/milestones",
    tag = "Issues",
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
pub async fn create_milestone(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(body): Json<CreateMilestoneRequest>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => {
            return AppError::unauthorized("authentication required".to_string()).into_response()
        }
    };
    let user_id: i64 = match claims.sub.parse::<i64>() {
        Ok(id) => id,
        Err(_) => {
            return AppError::unauthorized("invalid token subject".to_string()).into_response()
        }
    };

    let repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name).await
    {
        Ok(Some(r)) => r,
        Ok(None) => return AppError::not_found("repository not found".to_string()).into_response(),
        Err(e) => {
            return {
                tracing::error!(%e, "handler error");
                AppError::internal(e).into_response()
            }
        }
    };
    if !rg_core::repo::service::can_write(&state.db, &owner, &name, Some(user_id))
        .await
        .unwrap_or(false)
    {
        return AppError::forbidden("forbidden".to_string()).into_response();
    }
    let now = chrono::Utc::now();
    let due_date = body
        .due_date
        .as_deref()
        .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let model = rg_db::entities::milestone::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: sea_orm::Set(repo.id),
        title: sea_orm::Set(body.title),
        description: sea_orm::Set(body.description),
        state: sea_orm::Set(body.state.unwrap_or_else(|| "open".to_string())),
        due_date: sea_orm::Set(due_date),
        created_at: sea_orm::Set(now),
        updated_at: sea_orm::Set(now),
    };
    match rg_db::ops::milestone_ops::create(&state.db, model).await {
        Ok(m) => (StatusCode::CREATED, Json(serde_json::json!(m))).into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/milestones/{id}",
    tag = "Issues",
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
pub async fn get_milestone(
    State(state): State<AppState>,
    Path((owner, name, id)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let _repo = match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &name)
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return AppError::not_found("repository not found".to_string()).into_response(),
        Err(e) => {
            return {
                tracing::error!(%e, "handler error");
                AppError::internal(e).into_response()
            }
        }
    };
    match rg_db::ops::milestone_ops::find_by_id(&state.db, id).await {
        Ok(Some(m)) => (StatusCode::OK, Json(serde_json::json!(m))).into_response(),
        Ok(None) => AppError::not_found("milestone not found".to_string()).into_response(),
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct UpdateMilestoneRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub state: Option<String>,
    pub due_date: Option<Option<String>>,
}

#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/milestones/{id}",
    tag = "Issues",
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
pub async fn update_milestone(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, id)): Path<(String, String, i64)>,
    Json(body): Json<UpdateMilestoneRequest>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => {
            return AppError::unauthorized("authentication required".to_string()).into_response()
        }
    };
    let user_id: i64 = match claims.sub.parse::<i64>() {
        Ok(id) => id,
        Err(_) => {
            return AppError::unauthorized("invalid token subject".to_string()).into_response()
        }
    };

    if !rg_core::repo::service::can_write(&state.db, &owner, &name, Some(user_id))
        .await
        .unwrap_or(false)
    {
        return AppError::forbidden("forbidden".to_string()).into_response();
    }
    let existing = match rg_db::ops::milestone_ops::find_by_id(&state.db, id).await {
        Ok(Some(m)) => m,
        Ok(None) => return AppError::not_found("milestone not found".to_string()).into_response(),
        Err(e) => {
            return {
                tracing::error!(%e, "handler error");
                AppError::internal(e).into_response()
            }
        }
    };
    // Convert to ActiveModel; use Set() for changed fields (Unchanged means "skip in UPDATE")
    let mut active: rg_db::entities::milestone::ActiveModel = existing.into();
    if let Some(t) = body.title {
        active.title = sea_orm::Set(t);
    }
    if let Some(d) = body.description {
        active.description = sea_orm::Set(d);
    }
    if let Some(s) = body.state {
        if s == "open" || s == "closed" {
            active.state = sea_orm::Set(s);
        }
    }
    if let Some(d) = body.due_date {
        let dt = d
            .as_deref()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));
        active.due_date = sea_orm::Set(dt);
    }
    active.updated_at = sea_orm::Set(chrono::Utc::now());
    match rg_db::ops::milestone_ops::update(&state.db, active).await {
        Ok(m) => (StatusCode::OK, Json(serde_json::json!(m))).into_response(),
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/milestones/{id}",
    tag = "Issues",
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
pub async fn delete_milestone(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, id)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let claims = match extract_bearer_claims(&headers, &state.jwt_secret) {
        Some(c) => c,
        None => {
            return AppError::unauthorized("authentication required".to_string()).into_response()
        }
    };
    let user_id: i64 = match claims.sub.parse::<i64>() {
        Ok(id) => id,
        Err(_) => {
            return AppError::unauthorized("invalid token subject".to_string()).into_response()
        }
    };

    if !rg_core::repo::service::can_write(&state.db, &owner, &name, Some(user_id))
        .await
        .unwrap_or(false)
    {
        return AppError::forbidden("forbidden".to_string()).into_response();
    }
    match rg_db::ops::milestone_ops::delete_by_id(&state.db, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

// ── Issue Labels handlers ───────────────────────────────────────────────

/// GET /api/v1/repos/:owner/:name/issues/:number/labels
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues/{number}/labels",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_issue_labels(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    match rg_core::issue::get_issue(&state.db, &owner, &repo, number).await {
        Ok(issue) => match rg_core::label::service::get_issue_labels(&state.db, issue.id).await {
            Ok(labels) => (StatusCode::OK, Json(serde_json::json!(labels))).into_response(),
            Err(e) => {
                tracing::error!(%e, "handler error");
                AppError::internal(e).into_response()
            }
        },
        Err(e) => AppError::not_found(e.to_string()).into_response(),
    }
}
