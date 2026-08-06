use std::process::Command;

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
