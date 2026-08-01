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
use sea_orm::{ConnectionTrait, DatabaseConnection, QueryResult, Statement, TryGetable, Value};
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
///
/// `viewer_id` (`None` = anonymous) decides which repositories are in scope.
/// It is a parameter rather than a post-filter on the results because the
/// backends page in SQL: dropping rows afterwards would return short pages and
/// a `total` counting repos the caller cannot open.
pub async fn search(
    db: &DatabaseConnection,
    raw_query: &str,
    search_type: &str,
    viewer_id: Option<i64>,
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
        let (repos, count) = search_repos(db, raw_text, &filters, viewer_id, offset, limit).await?;
        total += count;
        results.extend(repos);
    }

    if search_type == "all" || search_type == "issues" {
        let (issues, count) =
            search_issues(db, raw_text, &filters, viewer_id, offset, limit).await?;
        total += count;
        results.extend(issues);
    }

    if search_type == "all" || search_type == "wiki" {
        let (wiki, count) = search_wiki(db, raw_text, &filters, viewer_id, offset, limit).await?;
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

/// Restrict a search to the repositories `viewer_id` (`None` = anonymous) is
/// allowed to see, `repo_alias` being the alias the `repositories` table is
/// joined under. Every branch is parameterized — the alias is the only thing
/// interpolated, and it is a literal at each call site.
///
/// Mirrors `rg_core::repo::service::can_read_repo` (owner, collaborator, org
/// member) and additionally drops soft-deleted repos, which stay in the FTS
/// tables after deletion because the row itself is never removed.
fn build_visibility_clause(repo_alias: &str, viewer_id: Option<i64>) -> (String, Vec<Value>) {
    let mut params = vec![Value::from(false)];

    let visible = match viewer_id {
        None => format!("{repo_alias}.is_private = ?"),
        Some(viewer) => {
            params.extend([
                Value::from(viewer),
                Value::from(viewer),
                Value::from(viewer),
            ]);
            format!(
                "({repo_alias}.is_private = ? \
                 OR {repo_alias}.owner_id = ? \
                 OR EXISTS (SELECT 1 FROM repo_collaborators rc_vis \
                            WHERE rc_vis.repo_id = {repo_alias}.id AND rc_vis.user_id = ?) \
                 OR EXISTS (SELECT 1 FROM organization_members om_vis \
                            WHERE om_vis.org_id = {repo_alias}.org_id AND om_vis.user_id = ?))"
            )
        }
    };

    (
        format!("{visible} AND {repo_alias}.deleted_at IS NULL"),
        params,
    )
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

/// Decode a column the search projection is obliged to produce.
///
/// `id` and `title` are `NOT NULL` in all three projections, so a value that
/// will not decode is a schema drift or a backend type mismatch — not a row
/// that happens to be blank. Reading it as `unwrap_or(0)` / `unwrap_or_default`
/// is how such a row leaves the server as a result pointing at entity `0` with
/// an empty title: a coherent-looking answer with nothing to trace it back to.
/// A decoded value is the only thing allowed out.
fn decode_required<T: TryGetable>(
    row: &QueryResult,
    index: usize,
    what: &str,
    column: &str,
) -> Result<T> {
    row.try_get_by_index::<T>(index)
        .with_context(|| format!("fts: {what}: decode `{column}`"))
}

/// Decode a nullable column, keeping `NULL` and "would not decode" apart.
///
/// Reading these as `try_get_by_index(..).ok()` collapses both into `None`, so
/// an owner or excerpt that failed to decode is served as the absence of an
/// owner or excerpt. Only SQL `NULL` may answer `None`.
fn decode_optional<T: TryGetable>(
    row: &QueryResult,
    index: usize,
    what: &str,
    column: &str,
) -> Result<Option<T>> {
    row.try_get_by_index::<Option<T>>(index)
        .with_context(|| format!("fts: {what}: decode `{column}`"))
}

/// Decode the single row a `COUNT(...)` aggregate is obliged to return.
///
/// Mirrors `rg_db::ops::org_ops::decode_count`: a missing row and an
/// undecodable count are failures of the count, not a total of zero. The total
/// travels next to the results, so folding it into `0` publishes a page that
/// contradicts itself — rows in hand, and a `total` telling the client there
/// were none to page through.
fn decode_total(rows: &[QueryResult], what: &str) -> Result<i64> {
    let row = rows
        .first()
        .with_context(|| format!("fts: {what}: aggregate returned no row"))?;
    decode_required(row, 0, what, "count")
}

/// Search repositories by name and description, with optional filters.
async fn search_repos(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<SearchResult>, i64)> {
    let backend = db.get_database_backend();
    let (mut filter_clauses, extra_joins, mut filter_params) = build_filter_clauses(filters, "r");

    let (visibility, visibility_params) = build_visibility_clause("r", viewer_id);
    filter_clauses.push(visibility);
    filter_params.extend(visibility_params);

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
        let id: i64 = decode_required(&row, 0, "search repos", "id")?;
        let title: String = decode_required(&row, 1, "search repos", "title")?;
        let excerpt: Option<String> = decode_optional(&row, 2, "search repos", "excerpt")?;
        let owner: Option<String> = decode_optional(&row, 3, "search repos", "owner_name")?;
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
    let total = decode_total(&count_rows, "count repos")?;

    Ok((results, total))
}

/// Search issues by title and body, with optional filters (repo, state, author, label).
async fn search_issues(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<SearchResult>, i64)> {
    let backend = db.get_database_backend();
    let (mut common_clauses, common_joins, mut common_params) = build_filter_clauses(filters, "i");
    let (issue_clauses, issue_joins, issue_params) = build_issue_filter_clauses(filters);

    common_clauses.extend(issue_clauses);
    common_params.extend(issue_params);

    let (visibility, visibility_params) = build_visibility_clause("r", viewer_id);
    common_clauses.push(visibility);
    common_params.extend(visibility_params);

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
        // `i.repo_id` (index 3) is selected but never published, so it is left
        // undecoded: a column no answer is built from must not be able to fail
        // the search.
        let id: i64 = decode_required(&row, 0, "search issues", "id")?;
        let title: String = decode_required(&row, 1, "search issues", "title")?;
        let excerpt: Option<String> = decode_optional(&row, 2, "search issues", "excerpt")?;
        let repo_name: Option<String> = decode_optional(&row, 4, "search issues", "repo_name")?;
        let owner: Option<String> = decode_optional(&row, 5, "search issues", "owner_name")?;
        let state: Option<String> = decode_optional(&row, 6, "search issues", "state")?;
        let number: Option<i64> = decode_optional(&row, 7, "search issues", "number")?;
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
        JOIN repositories r ON r.id = i.repo_id
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
    let total = decode_total(&count_rows, "count issues")?;

    Ok((results, total))
}

/// Search wiki pages by title and content, with optional filters.
async fn search_wiki(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<SearchResult>, i64)> {
    let backend = db.get_database_backend();
    let (mut filter_clauses, extra_joins, mut filter_params) = build_filter_clauses(filters, "w");

    let (visibility, visibility_params) = build_visibility_clause("r", viewer_id);
    filter_clauses.push(visibility);
    filter_params.extend(visibility_params);

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
        // `w.repo_id` (index 3) is selected but never published — see the same
        // note in `search_issues`.
        let id: i64 = decode_required(&row, 0, "search wiki", "id")?;
        let title: String = decode_required(&row, 1, "search wiki", "title")?;
        let excerpt: Option<String> = decode_optional(&row, 2, "search wiki", "excerpt")?;
        let repo_name: Option<String> = decode_optional(&row, 4, "search wiki", "repo_name")?;
        let owner: Option<String> = decode_optional(&row, 5, "search wiki", "owner_name")?;
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
    let total = decode_total(&count_rows, "count wiki")?;

    Ok((results, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Database;

    /// The decoders are the whole fix, and no caller can reach the branch they
    /// replace: on a healthy schema every one of these columns decodes. So they
    /// are tested here, on the rows a schema drift or a backend type mismatch
    /// actually produces — a column of the wrong type, and an aggregate that
    /// returned nothing.
    async fn memory_db() -> DatabaseConnection {
        Database::connect("sqlite::memory:")
            .await
            .expect("open an in-memory database")
    }

    async fn one_row(db: &DatabaseConnection, sql: &str) -> QueryResult {
        db.query_one(Statement::from_string(
            db.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("run the query")
        .expect("the query returned a row")
    }

    async fn all_rows(db: &DatabaseConnection, sql: &str) -> Vec<QueryResult> {
        db.query_all(Statement::from_string(
            db.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("run the aggregate")
    }

    #[tokio::test]
    async fn a_decoded_zero_total_is_still_a_total() {
        let db = memory_db().await;
        assert_eq!(
            decode_total(&all_rows(&db, "SELECT 0").await, "count repos").expect("zero decodes"),
            0,
            "an honest total of zero must stay a total, not become an error"
        );
    }

    #[tokio::test]
    async fn an_absent_aggregate_row_is_an_error_not_a_zero_total() {
        let error = decode_total(&[], "count repos")
            .expect_err("a COUNT(...) that returned no row did not answer the question");
        assert!(
            format!("{error:#}").contains("aggregate returned no row"),
            "the failure must name itself, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn an_undecodable_total_is_an_error_not_a_zero_total() {
        let db = memory_db().await;
        let rows = all_rows(&db, "SELECT 'not a number'").await;

        // Pin the regression to the input rather than to our wording: this is
        // exactly the row the pre-fix fold could not tell apart from an empty
        // result set.
        let old_fold = rows
            .first()
            .and_then(|r| r.try_get_by_index::<i64>(0).ok())
            .unwrap_or(0);
        assert_eq!(
            old_fold, 0,
            "the pre-fix expression answered `total = 0` here — that is the bug being guarded"
        );

        let error = decode_total(&rows, "count issues")
            .expect_err("a count that will not decode is a failed count, not a total of zero");
        assert!(
            format!("{error:#}").contains("decode `count`"),
            "the failure must name itself, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn an_undecodable_id_is_an_error_not_a_result_pointing_at_entity_zero() {
        let db = memory_db().await;
        let row = one_row(&db, "SELECT 'not a number' AS id").await;

        let old_fold: i64 = row.try_get_by_index(0).unwrap_or(0);
        assert_eq!(
            old_fold, 0,
            "the pre-fix expression published this row as a link to entity 0"
        );

        let error = decode_required::<i64>(&row, 0, "search repos", "id")
            .expect_err("a row whose `id` will not decode is not a result about entity 0");
        assert!(
            format!("{error:#}").contains("decode `id`"),
            "the failure must name itself, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn an_undecodable_title_is_an_error_not_an_empty_title() {
        let db = memory_db().await;
        let row = one_row(&db, "SELECT 7 AS title").await;

        let old_fold: String = row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(
            old_fold, "",
            "the pre-fix expression published this row with no title at all"
        );

        let error = decode_required::<String>(&row, 0, "search wiki", "title")
            .expect_err("a row whose `title` will not decode is not a row with an empty title");
        assert!(
            format!("{error:#}").contains("decode `title`"),
            "the failure must name itself, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn a_null_optional_column_is_absent_and_an_undecodable_one_is_an_error() {
        let db = memory_db().await;

        let null_row = one_row(&db, "SELECT NULL AS owner_name").await;
        assert_eq!(
            decode_optional::<String>(&null_row, 0, "search repos", "owner_name")
                .expect("a NULL column decodes"),
            None,
            "a repository with no owner row is still an answer, not a failure"
        );

        // The two inputs the old `.ok()` could not tell apart: the NULL above,
        // and this one.
        let bad_row = one_row(&db, "SELECT 7 AS owner_name").await;
        let old_fold: Option<String> = bad_row.try_get_by_index(0).ok();
        assert_eq!(
            old_fold, None,
            "the pre-fix expression served an undecodable owner as no owner"
        );

        let error = decode_optional::<String>(&bad_row, 0, "search repos", "owner_name")
            .expect_err("a column that will not decode is not an absent column");
        assert!(
            format!("{error:#}").contains("decode `owner_name`"),
            "the failure must name itself, got: {error:#}"
        );
    }
}
