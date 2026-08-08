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
    let db = rg_db::connect(&database_url)
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
    let db = rg_db::connect(database_url)
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

#[test]
fn import_and_package_list_help_name_the_sqlite_offline_requirement() {
    for args in [
        vec!["import".to_string(), "--help".to_string()],
        vec![
            "package".to_string(),
            "list".to_string(),
            "--help".to_string(),
        ],
    ] {
        let help = command_output(&args);
        let help_output = diagnostic(&help);
        assert!(help.status.success(), "{help_output}");
        assert!(help_output.contains("File-backed SQLite"), "{help_output}");
        assert!(help_output.contains("server"), "{help_output}");
        assert!(help_output.contains("stopped"), "{help_output}");
    }
}
