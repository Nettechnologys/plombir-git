//! Every scalar column that names a user carries a decision, and it is written down.
//!
//! `DELETE FROM users` reaches whatever the schema says it reaches. Twice now a
//! column naming the account that *produced* a row turned out to cascade into a
//! namespace that account did not own: card_1cfc81035e92 for the three columns
//! that own bytes, card_a2123e31ee6e for the six that own a repository's
//! configuration and history. Both were found by sweeping the schema by hand.
//!
//! This test is that sweep, kept. It reads both every `REFERENCES users(id)` and
//! every conventionally named scalar user-reference column off the live
//! migrated database, then matches them against the decisions below. A new
//! `user_id`, `author_id`, `*_by_id`, or other name from the vocabulary fails it
//! even when its migration forgot the foreign key entirely.
//!
//! * **`CASCADE`** — the row is meaningless without the account *and* lives in
//!   the account's own namespace: its credentials, sessions, subscriptions, the
//!   repositories it owns.
//! * **`SET NULL`** — the row lives in somebody else's namespace and stays
//!   useful without its author, who becomes a ghost.
//!
//! `NO FOREIGN KEY` is not an accidental omission. It is an explicit,
//! searchable contract: `organizations.owner_id` is enforced by a service
//! refusal, while author/action ids are durable snapshots. Their rows outlive
//! the account, the id stays available for audit/history, and every reader must
//! treat a missing `users` row as a ghost (card_7e4a56345094). Deciding is the
//! point; what must not happen again is a column arriving with an accidental
//! cascade or no declared rule and escaping the inventory.

use rg_db::sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

/// `(table, column, on_delete-or-absence, why)` for every scalar user reference.
const DECISIONS: &[(&str, &str, &str, &str)] = &[
    // ── The account's own namespace: it goes when the account goes ──────────
    (
        "access_tokens",
        "user_id",
        "CASCADE",
        "the account's own credential",
    ),
    (
        "ci_environment_approver_grants",
        "user_id",
        "CASCADE",
        "card_04db226ae9b3: the row is this account's authorization grant for one environment",
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
        "notifications",
        "user_id",
        "CASCADE",
        "card_dd3f86fde48e: an inbox row is meaningless without its recipient",
    ),
    (
        "oauth_accounts",
        "user_id",
        "CASCADE",
        "the account's own external identity link",
    ),
    (
        "organization_members",
        "user_id",
        "CASCADE",
        "card_dd3f86fde48e: the membership is the account's grant into the organization",
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
    (
        "protected_branch_push_grants",
        "user_id",
        "CASCADE",
        "card_04db226ae9b3: the row is this account's direct-push grant for one branch rule",
    ),
    (
        "protected_tag_push_grants",
        "user_id",
        "CASCADE",
        "card_04db226ae9b3: the row is this account's push grant for one tag rule",
    ),
    (
        "repo_collaborators",
        "user_id",
        "CASCADE",
        "card_dd3f86fde48e: the collaborator row is the account's repository grant",
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
    (
        "team_members",
        "user_id",
        "CASCADE",
        "card_dd3f86fde48e: the membership is the account's grant into the team",
    ),
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
    // ── User-like columns without a database reference ─────────────────────
    // `organizations.owner_id` has a complete service-level decision. The
    // remaining columns deliberately keep a durable numeric snapshot after the
    // user row is gone; routed account-deletion coverage proves their readers
    // expose a ghost instead of failing or filtering the authored row out.
    (
        "organizations",
        "owner_id",
        "NO FOREIGN KEY",
        "account deletion refuses while this account owns an organization; see \
         refuse_ownerships_this_deletion_may_not_cascade",
    ),
    (
        "audit_log",
        "user_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: append-only audit history keeps the actor id and denormalized username \
         as a durable ghost snapshot",
    ),
    (
        "issue_comments",
        "author_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the comment belongs to its issue; readers render a missing author as a \
         ghost",
    ),
    (
        "issues",
        "assignee_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the issue survives and its last assignment remains a ghost snapshot",
    ),
    (
        "issues",
        "author_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the issue belongs to its repository; readers render a missing author as \
         a ghost",
    ),
    (
        "merge_queue_entries",
        "enqueued_by_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the queue attempt belongs to its pull request and keeps its enqueuer as \
         a ghost",
    ),
    (
        "oci_repository",
        "owner_id",
        "NO FOREIGN KEY",
        "the value mirrors repositories.owner_id; repository deletion owns the OCI row, while \
         account deletion refuses foreign namespace ownership before the user row is removed",
    ),
    (
        "package_versions",
        "author_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: a published version belongs to its repository registry and keeps the \
         publisher as a ghost",
    ),
    (
        "packages",
        "owner_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: this is the first publisher, not namespace ownership; the package \
         belongs to its repository registry and outlives that publisher",
    ),
    (
        "pipelines",
        "triggered_by",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the run is repository history and keeps its trigger actor as a ghost",
    ),
    (
        "pr_reviewer_requests",
        "requested_by_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the request belongs to the pull request and keeps its requester as a \
         ghost",
    ),
    (
        "pr_reviews",
        "reviewer_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the review is immutable pull-request history and keeps its reviewer as \
         a ghost",
    ),
    (
        "pull_requests",
        "author_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the pull request belongs to its repository and keeps its author as a \
         ghost",
    ),
    (
        "pull_requests",
        "auto_merge_enabled_by_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the pull request keeps who enabled auto-merge as a ghost audit \
         snapshot",
    ),
    (
        "pull_requests",
        "reviewer_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the last assigned reviewer remains visible as a ghost on the pull \
         request",
    ),
    (
        "review_comments",
        "author_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the inline comment belongs to its review and keeps its author as a \
         ghost",
    ),
    (
        "review_comments",
        "resolved_by_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: thread resolution is history and keeps its resolver as a ghost",
    ),
    (
        "review_comments",
        "suggestion_applied_by_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: applying a suggestion is history and keeps its actor as a ghost",
    ),
    (
        "wiki_pages",
        "author_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the page belongs to its repository and keeps its last editor as a \
         ghost",
    ),
    (
        "wiki_revisions",
        "author_id",
        "NO FOREIGN KEY",
        "card_7e4a56345094: the immutable revision keeps its author as a ghost",
    ),
];

/// Scalar column names which mean "this row stores a users.id" in ForgeKeep.
///
/// Serialized arrays are deliberately not pretended into this scalar guard;
/// card_ce8b75ca7ed2 tracks the three JSON allow-lists and their own source
/// belt.
const USER_REFERENCE_COLUMN_NAMES: &[&str] = &[
    "actor_id",
    "approved_by",
    "assignee_id",
    "author_id",
    "auto_merge_enabled_by_id",
    "created_by",
    "created_by_id",
    "creator_id",
    "enqueued_by_id",
    "owner_id",
    "push_by",
    "requested_by_id",
    "resolved_by_id",
    "reviewer_id",
    "suggestion_applied_by_id",
    "triggered_by",
    "uploader_id",
    "user_id",
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

/// Every scalar column whose name says it stores a `users.id`, with its actual
/// `ON DELETE` action or the explicit `NO FOREIGN KEY` state.
async fn live_user_references(db: &DatabaseConnection) -> Vec<(String, String, String)> {
    let names = USER_REFERENCE_COLUMN_NAMES
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                r#"
            SELECT m.name AS child,
                   p.name AS column_name,
                   COALESCE(f."on_delete", 'NO FOREIGN KEY') AS on_delete
            FROM sqlite_master m
            JOIN pragma_table_info(m.name) p
            LEFT JOIN pragma_foreign_key_list(m.name) f
              ON f."from" = p.name AND f."table" = 'users' AND f."to" = 'id'
            WHERE m.type = 'table'
              AND m.name <> 'users'
              AND m.name NOT LIKE 'sqlite_%'
              AND p.name IN ({names})
            ORDER BY m.name, p.name
            "#
            ),
        ))
        .await
        .expect("read scalar columns that name users");

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

fn decision_drift(live: &[(String, String, String)]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut undeclared = Vec::new();
    let mut disagreeing = Vec::new();
    for (table, column, on_delete) in live {
        match DECISIONS
            .iter()
            .find(|(t, c, _, _)| t == table && c == column)
        {
            None => undeclared.push(format!("{table}.{column} -> {on_delete}")),
            Some((_, _, decided, _)) if decided != on_delete => disagreeing.push(format!(
                "{table}.{column}: the schema says {on_delete}, the decision says {decided}"
            )),
            Some(_) => {}
        }
    }

    let stale = DECISIONS
        .iter()
        .filter(|(table, column, _, _)| !live.iter().any(|(t, c, _)| t == table && c == column))
        .map(|(table, column, _, _)| format!("{table}.{column}"))
        .collect();
    (undeclared, disagreeing, stale)
}

#[tokio::test]
async fn every_scalar_user_reference_carries_a_written_down_decision() {
    let temp = TempDb::new();
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let live = live_user_references(&db).await;
    let (undeclared, disagreeing, stale) = decision_drift(&live);

    assert!(
        undeclared.is_empty(),
        "a scalar user-reference column arrived with no decision recorded in this test. Deciding \
         is the point — write down whether the row belongs to the account (CASCADE), lives in \
         somebody else's namespace (SET NULL), or temporarily has no FK and which card owns that \
         debt: {undeclared:?}"
    );
    assert!(
        disagreeing.is_empty(),
        "the schema no longer matches the decision recorded for it: {disagreeing:?}"
    );

    assert!(
        stale.is_empty(),
        "a decision is recorded for a user-like column the schema no longer has — drop it so this \
         list stays readable: {stale:?}"
    );
}

#[tokio::test]
async fn a_new_user_like_column_without_a_foreign_key_is_not_invisible() {
    let temp = TempDb::new();
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    db.execute_unprepared(
        "CREATE TABLE accidental_grants (
             id integer NOT NULL PRIMARY KEY AUTOINCREMENT,
             user_id bigint NOT NULL
         )",
    )
    .await
    .expect("mutate the live schema with an undecided user-like column");

    let live = live_user_references(&db).await;
    let (undeclared, _, _) = decision_drift(&live);
    assert!(
        undeclared
            .iter()
            .any(|entry| entry == "accidental_grants.user_id -> NO FOREIGN KEY"),
        "the guard failed to discover the undecided no-FK column: {undeclared:?}"
    );
}
