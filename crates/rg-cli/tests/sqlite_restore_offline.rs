use std::path::Path;
use std::process::{Command, Output};

use rg_db::sea_orm::{self, ConnectionTrait};

fn run_restore(database_url: &str, backup: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forgekeep"))
        .args([
            "restore-db",
            backup.to_str().expect("temporary path must be UTF-8"),
            "--db-url",
            database_url,
            "--force",
        ])
        .output()
        .expect("run the standalone restore process")
}

fn command_output(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[tokio::test]
async fn restore_refuses_a_live_sqlite_pool_without_touching_any_database_file() {
    let dir = tempfile::tempdir().unwrap();
    let database_path = dir.path().join("forgekeep.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let wal_path = Path::new(&format!("{}-wal", database_path.display())).to_path_buf();
    let shm_path = Path::new(&format!("{}-shm", database_path.display())).to_path_buf();
    let lock_path = Path::new(&format!("{}.forgekeep.lock", database_path.display())).to_path_buf();
    let backup_path = dir.path().join("backup.db");
    let backup_url = format!("sqlite://{}?mode=rwc", backup_path.display());

    let backup = rg_db::connect_with_pool(&backup_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .unwrap();
    backup
        .execute_unprepared("CREATE TABLE marker (value TEXT NOT NULL)")
        .await
        .unwrap();
    backup
        .execute_unprepared("INSERT INTO marker (value) VALUES ('restored')")
        .await
        .unwrap();
    backup.close().await.unwrap();

    let server_guard = rg_db::sqlite_process_guard::acquire_server(&database_url)
        .unwrap()
        .unwrap();
    let live = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .unwrap();
    live.execute_unprepared("CREATE TABLE marker (value TEXT NOT NULL)")
        .await
        .unwrap();
    live.execute_unprepared("INSERT INTO marker (value) VALUES ('live')")
        .await
        .unwrap();

    assert!(wal_path.is_file(), "the live pool must own a WAL file");
    assert!(shm_path.is_file(), "the live pool must own a SHM file");
    let before = [
        bytes(&database_path),
        bytes(&wal_path),
        bytes(&shm_path),
        bytes(&backup_path),
    ];

    let blocked = run_restore(&database_url, &backup_path);
    let blocked_output = command_output(&blocked);
    assert!(!blocked.status.success(), "{blocked_output}");
    assert!(
        blocked_output.contains("SQLite restore requires the ForgeKeep server to be stopped"),
        "{blocked_output}"
    );
    assert_eq!(bytes(&database_path), before[0], "target database changed");
    assert_eq!(bytes(&wal_path), before[1], "WAL changed");
    assert_eq!(bytes(&shm_path), before[2], "SHM changed");
    assert_eq!(bytes(&backup_path), before[3], "backup input changed");
    assert!(
        lock_path.is_file(),
        "the persistent process-lease sidecar must not be cleaned up"
    );

    let live_row = live
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT value FROM marker".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live_row.try_get::<String>("", "value").unwrap(), "live");
    live.close().await.unwrap();
    drop(server_guard);

    let restored = run_restore(&database_url, &backup_path);
    let restored_output = command_output(&restored);
    assert!(restored.status.success(), "{restored_output}");
    assert_eq!(bytes(&database_path), before[3]);
    assert!(!wal_path.exists(), "restore must remove the old WAL");
    assert!(!shm_path.exists(), "restore must remove the old SHM");
    assert!(
        lock_path.is_file(),
        "restore must preserve the process-lease sidecar"
    );

    let reopened = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .unwrap();
    let restored_row = reopened
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT value FROM marker".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        restored_row.try_get::<String>("", "value").unwrap(),
        "restored"
    );
}
