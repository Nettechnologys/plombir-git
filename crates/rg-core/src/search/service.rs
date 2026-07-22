//! Global search service — cross-backend full-text search.
//!
//! Supports GitHub-style search qualifiers:
//!   - `repo:owner/name` — filter by repository
//!   - `author:username` — filter by author/owner
//!   - `state:open|closed|all` — filter issue state
//!   - `label:name` — filter by label
//!   - `is:open|closed|merged` — filter issue state (alias)
//!   - `language:rust` — filter by primary language (future)
//!
//! Example: `q=bug fix repo:owner/repo state:open`
//!
//! The actual FTS predicate / ordering is produced per-backend by
//! [`crate::search::dialect`]; this module stays dialect-agnostic.

use anyhow::{Context, Result};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, Value};
use serde::Serialize;

use crate::search::dialect::{fts_match, ISSUES_FTS_COLS, REPOS_FTS_COLS, WIKI_FTS_COLS};

/// A unified search result.
#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub result_type: String,
    pub id: i64,
    pub title: String,
    pub excerpt: Option<String>,
    pub repo_owner: Option<String>,
    pub repo_name: Option<String>,
    /// For issues: the issue state (open/closed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// For issues: the issue number within its repo
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i64>,
}

/// Parsed search qualifiers extracted from the query string.
#[derive(Debug, Default, Clone)]
pub struct SearchFilters {
    /// Filter by repo: "owner/name" → resolved to repo_id
    pub repo: Option<String>,
    /// Filter by issue state: open, closed, all
    pub state: Option<String>,
    /// Filter by author username
    pub author: Option<String>,
    /// Filter by label name
    pub label: Option<String>,
    /// The remaining text query (without qualifiers)
    pub query: String,
}

impl SearchFilters {
    /// Parse a search query string, extracting qualifiers and returning the clean text query.
    pub fn parse(raw: &str) -> Self {
        let mut filters = SearchFilters::default();
        let mut query_parts = Vec::new();
        let tokens: Vec<&str> = raw.split_whitespace().collect();

        for token in tokens {
            if let Some((key, value)) = token.split_once(':') {
                let key_lower = key.to_lowercase();
                let clean_value = value.trim_matches('"').to_string();

                if clean_value.is_empty() {
                    query_parts.push(token.to_string());
                    continue;
                }

                match key_lower.as_str() {
                    "repo" => filters.repo = Some(clean_value),
                    "state" | "is" => filters.state = Some(clean_value.to_lowercase()),
                    "author" | "user" => filters.author = Some(clean_value),
                    "label" => filters.label = Some(clean_value),
                    _ => query_parts.push(token.to_string()),
                }
            } else {
                query_parts.push(token.to_string());
            }
        }

        filters.query = query_parts.join(" ");
        filters
    }
}

/// Search across repositories, issues, and/or wiki pages.
/// Supports qualifier-based filtering via `q` parameter.
pub async fn search(
    db: &DatabaseConnection,
    raw_query: &str,
    search_type: &str,
    page: u64,
    per_page: u64,
) -> Result<(Vec<SearchResult>, i64)> {
    let offset = (page.saturating_sub(1)) * per_page;
    let limit = per_page.min(100);

    let filters = SearchFilters::parse(raw_query);
    let raw_text = filters.query.as_str();

    let mut results = Vec::new();
    let mut total = 0i64;

    if search_type == "all" || search_type == "repos" {
        let (repos, count) = search_repos(db, raw_text, &filters, offset, limit).await?;
        total += count;
        results.extend(repos);
    }

    if search_type == "all" || search_type == "issues" {
        let (issues, count) = search_issues(db, raw_text, &filters, offset, limit).await?;
        total += count;
        results.extend(issues);
    }

    if search_type == "all" || search_type == "wiki" {
        let (wiki, count) = search_wiki(db, raw_text, &filters, offset, limit).await?;
        total += count;
        results.extend(wiki);
    }

    // For pagination when search_type is "all", apply offset/limit to the combined set
    if search_type == "all" {
        let skip = offset as usize;
        let take = limit as usize;
        let trimmed = results.into_iter().skip(skip).take(take).collect();
        return Ok((trimmed, total));
    }

    Ok((results, total))
}

/// Build SQL WHERE clauses from filters (parameterized — no SQL injection).
/// Returns (clauses, joins, params) where clauses/params must be used with `?` placeholders.
fn build_filter_clauses(
    filters: &SearchFilters,
    table_alias: &str,
) -> (Vec<String>, Vec<String>, Vec<Value>) {
    let mut clauses = Vec::new();
    let mut joins = Vec::new();
    let mut params = Vec::new();

    if let Some(ref repo) = filters.repo {
        if let Some((owner, name)) = repo.split_once('/') {
            if table_alias == "r" {
                clauses.push("u.username = ? AND r.name = ?".to_string());
            } else {
                joins.push(format!(
                    "JOIN repositories r_filt ON r_filt.id = {}.repo_id",
                    table_alias
                ));
                joins.push("LEFT JOIN users u_filt ON u_filt.id = r_filt.owner_id".to_string());
                clauses.push("u_filt.username = ? AND r_filt.name = ?".to_string());
            }
            params.push(Value::from(owner.to_string()));
            params.push(Value::from(name.to_string()));
        } else if table_alias == "r" {
            clauses.push("r.name = ?".to_string());
            params.push(Value::from(repo.to_string()));
        } else {
            joins.push(format!(
                "JOIN repositories r_filt ON r_filt.id = {}.repo_id",
                table_alias
            ));
            clauses.push("r_filt.name = ?".to_string());
            params.push(Value::from(repo.to_string()));
        }
    }

    if let Some(ref author) = filters.author {
        let user_id_column = if table_alias == "r" {
            "owner_id"
        } else {
            "author_id"
        };
        joins.push(format!(
            "LEFT JOIN users u_auth ON u_auth.id = {}.{}",
            table_alias, user_id_column
        ));
        clauses.push("u_auth.username = ?".to_string());
        params.push(Value::from(author.to_string()));
    }

    (clauses, joins, params)
}

/// Build issue-specific filter clauses (state, label) — parameterized.
fn build_issue_filter_clauses(filters: &SearchFilters) -> (Vec<String>, Vec<String>, Vec<Value>) {
    let mut clauses = Vec::new();
    let mut joins = Vec::new();
    let mut params = Vec::new();

    if let Some(ref state) = filters.state {
        if state != "all" {
            let safe_state = match state.as_str() {
                "open" | "closed" => state.as_str(),
                _ => "open",
            };
            clauses.push("i.state = ?".to_string());
            params.push(Value::from(safe_state.to_string()));
        }
    }

    if let Some(ref label) = filters.label {
        joins.push("LEFT JOIN issue_labels il_filt ON il_filt.issue_id = i.id".to_string());
        joins.push("LEFT JOIN labels lbl_filt ON lbl_filt.id = il_filt.label_id".to_string());
        clauses.push("lbl_filt.name = ?".to_string());
        params.push(Value::from(label.to_string()));
    }

    (clauses, joins, params)
}

/// Combine the FTS predicate + filter clauses into a single WHERE clause and the
/// parameter list (query values first, then filter values).
fn combine_where(match_pred: &str, filter_clauses: &[String]) -> (String, Vec<Value>) {
    match (match_pred.is_empty(), filter_clauses.is_empty()) {
        (true, true) => ("1=1".to_string(), Vec::new()),
        (true, false) => (filter_clauses.join(" AND "), Vec::new()),
        (false, true) => (match_pred.to_string(), Vec::new()),
        (false, false) => (
            format!("{} AND ({})", match_pred, filter_clauses.join(" AND ")),
            Vec::new(),
        ),
    }
}

/// Bind values in the order their placeholders appear in the generated SQL:
/// FTS predicate, ordinary filters, then the repeated FTS ranking expression.
fn search_params(
    query_values: &[String],
    filter_params: &[Value],
    include_order_value: bool,
) -> Vec<Value> {
    query_values
        .first()
        .cloned()
        .map(Value::from)
        .into_iter()
        .chain(filter_params.iter().cloned())
        .chain(
            include_order_value
                .then(|| query_values.get(1).cloned().map(Value::from))
                .flatten(),
        )
        .collect()
}

/// Search repositories by name and description, with optional filters.
async fn search_repos(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    offset: u64,
    limit: u64,
) -> Result<(Vec<SearchResult>, i64)> {
    let backend = db.get_database_backend();
    let (filter_clauses, extra_joins, filter_params) = build_filter_clauses(filters, "r");

    let (match_pred, order_clause, query_values) = if raw_query.is_empty() {
        (String::new(), String::new(), Vec::new())
    } else {
        fts_match(backend, "repos_fts", REPOS_FTS_COLS, raw_query)
    };

    let (where_clause, _) = combine_where(&match_pred, &filter_clauses);
    let joins_sql = if extra_joins.is_empty() {
        String::new()
    } else {
        format!("\n{}", extra_joins.join("\n"))
    };

    let sql = format!(
        r#"
        SELECT r.id, r.name as title, r.description as excerpt, u.username as owner_name
        FROM repos_fts
        JOIN repositories r ON r.id = repos_fts.rowid
        LEFT JOIN users u ON u.id = r.owner_id
        {}
        WHERE {}
        {}
        LIMIT {} OFFSET {}
        "#,
        joins_sql, where_clause, order_clause, limit, offset
    );

    let params = search_params(&query_values, &filter_params, true);
    let sql = rg_db::prepare_sql(backend, &sql);

    let rows = db
        .query_all(Statement::from_sql_and_values(backend, &sql, params))
        .await
        .context("fts: search repos")?;

    let mut results = Vec::new();
    for row in rows {
        let id: i64 = row.try_get_by_index(0).unwrap_or(0);
        let title: String = row.try_get_by_index(1).unwrap_or_default();
        let excerpt: Option<String> = row.try_get_by_index(2).ok();
        let owner: Option<String> = row.try_get_by_index(3).ok();
        results.push(SearchResult {
            result_type: "repo".to_string(),
            id,
            title: title.clone(),
            excerpt,
            repo_owner: owner.clone(),
            repo_name: Some(title),
            state: None,
            number: None,
        });
    }

    let count_sql = format!(
        r#"
        SELECT COUNT(DISTINCT repos_fts.rowid)
        FROM repos_fts
        JOIN repositories r ON r.id = repos_fts.rowid
        LEFT JOIN users u ON u.id = r.owner_id
        {}
        WHERE {}
        "#,
        joins_sql, where_clause
    );
    let count_params = search_params(&query_values, &filter_params, false);
    let count_sql = rg_db::prepare_sql(backend, &count_sql);
    let count_rows = db
        .query_all(Statement::from_sql_and_values(
            backend,
            &count_sql,
            count_params,
        ))
        .await
        .context("fts: count repos")?;
    let total: i64 = count_rows
        .first()
        .and_then(|r| r.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);

    Ok((results, total))
}

/// Search issues by title and body, with optional filters (repo, state, author, label).
async fn search_issues(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    offset: u64,
    limit: u64,
) -> Result<(Vec<SearchResult>, i64)> {
    let backend = db.get_database_backend();
    let (mut common_clauses, common_joins, mut common_params) = build_filter_clauses(filters, "i");
    let (issue_clauses, issue_joins, issue_params) = build_issue_filter_clauses(filters);

    common_clauses.extend(issue_clauses);
    common_params.extend(issue_params);
    let all_joins = format!("{}\n{}", common_joins.join("\n"), issue_joins.join("\n"));

    let (match_pred, order_clause, query_values) = if raw_query.is_empty() {
        (String::new(), String::new(), Vec::new())
    } else {
        fts_match(backend, "issues_fts", ISSUES_FTS_COLS, raw_query)
    };

    let (where_clause, _) = combine_where(&match_pred, &common_clauses);

    let sql = format!(
        r#"
        SELECT i.id, i.title, i.body as excerpt, i.repo_id, r.name as repo_name, u.username as owner_name, i.state, i.number
        FROM issues_fts
        JOIN issues i ON i.id = issues_fts.rowid
        JOIN repositories r ON r.id = i.repo_id
        LEFT JOIN users u ON u.id = r.owner_id
        {}
        WHERE {}
        {}
        LIMIT {} OFFSET {}
        "#,
        all_joins, where_clause, order_clause, limit, offset
    );

    let params = search_params(&query_values, &common_params, true);
    let sql = rg_db::prepare_sql(backend, &sql);

    let rows = db
        .query_all(Statement::from_sql_and_values(backend, &sql, params))
        .await
        .context("fts: search issues")?;

    let mut results = Vec::new();
    for row in rows {
        let id: i64 = row.try_get_by_index(0).unwrap_or(0);
        let title: String = row.try_get_by_index(1).unwrap_or_default();
        let excerpt: Option<String> = row.try_get_by_index(2).ok();
        let _repo_id: i64 = row.try_get_by_index(3).unwrap_or(0);
        let repo_name: Option<String> = row.try_get_by_index(4).ok();
        let owner: Option<String> = row.try_get_by_index(5).ok();
        let state: Option<String> = row.try_get_by_index(6).ok();
        let number: Option<i64> = row.try_get_by_index(7).ok();
        results.push(SearchResult {
            result_type: "issue".to_string(),
            id,
            title,
            excerpt,
            repo_owner: owner,
            repo_name,
            state,
            number,
        });
    }

    let count_sql = format!(
        r#"
        SELECT COUNT(DISTINCT i.id)
        FROM issues_fts
        JOIN issues i ON i.id = issues_fts.rowid
        {}
        WHERE {}
        "#,
        all_joins, where_clause
    );
    let count_params = search_params(&query_values, &common_params, false);
    let count_sql = rg_db::prepare_sql(backend, &count_sql);
    let count_rows = db
        .query_all(Statement::from_sql_and_values(
            backend,
            &count_sql,
            count_params,
        ))
        .await
        .context("fts: count issues")?;
    let total: i64 = count_rows
        .first()
        .and_then(|r| r.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);

    Ok((results, total))
}

/// Search wiki pages by title and content, with optional filters.
async fn search_wiki(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    offset: u64,
    limit: u64,
) -> Result<(Vec<SearchResult>, i64)> {
    let backend = db.get_database_backend();
    let (filter_clauses, extra_joins, filter_params) = build_filter_clauses(filters, "w");

    let (match_pred, order_clause, query_values) = if raw_query.is_empty() {
        (String::new(), String::new(), Vec::new())
    } else {
        fts_match(backend, "wiki_pages_fts", WIKI_FTS_COLS, raw_query)
    };

    let (where_clause, _) = combine_where(&match_pred, &filter_clauses);

    let sql = format!(
        r#"
        SELECT w.id, w.title, SUBSTR(w.content, 1, 200) as excerpt, w.repo_id, r.name as repo_name, u.username as owner_name
        FROM wiki_pages_fts
        JOIN wiki_pages w ON w.id = wiki_pages_fts.rowid
        JOIN repositories r ON r.id = w.repo_id
        LEFT JOIN users u ON u.id = r.owner_id
        {}
        WHERE {}
        {}
        LIMIT {} OFFSET {}
        "#,
        extra_joins.join("\n"),
        where_clause,
        order_clause,
        limit,
        offset
    );

    let params = search_params(&query_values, &filter_params, true);
    let sql = rg_db::prepare_sql(backend, &sql);

    let rows = db
        .query_all(Statement::from_sql_and_values(backend, &sql, params))
        .await
        .context("fts: search wiki")?;

    let mut results = Vec::new();
    for row in rows {
        let id: i64 = row.try_get_by_index(0).unwrap_or(0);
        let title: String = row.try_get_by_index(1).unwrap_or_default();
        let excerpt: Option<String> = row.try_get_by_index(2).ok();
        let _repo_id: i64 = row.try_get_by_index(3).unwrap_or(0);
        let repo_name: Option<String> = row.try_get_by_index(4).ok();
        let owner: Option<String> = row.try_get_by_index(5).ok();
        results.push(SearchResult {
            result_type: "wiki".to_string(),
            id,
            title,
            excerpt,
            repo_owner: owner,
            repo_name,
            state: None,
            number: None,
        });
    }

    let joins_sql = if extra_joins.is_empty() {
        String::new()
    } else {
        format!("\n{}", extra_joins.join("\n"))
    };

    let count_sql = format!(
        r#"
        SELECT COUNT(DISTINCT w.id)
        FROM wiki_pages_fts
        JOIN wiki_pages w ON w.id = wiki_pages_fts.rowid
        JOIN repositories r ON r.id = w.repo_id
        LEFT JOIN users u ON u.id = r.owner_id
        {}
        WHERE {}
        "#,
        joins_sql, where_clause
    );
    let count_params = search_params(&query_values, &filter_params, false);
    let count_sql = rg_db::prepare_sql(backend, &count_sql);
    let count_rows = db
        .query_all(Statement::from_sql_and_values(
            backend,
            &count_sql,
            count_params,
        ))
        .await
        .context("fts: count wiki")?;
    let total: i64 = count_rows
        .first()
        .and_then(|r| r.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);

    Ok((results, total))
}
