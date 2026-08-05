//! Every foreign key into `users` carries a decision, and it is written down.
//!
//! `DELETE FROM users` reaches whatever the schema says it reaches. Twice now a
//! column naming the account that *produced* a row turned out to cascade into a
//! namespace that account did not own: card_1cfc81035e92 for the three columns
//! that own bytes, card_a2123e31ee6e for the six that own a repository's
//! configuration and history. Both were found by sweeping the schema by hand.
//!
//! This test is that sweep, kept. It reads every `REFERENCES users(id)` off the
//! live migrated database and matches it against the decisions below. A new
//! foreign key into `users` fails it until somebody writes down which of the two
//! rules it follows and why:
//!
//! * **`CASCADE`** — the row is meaningless without the account *and* lives in
//!   the account's own namespace: its credentials, sessions, subscriptions, the
//!   repositories it owns.
//! * **`SET NULL`** — the row lives in somebody else's namespace and stays
//!   useful without its author, who becomes a ghost.
//!
//! Deciding is the point; either answer passes. What must not happen again is a
//! column arriving with `ON DELETE CASCADE` because that is what the previous
//! `create_table` migration happened to say.

use rg_db::sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

/// `(table, column, on_delete, why)` for every foreign key into `users(id)`.
const DECISIONS: &[(&str, &str, &str, &str)] = &[
    // ── The account's own namespace: it goes when the account goes ──────────
    (
        "access_tokens",
        "user_id",
        "CASCADE",
        "the account's own credential",
    ),
    (
        "import_tasks",
        "user_id",
        "CASCADE",
        "the account's own import job, targeted at a repository it is creating",
    ),
    (
        "mfa_backup_codes",
        "user_id",
        "CASCADE",
        "the account's own second factor",
    ),
    (
        "oauth_accounts",
        "user_id",
        "CASCADE",
        "the account's own external identity link",
    ),
    (
        "passkey_credentials",
        "user_id",
        "CASCADE",
        "the account's own credential",
    ),
    (
        "password_reset_tokens",
        "user_id",
        "CASCADE",
        "the account's own one-shot reset grant",
    ),
    (
        "pr_reviewer_requests",
        "reviewer_id",
        "CASCADE",
        "the row asks this specific person for a review, and nothing can be asked of a ghost \
         (weighed with card_a2123e31ee6e and deliberately left cascading)",
    ),
    ("repo_stars", "user_id", "CASCADE", "the account's own star"),
    (
        "repo_watches",
        "user_id",
        "CASCADE",
        "the account's own subscription",
    ),
    (
        "repositories",
        "owner_id",
        "CASCADE",
        "the account owns the repository; `rg_core::user::service::delete_user` retires its \
         storage first, see user_delete_cascades_repositories.rs",
    ),
    ("ssh_keys", "user_id", "CASCADE", "the account's own key"),
    // ── Somebody else's namespace: the row outlives its author ─────────────
    (
        "attachments",
        "uploader_id",
        "SET NULL",
        "card_1cfc81035e92: the file lives in whichever repository it was uploaded into",
    ),
    (
        "release_assets",
        "uploader_id",
        "SET NULL",
        "card_1cfc81035e92: the asset belongs to its release, not to its uploader",
    ),
    (
        "releases",
        "author_id",
        "SET NULL",
        "card_1cfc81035e92: the release belongs to its repository, and took every asset with it",
    ),
    (
        "login_logs",
        "user_id",
        "SET NULL",
        "the instance's audit record of a sign-in attempt",
    ),
    (
        "pr_events",
        "actor_id",
        "SET NULL",
        "the pull request's timeline, in whichever repository it lives",
    ),
    (
        "ci_secrets",
        "created_by_id",
        "SET NULL",
        "card_a2123e31ee6e: the secret belongs to its repository; losing it breaks that \
         repository's pipelines",
    ),
    (
        "deploy_keys",
        "created_by_id",
        "SET NULL",
        "card_a2123e31ee6e: repository access the owner configured, not the departing person's \
         credential",
    ),
    (
        "commit_statuses",
        "creator_id",
        "SET NULL",
        "card_a2123e31ee6e: a check result is a fact about a commit, including on merged PRs",
    ),
    (
        "boards",
        "created_by",
        "SET NULL",
        "card_a2123e31ee6e: the board belongs to its repository or organization, together with \
         its columns and cards",
    ),
    (
        "ci_environment_approvals",
        "approved_by",
        "SET NULL",
        "card_a2123e31ee6e: the row is the audit record that an approval happened",
    ),
    (
        "time_entries",
        "user_id",
        "SET NULL",
        "card_a2123e31ee6e: the hours are summed into the issue's total by \
         time_entry_ops::total_minutes_by_issue",
    ),
];

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-user-fk-decisions-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
}

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "cleanup must not mask the assertion that failed the test"
    )]
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

/// Every `(table, column, on_delete)` pointing at `users(id)`, off the live
/// schema rather than off the migrations that were meant to produce it.
async fn live_user_foreign_keys(db: &DatabaseConnection) -> Vec<(String, String, String)> {
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            r#"
            SELECT m.name AS child, f."from" AS column_name, f."on_delete" AS on_delete
            FROM sqlite_master m
            JOIN pragma_foreign_key_list(m.name) f
            WHERE m.type = 'table' AND f."table" = 'users'
            ORDER BY m.name, f."from"
            "#,
        ))
        .await
        .expect("read the foreign keys pointing at users");

    rows.iter()
        .map(|row| {
            (
                row.try_get::<String>("", "child").expect("decode table"),
                row.try_get::<String>("", "column_name")
                    .expect("decode column"),
                row.try_get::<String>("", "on_delete")
                    .expect("decode on delete"),
            )
        })
        .collect()
}

#[tokio::test]
async fn every_users_foreign_key_carries_a_written_down_decision() {
    let temp = TempDb::new();
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let live = live_user_foreign_keys(&db).await;

    let mut undeclared = Vec::new();
    let mut disagreeing = Vec::new();
    for (table, column, on_delete) in &live {
        match DECISIONS
            .iter()
            .find(|(t, c, _, _)| t == table && c == column)
        {
            None => undeclared.push(format!("{table}.{column} -> ON DELETE {on_delete}")),
            Some((_, _, decided, _)) if decided != on_delete => disagreeing.push(format!(
                "{table}.{column}: the schema says ON DELETE {on_delete}, the decision says \
                 {decided}"
            )),
            Some(_) => {}
        }
    }

    assert!(
        undeclared.is_empty(),
        "a foreign key into `users` arrived with no decision recorded in this test. Deciding is \
         the point — write down whether the row belongs to the account (CASCADE) or lives in \
         somebody else's namespace and should outlive it (SET NULL), and why: {undeclared:?}"
    );
    assert!(
        disagreeing.is_empty(),
        "the schema no longer matches the decision recorded for it: {disagreeing:?}"
    );

    let stale: Vec<_> = DECISIONS
        .iter()
        .filter(|(table, column, _, _)| !live.iter().any(|(t, c, _)| t == table && c == column))
        .map(|(table, column, _, _)| format!("{table}.{column}"))
        .collect();
    assert!(
        stale.is_empty(),
        "a decision is recorded for a foreign key the schema no longer has — drop it so this \
         list stays readable: {stale:?}"
    );
}
