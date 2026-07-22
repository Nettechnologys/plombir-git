//! Global search API handler.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use crate::error::AppError;
use crate::pagination::PaginationParams;
use crate::AppState;

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default = "default_search_type")]
    pub r#type: String,
    // L-4: Use standard PaginationParams defaults instead of duplicate functions.
    #[serde(default = "crate::pagination::default_page")]
    pub page: u64,
    #[serde(default = "crate::pagination::default_per_page")]
    pub per_page: u64,
}

fn default_search_type() -> String {
    "all".to_string()
}

fn normalize_search_type(raw: &str) -> Option<&'static str> {
    match raw {
        "all" => Some("all"),
        "repo" | "repos" => Some("repos"),
        "issue" | "issues" => Some("issues"),
        "wiki" => Some("wiki"),
        _ => None,
    }
}

/// GET /api/v1/search?q=keyword&type=all|repos|issues|wiki&page=1&per_page=20
#[utoipa::path(
    get,
    path = "/search",
    tag = "Search",
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchQuery>,
) -> impl IntoResponse {
    let pagination = PaginationParams::new(params.page, params.per_page);

    if params.q.trim().is_empty() {
        return AppError::bad_request("search query 'q' parameter is required").into_response();
    }

    let Some(search_type) = normalize_search_type(&params.r#type) else {
        return AppError::bad_request(
            "invalid type, must be one of: all, repo/repos, issue/issues, wiki",
        )
        .into_response();
    };

    match rg_core::search::service::search(
        &state.db,
        &params.q,
        search_type,
        pagination.page,
        pagination.per_page,
    )
    .await
    {
        Ok((results, total)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "results": results,
                "total": total,
                "page": pagination.page,
                "per_page": pagination.per_page,
            })),
        )
            .into_response(),
        Err(e) => AppError::internal(e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_search_type;

    #[test]
    fn normalizes_search_type_aliases() {
        assert_eq!(normalize_search_type("all"), Some("all"));
        assert_eq!(normalize_search_type("repo"), Some("repos"));
        assert_eq!(normalize_search_type("repos"), Some("repos"));
        assert_eq!(normalize_search_type("issue"), Some("issues"));
        assert_eq!(normalize_search_type("issues"), Some("issues"));
        assert_eq!(normalize_search_type("wiki"), Some("wiki"));
        assert_eq!(normalize_search_type("unknown"), None);
    }
}
