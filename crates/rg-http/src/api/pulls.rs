//! REST API handlers for Pull Requests.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};

use super::repo_access::{require_authenticated_read, require_read, require_write};
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::AppState;

// ── Request / Response types ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreatePrRequest {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    /// Head branch reference. Supports "owner:branch" for fork PRs, or just "branch" for same-repo.
    pub head: String,
    pub base: String,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Deserialize)]
pub struct UpdatePrRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub draft: Option<bool>,
}

#[derive(Deserialize)]
pub struct MergePrRequest {
    /// merge / squash / rebase
    pub strategy: String,
}

#[derive(Deserialize)]
pub struct EnableAutoMergeRequest {
    /// merge / squash / rebase
    pub strategy: String,
}

#[derive(Serialize)]
pub struct MergeQueueEntryResponse {
    pub id: i64,
    pub position: usize,
    pub pr_id: i64,
    pub pr_number: i64,
    pub title: String,
    pub strategy: String,
    pub status: String,
    pub enqueued_by_id: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub state: Option<String>,
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

// ── PR handlers ─────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls",
    tag = "Pull Requests",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_prs(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Query(params): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_read(&state, &headers, &owner, &repo).await {
        return e.into_response();
    }

    let state_filter = params.state.as_deref();
    let pagination = params.pagination.clamp();
    match rg_core::pull_request::list_prs_paginated(
        &state.db,
        &owner,
        &repo,
        state_filter,
        pagination.offset(),
        pagination.limit(),
    )
    .await
    {
        Ok((data, total)) => (
            StatusCode::OK,
            Json(PaginatedResponse::new(data, &pagination, total as u64)),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls/{number}",
    tag = "Pull Requests",
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
pub async fn get_pr(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_read(&state, &headers, &owner, &repo).await {
        return e.into_response();
    }

    match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => (StatusCode::OK, Json(pr)).into_response(),
        Err(e) => AppError::not_found(e.to_string()).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls",
    tag = "Pull Requests",
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
pub async fn create_pr(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreatePrRequest>,
) -> impl IntoResponse {
    let (repo_model, user_id) =
        match require_authenticated_read(&state, &headers, &owner, &repo).await {
            Ok(access) => access,
            Err(e) => return e.into_response(),
        };
    let repo_id = repo_model.id;

    if req.head.trim().is_empty() || req.base.trim().is_empty() {
        return AppError::bad_request("head and base branches are required").into_response();
    };

    match rg_core::pull_request::resolve_head_ref(&state.db, repo_id, &req.head).await {
        Ok((head_branch, head_repo_id)) => {
            match rg_core::pull_request::create_pr(
                &state.db,
                &state.repo_root,
                repo_id,
                user_id,
                req.title,
                req.body,
                head_branch,
                req.base,
                head_repo_id,
                req.draft,
            )
            .await
            {
                Ok(pr) => {
                    // CODEOWNERS is advisory: a malformed/missing file or an
                    // unavailable diff must not prevent PR creation.
                    match rg_core::pull_request::compute_diff(
                        &state.db,
                        &state.repo_root,
                        &owner,
                        &repo,
                        pr.number,
                    )
                    .await
                    {
                        Ok(diff) => {
                            let paths = diff
                                .files_changed
                                .into_iter()
                                .map(|file| file.path)
                                .collect::<Vec<_>>();
                            let repo_path = state.repo_root.join(format!("{owner}/{repo}.git"));
                            if let Err(error) = rg_core::review::codeowners::request_codeowners(
                                &state.db,
                                &repo_path,
                                &pr.base_branch,
                                &paths,
                                &repo_model,
                                pr.id,
                                pr.author_id,
                                user_id,
                            )
                            .await
                            {
                                tracing::warn!(pr_id = pr.id, %error, "CODEOWNERS reviewer request failed");
                            }
                        }
                        Err(error) => {
                            tracing::warn!(pr_id = pr.id, %error, "CODEOWNERS diff unavailable");
                        }
                    }
                    (StatusCode::CREATED, Json(pr)).into_response()
                }
                Err(e) => AppError::bad_request(e.to_string()).into_response(),
            }
        }
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/pulls/{number}",
    tag = "Pull Requests",
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
pub async fn update_pr(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<UpdatePrRequest>,
) -> impl IntoResponse {
    let (repo_model, actor_id) =
        match require_authenticated_read(&state, &headers, &owner, &repo).await {
            Ok(access) => access,
            Err(e) => return e.into_response(),
        };
    let existing = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(e) => return AppError::not_found(e.to_string()).into_response(),
    };
    let can_write = rg_core::repo::service::can_write_repo(&state.db, &repo_model, Some(actor_id))
        .await
        .unwrap_or(false);
    if existing.author_id != actor_id && !can_write {
        return AppError::forbidden("only the PR author or a repository writer may update this PR")
            .into_response();
    }
    if req.state.as_deref() == Some("merged") {
        return AppError::bad_request("use the merge endpoint to merge a pull request")
            .into_response();
    }

    match rg_core::pull_request::update_pr(
        &state.db, &owner, &repo, number, req.title, req.body, req.state, req.draft, actor_id,
    )
    .await
    {
        Ok(pr) => (StatusCode::OK, Json(pr)).into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls/{number}/diff",
    tag = "Pull Requests",
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
pub async fn get_diff(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_read(&state, &headers, &owner, &repo).await {
        return e.into_response();
    }

    match rg_core::pull_request::compute_diff(&state.db, &state.repo_root, &owner, &repo, number)
        .await
    {
        Ok(diff) => (StatusCode::OK, Json(diff)).into_response(),
        Err(e) => {
            tracing::error!(%e, "handler error");
            AppError::internal(e).into_response()
        }
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls/{number}/merge",
    tag = "Pull Requests",
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
pub async fn merge_pr(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<MergePrRequest>,
) -> impl IntoResponse {
    let (repo_model, _actor_id) = match require_write(&state, &headers, &owner, &repo).await {
        Ok(access) => access,
        Err(e) => return e.into_response(),
    };

    let strategy = match rg_core::pull_request::MergeStrategy::parse(&req.strategy) {
        Ok(strategy) => strategy,
        Err(error) => return AppError::bad_request(error).into_response(),
    };

    // Check branch protection before merging
    if let Ok(pr) = rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        if pr.is_draft {
            return AppError::conflict("draft pull requests cannot be merged").into_response();
        }
        if let Err(e) = rg_core::branch_protection::service::check_merge_allowed(
            &state.db,
            repo_model.id,
            &pr.base_branch,
            pr.id,
        )
        .await
        {
            return AppError::forbidden(e.to_string()).into_response();
        }
    }

    match rg_core::pull_request::merge_pr(
        &state.db,
        &state.repo_root,
        &owner,
        &repo,
        number,
        strategy,
    )
    .await
    {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => AppError::bad_request(e.to_string()).into_response(),
    }
}

#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/pulls/{number}/auto-merge",
    tag = "Pull Requests",
    request_body(content = serde_json::Value),
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
    ),
    responses((status = 200, body = serde_json::Value), (status = 403, body = serde_json::Value))
)]
pub async fn enable_auto_merge(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<EnableAutoMergeRequest>,
) -> impl IntoResponse {
    let (_, actor_id) = match require_write(&state, &headers, &owner, &repo).await {
        Ok(access) => access,
        Err(error) => return error.into_response(),
    };
    let strategy = match rg_core::pull_request::MergeStrategy::parse(&req.strategy) {
        Ok(strategy) => strategy,
        Err(error) => return AppError::bad_request(error).into_response(),
    };
    if let Err(error) = rg_core::pull_request::enable_auto_merge(
        &state.db, &owner, &repo, number, strategy, actor_id,
    )
    .await
    {
        return AppError::bad_request(error).into_response();
    }
    match rg_core::pull_request::try_auto_merge(&state.db, &state.repo_root, &owner, &repo, number)
        .await
    {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(error) => AppError::bad_request(error).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/pulls/{number}/auto-merge",
    tag = "Pull Requests",
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
    ),
    responses((status = 200, body = serde_json::Value), (status = 403, body = serde_json::Value))
)]
pub async fn disable_auto_merge(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let (_, actor_id) = match require_write(&state, &headers, &owner, &repo).await {
        Ok(access) => access,
        Err(error) => return error.into_response(),
    };
    match rg_core::pull_request::disable_auto_merge(&state.db, &owner, &repo, number, actor_id)
        .await
    {
        Ok(pr) => (StatusCode::OK, Json(pr)).into_response(),
        Err(error) => AppError::bad_request(error).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/merge-queue",
    tag = "Pull Requests",
    params(("owner" = String, Path), ("name" = String, Path)),
    responses((status = 200, body = serde_json::Value))
)]
pub async fn list_merge_queue(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let repository = match require_read(&state, &headers, &owner, &repo).await {
        Ok(repository) => repository,
        Err(error) => return error.into_response(),
    };
    let entries = match rg_db::ops::merge_queue_ops::list_by_repo(&state.db, repository.id).await {
        Ok(entries) => entries,
        Err(error) => return AppError::internal(error).into_response(),
    };
    let mut response = Vec::with_capacity(entries.len());
    for (index, entry) in entries.into_iter().enumerate() {
        let pr = match rg_db::entities::pull_request::Entity::find_by_id(entry.pr_id)
            .one(&state.db)
            .await
        {
            Ok(Some(pr)) if pr.repo_id == repository.id => pr,
            Ok(_) => continue,
            Err(error) => return AppError::internal(error).into_response(),
        };
        response.push(MergeQueueEntryResponse {
            id: entry.id,
            position: index + 1,
            pr_id: entry.pr_id,
            pr_number: pr.number,
            title: pr.title,
            strategy: entry.strategy,
            status: entry.status,
            enqueued_by_id: entry.enqueued_by_id,
            created_at: entry.created_at,
        });
    }
    (StatusCode::OK, Json(response)).into_response()
}

#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/pulls/{number}/merge-queue",
    tag = "Pull Requests",
    request_body(content = serde_json::Value),
    params(("owner" = String, Path), ("name" = String, Path), ("number" = i64, Path)),
    responses((status = 200, body = serde_json::Value), (status = 403, body = serde_json::Value))
)]
pub async fn enqueue_merge_queue(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<EnableAutoMergeRequest>,
) -> impl IntoResponse {
    let (repository, actor_id) = match require_write(&state, &headers, &owner, &repo).await {
        Ok(access) => access,
        Err(error) => return error.into_response(),
    };
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(error) => return AppError::not_found(error.to_string()).into_response(),
    };
    let strategy = match rg_core::pull_request::MergeStrategy::parse(&req.strategy) {
        Ok(strategy) => strategy,
        Err(error) => return AppError::bad_request(error).into_response(),
    };
    let entry = match rg_core::pull_request::merge_queue::enqueue(
        &state.db,
        &repository,
        &pr,
        actor_id,
        strategy,
    )
    .await
    {
        Ok(entry) => entry,
        Err(error) => return AppError::bad_request(error).into_response(),
    };
    let ci = rg_core::pull_request::merge_queue::MergeQueueCi {
        trigger: &*state.ci_engine,
        docker_enabled: state.docker_enabled,
        external_runners: state.external_runners,
        jwt_secret: Some(&state.jwt_secret),
        external_url: state.external_url.as_deref(),
    };
    let process = match rg_core::pull_request::merge_queue::process_repository_with_ci(
        &state.db,
        &state.repo_root,
        &repository,
        &ci,
    )
    .await
    {
        Ok(process) => process,
        Err(error) => return AppError::internal(error).into_response(),
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({"entry": entry, "process": process})),
    )
        .into_response()
}

#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/pulls/{number}/merge-queue",
    tag = "Pull Requests",
    params(("owner" = String, Path), ("name" = String, Path), ("number" = i64, Path)),
    responses((status = 204), (status = 404, body = serde_json::Value))
)]
pub async fn cancel_merge_queue(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let (repository, actor_id) = match require_write(&state, &headers, &owner, &repo).await {
        Ok(access) => access,
        Err(error) => return error.into_response(),
    };
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(error) => return AppError::not_found(error.to_string()).into_response(),
    };
    match rg_core::pull_request::merge_queue::cancel(
        &state.db,
        &state.repo_root,
        &repository,
        &pr,
        actor_id,
    )
    .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => AppError::not_found("pull request is not queued").into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}
