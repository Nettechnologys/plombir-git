//! A comment on an issue must belong to an issue the database knows.
//!
//! `issue_comments.issue_id` was created without a foreign key
//! (`m20260424_000004_create_issues`), with only an index beside it. Nothing
//! stopped a writer from putting another table's id there, and one did for
//! years: importing a GitHub pull request or a GitLab merge request stored its
//! conversation under `issue_id = <pull_requests.id>`. Ids of the two tables
//! come from separate sequences, so those comments showed up under whichever
//! issue — in any repository of the instance, public ones included — happened
//! to have that primary key, and the pull request itself showed none
//! (card_19723ffddde8). The import is fixed; this migration makes the database
//! refuse the next such writer itself (card_8357c4e5db99), and makes deleting an
//! issue take its comments with it rather than leave them pointing at nothing.
//!
//! ## What happens to rows that already point at nothing
//!
//! A comment whose `issue_id` names no issue cannot be shown anywhere and has
//! no subject to move with, so it is removed while the constraint is
//! installed, as `m20260805_000005_account_owned_rows_follow_their_parent`
//! does for its orphans. Two things are done first and said out loud:
//!
//! * the count goes to the log, together with how many of the removed rows
//!   carry the id of an existing pull request — almost certainly a pull
//!   request conversation an import put here, which re-importing the
//!   repository now stores where it belongs;
//! * an attachment uploaded into such a comment is detached from it
//!   (`issue_comment_id = NULL`) rather than cascaded away: the attachment row
//!   is what names its stored file, and deleting the row here would leave the
//!   file in storage with nothing that can find it again.
//!
//! What this cannot repair is a misfiled comment whose `issue_id` happens to
//! name an existing issue — it satisfies the constraint, and nothing in the row
//! says which repository it came from.
//!
//! The SQLite half goes through the guarded table rebuild in
//! [`super::ghost_author`]; PostgreSQL and MySQL add the constraint in place.

use sea_orm::{ConnectionTrait, Statement, TryGetable};
use sea_orm_migration::prelude::*;

use super::ghost_author::{RequiredReference, RequiredReferenceRebuild, SqliteTable};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        MIGRATION
    }
}

const MIGRATION: &str = "m20261007_000003_issue_comments_reference_their_issue";

pub(super) const REBUILD: RequiredReferenceRebuild = RequiredReferenceRebuild {
    migration: MIGRATION,
    staging_suffix: "_pre_issue_reference",
    references: &[RequiredReference {
        table: "issue_comments",
        column: "issue_id",
        parent_table: "issues",
        parent_column: "id",
        constraint: "fk_issue_comments_issue_id_issues",
    }],
    sqlite_tables: SQLITE_TABLES,
    cascade_warning: "Running the rebuild without the proven pragma combination would drop \
                      `issue_comments` while `attachments` still cascades from it, and every \
                      attachment uploaded into a comment would go with it.",
};

/// The comments whose `issue_id` names no issue.
const ORPHANS: &str =
    "SELECT c.id FROM issue_comments c WHERE NOT EXISTS (SELECT 1 FROM issues i WHERE i.id = c.issue_id)";

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
    let orphans = count(db, &format!("SELECT COUNT(*) FROM ({ORPHANS}) orphans")).await?;
    if orphans == 0 {
        return Ok(());
    }
    let pull_request_shaped = count(
        db,
        "SELECT COUNT(*) FROM issue_comments c \
         WHERE NOT EXISTS (SELECT 1 FROM issues i WHERE i.id = c.issue_id) \
           AND EXISTS (SELECT 1 FROM pull_requests p WHERE p.id = c.issue_id)",
    )
    .await?;
    let detached = db
        .execute_unprepared(&format!(
            "UPDATE attachments SET issue_comment_id = NULL WHERE issue_comment_id IN \
             (SELECT id FROM ({ORPHANS}) orphans)"
        ))
        .await?
        .rows_affected();
    tracing::warn!(
        migration = MIGRATION,
        removed_comments = orphans,
        pull_request_shaped,
        detached_attachments = detached,
        "removing issue comments whose issue does not exist before installing the \
         issue_comments.issue_id foreign key; the ones carrying a pull request's id are almost \
         certainly a pull request conversation an earlier import misfiled — re-import the \
         repository to store it on the pull request"
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

const SQLITE_TABLES: &[SqliteTable] = &[SqliteTable {
    name: "issue_comments",
    columns: &[
        "id",
        "issue_id",
        "author_id",
        "body",
        "created_at",
        "updated_at",
    ],
    body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "issue_id" bigint NOT NULL,
            "author_id" bigint NOT NULL,
            "body" varchar NOT NULL,
            "created_at" timestamp_with_timezone_text NOT NULL DEFAULT CURRENT_TIMESTAMP,
            "updated_at" timestamp_with_timezone_text NOT NULL DEFAULT CURRENT_TIMESTAMP"#,
    foreign_keys: "",
    indexes: &[(
        "idx_issue_comments_issue_id",
        r#"CREATE INDEX "idx_issue_comments_issue_id" ON "issue_comments" ("issue_id");"#,
    )],
    sequence_row: r#"
            INSERT INTO issue_comments (id, issue_id, author_id, body, created_at, updated_at)
            SELECT {seq}, 0, 0, '_forgekeep_sequence_high_water', CURRENT_TIMESTAMP,
                   CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM issue_comments), 0);
            DELETE FROM issue_comments
            WHERE id = {seq} AND issue_id = 0 AND body = '_forgekeep_sequence_high_water';"#,
}];

#[cfg(test)]
#[path = "m20261007_000003_issue_comments_reference_their_issue_tests.rs"]
mod tests;
