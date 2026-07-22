//! Cross-backend full-text-search (FTS) SQL dialect helpers.
//!
//! IronForge supports three database backends. Each expresses full-text search
//! very differently:
//!
//! | Backend    | FTS mechanism                                  |
//! |-----------|-------------------------------------------------|
//! | SQLite     | FTS5 virtual tables + `MATCH` + `rank` + `snippet()` |
//! | Postgres   | `tsvector` generated column + GIN index + `plainto_tsquery` + `ts_rank_cd` + `ts_headline` |
//! | MySQL      | `FULLTEXT` index + `MATCH(... ) AGAINST (... )`    |
//!
//! This module centralises the backend-specific SQL fragments so the search
//! service and code indexer stay backend-agnostic. The DDL that *creates* the
//! FTS tables lives in the rg-db migrations (one per backend), mirroring the
//! column layouts assumed here.

use sea_orm::DatabaseBackend;

/// Column sets per FTS table. Used for MySQL `MATCH (...)` and as documentation
/// of the indexed columns for the other backends.
pub const REPOS_FTS_COLS: &[&str] = &["name", "description"];
pub const ISSUES_FTS_COLS: &[&str] = &["title", "body"];
pub const WIKI_FTS_COLS: &[&str] = &["title", "content"];
/// code_fts indexes content + metadata so file/path/language are searchable too.
pub const CODE_FTS_COLS: &[&str] = &["content", "file_path", "file_name", "language"];

/// SQLite FTS5 phrase-escape: wrap in double quotes and double inner quotes.
pub fn fts_phrase_escape(query: &str) -> String {
    let escaped = query.replace('"', "\"\"");
    format!("\"{}\"", escaped)
}

/// Produce the FTS `WHERE` predicate, `ORDER BY` clause, and the list of query
/// values to bind (parameterized).
///
/// The returned `query_values` length tells the caller how many times to push
/// the (transformed) query: **1 for SQLite**, **2 for Postgres/MySQL** — the
/// `ORDER BY` re-references the same `tsquery` / `AGAINST` expression.
///
/// * `table`   — the FTS table name (e.g. `"repos_fts"`).
/// * `columns` — the text columns for MySQL's `MATCH (...)`. Ignored elsewhere.
/// * `raw_query` — the user's search text (not yet escaped).
pub fn fts_match(
    backend: DatabaseBackend,
    table: &str,
    columns: &[&str],
    raw_query: &str,
) -> (String, String, Vec<String>) {
    match backend {
        DatabaseBackend::Sqlite => (
            format!("{table} MATCH ?"),
            "ORDER BY rank".to_string(),
            vec![fts_phrase_escape(raw_query)],
        ),
        DatabaseBackend::Postgres => (
            format!("{table}.tsv @@ plainto_tsquery('simple', ?)"),
            format!("ORDER BY ts_rank_cd({table}.tsv, plainto_tsquery('simple', ?)) DESC"),
            vec![raw_query.to_string(), raw_query.to_string()],
        ),
        DatabaseBackend::MySql => {
            let cols = columns
                .iter()
                .map(|column| format!("{table}.{column}"))
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!("MATCH({cols}) AGAINST (? IN NATURAL LANGUAGE MODE)"),
                format!("ORDER BY MATCH({cols}) AGAINST (? IN NATURAL LANGUAGE MODE) DESC"),
                vec![raw_query.to_string(), raw_query.to_string()],
            )
        }
    }
}

/// SELECT expression for a highlighted snippet of `content` in code search.
///
/// * SQLite uses FTS5 `snippet()` (HTML-bold markers).
/// * Postgres / MySQL return a plain substring — no HTML highlight. The caller
///   stores the result in the `snippet` field. `has_query` should be `false`
///   when there is no FTS predicate (e.g. empty query), in which case we just
///   return the raw `content` column.
pub fn code_fts_snippet_expr(backend: DatabaseBackend, table: &str, has_query: bool) -> String {
    if !has_query {
        return format!("{table}.content");
    }
    match backend {
        DatabaseBackend::Sqlite => format!("snippet({table}, 3, '<b>', '</b>', '...', 20)"),
        DatabaseBackend::Postgres | DatabaseBackend::MySql => {
            format!("SUBSTRING({table}.content, 1, 200)")
        }
    }
}

/// Build an upsert (insert-or-update) statement for the *metadata* FTS tables
/// (`repos_fts` / `issues_fts` / `wiki_pages_fts`), which are keyed on `rowid`
/// and maintained both by DB triggers and by explicit writes in service code.
///
/// `non_key_cols` is a comma-separated list of the remaining columns, e.g.
/// `"name, description"`. The bound-parameter order must be: `rowid` first,
/// then each non-key column in the same order.
///
/// * SQLite uses `INSERT OR REPLACE`.
/// * Postgres uses `INSERT ... ON CONFLICT (rowid) DO UPDATE`.
/// * MySQL uses `INSERT ... ON DUPLICATE KEY UPDATE`.
///
/// Using an upsert (instead of a plain `INSERT`) is what keeps the explicit
/// writes coexisting safely with the DB triggers across all three backends:
/// the trigger already inserted the row, so the explicit write must not fail
/// with a duplicate-key error.
pub fn metadata_fts_upsert_sql(
    backend: DatabaseBackend,
    table: &str,
    non_key_cols: &str,
) -> String {
    let placeholders = vec!["?"; non_key_cols.split(',').count() + 1].join(", ");
    match backend {
        DatabaseBackend::Sqlite => {
            format!("INSERT OR REPLACE INTO {table}(rowid, {non_key_cols}) VALUES ({placeholders})")
        }
        DatabaseBackend::Postgres => {
            let set = non_key_cols
                .split(',')
                .map(|c| {
                    let c = c.trim();
                    format!("{c} = EXCLUDED.{c}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "INSERT INTO {table}(rowid, {non_key_cols}) VALUES ({placeholders}) ON CONFLICT (rowid) DO UPDATE SET {set}"
            )
        }
        DatabaseBackend::MySql => {
            let set = non_key_cols
                .split(',')
                .map(|c| {
                    let c = c.trim();
                    format!("{c} = VALUES({c})")
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "INSERT INTO {table}(rowid, {non_key_cols}) VALUES ({placeholders}) ON DUPLICATE KEY UPDATE {set}"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_fts_columns_are_qualified_to_avoid_join_ambiguity() {
        let (predicate, order, values) = fts_match(
            DatabaseBackend::MySql,
            "issues_fts",
            ISSUES_FTS_COLS,
            "needle",
        );
        assert!(predicate.contains("issues_fts.title, issues_fts.body"));
        assert!(order.contains("issues_fts.title, issues_fts.body"));
        assert!(predicate.ends_with("AGAINST (? IN NATURAL LANGUAGE MODE)"));
        assert!(order.ends_with("AGAINST (? IN NATURAL LANGUAGE MODE) DESC"));
        assert_eq!(values, ["needle", "needle"]);
    }
}
