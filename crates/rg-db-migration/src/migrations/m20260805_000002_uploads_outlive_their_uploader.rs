//! An account's departure must not delete other people's files.
//!
//! Three columns named the account that *produced* a piece of content, and all
//! three were declared `REFERENCES users(id) ON DELETE CASCADE`:
//!
//! * `attachments.uploader_id` (`m20260714_000001_create_attachments`)
//! * `release_assets.uploader_id` (`m20260508_000002_create_releases`)
//! * `releases.author_id` (`m20260508_000002_create_releases`)
//!
//! None of those rows belong to the uploader — they live in whichever
//! repository the file was uploaded *into*, which is routinely somebody else's.
//! So `DELETE FROM users` destroyed attachments inside other people's issues,
//! assets inside other people's releases and, through
//! `release_assets.release_id ON DELETE CASCADE`, entire releases of other
//! people's repositories together with every asset any account had ever
//! uploaded to them.
//!
//! The bytes did not go with them. `attachments/<repo_id>/<uuid>/<file>` and
//! `releases/<owner>/<repo>/<release_id>/<asset_id>/<file>` stay live: the
//! repository is not being deleted, so no retirement path walks its prefixes,
//! and the row that named the object is gone, so nothing can ever find it
//! again. In the UI the release simply loses its files (card_1cfc81035e92).
//!
//! The replacement is the answer every forge converged on: the *content* stays
//! and its author becomes a ghost. Each column becomes nullable with
//! `ON DELETE SET NULL`, so the database itself — not one careful call site —
//! guarantees that removing an account cannot remove a file from a repository
//! that is still alive. `pr_events.actor_id` already carried exactly this rule
//! (`m20260712_000001_create_pr_events`); this migration extends it to the three
//! columns that own bytes, and
//! `m20260805_000004_repo_config_outlives_its_author` to the ones that own a
//! repository's configuration and history.
//!
//! What still cascades is unchanged and correct: the *repository* the content
//! lives in (`repo_id` / `release_id`), whose own deletion retires the storage
//! first through the staged, compensated path.
//!
//! The rebuild machinery, its per-backend notes and the reasoning behind the
//! SQLite pragma pair live in [`super::ghost_author`]; this file is the recipe
//! it is handed.

use sea_orm_migration::prelude::*;

use super::ghost_author::{GhostAuthorRebuild, Shape, SqliteTable};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        MIGRATION
    }
}

const MIGRATION: &str = "m20260805_000002_uploads_outlive_their_uploader";

/// The three (table, column) pairs whose `ON DELETE` action this migration
/// turns from `CASCADE` into `SET NULL`, and how to rebuild them on SQLite.
pub(super) const REBUILD: GhostAuthorRebuild = GhostAuthorRebuild {
    migration: MIGRATION,
    staging_suffix: "_pre_ghost_uploader",
    columns: &[
        ("releases", "author_id"),
        ("release_assets", "uploader_id"),
        ("attachments", "uploader_id"),
    ],
    sqlite_tables: SQLITE_TABLES,
    cascade_warning: "Running the rebuild anyway would drop `releases` while foreign keys are \
                      enforced, and every asset of every release would be cascaded away.",
};

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        REBUILD.apply(manager, Shape::Ghost).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        REBUILD.apply(manager, Shape::Owned).await
    }
}

const SQLITE_TABLES: &[SqliteTable] = &[
    SqliteTable {
        name: "releases",
        columns: &[
            "id",
            "repo_id",
            "tag_name",
            "target_commitish",
            "title",
            "body",
            "is_draft",
            "is_prerelease",
            "author_id",
            "created_at",
            "updated_at",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "repo_id" bigint NOT NULL,
            "tag_name" varchar NOT NULL,
            "target_commitish" varchar NOT NULL DEFAULT 'main',
            "title" varchar NOT NULL,
            "body" text NULL,
            "is_draft" boolean NOT NULL DEFAULT FALSE,
            "is_prerelease" boolean NOT NULL DEFAULT FALSE,
            "author_id" bigint {rule},
            "created_at" timestamp_with_timezone_text NOT NULL,
            "updated_at" timestamp_with_timezone_text NOT NULL,
            CONSTRAINT "idx_releases_repo_tag_unique" UNIQUE ("repo_id", "tag_name")"#,
        foreign_keys: r#"
            FOREIGN KEY ("repo_id") REFERENCES "repositories" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("author_id") REFERENCES "users" ("id") ON DELETE {on_delete}"#,
        indexes: &[(
            "idx_releases_repo_id",
            r#"CREATE INDEX "idx_releases_repo_id" ON "releases" ("repo_id");"#,
        )],
        sequence_row: r#"
            INSERT INTO releases
                (id, repo_id, tag_name, target_commitish, title, created_at, updated_at)
            SELECT {seq}, 0, '_forgekeep_sequence_high_water', 'main', 'x',
                   CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM releases), 0);
            DELETE FROM releases
            WHERE id = {seq} AND repo_id = 0 AND tag_name = '_forgekeep_sequence_high_water';"#,
    },
    SqliteTable {
        name: "release_assets",
        columns: &[
            "id",
            "release_id",
            "filename",
            "size",
            "content_type",
            "download_count",
            "uploader_id",
            "created_at",
            "sha256",
            "attestation",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "release_id" bigint NOT NULL,
            "filename" varchar NOT NULL,
            "size" bigint NOT NULL DEFAULT 0,
            "content_type" varchar NOT NULL DEFAULT 'application/octet-stream',
            "download_count" bigint NOT NULL DEFAULT 0,
            "uploader_id" bigint {rule},
            "created_at" timestamp_with_timezone_text NOT NULL,
            "sha256" varchar NULL,
            "attestation" text NULL"#,
        foreign_keys: r#"
            FOREIGN KEY ("release_id") REFERENCES "releases" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("uploader_id") REFERENCES "users" ("id") ON DELETE {on_delete}"#,
        indexes: &[(
            "idx_release_assets_release_id",
            r#"CREATE INDEX "idx_release_assets_release_id" ON "release_assets" ("release_id");"#,
        )],
        // `uploader_id` is NOT NULL in the `Owned` shape, so the probe row has
        // to carry one; user 0 does not exist, which is why the rebuild's
        // foreign keys are off while it lands and gone again before they are
        // back on.
        sequence_row: r#"
            INSERT INTO release_assets
                (id, release_id, filename, size, content_type, download_count, uploader_id,
                 created_at)
            SELECT {seq}, 0, '_forgekeep_sequence_high_water', 0, 'application/octet-stream', 0, 0,
                   CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM release_assets), 0);
            DELETE FROM release_assets
            WHERE id = {seq} AND release_id = 0
              AND filename = '_forgekeep_sequence_high_water';"#,
    },
    SqliteTable {
        name: "attachments",
        columns: &[
            "id",
            "uuid",
            "repo_id",
            "uploader_id",
            "issue_id",
            "pull_request_id",
            "issue_comment_id",
            "review_comment_id",
            "filename",
            "blob_key",
            "content_type",
            "size",
            "download_count",
            "created_at",
            "sha256",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "uuid" varchar(36) NOT NULL UNIQUE,
            "repo_id" bigint NOT NULL,
            "uploader_id" bigint {rule},
            "issue_id" bigint NULL,
            "pull_request_id" bigint NULL,
            "issue_comment_id" bigint NULL,
            "review_comment_id" bigint NULL,
            "filename" varchar(255) NOT NULL,
            "blob_key" varchar(1024) NOT NULL,
            "content_type" varchar(255) NOT NULL,
            "size" bigint NOT NULL,
            "download_count" bigint NOT NULL DEFAULT 0,
            "created_at" timestamp_with_timezone_text NOT NULL,
            "sha256" varchar NULL"#,
        foreign_keys: r#"
            FOREIGN KEY ("repo_id") REFERENCES "repositories" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("uploader_id") REFERENCES "users" ("id") ON DELETE {on_delete},
            FOREIGN KEY ("issue_id") REFERENCES "issues" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("pull_request_id") REFERENCES "pull_requests" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("issue_comment_id") REFERENCES "issue_comments" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("review_comment_id") REFERENCES "review_comments" ("id") ON DELETE CASCADE"#,
        indexes: &[
            (
                "idx_attachments_blob_key",
                r#"CREATE UNIQUE INDEX "idx_attachments_blob_key" ON "attachments" ("blob_key");"#,
            ),
            (
                "idx_attachments_repo",
                r#"CREATE INDEX "idx_attachments_repo" ON "attachments" ("repo_id");"#,
            ),
            (
                "idx_attachments_issue",
                r#"CREATE INDEX "idx_attachments_issue" ON "attachments" ("issue_id");"#,
            ),
            (
                "idx_attachments_pr",
                r#"CREATE INDEX "idx_attachments_pr" ON "attachments" ("pull_request_id");"#,
            ),
            (
                "idx_attachments_issue_comment",
                r#"CREATE INDEX "idx_attachments_issue_comment" ON "attachments" ("issue_comment_id");"#,
            ),
            (
                "idx_attachments_review_comment",
                r#"CREATE INDEX "idx_attachments_review_comment" ON "attachments" ("review_comment_id");"#,
            ),
        ],
        sequence_row: r#"
            INSERT INTO attachments
                (id, uuid, repo_id, uploader_id, filename, blob_key, content_type, size,
                 created_at)
            SELECT {seq}, '_forgekeep_sequence_high_water', 0, 0,
                   '_forgekeep_sequence_high_water', '_forgekeep_sequence_high_water',
                   'application/octet-stream', 0, CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM attachments), 0);
            DELETE FROM attachments
            WHERE id = {seq} AND repo_id = 0 AND uuid = '_forgekeep_sequence_high_water';"#,
    },
];

#[cfg(test)]
#[path = "m20260805_000002_uploads_outlive_their_uploader_tests.rs"]
mod tests;
