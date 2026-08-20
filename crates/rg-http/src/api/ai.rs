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

use crate::api::repo_access::{RepoRead, RepoWrite};
use crate::error::AppError;
use crate::AppState;

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

const DEFAULT_AI_LIMIT: i64 = 20;
const MAX_AI_LIMIT: i64 = 100;

/// Validate the signed HTTP boundary before it becomes a SQL `LIMIT`.
///
/// The result is the unit the database takes, not the unit `Iterator::take`
/// takes: the whole point of the boundary is that the page is built by the
/// query. Handing back a `usize` invited the caller to spend it on the
/// materialised vector instead (card_c386beea2fe0).
fn ai_limit(limit: Option<i64>) -> Result<u64, AppError> {
    let limit = limit.unwrap_or(DEFAULT_AI_LIMIT);
    if limit <= 0 {
        return Err(AppError::bad_request("limit must be greater than zero"));
    }

    // The positive value is capped at 100 before conversion, so it fits `u64`
    // without a wrapping cast.
    Ok(u64::try_from(limit.min(MAX_AI_LIMIT)).expect("validated AI result limit is at most 100"))
}

// ── Handlers ──────────────────────────────────────

/// GET /api/v1/ai/repos/{owner}/{name}/summary
#[utoipa::path(
    get,
    path = "/ai/repos/{owner}/{name}/summary",
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
    path = "/ai/repos/{owner}/{name}/issues",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("state" = Option<String>, Query, description = "open | closed (default: open)"),
        ("limit" = Option<i64>, Query, description = "Max results (1-100, default 20)"),
    ),
    responses(
        (status = 200, description = "Issue list", body = Vec<IssueSummary>),
        (status = 400, description = "Invalid limit"),
        (status = 404, description = "Repository not found"),
    ),
    tag = "ai",
)]
pub async fn ai_list_issues(
    State(state): State<AppState>,
    RepoRead { .. }: RepoRead,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<IssueListQuery>,
) -> Result<(StatusCode, Json<Vec<IssueSummary>>), AppError> {
    let limit = ai_limit(params.limit)?;
    let state_filter = params.state.as_deref().unwrap_or("open");

    // The page is cut by the database. Reading every open issue of the
    // repository and dropping all but `limit` of them in Rust honoured the
    // documented maximum on the way out while ignoring it on the way in: the
    // cost of `?limit=20` was set by the repository's issue count, and this is
    // the surface an agent polls.
    let (issues, _total) = rg_core::issue::service::list_issues_paginated(
        &state.db,
        &owner,
        &name,
        Some(state_filter),
        0,
        limit,
    )
    .await
    .map_err(AppError::from)?;

    let summaries = issues
        .into_iter()
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
    path = "/ai/repos/{owner}/{name}/prs",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("state" = Option<String>, Query, description = "open | closed | merged (default: open)"),
        ("limit" = Option<i64>, Query, description = "Max results (1-100, default 20)"),
    ),
    responses(
        (status = 200, description = "PR list", body = Vec<PrSummary>),
        (status = 400, description = "Invalid limit"),
        (status = 404, description = "Repository not found"),
    ),
    tag = "ai",
)]
pub async fn ai_list_prs(
    State(state): State<AppState>,
    RepoRead { .. }: RepoRead,
    Path((owner, name)): Path<(String, String)>,
    Query(params): Query<PrListQuery>,
) -> Result<(StatusCode, Json<Vec<PrSummary>>), AppError> {
    let limit = ai_limit(params.limit)?;
    let state_filter = params.state.as_deref().unwrap_or("open");

    // Same bound, same reason as `ai_list_issues`: the requested page is the
    // page the query builds, not what survives a `.take()` over the repository's
    // whole open set.
    let (prs, _total) = rg_core::pull_request::service::list_prs_paginated(
        &state.db,
        &owner,
        &name,
        Some(state_filter),
        0,
        limit,
    )
    .await
    .map_err(AppError::from)?;

    let summaries = prs
        .into_iter()
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
    path = "/ai/repos/{owner}/{name}/tree",
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
    RepoRead { .. }: RepoRead,
    Path((_, _)): Path<(String, String)>,
    Query(_params): Query<TreeQuery>,
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
    path = "/ai/repos/{owner}/{name}/search/code",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("q" = String, Query, description = "Search query"),
        ("ref" = Option<String>, Query, description = "Branch/tag/SHA (default: default branch)"),
        ("limit" = Option<i64>, Query, description = "Max results (1-100, default 20)"),
    ),
    responses(
        (status = 200, description = "Code search results", body = Vec<CodeSearchResult>),
        (status = 400, description = "Invalid limit or repository not indexed"),
        (status = 404, description = "Repository not found"),
    ),
    tag = "ai",
)]
pub async fn ai_search_code(
    State(state): State<AppState>,
    RepoRead { repo }: RepoRead,
    Path((_, _)): Path<(String, String)>,
    Query(params): Query<SearchCodeQuery>,
) -> Result<(StatusCode, Json<Vec<CodeSearchResult>>), AppError> {
    let limit = ai_limit(params.limit)?;
    let offset = 0u64;

    let indexer = rg_core::search::code_indexer::CodeIndexer::new(state.db.clone());

    // Check if repo is indexed.
    //
    // The message used to promise that pushing to the repository would build
    // the index. It never did — no push path touched `code_fts` — and the
    // promise sent people to do the one thing that could not work
    // (card_a1237efc85b7). A push now *refreshes* an existing snapshot, so the
    // wording names the act that actually creates one.
    let indexed_count = indexer
        .indexed_file_count(repo.id)
        .await
        .map_err(AppError::from)?;

    if indexed_count == 0 {
        return Err(AppError::bad_request(
            "Repository not indexed. Build the index first with POST /api/v1/ai/repos/{owner}/{name}/index (or the `forgekeep index-repo` command); \
             later pushes to the default branch keep it up to date."
                .to_string(),
        ));
    }

    // `search_code` is three database round-trips behind an `anyhow::Error`
    // (the FTS count, the result page, the row decoding), so its failure is the
    // database's failure. Formatting it into a message threw the type away and
    // answered a connection-level outage with a flat 500 — "we broke, and it is
    // permanent" — while `AppError::from` downcasts through the anyhow chain to
    // the original `DbErr` and answers the retryable 503 the index-status probe
    // above already answers. The client-facing body stays generic either way;
    // only the operator log keeps the detail.
    let (results, _total) = indexer
        .search_code(&params.q, Some(repo.id), limit, offset)
        .await
        .map_err(AppError::from)?;

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
///
/// The write half of the AI code-search surface. `ai_search_code` reads
/// `code_fts`, and until this door was mounted nothing outside the server's own
/// shell could fill it: the only producer was the `forgekeep index-repo` CLI
/// command, so a hosted instance answered every AI code search out of an index
/// that could never be built (card_928d72df493a).
///
/// `RepoWrite`, not `RepoRead`: indexing replaces the repository's entire
/// `code_fts` snapshot. Read access is the wrong question to ask of a caller
/// who is about to rewrite stored rows, and this is also a whole-tree traversal
/// — not something a reader of a public repository gets to trigger.
#[utoipa::path(
    post,
    path = "/ai/repos/{owner}/{name}/index",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "Indexing completed", body = IndexResponse),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Repository write access required"),
        (status = 404, description = "Repository not found"),
        (status = 409, description = "HEAD points at a branch that does not exist while other branches do"),
        (status = 500, description = "Indexing error"),
    ),
    tag = "ai",
)]
pub async fn ai_index_repository(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> axum::response::Response {
    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, name));
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
    }

    // A repository nobody has pushed to yet has an unborn HEAD, and the indexer
    // resolves the default branch before it walks anything — so handing it an
    // empty repository turns a healthy state into a `500`. Answering "indexed
    // nothing" is both true and the same reading the contents API gives an
    // unborn HEAD. The index itself is left alone: an empty answer here is
    // "there is no snapshot to take", not "replace the snapshot with nothing".
    //
    // An unborn HEAD *over existing branches* is the opposite situation: there
    // is a history to index and no branch name to reach it by. Reporting zero
    // indexed files there would quietly leave the repository unsearchable.
    match crate::api::repo_content::classify_repo_emptiness(&repo_path) {
        crate::api::repo_content::RepoEmptiness::Empty => {
            return (StatusCode::OK, Json(IndexResponse { indexed_files: 0 })).into_response();
        }
        crate::api::repo_content::RepoEmptiness::HeadWithoutBranch { head, branches } => {
            return crate::api::repo_content::head_without_branch_error(&head, &branches)
                .into_response();
        }
        crate::api::repo_content::RepoEmptiness::NotEmpty => {}
    }

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
        // Same boundary, same rule as `ai_search_code`: `index_repository`
        // writes every indexed file into `code_fts`, so a pool outage mid-index
        // is a `DbErr` under the anyhow chain and has to stay retryable.
        Err(e) => AppError::from(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::ai_limit;
    use axum::http::StatusCode;

    #[test]
    fn ai_limit_rejects_non_positive_values_and_caps_positive_values() {
        for invalid in [i64::MIN, -1, 0] {
            let error = ai_limit(Some(invalid)).expect_err("non-positive limit must fail");
            assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        }

        for (input, expected) in [
            (None, 20),
            (Some(1), 1),
            (Some(100), 100),
            (Some(101), 100),
            (Some(i64::MAX), 100),
        ] {
            assert_eq!(ai_limit(input).expect("valid limit"), expected);
        }
    }
}
