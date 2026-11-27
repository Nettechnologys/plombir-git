//! A review, and a review comment, must belong to a pull request the database
//! knows (card_09d54b1cd124).
//!
//! `pr_reviews.pr_id`, `review_comments.pr_id` and `review_comments.review_id`
//! were created without a foreign key on any backend
//! (`m20260424_000008_create_phase6`), and all three are read "by parent":
//! a pull request's reviews, its review comments, a review's comments. Deleting
//! a pull request took them with it only because
//! `pull_request_ops::delete_with_dependents` deletes them by hand in the same
//! transaction — a convention, not something the database holds. The next
//! deleter that forgets, or a writer that stores another table's id, leaves
//! rows that point at nothing, or at a pull request in another repository that
//! happens to carry the id. `ON DELETE CASCADE` makes the database do it.
//!
//! ## What happens to rows that already point at nothing
//!
//! A review or a comment with no pull request cannot be shown anywhere, and a
//! comment whose review is gone has no thread to sit in, so they are removed
//! while the constraints are installed — as
//! `m20261007_000003_issue_comments_reference_their_issue` does for issue
//! comments. Before that, and said out loud in the log:
//!
//! * the counts;
//! * an attachment uploaded into such a comment is detached from it
//!   (`review_comment_id = NULL`) rather than cascaded away: the attachment row
//!   is what names its stored file, and deleting it here would leave the file in
//!   storage with nothing that can find it again.
//!
//! The SQLite half goes through the guarded table rebuild in
//! [`super::ghost_author`]; PostgreSQL and MySQL add the constraints in place.

use sea_orm::{ConnectionTrait, Statement, TryGetable};
use sea_orm_migration::prelude::*;

use super::ghost_author::{RequiredReference, RequiredReferenceRebuild, SqliteTable};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        MIGRATION
    }
}

pub(super) const MIGRATION: &str = "m20261127_000003_reviews_reference_their_pull_request";

pub(super) const REBUILD: RequiredReferenceRebuild = RequiredReferenceRebuild {
    migration: MIGRATION,
    staging_suffix: "_pre_pull_request_reference",
    // Reviews before comments: a comment whose review goes with an orphaned
    // pull request goes too.
    references: &[
        RequiredReference {
            table: "pr_reviews",
            column: "pr_id",
            parent_table: "pull_requests",
            parent_column: "id",
            constraint: "fk_pr_reviews_pr_id_pull_requests",
        },
        RequiredReference {
            table: "review_comments",
            column: "pr_id",
            parent_table: "pull_requests",
            parent_column: "id",
            constraint: "fk_review_comments_pr_id_pull_requests",
        },
        RequiredReference {
            table: "review_comments",
            column: "review_id",
            parent_table: "pr_reviews",
            parent_column: "id",
            constraint: "fk_review_comments_review_id_pr_reviews",
        },
    ],
    sqlite_tables: SQLITE_TABLES,
    cascade_warning: "Running the rebuild without the proven pragma combination would drop \
                      `review_comments` while `attachments` still cascades from it, and every \
                      attachment uploaded into a review comment would go with it.",
};

/// The reviews whose `pr_id` names no pull request.
const ORPHAN_REVIEWS: &str = "SELECT r.id FROM pr_reviews r \
     WHERE NOT EXISTS (SELECT 1 FROM pull_requests p WHERE p.id = r.pr_id)";

/// The comments that go: no pull request, no review, or a review that goes.
const ORPHAN_COMMENTS: &str = "SELECT c.id FROM review_comments c \
     WHERE NOT EXISTS (SELECT 1 FROM pull_requests p WHERE p.id = c.pr_id) \
        OR NOT EXISTS (SELECT 1 FROM pr_reviews r WHERE r.id = c.review_id) \
        OR EXISTS (SELECT 1 FROM pr_reviews r WHERE r.id = c.review_id \
                   AND NOT EXISTS (SELECT 1 FROM pull_requests p WHERE p.id = r.pr_id))";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        settle_orphans(manager.get_connection()).await?;
        REBUILD.apply(manager, true).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        REBUILD.apply(manager, false).await
    }
}

/// Report the orphans the rebuild is about to remove, and detach the
/// attachments that would otherwise cascade away with them.
pub(super) async fn settle_orphans<C: ConnectionTrait>(db: &C) -> Result<(), DbErr> {
    let reviews = count(
        db,
        &format!("SELECT COUNT(*) FROM ({ORPHAN_REVIEWS}) orphans"),
    )
    .await?;
    let comments = count(
        db,
        &format!("SELECT COUNT(*) FROM ({ORPHAN_COMMENTS}) orphans"),
    )
    .await?;
    if reviews == 0 && comments == 0 {
        return Ok(());
    }
    // MySQL refuses a subquery that reads the table being updated; this one
    // reads `review_comments` only.
    let detached = db
        .execute_unprepared(&format!(
            "UPDATE attachments SET review_comment_id = NULL WHERE review_comment_id IN \
             (SELECT id FROM ({ORPHAN_COMMENTS}) orphans)"
        ))
        .await?
        .rows_affected();
    tracing::warn!(
        migration = MIGRATION,
        removed_reviews = reviews,
        removed_review_comments = comments,
        detached_attachments = detached,
        "removing reviews and review comments whose pull request (or review) does not exist \
         before installing their foreign keys"
    );
    Ok(())
}

async fn count<C: ConnectionTrait>(db: &C, sql: &str) -> Result<i64, DbErr> {
    let row = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            sql.to_string(),
        ))
        .await?
        .ok_or_else(|| DbErr::Migration(format!("{MIGRATION}: `{sql}` returned no row")))?;
    i64::try_get_by_index(&row, 0)
        .map_err(|error| DbErr::Migration(format!("{MIGRATION}: decode `{sql}`: {error:?}")))
}

const SQLITE_TABLES: &[SqliteTable] = &[
    SqliteTable {
        name: "pr_reviews",
        columns: &[
            "id",
            "pr_id",
            "repo_id",
            "reviewer_id",
            "action",
            "body",
            "commit_id",
            "created_at",
            "dismissed_at",
            "dismissed_by",
            "original_author",
            "original_author_id",
            "original_service",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "pr_id" bigint NOT NULL,
            "repo_id" bigint NOT NULL,
            "reviewer_id" bigint NOT NULL,
            "action" varchar NOT NULL,
            "body" varchar NULL,
            "commit_id" varchar NULL,
            "created_at" timestamp_with_timezone_text NOT NULL DEFAULT CURRENT_TIMESTAMP,
            "dismissed_at" timestamp_with_timezone_text NULL,
            "dismissed_by" bigint NULL,
            "original_author" varchar NULL,
            "original_author_id" bigint NULL,
            "original_service" varchar NULL"#,
        foreign_keys: "",
        indexes: &[(
            "idx_pr_reviews_pr_id",
            r#"CREATE INDEX "idx_pr_reviews_pr_id" ON "pr_reviews" ("pr_id");"#,
        )],
        sequence_row: r#"
            INSERT INTO pr_reviews (id, pr_id, repo_id, reviewer_id, action, created_at)
            SELECT {seq}, 0, 0, 0, '_forgekeep_sequence_high_water', CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM pr_reviews), 0);
            DELETE FROM pr_reviews
            WHERE id = {seq} AND pr_id = 0 AND action = '_forgekeep_sequence_high_water';"#,
    },
    SqliteTable {
        name: "review_comments",
        columns: &[
            "id",
            "review_id",
            "pr_id",
            "author_id",
            "path",
            "position",
            "line",
            "side",
            "body",
            "commit_id",
            "reply_to_id",
            "created_at",
            "updated_at",
            "resolved_at",
            "resolved_by_id",
            "suggestion",
            "suggestion_applied_at",
            "suggestion_applied_by_id",
            "suggestion_commit_sha",
            "start_line",
            "start_side",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "review_id" bigint NOT NULL,
            "pr_id" bigint NOT NULL,
            "author_id" bigint NOT NULL,
            "path" varchar NOT NULL,
            "position" bigint NULL,
            "line" bigint NULL,
            "side" varchar NULL,
            "body" varchar NOT NULL,
            "commit_id" varchar NULL,
            "reply_to_id" bigint NULL,
            "created_at" timestamp_with_timezone_text NOT NULL DEFAULT CURRENT_TIMESTAMP,
            "updated_at" timestamp_with_timezone_text NOT NULL DEFAULT CURRENT_TIMESTAMP,
            "resolved_at" timestamp_with_timezone_text NULL,
            "resolved_by_id" bigint NULL,
            "suggestion" text NULL,
            "suggestion_applied_at" timestamp_with_timezone_text NULL,
            "suggestion_applied_by_id" bigint NULL,
            "suggestion_commit_sha" varchar NULL,
            "start_line" bigint NULL,
            "start_side" varchar NULL"#,
        foreign_keys: "",
        indexes: &[
            (
                "idx_review_comments_pr_id",
                r#"CREATE INDEX "idx_review_comments_pr_id" ON "review_comments" ("pr_id");"#,
            ),
            (
                "idx_review_comments_review_id",
                r#"CREATE INDEX "idx_review_comments_review_id" ON "review_comments" ("review_id");"#,
            ),
        ],
        sequence_row: r#"
            INSERT INTO review_comments (id, review_id, pr_id, author_id, path, body, created_at,
                                         updated_at)
            SELECT {seq}, 0, 0, 0, '', '_forgekeep_sequence_high_water', CURRENT_TIMESTAMP,
                   CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM review_comments), 0);
            DELETE FROM review_comments
            WHERE id = {seq} AND pr_id = 0 AND body = '_forgekeep_sequence_high_water';"#,
    },
];

#[cfg(test)]
#[path = "m20261127_000003_reviews_reference_their_pull_request_tests.rs"]
mod tests;
