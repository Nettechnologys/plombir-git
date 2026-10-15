//! An account's departure must not reconfigure other people's repositories.
//!
//! `m20260805_000002_uploads_outlive_their_uploader` fixed this class for the
//! three columns that own *bytes*. A sweep of every remaining
//! `REFERENCES users(id)` in the schema found six more carrying
//! `ON DELETE CASCADE`, where what disappears is not a file but a live
//! repository's configuration and history (card_a2123e31ee6e):
//!
//! | Column | What `DELETE FROM users` took |
//! |---|---|
//! | `ci_secrets.created_by_id` | the repository's CI secrets — pipelines then fail on an empty variable |
//! | `deploy_keys.created_by_id` | a repository's deploy key — CI and deployments silently lose access |
//! | `commit_statuses.creator_id` | the check results on commits, including those of merged pull requests |
//! | `boards.created_by` | a repository's or organization's whole board, and through `board_columns` / `board_cards`, every column and card on it |
//! | `ci_environment_approvals.approved_by` | the record of who approved a deployment into a protected environment |
//! | `time_entries.user_id` | the hours logged against an issue, and with them the issue's total |
//!
//! None of those rows belong to the account named in them. They live in the
//! namespace of whichever repository or organization they configure — routinely
//! somebody else's, who never learns that a collaborator's account was removed
//! and finds out when a pipeline one day stops building.
//!
//! So all six become nullable with `ON DELETE SET NULL`: the configuration
//! stays, its author becomes a ghost, and the guarantee is the database's rather
//! than one careful call site's.
//!
//! ## Why ghosting, per column
//!
//! * **`ci_secrets`** — the secret belongs to the repository; the column only
//!   records who introduced it. Losing the row does not protect anything (the
//!   ciphertext is the repository's, not the departed account's) and breaks
//!   every pipeline that reads the variable.
//! * **`deploy_keys`** — the one case where the cascade fails *closed*, which is
//!   why it is called out rather than swept in: the key vanishing means access
//!   is lost, not gained. It is still wrong. A deploy key is repository access
//!   the owner configured and can see and revoke in the repository's settings;
//!   it is not the departing person's credential (theirs are `ssh_keys` and
//!   `access_tokens`, which still cascade, correctly). Silently disabling
//!   somebody else's deployments — with no notice, no audit line, and no way to
//!   tell afterwards what was removed — is the worse failure.
//! * **`commit_statuses`** — a check result is a fact about a commit. Deleting
//!   it rewrites the history of merged pull requests and can flip a branch
//!   protection verdict from met to unmet.
//! * **`boards`** — a board belongs to its repository or organization. The
//!   cascade reached furthest here: one departing collaborator took an entire
//!   organization's board, its columns and every card on it.
//! * **`ci_environment_approvals`** — the row *is* the audit record of an
//!   approval. Cascading it says nobody approved; nulling it says somebody did
//!   and their account is gone, which is the truth. `(job_id, approved_by)`
//!   stays UNIQUE and keeps working: SQL treats NULLs as distinct, so several
//!   ghosted approvals of one job coexist and a live approver is still limited
//!   to one.
//! * **`time_entries`** — the one card_a2123e31ee6e parked for a decision of its
//!   own, because the row does describe the departing person. It is here because
//!   the row is also read as a property of the *issue*:
//!   `time_entry_ops::total_minutes_by_issue` sums it, so cascading silently
//!   lowers the hours recorded against an issue in somebody else's repository.
//!   "Three hours were spent on this issue" stays true after its author leaves.
//!
//! `pr_reviewer_requests.reviewer_id` was weighed with them and deliberately
//! left cascading: the row asks a specific person for a review, and nothing can
//! be asked of a ghost. `crates/rg-db/tests/user_delete_decisions.rs` records
//! that verdict, and every other one, against the live schema.
//!
//! What still cascades from these tables is unchanged and correct: the
//! repository, organization, environment or job the row configures.
//!
//! The rebuild machinery and its per-backend notes live in
//! [`super::ghost_author`]; this file is the recipe it is handed.
//! `crates/rg-db/tests/user_delete_decisions.rs` holds the decision taken on
//! *every* `users` foreign key in the schema, and fails when a new one appears
//! without one.

use sea_orm_migration::prelude::*;

use super::ghost_author::{GhostAuthorRebuild, Shape, SqliteTable};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        MIGRATION
    }
}

const MIGRATION: &str = "m20260805_000004_repo_config_outlives_its_author";

/// The six (table, column) pairs whose `ON DELETE` action this migration turns
/// from `CASCADE` into `SET NULL`, and how to rebuild them on SQLite.
pub(super) const REBUILD: GhostAuthorRebuild = GhostAuthorRebuild {
    migration: MIGRATION,
    staging_suffix: "_pre_ghost_author",
    columns: &[
        ("ci_secrets", "created_by_id"),
        ("deploy_keys", "created_by_id"),
        ("commit_statuses", "creator_id"),
        ("boards", "created_by"),
        ("ci_environment_approvals", "approved_by"),
        ("time_entries", "user_id"),
    ],
    sqlite_tables: SQLITE_TABLES,
    cascade_warning: "Running the rebuild anyway would drop `boards` while foreign keys are \
                      enforced, and every column and card of every board would be cascaded away.",
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
        name: "ci_secrets",
        columns: &[
            "id",
            "repo_id",
            "name",
            "encrypted_value",
            "created_by_id",
            "created_at",
            "updated_at",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "repo_id" bigint NOT NULL,
            "name" varchar NOT NULL,
            "encrypted_value" text NOT NULL,
            "created_by_id" bigint {rule},
            "created_at" timestamp_with_timezone_text NOT NULL,
            "updated_at" timestamp_with_timezone_text NOT NULL,
            CONSTRAINT "uq_ci_secrets_repo_name" UNIQUE ("repo_id", "name")"#,
        foreign_keys: r#"
            FOREIGN KEY ("repo_id") REFERENCES "repositories" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("created_by_id") REFERENCES "users" ("id") ON DELETE {on_delete}"#,
        indexes: &[],
        // `created_by_id` is NOT NULL in the `Owned` shape, so the probe row has
        // to carry one; user 0 does not exist, which is why the rebuild's
        // foreign keys are off while it lands and gone again before they are
        // back on.
        sequence_row: r#"
            INSERT INTO ci_secrets
                (id, repo_id, name, encrypted_value, created_by_id, created_at, updated_at)
            SELECT {seq}, 0, '_forgekeep_sequence_high_water', '', 0,
                   CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM ci_secrets), 0);
            DELETE FROM ci_secrets
            WHERE id = {seq} AND repo_id = 0 AND name = '_forgekeep_sequence_high_water';"#,
    },
    SqliteTable {
        name: "deploy_keys",
        columns: &[
            "id",
            "repo_id",
            "created_by_id",
            "title",
            "public_key",
            "fingerprint",
            "read_only",
            "created_at",
            "last_used_at",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "repo_id" bigint NOT NULL,
            "created_by_id" bigint {rule},
            "title" varchar NOT NULL,
            "public_key" text NOT NULL,
            "fingerprint" varchar NOT NULL UNIQUE,
            "read_only" boolean NOT NULL DEFAULT TRUE,
            "created_at" timestamp_with_timezone_text NOT NULL,
            "last_used_at" timestamp_with_timezone_text NULL"#,
        foreign_keys: r#"
            FOREIGN KEY ("repo_id") REFERENCES "repositories" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("created_by_id") REFERENCES "users" ("id") ON DELETE {on_delete}"#,
        indexes: &[(
            "idx_deploy_keys_repo_created",
            r#"CREATE INDEX "idx_deploy_keys_repo_created" ON "deploy_keys" ("repo_id", "created_at");"#,
        )],
        sequence_row: r#"
            INSERT INTO deploy_keys
                (id, repo_id, created_by_id, title, public_key, fingerprint, read_only, created_at)
            SELECT {seq}, 0, 0, '_forgekeep_sequence_high_water', '',
                   '_forgekeep_sequence_high_water', TRUE, CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM deploy_keys), 0);
            DELETE FROM deploy_keys
            WHERE id = {seq} AND repo_id = 0
              AND fingerprint = '_forgekeep_sequence_high_water';"#,
    },
    SqliteTable {
        name: "commit_statuses",
        columns: &[
            "id",
            "repo_id",
            "sha",
            "state",
            "context",
            "description",
            "target_url",
            "creator_id",
            "created_at",
            "updated_at",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "repo_id" bigint NOT NULL,
            "sha" varchar(40) NOT NULL,
            "state" varchar(20) NOT NULL,
            "context" varchar(255) NOT NULL,
            "description" varchar(500) NULL,
            "target_url" varchar(500) NULL,
            "creator_id" bigint {rule},
            "created_at" timestamp_with_timezone_text NOT NULL,
            "updated_at" timestamp_with_timezone_text NOT NULL,
            CONSTRAINT "idx_commit_statuses_repo_sha_context_unique"
                UNIQUE ("repo_id", "sha", "context")"#,
        foreign_keys: r#"
            FOREIGN KEY ("repo_id") REFERENCES "repositories" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("creator_id") REFERENCES "users" ("id") ON DELETE {on_delete}"#,
        indexes: &[(
            "idx_commit_statuses_repo_sha",
            r#"CREATE INDEX "idx_commit_statuses_repo_sha" ON "commit_statuses" ("repo_id", "sha");"#,
        )],
        sequence_row: r#"
            INSERT INTO commit_statuses
                (id, repo_id, sha, state, context, creator_id, created_at, updated_at)
            SELECT {seq}, 0, '_forgekeep_sequence_high_water', 'pending',
                   '_forgekeep_sequence_high_water', 0, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM commit_statuses), 0);
            DELETE FROM commit_statuses
            WHERE id = {seq} AND repo_id = 0 AND sha = '_forgekeep_sequence_high_water';"#,
    },
    // `board_columns` carries `REFERENCES "boards"`, so this is the rebuild the
    // pragma probe is protecting: if the rename at its head were followed, the
    // closing `DROP TABLE` would take every column and card of every board.
    SqliteTable {
        name: "boards",
        columns: &[
            "id",
            "repo_id",
            "org_id",
            "name",
            "description",
            "created_by",
            "created_at",
            "updated_at",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "repo_id" bigint NULL,
            "org_id" bigint NULL,
            "name" varchar NOT NULL,
            "description" text NULL,
            "created_by" bigint {rule},
            "created_at" timestamp_with_timezone_text NOT NULL,
            "updated_at" timestamp_with_timezone_text NOT NULL"#,
        foreign_keys: r#"
            FOREIGN KEY ("repo_id") REFERENCES "repositories" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("org_id") REFERENCES "organizations" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("created_by") REFERENCES "users" ("id") ON DELETE {on_delete}"#,
        indexes: &[],
        sequence_row: r#"
            INSERT INTO boards (id, name, created_by, created_at, updated_at)
            SELECT {seq}, '_forgekeep_sequence_high_water', 0,
                   CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM boards), 0);
            DELETE FROM boards
            WHERE id = {seq} AND name = '_forgekeep_sequence_high_water'
              AND repo_id IS NULL AND org_id IS NULL;"#,
    },
    SqliteTable {
        name: "ci_environment_approvals",
        columns: &[
            "id",
            "job_id",
            "environment_id",
            "approved_by",
            "created_at",
        ],
        // `job_id` is `integer` rather than `bigint` because `pipeline_jobs.id`
        // is, and MySQL requires exact type parity across a foreign key.
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "job_id" integer NOT NULL,
            "environment_id" bigint NOT NULL,
            "approved_by" bigint {rule},
            "created_at" timestamp_with_timezone_text NOT NULL,
            CONSTRAINT "uq_ci_environment_approval_job_user" UNIQUE ("job_id", "approved_by")"#,
        foreign_keys: r#"
            FOREIGN KEY ("job_id") REFERENCES "pipeline_jobs" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("environment_id") REFERENCES "ci_environments" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("approved_by") REFERENCES "users" ("id") ON DELETE {on_delete}"#,
        indexes: &[],
        sequence_row: r#"
            INSERT INTO ci_environment_approvals
                (id, job_id, environment_id, approved_by, created_at)
            SELECT {seq}, 0, 0, 0, CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM ci_environment_approvals), 0);
            DELETE FROM ci_environment_approvals
            WHERE id = {seq} AND job_id = 0 AND environment_id = 0;"#,
    },
    SqliteTable {
        name: "time_entries",
        columns: &[
            "id",
            "issue_id",
            "user_id",
            "duration_minutes",
            "description",
            "created_at",
        ],
        body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "issue_id" bigint NOT NULL,
            "user_id" bigint {rule},
            "duration_minutes" bigint NOT NULL,
            "description" text NULL,
            "created_at" timestamp_with_timezone_text NOT NULL"#,
        foreign_keys: r#"
            FOREIGN KEY ("issue_id") REFERENCES "issues" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("user_id") REFERENCES "users" ("id") ON DELETE {on_delete}"#,
        indexes: &[
            (
                "idx_time_entries_issue",
                r#"CREATE INDEX "idx_time_entries_issue" ON "time_entries" ("issue_id");"#,
            ),
            (
                "idx_time_entries_user",
                r#"CREATE INDEX "idx_time_entries_user" ON "time_entries" ("user_id");"#,
            ),
        ],
        sequence_row: r#"
            INSERT INTO time_entries (id, issue_id, user_id, duration_minutes, description,
                                      created_at)
            SELECT {seq}, 0, 0, 0, '_forgekeep_sequence_high_water', CURRENT_TIMESTAMP
            WHERE {seq} > COALESCE((SELECT MAX(id) FROM time_entries), 0);
            DELETE FROM time_entries
            WHERE id = {seq} AND issue_id = 0
              AND description = '_forgekeep_sequence_high_water';"#,
    },
];

#[cfg(test)]
#[path = "m20260805_000004_repo_config_outlives_its_author_tests.rs"]
mod tests;
