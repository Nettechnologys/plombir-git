//! Account-owned grants and inbox rows must leave with their account.
//!
//! The live schema used to carry no `users` foreign key at all on
//! `repo_collaborators.user_id`, `organization_members.user_id`,
//! `team_members.user_id`, or `notifications.user_id`. Deleting an account
//! therefore left an access grant, two memberships, and an inbox addressed to
//! an id that no longer resolved. `repo_collaborators.repo_id` had the same
//! omission on the other axis, so deleting a repository left its grants behind
//! as well (card_dd3f86fde48e).
//!
//! These are ownership references, not author references: none of the rows has
//! a meaning once its account or repository is gone. The five missing links
//! therefore become `ON DELETE CASCADE` constraints. Databases that already
//! contain orphan rows are migrated rather than bricked: the orphan rows are
//! removed while the new shape is installed, because there is no live subject
//! to preserve or ghost.
//!
//! SQLite cannot add a foreign key in place, so the tables go through the same
//! pinned-connection, pragma-proven, transactional rebuild used by the
//! ghost-author migrations. That machinery also preserves named indexes and
//! the `AUTOINCREMENT` high-water mark. PostgreSQL and MySQL clean historical
//! orphans before adding named constraints in place.

use sea_orm_migration::prelude::*;

use super::ghost_author::{RequiredReference, RequiredReferenceRebuild, SqliteTable};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        MIGRATION
    }
}

const MIGRATION: &str = "m20260805_000005_account_owned_rows_follow_their_parent";

pub(super) const REBUILD: RequiredReferenceRebuild = RequiredReferenceRebuild {
    migration: MIGRATION,
    staging_suffix: "_pre_required_reference",
    references: &[
        RequiredReference {
            table: "repo_collaborators",
            column: "repo_id",
            parent_table: "repositories",
            parent_column: "id",
            constraint: "fk_repo_collaborators_repo_id_repositories",
        },
        RequiredReference {
            table: "repo_collaborators",
            column: "user_id",
            parent_table: "users",
            parent_column: "id",
            constraint: "fk_repo_collaborators_user_id_users",
        },
        RequiredReference {
            table: "organization_members",
            column: "user_id",
            parent_table: "users",
            parent_column: "id",
            constraint: "fk_organization_members_user_id_users",
        },
        RequiredReference {
            table: "team_members",
            column: "user_id",
            parent_table: "users",
            parent_column: "id",
            constraint: "fk_team_members_user_id_users",
        },
        RequiredReference {
            table: "notifications",
            column: "user_id",
            parent_table: "users",
            parent_column: "id",
            constraint: "fk_notifications_user_id_users",
        },
    ],
    sqlite_tables: SQLITE_TABLES,
    cascade_warning: "Running the rebuild without the proven pragma combination could drop \
                      membership, collaborator, or notification rows through an existing \
                      parent constraint.",
};

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        REBUILD.apply(manager, true).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        REBUILD.apply(manager, false).await
    }
}

const SQLITE_TABLES: &[SqliteTable] = &[
    SqliteTable {
        name: "repo_collaborators",
        columns: &["id", "repo_id", "user_id", "permission", "created_at"],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "repo_id" bigint NOT NULL,
            "user_id" bigint NOT NULL,
            "permission" varchar NOT NULL DEFAULT 'read',
            "created_at" timestamp_with_timezone_text NOT NULL"#,
        foreign_keys: "",
        indexes: &[(
            "idx_repo_collaborators_repo_user",
            r#"CREATE UNIQUE INDEX "idx_repo_collaborators_repo_user"
               ON "repo_collaborators" ("repo_id", "user_id");"#,
        )],
        sequence_row: r#"
            INSERT INTO repo_collaborators (id, repo_id, user_id, permission, created_at)
            SELECT {seq}, 0, 0, 'read', CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM repo_collaborators), 0);
            DELETE FROM repo_collaborators
            WHERE id = {seq} AND repo_id = 0 AND user_id = 0;"#,
    },
    SqliteTable {
        name: "organization_members",
        columns: &["id", "org_id", "user_id", "role", "created_at"],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "org_id" bigint NOT NULL,
            "user_id" bigint NOT NULL,
            "role" varchar NOT NULL DEFAULT 'member',
            "created_at" timestamp_with_timezone_text NOT NULL,
            UNIQUE ("org_id", "user_id")"#,
        foreign_keys: r#"
            FOREIGN KEY ("org_id") REFERENCES "organizations" ("id") ON DELETE CASCADE"#,
        indexes: &[],
        sequence_row: r#"
            INSERT INTO organization_members (id, org_id, user_id, role, created_at)
            SELECT {seq}, 0, 0, 'member', CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM organization_members), 0);
            DELETE FROM organization_members
            WHERE id = {seq} AND org_id = 0 AND user_id = 0;"#,
    },
    SqliteTable {
        name: "team_members",
        columns: &["id", "team_id", "user_id", "role", "created_at"],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "team_id" bigint NOT NULL,
            "user_id" bigint NOT NULL,
            "role" varchar NOT NULL DEFAULT 'member',
            "created_at" timestamp_with_timezone_text NOT NULL,
            UNIQUE ("team_id", "user_id")"#,
        foreign_keys: r#"
            FOREIGN KEY ("team_id") REFERENCES "teams" ("id") ON DELETE CASCADE"#,
        indexes: &[],
        sequence_row: r#"
            INSERT INTO team_members (id, team_id, user_id, role, created_at)
            SELECT {seq}, 0, 0, 'member', CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM team_members), 0);
            DELETE FROM team_members
            WHERE id = {seq} AND team_id = 0 AND user_id = 0;"#,
    },
    SqliteTable {
        name: "notifications",
        columns: &[
            "id",
            "user_id",
            "event_type",
            "title",
            "body",
            "repo_id",
            "is_read",
            "created_at",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "user_id" bigint NOT NULL,
            "event_type" varchar NOT NULL,
            "title" varchar NOT NULL,
            "body" varchar NULL,
            "repo_id" bigint NULL,
            "is_read" boolean NOT NULL DEFAULT FALSE,
            "created_at" timestamp_with_timezone_text NOT NULL"#,
        foreign_keys: "",
        indexes: &[
            (
                "idx_notifications_user_id_is_read",
                r#"CREATE INDEX "idx_notifications_user_id_is_read"
                   ON "notifications" ("user_id", "is_read");"#,
            ),
            (
                "idx_notifications_repo_id",
                r#"CREATE INDEX "idx_notifications_repo_id"
                   ON "notifications" ("repo_id");"#,
            ),
        ],
        sequence_row: r#"
            INSERT INTO notifications
                (id, user_id, event_type, title, body, repo_id, is_read, created_at)
            SELECT {seq}, 0, '_forgekeep_sequence_high_water', '', NULL, NULL, FALSE,
                   CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM notifications), 0);
            DELETE FROM notifications
            WHERE id = {seq} AND user_id = 0
              AND event_type = '_forgekeep_sequence_high_water';"#,
    },
];

#[cfg(test)]
#[path = "m20260805_000005_account_owned_rows_follow_their_parent_tests.rs"]
mod tests;
