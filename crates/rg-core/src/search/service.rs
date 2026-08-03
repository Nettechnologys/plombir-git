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
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, QueryResult, Statement, TryGetable, Value,
};
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
    // `page` is whatever the caller put in the query string, so the product is
    // saturated rather than left to wrap: an overflowed offset would silently
    // hand back page one under the name of page 10^18.
    let offset = page.saturating_sub(1).saturating_mul(per_page);
    let limit = per_page.min(100);

    let filters = SearchFilters::parse(raw_query);
    let raw_text = filters.query.as_str();

    match search_type {
        "repos" => Ok((
            fetch_repos(db, raw_text, &filters, viewer_id, offset, limit).await?,
            count_repos(db, raw_text, &filters, viewer_id).await?,
        )),
        "issues" => Ok((
            fetch_issues(db, raw_text, &filters, viewer_id, offset, limit).await?,
            count_issues(db, raw_text, &filters, viewer_id).await?,
        )),
        "wiki" => Ok((
            fetch_wiki(db, raw_text, &filters, viewer_id, offset, limit).await?,
            count_wiki(db, raw_text, &filters, viewer_id).await?,
        )),
        "all" => search_all(db, raw_text, &filters, viewer_id, offset, limit).await,
        _ => Ok((Vec::new(), 0)),
    }
}

/// The kinds a `type=all` page interleaves, in the order they take their turn.
const KINDS: usize = 3;

/// `type=all` pages **one** sequence, not three independent ones.
///
/// The three backends are merged round-robin: each contributes its next match
/// in turn until it runs out. That sequence is fixed by the three totals alone,
/// so the window `[offset, offset + limit)` of it maps back to exactly one
/// contiguous slice per backend — which is what makes every match reachable on
/// exactly one page, keeps pages full, and leaves `total` counting the same
/// sequence the pages walk.
///
/// Handing each backend the *page's* own `offset`/`limit` and then cutting the
/// concatenation a second time (as this used to) is not a bad sort order — it
/// is a hole: on page one the trim keeps the repositories and drops everything
/// the other two backends returned, and on page two those same rows have moved
/// out of the window, so the first `per_page` issues are served by no page at
/// all while `total` keeps promising them.
///
/// The counts must be known before the rows — they are what places a match in
/// the merged order — but that costs no extra round-trip: each backend was
/// already answering a `COUNT` next to its page.
async fn search_all(
    db: &DatabaseConnection,
    raw_text: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<SearchResult>, i64)> {
    let counts = [
        count_repos(db, raw_text, filters, viewer_id).await?,
        count_issues(db, raw_text, filters, viewer_id).await?,
        count_wiki(db, raw_text, filters, viewer_id).await?,
    ];
    let total: i64 = counts.iter().sum();
    let lengths = counts.map(|count| count.max(0) as u64);

    let mut page: Vec<(u64, SearchResult)> = Vec::new();
    for kind in 0..KINDS {
        let (slice_offset, slice_limit) = merge_slice(&lengths, kind, offset, limit);
        if slice_limit == 0 {
            continue;
        }
        let rows = match kind {
            0 => fetch_repos(db, raw_text, filters, viewer_id, slice_offset, slice_limit).await?,
            1 => fetch_issues(db, raw_text, filters, viewer_id, slice_offset, slice_limit).await?,
            _ => fetch_wiki(db, raw_text, filters, viewer_id, slice_offset, slice_limit).await?,
        };
        for (index, row) in rows.into_iter().enumerate() {
            page.push((
                merge_position(&lengths, kind, slice_offset + index as u64),
                row,
            ));
        }
    }

    page.sort_by_key(|(position, _)| *position);
    Ok((page.into_iter().map(|(_, row)| row).collect(), total))
}

/// Where the `index`-th match of `kind` lands in the round-robin sequence.
///
/// Round `t` emits one match from every kind that still has one, in kind order,
/// so everything from rounds before `index` comes first, then the kinds ahead
/// of this one within round `index` itself.
fn merge_position(lengths: &[u64; KINDS], kind: usize, index: u64) -> u64 {
    let earlier_rounds: u64 = lengths.iter().map(|&length| length.min(index)).sum();
    let ahead_this_round = lengths[..kind]
        .iter()
        .filter(|&&length| length > index)
        .count() as u64;
    earlier_rounds + ahead_this_round
}

/// The half-open slice of `kind`'s own ordering that the merged window
/// `[offset, offset + limit)` draws from, as `(offset, limit)` for its SQL.
fn merge_slice(lengths: &[u64; KINDS], kind: usize, offset: u64, limit: u64) -> (u64, u64) {
    let length = lengths[kind];
    if length == 0 || limit == 0 {
        return (0, 0);
    }
    let window_end = offset.saturating_add(limit);
    let start = first_index_where(length, |index| {
        merge_position(lengths, kind, index) >= offset
    });
    let end = first_index_where(length, |index| {
        merge_position(lengths, kind, index) >= window_end
    });
    (start, end - start)
}

/// The first index in `0..length` satisfying `reached`, or `length` if none
/// does. `merge_position` is strictly increasing in the index, so the predicate
/// is monotone and a binary search is exact — which is what keeps a deep page
/// from having to read everything before it.
fn first_index_where(length: u64, reached: impl Fn(u64) -> bool) -> u64 {
    let (mut low, mut high) = (0u64, length);
    while low < high {
        let mid = low + (high - low) / 2;
        if reached(mid) {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    low
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

/// The `WHERE` / `ORDER BY` / parameters a backend's page query and its
/// `COUNT` are both built from. One value feeding both is what keeps `total` an
/// answer about the very rows the page is drawn from.
struct QueryParts {
    joins_sql: String,
    where_clause: String,
    order_clause: String,
    params: Vec<Value>,
    count_params: Vec<Value>,
}

/// Append the tiebreaker a paged query needs on top of the FTS ranking.
///
/// A page and its successor are only disjoint if the rows carry a total order.
/// The rank alone does not give one — ties are resolved however the engine
/// happens to scan — and once the qualifiers have eaten the whole query there
/// is no rank at all, so `LIMIT/OFFSET` would page over an order the database
/// is free to change between two requests, serving a match twice or never.
/// The primary key breaks every remaining tie.
fn ordered_by(rank_clause: &str, id_column: &str) -> String {
    if rank_clause.is_empty() {
        format!("ORDER BY {id_column} DESC")
    } else {
        format!("{rank_clause}, {id_column} DESC")
    }
}

/// Shared SQL for the repository backend.
fn repos_parts(
    backend: DatabaseBackend,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
) -> QueryParts {
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

    QueryParts {
        params: search_params(&query_values, &filter_params, true),
        count_params: search_params(&query_values, &filter_params, false),
        order_clause: ordered_by(&order_clause, "r.id"),
        joins_sql,
        where_clause,
    }
}

/// One page of repositories matching by name and description.
async fn fetch_repos(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
    offset: u64,
    limit: u64,
) -> Result<Vec<SearchResult>> {
    let backend = db.get_database_backend();
    let parts = repos_parts(backend, raw_query, filters, viewer_id);

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
        parts.joins_sql, parts.where_clause, parts.order_clause, limit, offset
    );

    let sql = rg_db::prepare_sql(backend, &sql);

    let rows = db
        .query_all(Statement::from_sql_and_values(backend, &sql, parts.params))
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

    Ok(results)
}

/// How many repositories match — the same predicate the page is cut from.
async fn count_repos(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
) -> Result<i64> {
    let backend = db.get_database_backend();
    let parts = repos_parts(backend, raw_query, filters, viewer_id);

    let count_sql = format!(
        r#"
        SELECT COUNT(DISTINCT repos_fts.rowid)
        FROM repos_fts
        JOIN repositories r ON r.id = repos_fts.rowid
        LEFT JOIN users u ON u.id = r.owner_id
        {}
        WHERE {}
        "#,
        parts.joins_sql, parts.where_clause
    );
    let count_sql = rg_db::prepare_sql(backend, &count_sql);
    let count_rows = db
        .query_all(Statement::from_sql_and_values(
            backend,
            &count_sql,
            parts.count_params,
        ))
        .await
        .context("fts: count repos")?;
    decode_total(&count_rows, "count repos")
}

/// Shared SQL for the issue backend (repo, state, author, label filters).
fn issues_parts(
    backend: DatabaseBackend,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
) -> QueryParts {
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

    QueryParts {
        params: search_params(&query_values, &common_params, true),
        count_params: search_params(&query_values, &common_params, false),
        order_clause: ordered_by(&order_clause, "i.id"),
        joins_sql: all_joins,
        where_clause,
    }
}

/// One page of issues matching by title and body.
async fn fetch_issues(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
    offset: u64,
    limit: u64,
) -> Result<Vec<SearchResult>> {
    let backend = db.get_database_backend();
    let parts = issues_parts(backend, raw_query, filters, viewer_id);

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
        parts.joins_sql, parts.where_clause, parts.order_clause, limit, offset
    );

    let params = parts.params;
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

    Ok(results)
}

/// How many issues match — the same predicate the page is cut from.
async fn count_issues(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
) -> Result<i64> {
    let backend = db.get_database_backend();
    let parts = issues_parts(backend, raw_query, filters, viewer_id);

    let count_sql = format!(
        r#"
        SELECT COUNT(DISTINCT i.id)
        FROM issues_fts
        JOIN issues i ON i.id = issues_fts.rowid
        JOIN repositories r ON r.id = i.repo_id
        {}
        WHERE {}
        "#,
        parts.joins_sql, parts.where_clause
    );
    let count_sql = rg_db::prepare_sql(backend, &count_sql);
    let count_rows = db
        .query_all(Statement::from_sql_and_values(
            backend,
            &count_sql,
            parts.count_params,
        ))
        .await
        .context("fts: count issues")?;
    decode_total(&count_rows, "count issues")
}

/// Shared SQL for the wiki backend.
fn wiki_parts(
    backend: DatabaseBackend,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
) -> QueryParts {
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
    let joins_sql = if extra_joins.is_empty() {
        String::new()
    } else {
        format!("\n{}", extra_joins.join("\n"))
    };

    QueryParts {
        params: search_params(&query_values, &filter_params, true),
        count_params: search_params(&query_values, &filter_params, false),
        order_clause: ordered_by(&order_clause, "w.id"),
        joins_sql,
        where_clause,
    }
}

/// One page of wiki pages matching by title and content.
async fn fetch_wiki(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
    offset: u64,
    limit: u64,
) -> Result<Vec<SearchResult>> {
    let backend = db.get_database_backend();
    let parts = wiki_parts(backend, raw_query, filters, viewer_id);

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
        parts.joins_sql, parts.where_clause, parts.order_clause, limit, offset
    );

    let params = parts.params;
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

    Ok(results)
}

/// How many wiki pages match — the same predicate the page is cut from.
async fn count_wiki(
    db: &DatabaseConnection,
    raw_query: &str,
    filters: &SearchFilters,
    viewer_id: Option<i64>,
) -> Result<i64> {
    let backend = db.get_database_backend();
    let parts = wiki_parts(backend, raw_query, filters, viewer_id);

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
        parts.joins_sql, parts.where_clause
    );
    let count_sql = rg_db::prepare_sql(backend, &count_sql);
    let count_rows = db
        .query_all(Statement::from_sql_and_values(
            backend,
            &count_sql,
            parts.count_params,
        ))
        .await
        .context("fts: count wiki")?;
    decode_total(&count_rows, "count wiki")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Database;

    /// One `type=all` page, expressed the way `search_all` builds it: plan a
    /// slice per kind, then put the slices back in merged order. Each element
    /// is `(kind, index within that kind)`.
    fn merged_page(lengths: &[u64; KINDS], offset: u64, limit: u64) -> Vec<(usize, u64)> {
        let mut page = Vec::new();
        for kind in 0..KINDS {
            let (slice_offset, slice_limit) = merge_slice(lengths, kind, offset, limit);
            for index in slice_offset..slice_offset + slice_limit {
                page.push((merge_position(lengths, kind, index), (kind, index)));
            }
        }
        page.sort_by_key(|(position, _)| *position);
        page.into_iter().map(|(_, item)| item).collect()
    }

    /// The property the old double-cut broke: walking the pages of `type=all`
    /// must hand out every match exactly once, and `total` must be the number
    /// of matches handed out. Checked over every shape of corpus small enough
    /// to enumerate — including the one from the report, where one kind alone
    /// is longer than a page and used to hide the other two entirely.
    #[test]
    fn every_match_is_reachable_on_exactly_one_page() {
        for repos in 0..=5u64 {
            for issues in 0..=5u64 {
                for wiki in 0..=5u64 {
                    let lengths = [repos, issues, wiki];
                    let total = repos + issues + wiki;
                    for per_page in 1..=4u64 {
                        let mut seen = Vec::new();
                        let pages = total.div_ceil(per_page);
                        for page in 0..pages {
                            let items = merged_page(&lengths, page * per_page, per_page);
                            let expected = (total - page * per_page).min(per_page);
                            assert_eq!(
                                items.len() as u64,
                                expected,
                                "{lengths:?} per_page={per_page} page={page}: short page"
                            );
                            seen.extend(items);
                        }
                        assert!(
                            merged_page(&lengths, pages * per_page, per_page).is_empty(),
                            "{lengths:?} per_page={per_page}: a page past `total` returned rows"
                        );

                        let mut unique = seen.clone();
                        unique.sort_unstable();
                        unique.dedup();
                        assert_eq!(
                            unique.len(),
                            seen.len(),
                            "{lengths:?} per_page={per_page}: a match was served twice"
                        );
                        assert_eq!(
                            unique.len() as u64,
                            total,
                            "{lengths:?} per_page={per_page}: matches served != total"
                        );
                    }
                }
            }
        }
    }

    /// The visible half of the same bug: with 40 matching repositories and 40
    /// matching issues, page one used to be repositories only.
    #[test]
    fn the_first_page_mixes_the_kinds_that_have_matches() {
        let page = merged_page(&[40, 40, 0], 0, 20);
        let kinds: Vec<usize> = page.iter().map(|(kind, _)| *kind).collect();
        assert!(
            kinds.contains(&0) && kinds.contains(&1),
            "page one drew from one kind only: {kinds:?}"
        );
        assert_eq!(
            &kinds[..4],
            &[0, 1, 0, 1],
            "the kinds must alternate while both still have matches"
        );

        // The first issue is on page one, not lost between the pages.
        assert_eq!(page[1], (1, 0));
    }

    /// A kind that runs out mid-corpus must not leave a gap: the rest of the
    /// pages stay full and keep the surviving kinds in order.
    #[test]
    fn an_exhausted_kind_yields_its_turn() {
        // Repos run out after 2, wiki after 1; issues carry the tail.
        let lengths = [2, 5, 1];
        assert_eq!(
            merged_page(&lengths, 0, 4),
            vec![(0, 0), (1, 0), (2, 0), (0, 1)]
        );
        assert_eq!(
            merged_page(&lengths, 4, 4),
            vec![(1, 1), (1, 2), (1, 3), (1, 4)]
        );
        assert_eq!(merged_page(&lengths, 7, 4), vec![(1, 4)]);
    }

    /// A page past the corpus asks each backend for nothing at all — the empty
    /// slice is what `search_all` skips the round-trip on.
    #[test]
    fn a_page_past_the_corpus_plans_no_query() {
        let lengths = [3, 3, 3];
        for kind in 0..KINDS {
            assert_eq!(merge_slice(&lengths, kind, 9, 20).1, 0);
            assert_eq!(merge_slice(&lengths, kind, u64::MAX, 20).1, 0);
        }
    }

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
