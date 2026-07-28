//! REST API endpoints dedicated to AI agents.
//!
//! These endpoints are registered under `/api/v1/ai/` and expose high-level
//! semantic data better suited for AI agent consumption than the generic REST API.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::repo_access::RepoRead;
use crate::error::AppError;
use crate::AppState;
use sea_orm::{ConnectionTrait, Statement};

// ── Response types ───────────────────────────────────

/// Repository summary response (AI-friendly format).
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

/// Issue summary (AI-friendly format).
#[derive(Serialize, ToSchema)]
pub struct IssueSummary {
    pub number: i64,
    pub title: String,
    pub state: String,
    pub author_id: i64,
    pub created_at: String,
}

/// PR summary (AI-friendly format).
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
    Path((owner, name)): Path<(String, String)>,
    RepoRead { repo }: RepoRead,
) -> Result<(StatusCode, Json<RepoSummary>), AppError> {
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
    Query(params): Query<IssueListQuery>,
    RepoRead { .. }: RepoRead,
) -> Result<(StatusCode, Json<Vec<IssueSummary>>), AppError> {
    let state_filter = params.state.as_deref().unwrap_or("open");

    let issues = rg_core::issue::service::list_issues(&state.db, &owner, &name, Some(state_filter))
        .await
        .map_err(AppError::from)?;

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
    Query(params): Query<PrListQuery>,
    RepoRead { .. }: RepoRead,
) -> Result<(StatusCode, Json<Vec<PrSummary>>), AppError> {
    let state_filter = params.state.as_deref().unwrap_or("open");

    let prs =
        rg_core::pull_request::service::list_prs(&state.db, &owner, &name, Some(state_filter))
            .await
            .map_err(AppError::from)?;

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
    Path((_, _)): Path<(String, String)>,
    Query(_params): Query<TreeQuery>,
    RepoRead { .. }: RepoRead,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
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
    Path((_, _)): Path<(String, String)>,
    Query(params): Query<SearchCodeQuery>,
    RepoRead { repo }: RepoRead,
) -> Result<(StatusCode, Json<Vec<CodeSearchResult>>), AppError> {
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
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::internal("Failed to check index status".to_string()))?;
    let indexed_count: i64 = check_result.try_get_by_index(0).map_err(AppError::from)?;

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
    RepoRead { repo }: RepoRead,
    Json(_body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Resolve repository

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, name));
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
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
