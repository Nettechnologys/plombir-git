use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::{Command, Output};

use rg_db::sea_orm::{self, ConnectionTrait};

fn run_restore(database_url: &str, backup: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_plombir-git"))
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

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct MetadataSnapshot {
    dev: u64,
    ino: u64,
    mode: u32,
    nlink: u64,
    uid: u32,
    gid: u32,
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
}

#[cfg(unix)]
impl MetadataSnapshot {
    fn read(path: &Path, follow_symlink: bool) -> Self {
        use std::os::unix::fs::MetadataExt;

        let metadata = if follow_symlink {
            std::fs::metadata(path)
        } else {
            std::fs::symlink_metadata(path)
        }
        .unwrap_or_else(|error| panic!("read metadata for {}: {error}", path.display()));
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            mode: metadata.mode(),
            nlink: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            size: metadata.size(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
        }
    }
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct FileSnapshot {
    bytes: Vec<u8>,
    followed: MetadataSnapshot,
    directory_entry: MetadataSnapshot,
}

#[cfg(unix)]
impl FileSnapshot {
    fn read(path: &Path) -> Self {
        Self {
            bytes: bytes(path),
            followed: MetadataSnapshot::read(path, true),
            directory_entry: MetadataSnapshot::read(path, false),
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum SameFileAlias {
    Direct,
    Dot,
    DotDot,
    Symlink,
    Hardlink,
}

#[cfg(unix)]
fn same_file_target(alias: SameFileAlias, input: &Path) -> PathBuf {
    let parent = input.parent().unwrap();
    match alias {
        SameFileAlias::Direct => input.to_path_buf(),
        SameFileAlias::Dot => parent.join(".").join(input.file_name().unwrap()),
        SameFileAlias::DotDot => {
            let detour = parent.join("detour");
            std::fs::create_dir(&detour).unwrap();
            detour.join("..").join(input.file_name().unwrap())
        }
        SameFileAlias::Symlink => {
            use std::os::unix::fs::symlink;

            let target = parent.join("target-symlink.db");
            symlink(input, &target).unwrap();
            target
        }
        SameFileAlias::Hardlink => {
            let target = parent.join("target-hardlink.db");
            std::fs::hard_link(input, &target).unwrap();
            target
        }
    }
}

#[cfg(unix)]
#[test]
fn restore_rejects_every_same_file_alias_before_touching_database_files() {
    for alias in [
        SameFileAlias::Direct,
        SameFileAlias::Dot,
        SameFileAlias::DotDot,
        SameFileAlias::Symlink,
        SameFileAlias::Hardlink,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("backup.db");
        std::fs::write(&input, format!("original database for {alias:?}")).unwrap();
        let target = same_file_target(alias, &input);
        let wal = PathBuf::from(format!("{}-wal", target.display()));
        let shm = PathBuf::from(format!("{}-shm", target.display()));
        std::fs::write(&wal, format!("WAL for {alias:?}")).unwrap();
        std::fs::write(&shm, format!("SHM for {alias:?}")).unwrap();

        let canonical_target = target.canonicalize().unwrap();
        let lock = PathBuf::from(format!("{}.plombir-git.lock", canonical_target.display()));
        assert!(!lock.exists(), "test precondition failed for {alias:?}");

        let before = [
            FileSnapshot::read(&input),
            FileSnapshot::read(&target),
            FileSnapshot::read(&wal),
            FileSnapshot::read(&shm),
        ];
        let database_url = format!("sqlite://{}?mode=rwc", target.display());

        let rejected = run_restore(&database_url, &input);
        let rejected_output = command_output(&rejected);
        assert!(
            !rejected.status.success(),
            "{alias:?} unexpectedly restored:\n{rejected_output}"
        );
        assert!(
            rejected_output.contains("backup input and target database refer to the same file")
                && rejected_output
                    .contains("choose a different backup input or target database path"),
            "{alias:?} returned a non-actionable error:\n{rejected_output}"
        );

        assert_eq!(
            FileSnapshot::read(&input),
            before[0],
            "{alias:?} changed the backup input"
        );
        assert_eq!(
            FileSnapshot::read(&target),
            before[1],
            "{alias:?} changed the target entry"
        );
        assert_eq!(
            FileSnapshot::read(&wal),
            before[2],
            "{alias:?} changed the WAL"
        );
        assert_eq!(
            FileSnapshot::read(&shm),
            before[3],
            "{alias:?} changed the SHM"
        );
        assert!(
            !lock.exists(),
            "{alias:?} reached the process lease before the identity preflight"
        );
    }
}

#[tokio::test]
async fn restore_refuses_a_live_sqlite_pool_without_touching_any_database_file() {
    let dir = tempfile::tempdir().unwrap();
    let database_path = dir.path().join("plombir-git.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let wal_path = Path::new(&format!("{}-wal", database_path.display())).to_path_buf();
    let shm_path = Path::new(&format!("{}-shm", database_path.display())).to_path_buf();
    let lock_path =
        Path::new(&format!("{}.plombir-git.lock", database_path.display())).to_path_buf();
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
        blocked_output.contains("SQLite restore requires the Plombir Git server to be stopped"),
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
