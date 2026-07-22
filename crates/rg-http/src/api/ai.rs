//! AI Agent 专用 REST API 端点。
//!
//! 这些端点在 `/api/v1/ai/` 下注册，提供比通用 REST API
//! 更适合 AI Agent 消费的高层语义数据。

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::auth::extract_bearer_claims;
use crate::error::AppError;
use crate::AppState;
use sea_orm::{ConnectionTrait, Statement};

// ── Response types ───────────────────────────────────

/// 仓库摘要响应（AI 友好格式）
#[derive(Serialize, ToSchema)]
pub struct RepoSummary {
    pub full_name: String,
    pub description: Option<String>,
    pub default_branch: String,
    pub stars_count: i64,
    pub forks_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Issue 摘要（AI 友好格式）
#[derive(Serialize, ToSchema)]
pub struct IssueSummary {
    pub number: i64,
    pub title: String,
    pub state: String,
    pub author_id: i64,
    pub created_at: String,
}

/// PR 摘要（AI 友好格式）
#[derive(Serialize, ToSchema)]
pub struct PrSummary {
    pub number: i64,
    pub title: String,
    pub state: String,
    pub author_id: i64,
    pub head_branch: String,
    pub base_branch: String,
    pub created_at: String,
}

// ── Query structs ────────────────────────────────────

#[derive(Deserialize)]
pub struct IssueListQuery {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct PrListQuery {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Query params for tree endpoint
#[derive(Deserialize)]
pub struct TreeQuery {
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

/// Query params for code search endpoint
#[derive(Deserialize)]
pub struct SearchCodeQuery {
    pub q: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

// ── Helpers ─────────────────────────────────────

async fn resolve_repo(
    state: &AppState,
    owner: &str,
    name: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, name)
        .await
        .map_err(|e| {
            AppError::not_found(format!("repository not found: {}/{}: {}", owner, name, e))
        })?
        .ok_or_else(|| AppError::not_found(format!("repository not found: {}/{}", owner, name)))
}

/// Require read access for a repo. Public repos are always accessible;
/// private repos require a valid JWT and the user must have read permission.
async fn require_repo_read_access(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
) -> Result<(), AppError> {
    if !repo.is_private {
        return Ok(());
    }
    let claims = extract_bearer_claims(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let user_id = claims
        .sub
        .parse::<i64>()
        .map_err(|_| AppError::unauthorized("invalid token subject".to_string()))?;

    if !rg_core::repo::service::can_read_repo(&state.db, repo, Some(user_id))
        .await
        .unwrap_or(false)
    {
        return Err(AppError::forbidden("access denied"));
    }
    Ok(())
}

// ── Handlers ──────────────────────────────────────

/// GET /api/v1/ai/repos/{owner}/{name}/summary
#[utoipa::path(
    get,
    path = "/api/v1/ai/repos/{owner}/{name}/summary",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Repository summary", body = RepoSummary),
        (status = 404, description = "Repository not found"),
    ),
    tag = "ai",
)]
pub async fn ai_repo_summary(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<RepoSummary>), AppError> {
    let repo = resolve_repo(&state, &owner, &name).await?;
    require_repo_read_access(&state, &headers, &repo).await?;

    let summary = RepoSummary {
        full_name: format!("{}/{}", owner, name),
        description: repo.description,
        default_branch: repo.default_branch.clone(),
        stars_count: repo.stars_count,
        forks_count: repo.forks_count,
        created_at: repo.created_at.to_rfc3339(),
        updated_at: repo.updated_at.to_rfc3339(),
    };

    Ok((StatusCode::OK, Json(summary)))
}

/// GET /api/v1/ai/repos/{owner}/{name}/issues
#[utoipa::path(
    get,
    path = "/api/v1/ai/repos/{owner}/{name}/issues",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("state" = Option<String>, Query, description = "open | closed (default: open)"),
        ("limit" = Option<i64>, Query, description = "Max results (default 20)"),
    ),
    responses(
        (status = 200, description = "Issue list", body = Vec<IssueSummary>),
        (status = 404, description = "Repository not found"),
    ),
    tag = "ai",
)]
pub async fn ai_list_issues(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    Query(params): Query<IssueListQuery>,
) -> Result<(StatusCode, Json<Vec<IssueSummary>>), AppError> {
    let _repo = resolve_repo(&state, &owner, &name).await?;
    require_repo_read_access(&state, &headers, &_repo).await?;

    let state_filter = params.state.as_deref().unwrap_or("open");

    let issues = rg_core::issue::service::list_issues(&state.db, &owner, &name, Some(state_filter))
        .await
        .map_err(|e| AppError::internal(format!("DB error: {}", e)))?;

    let limit = params.limit.unwrap_or(20) as usize;
    let summaries = issues
        .into_iter()
        .take(limit)
        .map(|issue| IssueSummary {
            number: issue.number,
            title: issue.title,
            state: issue.state,
            author_id: issue.author_id,
            created_at: issue.created_at.to_rfc3339(),
        })
        .collect();

    Ok((StatusCode::OK, Json(summaries)))
}

/// GET /api/v1/ai/repos/{owner}/{name}/prs
#[utoipa::path(
    get,
    path = "/api/v1/ai/repos/{owner}/{name}/prs",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("state" = Option<String>, Query, description = "open | closed | merged (default: open)"),
        ("limit" = Option<i64>, Query, description = "Max results (default 20)"),
    ),
    responses(
        (status = 200, description = "PR list", body = Vec<PrSummary>),
        (status = 404, description = "Repository not found"),
    ),
    tag = "ai",
)]
pub async fn ai_list_prs(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    Query(params): Query<PrListQuery>,
) -> Result<(StatusCode, Json<Vec<PrSummary>>), AppError> {
    let _repo = resolve_repo(&state, &owner, &name).await?;
    require_repo_read_access(&state, &headers, &_repo).await?;

    let state_filter = params.state.as_deref().unwrap_or("open");

    let prs =
        rg_core::pull_request::service::list_prs(&state.db, &owner, &name, Some(state_filter))
            .await
            .map_err(|e| AppError::internal(format!("DB error: {}", e)))?;

    let limit = params.limit.unwrap_or(20) as usize;
    let summaries = prs
        .into_iter()
        .take(limit)
        .map(|pr| PrSummary {
            number: pr.number,
            title: pr.title,
            state: pr.state,
            author_id: pr.author_id,
            head_branch: pr.head_branch,
            base_branch: pr.base_branch,
            created_at: pr.created_at.to_rfc3339(),
        })
        .collect();

    Ok((StatusCode::OK, Json(summaries)))
}

// ── Stub handlers (NOT IMPLEMENTED) ─────────────────

/// GET /api/v1/ai/repos/{owner}/{name}/tree
#[utoipa::path(
    get,
    path = "/api/v1/ai/repos/{owner}/{name}/tree",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("ref" = Option<String>, Query, description = "Branch/tag/SHA (default: default branch)"),
        ("path" = Option<String>, Query, description = "Subdirectory path (default: root)"),
    ),
    responses(
        (status = 200, description = "Repository file tree"),
        (status = 404, description = "Repository not found"),
        (status = 501, description = "Not yet implemented"),
    ),
    tag = "ai",
)]
pub async fn ai_repo_tree(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    Query(_params): Query<TreeQuery>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let _repo = resolve_repo(&state, &owner, &name).await?;
    require_repo_read_access(&state, &headers, &_repo).await?;
    Ok((
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({"error": "repo_tree not yet implemented"})),
    ))
}

/// Code search result for AI API
#[derive(Serialize, ToSchema)]
pub struct CodeSearchResult {
    pub repo_id: i64,
    pub file_path: String,
    pub file_name: String,
    pub language: String,
    pub snippet: String,
}

/// GET /api/v1/ai/repos/{owner}/{name}/search/code
#[utoipa::path(
    get,
    path = "/api/v1/ai/repos/{owner}/{name}/search/code",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("q" = String, Query, description = "Search query"),
        ("ref" = Option<String>, Query, description = "Branch/tag/SHA (default: default branch)"),
        ("limit" = Option<i64>, Query, description = "Max results (default 20)"),
    ),
    responses(
        (status = 200, description = "Code search results", body = Vec<CodeSearchResult>),
        (status = 400, description = "Repository not indexed"),
        (status = 404, description = "Repository not found"),
    ),
    tag = "ai",
)]
pub async fn ai_search_code(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    Query(params): Query<SearchCodeQuery>,
) -> Result<(StatusCode, Json<Vec<CodeSearchResult>>), AppError> {
    let repo = resolve_repo(&state, &owner, &name).await?;
    require_repo_read_access(&state, &headers, &repo).await?;

    let limit = params.limit.unwrap_or(20).min(100) as u64;
    let offset = 0u64;

    let indexer = rg_core::search::code_indexer::CodeIndexer::new(state.db.clone());

    // Check if repo is indexed
    let backend = state.db.get_database_backend();
    let check_sql = rg_db::prepare_sql(
        backend,
        "SELECT COUNT(*) as cnt FROM code_fts WHERE repo_id = ?",
    );
    let check_result = state
        .db
        .query_one(Statement::from_sql_and_values(
            backend,
            check_sql,
            [repo.id.into()],
        ))
        .await
        .map_err(|e| AppError::internal(format!("DB error: {}", e)))?
        .ok_or_else(|| AppError::internal("Failed to check index status".to_string()))?;
    let indexed_count: i64 = check_result
        .try_get_by_index(0)
        .map_err(|e| AppError::internal(format!("DB error: {}", e)))?;

    if indexed_count == 0 {
        return Err(AppError::bad_request(
            "Repository not indexed. Please trigger indexing first by pushing to the repository or calling the index endpoint.".to_string()
        ));
    }

    let (results, _total) = indexer
        .search_code(&params.q, Some(repo.id), limit, offset)
        .await
        .map_err(|e| AppError::internal(format!("Search error: {}", e)))?;

    let api_results = results
        .into_iter()
        .map(|r| CodeSearchResult {
            repo_id: r.repo_id,
            file_path: r.file_path,
            file_name: r.file_name,
            language: r.language,
            snippet: r.snippet,
        })
        .collect();

    Ok((StatusCode::OK, Json(api_results)))
}

/// Response for index trigger.
#[derive(Serialize, ToSchema)]
pub struct IndexResponse {
    pub indexed_files: usize,
}

/// POST /api/v1/ai/repos/{owner}/{name}/index
#[utoipa::path(
    post,
    path = "/api/v1/ai/repos/{owner}/{name}/index",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Indexing triggered successfully", body = IndexResponse),
        (status = 404, description = "Repository not found"),
        (status = 500, description = "Indexing error"),
    ),
    tag = "ai",
)]
pub async fn ai_index_repository(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
    Json(_body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Resolve repository
    let repo = match resolve_repo(&state, &owner, &name).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    // Check read access
    if let Err(e) = require_repo_read_access(&state, &headers, &repo).await {
        return e.into_response();
    }

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, name));
    if !repo_path.exists() {
        return AppError::not_found("repository not found on disk").into_response();
    }

    // Placeholder - will call indexer later
    // Actually call the indexer
    let indexer = rg_core::search::code_indexer::CodeIndexer::new(state.db.clone());
    match indexer
        .index_repository(repo.id, &repo_path, &repo.default_branch)
        .await
    {
        Ok(count) => (
            StatusCode::OK,
            Json(IndexResponse {
                indexed_files: count,
            }),
        )
            .into_response(),
        Err(e) => AppError::internal(format!("Indexing error: {}", e)).into_response(),
    }
}
