use std::process::Command;

use rg_db::sea_orm::{ConnectionTrait, Statement};

fn command_output(args: &[String]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_forgekeep"))
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run `forgekeep {}`: {error}", args.join(" ")))
}

fn diagnostic(output: &std::process::Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

async fn create_pending_database() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let database_path = dir.path().join("forgekeep.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let db = rg_db::connect_with_pool(
        &database_url,
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
        rg_db::DEFAULT_MAX_CONNECTIONS,
    )
    .await
    .expect("open the pending-migration fixture");
    db.execute_unprepared(
        "CREATE TABLE seaql_migrations (\
             version VARCHAR NOT NULL PRIMARY KEY, \
             applied_at BIGINT NOT NULL\
         )",
    )
    .await
    .expect("create an empty migration history");
    db.close()
        .await
        .expect("close the pending-migration fixture");
    (dir, database_url)
}

async fn migration_versions(database_url: &str) -> Vec<String> {
    let db = rg_db::connect_with_pool(
        database_url,
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
        rg_db::DEFAULT_MAX_CONNECTIONS,
    )
    .await
    .expect("reopen the migration-history fixture");
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT version FROM seaql_migrations ORDER BY version".to_string(),
        ))
        .await
        .expect("read migration history");
    let versions = rows
        .into_iter()
        .map(|row| row.try_get_by_index(0).expect("decode migration version"))
        .collect();
    db.close()
        .await
        .expect("close the migration-history fixture");
    versions
}

async fn assert_blocked_before_migrations(database_url: &str, args: &[String]) {
    let before = migration_versions(database_url).await;
    assert!(
        before.is_empty(),
        "fixture must start with pending migrations"
    );

    let server_guard = rg_db::sqlite_process_guard::acquire_server(database_url)
        .unwrap()
        .unwrap();
    let blocked = command_output(args);
    let blocked_output = diagnostic(&blocked);
    assert!(!blocked.status.success(), "{blocked_output}");
    assert!(
        blocked_output.contains("server to be stopped"),
        "{blocked_output}"
    );
    drop(server_guard);

    assert_eq!(
        migration_versions(database_url).await,
        before,
        "the refused command changed seaql_migrations"
    );
}

#[test]
fn standalone_migrate_process_requires_the_file_backed_sqlite_server_to_stop() {
    let dir = tempfile::tempdir().unwrap();
    let database_path = dir.path().join("forgekeep.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let server_guard = rg_db::sqlite_process_guard::acquire_server(&database_url)
        .unwrap()
        .unwrap();

    let blocked = Command::new(env!("CARGO_BIN_EXE_forgekeep"))
        .args(["migrate", "--db-url", &database_url])
        .output()
        .expect("run the standalone migrate process while the server lease is held");
    let blocked_output = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&blocked.stdout),
        String::from_utf8_lossy(&blocked.stderr)
    );
    assert!(!blocked.status.success(), "{blocked_output}");
    assert!(
        blocked_output.contains("server to be stopped"),
        "{blocked_output}"
    );

    drop(server_guard);

    let migrated = Command::new(env!("CARGO_BIN_EXE_forgekeep"))
        .args(["migrate", "--db-url", &database_url])
        .output()
        .expect("run the standalone migrate process after the server stops");
    let migrated_output = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&migrated.stdout),
        String::from_utf8_lossy(&migrated.stderr)
    );
    assert!(migrated.status.success(), "{migrated_output}");
    assert!(database_path.is_file(), "{migrated_output}");
    assert!(
        std::fs::metadata(&database_path).unwrap().len() > 0,
        "{migrated_output}"
    );
}

#[tokio::test]
async fn import_refuses_a_live_sqlite_server_before_migrating() {
    let (import_dir, import_database_url) = create_pending_database().await;
    let impossible_repo_root = import_dir.path().join("not-a-directory");
    std::fs::write(&impossible_repo_root, b"block create_dir_all").unwrap();
    assert_blocked_before_migrations(
        &import_database_url,
        &[
            "import".to_string(),
            "github".to_string(),
            "https://github.com/alice/site".to_string(),
            "--target-owner".to_string(),
            "alice".to_string(),
            "--repo-root".to_string(),
            impossible_repo_root.display().to_string(),
            "--db-url".to_string(),
            import_database_url.clone(),
        ],
    )
    .await;
}

#[tokio::test]
async fn package_list_refuses_a_live_sqlite_server_before_migrating() {
    let (_package_dir, package_database_url) = create_pending_database().await;
    assert_blocked_before_migrations(
        &package_database_url,
        &[
            "package".to_string(),
            "list".to_string(),
            "alice".to_string(),
            "site".to_string(),
            "cargo".to_string(),
            "--db-url".to_string(),
            package_database_url.clone(),
        ],
    )
    .await;
}

/// Every command that refuses a live file-backed SQLite server says so in its
/// own `--help`, in the same words.
///
/// The two maintenance commands joined the list for a different reason than the
/// migrating ones — they hold the write lock rather than change the schema —
/// but an operator reads one sentence and needs it to mean the same thing
/// (card_74c8b8754e97).
#[test]
fn offline_only_commands_name_the_sqlite_requirement_in_their_help() {
    for args in [
        vec!["import".to_string(), "--help".to_string()],
        vec![
            "package".to_string(),
            "list".to_string(),
            "--help".to_string(),
        ],
        vec!["rebuild-fts".to_string(), "--help".to_string()],
        vec!["rotate-encryption-key".to_string(), "--help".to_string()],
    ] {
        let help = command_output(&args);
        let help_output = diagnostic(&help);
        assert!(help.status.success(), "{help_output}");
        assert!(help_output.contains("File-backed SQLite"), "{help_output}");
        assert!(help_output.contains("server"), "{help_output}");
        assert!(help_output.contains("stopped"), "{help_output}");
    }
}

/// A database with the schema applied and nothing in it — what a maintenance
/// command is pointed at.
async fn migrated_database() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let database_path = dir.path().join("forgekeep.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let db = rg_db::connect_with_pool(
        &database_url,
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
        rg_db::DEFAULT_MAX_CONNECTIONS,
    )
    .await
    .expect("open the maintenance fixture");
    rg_db::run_migrations(&db)
        .await
        .expect("apply the schema the maintenance pass will read");
    db.close().await.expect("close the maintenance fixture");
    (dir, database_url)
}

/// `rebuild-fts` rebuilds all three FTS tables under one write transaction, so
/// on file-backed SQLite it holds the single write lock from the first `DELETE`
/// to the commit — and a live writer that meets a held write lock is refused
/// with `database is locked`, not queued behind it. It used to open an ordinary
/// pool and start anyway (card_74c8b8754e97).
///
/// Both halves matter. Refusing while the server is up is the contract; running
/// once the server stops is what keeps the contract from being a way to make
/// the command permanently unusable.
#[tokio::test]
async fn rebuild_fts_refuses_a_live_sqlite_server_and_runs_once_it_stops() {
    let (_dir, database_url) = migrated_database().await;
    let args = vec![
        "rebuild-fts".to_string(),
        "--db-url".to_string(),
        database_url.clone(),
    ];

    let server_guard = rg_db::sqlite_process_guard::acquire_server(&database_url)
        .unwrap()
        .unwrap();
    let blocked = command_output(&args);
    let blocked_output = diagnostic(&blocked);
    assert!(
        !blocked.status.success(),
        "the rebuild ran against a live server: {blocked_output}"
    );
    assert!(
        blocked_output.contains("server to be stopped"),
        "{blocked_output}"
    );
    assert!(
        blocked_output.contains("forgekeep rebuild-fts"),
        "the refusal has to name the command the operator ran, not a migration: {blocked_output}"
    );
    drop(server_guard);

    let allowed = command_output(&args);
    assert!(allowed.status.success(), "{}", diagnostic(&allowed));
}

/// The same contract on the other whole-database pass. `rekey` opens one
/// transaction and rewrites every encrypted value inside it, and `--dry-run`
/// walks exactly the same rows before rolling back — so the dry run holds the
/// lock for as long as the real thing and is refused for the same reason.
#[tokio::test]
async fn rotate_encryption_key_dry_run_refuses_a_live_sqlite_server() {
    let (_dir, database_url) = migrated_database().await;
    let args = vec![
        "rotate-encryption-key".to_string(),
        "--db-url".to_string(),
        database_url.clone(),
        "--old".to_string(),
        "old-key-for-the-offline-contract-test".to_string(),
        "--new".to_string(),
        "new-key-for-the-offline-contract-test".to_string(),
        "--dry-run".to_string(),
    ];

    let server_guard = rg_db::sqlite_process_guard::acquire_server(&database_url)
        .unwrap()
        .unwrap();
    let blocked = command_output(&args);
    let blocked_output = diagnostic(&blocked);
    assert!(
        !blocked.status.success(),
        "the re-encryption pass ran against a live server: {blocked_output}"
    );
    assert!(
        blocked_output.contains("server to be stopped"),
        "{blocked_output}"
    );
    assert!(
        blocked_output.contains("forgekeep rotate-encryption-key"),
        "{blocked_output}"
    );
    drop(server_guard);
}
