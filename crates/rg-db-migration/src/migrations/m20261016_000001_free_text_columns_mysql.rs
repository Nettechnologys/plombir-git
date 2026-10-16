//! Text a person writes did not fit its column on MySQL (card_9ea48ee6a422).
//!
//! sea-query's `string()` is `VARCHAR(255)` on MySQL and an unbounded type on
//! PostgreSQL and SQLite, so the tests on SQLite never saw it. Under MySQL's
//! default strict mode any issue, pull request, comment, review or wiki body
//! longer than 255 characters was `ERROR 1406 Data too long` — a `500`.
//!
//! The same width sat under values the server composes or receives without a
//! ceiling of its own:
//! - `pipeline_jobs.script` holds a job's commands and `pipeline_jobs.log` the
//!   full log a runner uploads in replace mode (up to 8 MiB);
//! - `notifications.title` is `owner/repo #N: <title>`, longer than the title;
//! - `packages.description` / `homepage` / `repository_url` come from client
//!   manifests (the RubyGems adapter alone keeps up to 500 characters);
//! - the JSON lists in `protected_branches`, `runners.labels`,
//!   `pipeline_jobs.tags` and `pipeline_jobs.cache_key` grow with their input;
//! - `review_comments.path` is a path inside the repository.
//!
//! A few bodies already were `TEXT`, whose 65 535 bytes are fewer than the
//! 65 536-character ceiling the API now enforces (`rg_core::text_limits`):
//! releases, PR timeline events, review suggestions, wiki revisions and the
//! full-text mirrors the triggers copy issue and wiki bodies into. They go to
//! `MEDIUMTEXT` with the rest, or a body the source row accepted would fail in
//! its mirror.
//!
//! Each column keeps its nullability and default; only the type widens, in
//! place, so live rows survive. PostgreSQL and SQLite are already unbounded.

use sea_orm::{ConnectionTrait, DatabaseBackend};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// One `ALTER TABLE` per table: `(table, [(column, definition)])`.
const WIDENED: &[(&str, &[(&str, &str)])] = &[
    ("issues", &[("body", "MEDIUMTEXT NULL")]),
    ("issues_fts", &[("body", "MEDIUMTEXT NULL")]),
    ("issue_comments", &[("body", "MEDIUMTEXT NOT NULL")]),
    ("pull_requests", &[("body", "MEDIUMTEXT NULL")]),
    ("pr_reviews", &[("body", "MEDIUMTEXT NULL")]),
    ("pr_events", &[("body", "MEDIUMTEXT NULL")]),
    (
        "review_comments",
        &[
            ("body", "MEDIUMTEXT NOT NULL"),
            ("suggestion", "MEDIUMTEXT NULL"),
            ("path", "TEXT NOT NULL"),
        ],
    ),
    (
        "notifications",
        &[("title", "TEXT NOT NULL"), ("body", "MEDIUMTEXT NULL")],
    ),
    (
        "wiki_pages",
        &[
            ("content", "MEDIUMTEXT NOT NULL"),
            ("message", "MEDIUMTEXT NULL"),
        ],
    ),
    ("wiki_pages_fts", &[("content", "MEDIUMTEXT NULL")]),
    (
        "wiki_revisions",
        &[
            ("content", "MEDIUMTEXT NOT NULL"),
            ("message", "MEDIUMTEXT NULL"),
        ],
    ),
    ("releases", &[("body", "MEDIUMTEXT NULL")]),
    ("milestones", &[("description", "MEDIUMTEXT NULL")]),
    (
        "packages",
        &[
            ("description", "TEXT NULL"),
            ("homepage", "TEXT NULL"),
            ("repository_url", "TEXT NULL"),
        ],
    ),
    (
        "pipeline_jobs",
        &[
            ("script", "MEDIUMTEXT NOT NULL"),
            ("log", "MEDIUMTEXT NULL"),
            ("tags", "TEXT NULL"),
            ("cache_key", "TEXT NULL"),
        ],
    ),
    (
        "protected_branches",
        &[
            ("allowed_push_user_ids", "TEXT NULL"),
            ("required_status_checks", "TEXT NULL"),
        ],
    ),
    // An expression default: MySQL 8.0.13+ allows one on `TEXT`.
    ("runners", &[("labels", "TEXT NOT NULL DEFAULT ('[]')")]),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::MySql {
            return Ok(());
        }
        for (table, columns) in WIDENED {
            let modifications = columns
                .iter()
                .map(|(column, definition)| format!("MODIFY COLUMN `{column}` {definition}"))
                .collect::<Vec<_>>()
                .join(", ");
            manager
                .get_connection()
                .execute_unprepared(&format!("ALTER TABLE `{table}` {modifications}"))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Narrowing after accepting longer values would truncate live rows.
        Ok(())
    }
}
