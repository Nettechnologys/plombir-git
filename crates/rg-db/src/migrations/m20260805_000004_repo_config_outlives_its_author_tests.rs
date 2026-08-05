use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TryGetable};
use sea_orm_migration::{MigratorTrait, SchemaManager};

use super::super::ghost_author::{Shape, SqliteRebuildPoint};
use super::REBUILD;

/// A commit status that is hard-deleted before the rebuild, so the table's
/// `AUTOINCREMENT` high-water mark sits above its highest surviving row.
const HIGH_WATER_STATUS_ID: i64 = 50;

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-ghost-config-{label}-{}.db",
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

/// A database at the schema immediately before this migration, holding one of
/// each row the rebuild has to carry across: the guest's CI secret, deploy key,
/// commit status, board and environment approval, all inside the *host's*
/// repository — plus the board's column and card, which `DROP TABLE "boards"`
/// would cascade away if the pragmas failed.
async fn fixture(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    // One pooled connection on purpose. A rebuild swaps five tables out from
    // under every *other* connection of the pool, and SQLite lets the first
    // statement one of them runs afterwards fail once with a bare
    // `no such table: users` before it reloads its schema (card_a28a7004b108) —
    // which would make these tests flaky for a reason that has nothing to do
    // with what they assert.
    let db = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway SQLite database");

    const NAME: &str = "m20260805_000004_repo_config_outlives_its_author";
    let before_rebuild = crate::migrations::Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == NAME)
        .unwrap_or_else(|| panic!("{NAME} must still be part of the migration list"));
    let before_rebuild = u32::try_from(before_rebuild).expect("migration index fits in u32");
    crate::migrations::Migrator::up(&db, Some(before_rebuild))
        .await
        .expect("migrate to the schema immediately before the rebuild");

    db.execute_unprepared(&format!(
        r#"
        INSERT INTO users (id, username, email, password_hash, created_at, updated_at)
        VALUES
            (1, 'config-host', 'config-host@example.invalid', '',
             CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            (2, 'config-guest', 'config-guest@example.invalid', '',
             CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
        VALUES (1, 1, 'shared', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);

        INSERT INTO ci_secrets
            (id, repo_id, name, encrypted_value, created_by_id, created_at, updated_at)
        VALUES (1, 1, 'DEPLOY_TOKEN', 'ciphertext', 2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);

        INSERT INTO deploy_keys
            (id, repo_id, created_by_id, title, public_key, fingerprint, read_only, created_at)
        VALUES (1, 1, 2, 'ci', 'ssh-ed25519 AAAA', 'SHA256:deadbeef', TRUE, CURRENT_TIMESTAMP);

        INSERT INTO commit_statuses
            (id, repo_id, sha, state, context, creator_id, created_at, updated_at)
        VALUES
            (1, 1, 'c0ffee', 'success', 'ci/build', 2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            ({HIGH_WATER_STATUS_ID}, 1, 'c0ffee', 'success', 'ci/gone', 2,
             CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);

        INSERT INTO boards (id, repo_id, name, created_by, created_at, updated_at)
        VALUES
            (1, 1, 'Guest board', 2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
            (2, 1, 'Host board', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO board_columns (id, board_id, name, position, created_at)
        VALUES (1, 1, 'To do', 0, CURRENT_TIMESTAMP);
        INSERT INTO board_cards (id, column_id, note, position, created_at, updated_at)
        VALUES (1, 1, 'a card', 0, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);

        INSERT INTO ci_environments
            (id, repo_id, name, protected, required_approvals, created_at, updated_at)
        VALUES (1, 1, 'production', TRUE, 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO pipeline_jobs (id, stage_id, name, script, status)
        VALUES (1, 1, 'deploy', 'make deploy', 'success');
        INSERT INTO ci_environment_approvals
            (id, job_id, environment_id, approved_by, created_at)
        VALUES (1, 1, 1, 2, CURRENT_TIMESTAMP);

        INSERT INTO issues (id, repo_id, number, title, author_id, created_at, updated_at)
        VALUES (1, 1, 1, 'A bug', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO time_entries
            (id, issue_id, user_id, duration_minutes, description, created_at)
        VALUES (1, 1, 2, 180, 'debugging', CURRENT_TIMESTAMP);

        DELETE FROM commit_statuses WHERE id = {HIGH_WATER_STATUS_ID};
        "#
    ))
    .await
    .expect("seed the guest's configuration inside the host's repository");

    assert_eq!(
        scalar(
            &db,
            "SELECT seq FROM sqlite_sequence WHERE name = 'commit_statuses'",
        )
        .await,
        HIGH_WATER_STATUS_ID
    );
    (db, temp)
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            sql.to_string(),
        ))
        .await
        .expect("read scalar")
        .expect("scalar row exists");
    i64::try_get_by_index(&row, 0).expect("decode scalar")
}

async fn table_sql(db: &DatabaseConnection, table: &str) -> String {
    db.query_one(Statement::from_string(
        DatabaseBackend::Sqlite,
        format!("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = '{table}'"),
    ))
    .await
    .expect("read table schema")
    .unwrap_or_else(|| panic!("{table} exists"))
    .try_get::<String>("", "sql")
    .expect("decode table schema")
}

/// Everything the rebuild must not lose, whichever shape it produced.
async fn assert_rebuild_invariants(db: &DatabaseConnection) {
    for (table, expected) in [
        ("ci_secrets", 1),
        ("deploy_keys", 1),
        ("commit_statuses", 1),
        ("boards", 2),
        ("ci_environment_approvals", 1),
        ("time_entries", 1),
        // The children of `boards`: a rebuild whose `DROP TABLE` cascaded would
        // empty these while leaving the boards themselves intact.
        ("board_columns", 1),
        ("board_cards", 1),
    ] {
        assert_eq!(
            scalar(db, &format!("SELECT count(*) FROM {table}")).await,
            expected,
            "the rebuild lost rows from {table}"
        );
    }
    assert_eq!(
        scalar(
            db,
            "SELECT seq FROM sqlite_sequence WHERE name = 'commit_statuses'",
        )
        .await,
        HIGH_WATER_STATUS_ID,
        "the rebuild lowered the AUTOINCREMENT high-water mark, so a new row would reuse an id \
         an API caller already stored"
    );
    assert_eq!(
        scalar(
            db,
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name IN \
             ('idx_deploy_keys_repo_created', 'idx_commit_statuses_repo_sha', \
              'idx_time_entries_issue', 'idx_time_entries_user')",
        )
        .await,
        4,
        "the rebuild did not restore every named index"
    );
    assert_eq!(
        scalar(db, "SELECT count(*) FROM pragma_foreign_key_check").await,
        0,
        "the rebuild left a dangling foreign key reference behind"
    );
}

/// The rule the migration exists to install, read back off the live schema.
async fn assert_shape(db: &DatabaseConnection, shape: Shape) {
    let (expected_action, expected_notnull) = match shape {
        Shape::Ghost => ("SET NULL", 0),
        Shape::Owned => ("CASCADE", 1),
    };
    for &(table, column) in REBUILD.columns {
        let sql = table_sql(db, table).await;
        let normalized = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let needle = format!(r#"FOREIGN KEY ("{column}") REFERENCES "users" ("id") ON DELETE "#);
        let rest = normalized
            .split(&needle)
            .nth(1)
            .unwrap_or_else(|| panic!("{table} has no users foreign key on {column}: {sql}"));
        assert!(
            rest.starts_with(expected_action),
            "{table}.{column} carries the wrong ON DELETE action: {sql}"
        );
        assert_eq!(
            scalar(
                db,
                &format!(
                    "SELECT \"notnull\" FROM pragma_table_info('{table}') WHERE name = '{column}'"
                ),
            )
            .await,
            expected_notnull,
            "{table}.{column} carries the wrong nullability"
        );
    }
}

/// The whole point: with the account gone, the configuration it left in
/// somebody else's repository still works, and only its author is missing.
#[tokio::test]
async fn deleting_the_author_ghosts_the_configuration_instead_of_destroying_it() {
    let (db, _temp) = fixture("ghosted-author").await;
    let manager = SchemaManager::new(&db);
    REBUILD
        .sqlite(&manager, Shape::Ghost)
        .await
        .expect("rebuild the configuration tables");
    assert_shape(&db, Shape::Ghost).await;
    assert_rebuild_invariants(&db).await;

    db.execute_unprepared("DELETE FROM users WHERE id = 2")
        .await
        .expect("delete the account that configured somebody else's repository");

    for (table, column, lost) in [
        (
            "ci_secrets",
            "created_by_id",
            "the repository's CI secret died with the account that set it — its pipelines now \
             fail on an empty variable",
        ),
        (
            "deploy_keys",
            "created_by_id",
            "the repository's deploy key died with the account that added it — its deployments \
             now silently have no access",
        ),
        (
            "commit_statuses",
            "creator_id",
            "the commit's check result died with the account that reported it",
        ),
        (
            "boards",
            "created_by",
            "the board died with the account that created it",
        ),
        (
            "ci_environment_approvals",
            "approved_by",
            "the record of who approved the protected deployment died with the approver, so \
             nothing says it was ever approved",
        ),
        (
            "time_entries",
            "user_id",
            "the hours logged against the host's issue died with the account that logged them, \
             lowering the issue's total",
        ),
    ] {
        assert_eq!(
            scalar(&db, &format!("SELECT count(*) FROM {table} WHERE id = 1")).await,
            1,
            "{lost}"
        );
        assert_eq!(
            scalar(
                &db,
                &format!("SELECT count(*) FROM {table} WHERE id = 1 AND {column} IS NULL"),
            )
            .await,
            1,
            "{table}.{column} was not ghosted"
        );
    }

    assert_eq!(
        scalar(&db, "SELECT count(*) FROM board_columns").await,
        1,
        "the board's columns died with the account that created the board"
    );
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM board_cards").await,
        1,
        "the board's cards died with the account that created the board"
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM boards WHERE id = 2 AND created_by = 1"
        )
        .await,
        1,
        "a board created by somebody else lost its author"
    );
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM repositories WHERE id = 1").await,
        1,
        "deleting the guest reached the host's repository"
    );
}

/// A failure part-way through leaves every table, row and sequence exactly as it
/// was — and the same migration can simply be run again.
#[tokio::test]
async fn failure_mid_rebuild_rolls_back_and_the_same_rebuild_can_retry() {
    let (db, _temp) = fixture("rollback-retry").await;
    let old_ci_secrets = table_sql(&db, "ci_secrets").await;
    let manager = SchemaManager::new(&db);

    let error = REBUILD
        .sqlite_with_hook(&manager, Shape::Ghost, |point| async move {
            if point == SqliteRebuildPoint::TableRebuilt("ci_secrets") {
                Err(sea_orm::DbErr::Custom(
                    "injected failure after rebuilding ci_secrets".to_string(),
                ))
            } else {
                Ok(())
            }
        })
        .await
        .expect_err("injected rebuild failure must escape");
    assert!(error.to_string().contains("injected failure"));

    assert_eq!(
        table_sql(&db, "ci_secrets").await,
        old_ci_secrets,
        "the rolled-back rebuild left the replacement `ci_secrets` table behind"
    );
    assert_shape(&db, Shape::Owned).await;
    assert_rebuild_invariants(&db).await;

    db.execute_unprepared(
        "INSERT INTO commit_statuses
             (repo_id, sha, state, context, creator_id, created_at, updated_at)
         VALUES (1, 'c0ffee', 'failure', 'ci/after-failure', 2,
                 CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
    )
    .await
    .expect("the rolled-back tables remain writable");

    REBUILD
        .sqlite(&manager, Shape::Ghost)
        .await
        .expect("the same migration can be retried after a rollback");
    assert_shape(&db, Shape::Ghost).await;
    assert_eq!(
        scalar(&db, "SELECT count(*) FROM commit_statuses").await,
        2,
        "the retried rebuild lost the row written between the two attempts"
    );
}

/// `down` is a real reversal, and it refuses rather than deleting other
/// people's configuration to make `NOT NULL` fit again.
#[tokio::test]
async fn down_restores_the_cascade_and_refuses_once_a_row_is_ghosted() {
    let (db, _temp) = fixture("reversal").await;
    let manager = SchemaManager::new(&db);
    REBUILD
        .sqlite(&manager, Shape::Ghost)
        .await
        .expect("rebuild the configuration tables");

    REBUILD
        .sqlite(&manager, Shape::Owned)
        .await
        .expect("reverse a rebuild nothing has ghosted yet");
    assert_shape(&db, Shape::Owned).await;
    assert_rebuild_invariants(&db).await;

    REBUILD
        .sqlite(&manager, Shape::Ghost)
        .await
        .expect("rebuild the configuration tables again");
    db.execute_unprepared("DELETE FROM users WHERE id = 2")
        .await
        .expect("ghost the author");

    let error = REBUILD
        .sqlite(&manager, Shape::Owned)
        .await
        .expect_err("a ghosted row has no author to restore, so the reversal must fail");
    assert!(
        error.to_string().contains("NOT NULL"),
        "the reversal failed for the wrong reason: {error}"
    );
    assert_shape(&db, Shape::Ghost).await;
    assert_rebuild_invariants(&db).await;
}
