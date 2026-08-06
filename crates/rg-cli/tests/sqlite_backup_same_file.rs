use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::{Command, Output};

use rg_db::sea_orm::{self, ConnectionTrait};

fn run_backup(database_url: &str, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forgekeep"))
        .args([
            "backup-db",
            output.to_str().expect("temporary path must be UTF-8"),
            "--db-url",
            database_url,
            "--force",
        ])
        .output()
        .expect("run the standalone backup process")
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

async fn seed_database(path: &Path, marker: &str) -> sea_orm::DatabaseConnection {
    let database_url = format!("sqlite://{}?mode=rwc", path.display());
    let database = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .unwrap();
    database
        .execute_unprepared("CREATE TABLE marker (value TEXT NOT NULL)")
        .await
        .unwrap();
    database
        .execute(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO marker (value) VALUES (?)",
            [marker.into()],
        ))
        .await
        .unwrap();
    database
}

async fn read_marker(database_url: &str) -> String {
    let database = rg_db::connect_with_pool(database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .unwrap();
    let row = database
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT value FROM marker",
        ))
        .await
        .unwrap()
        .expect("the marker row must survive");
    let marker = row.try_get::<String>("", "value").unwrap();
    database.close().await.unwrap();
    marker
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
fn same_file_output(alias: SameFileAlias, source: &Path) -> PathBuf {
    let parent = source.parent().unwrap();
    match alias {
        SameFileAlias::Direct => source.to_path_buf(),
        SameFileAlias::Dot => parent.join(".").join(source.file_name().unwrap()),
        SameFileAlias::DotDot => {
            let detour = parent.join("detour");
            std::fs::create_dir(&detour).unwrap();
            detour.join("..").join(source.file_name().unwrap())
        }
        SameFileAlias::Symlink => {
            use std::os::unix::fs::symlink;

            let output = parent.join("backup-symlink.db");
            symlink(source, &output).unwrap();
            output
        }
        SameFileAlias::Hardlink => {
            let output = parent.join("backup-hardlink.db");
            std::fs::hard_link(source, &output).unwrap();
            output
        }
    }
}

#[tokio::test]
async fn backup_force_same_path_preserves_the_live_marker() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("forgekeep.db");
    let database_url = format!("sqlite://{}?mode=rwc", source.display());
    seed_database(&source, "must-survive")
        .await
        .close()
        .await
        .unwrap();

    let rejected = run_backup(&database_url, &source);
    let rejected_output = command_output(&rejected);
    assert!(!rejected.status.success(), "{rejected_output}");
    assert_eq!(read_marker(&database_url).await, "must-survive");
    assert!(
        rejected_output.contains("source database and backup output refer to the same file")
            && rejected_output.contains("choose a different backup output path"),
        "the refusal was not actionable:\n{rejected_output}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn backup_rejects_every_same_file_alias_before_touching_database_files() {
    for alias in [
        SameFileAlias::Direct,
        SameFileAlias::Dot,
        SameFileAlias::DotDot,
        SameFileAlias::Symlink,
        SameFileAlias::Hardlink,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("forgekeep.db");
        let database_url = format!("sqlite://{}?mode=rwc", source.display());
        let live = seed_database(&source, &format!("marker-{alias:?}")).await;
        let wal = PathBuf::from(format!("{}-wal", source.display()));
        let shm = PathBuf::from(format!("{}-shm", source.display()));
        assert!(wal.is_file(), "the live pool must own a WAL for {alias:?}");
        assert!(shm.is_file(), "the live pool must own an SHM for {alias:?}");

        let output = same_file_output(alias, &source);
        let before = [
            FileSnapshot::read(&source),
            FileSnapshot::read(&wal),
            FileSnapshot::read(&shm),
            FileSnapshot::read(&output),
        ];

        let rejected = run_backup(&database_url, &output);
        let rejected_output = command_output(&rejected);
        assert!(
            !rejected.status.success(),
            "{alias:?} unexpectedly backed up:\n{rejected_output}"
        );
        assert!(
            rejected_output.contains("source database and backup output refer to the same file")
                && rejected_output.contains("choose a different backup output path"),
            "{alias:?} returned a non-actionable error:\n{rejected_output}"
        );
        assert_eq!(
            FileSnapshot::read(&source),
            before[0],
            "{alias:?} changed the source database"
        );
        assert_eq!(
            FileSnapshot::read(&wal),
            before[1],
            "{alias:?} changed the source WAL"
        );
        assert_eq!(
            FileSnapshot::read(&shm),
            before[2],
            "{alias:?} changed the source SHM"
        );
        assert_eq!(
            FileSnapshot::read(&output),
            before[3],
            "{alias:?} changed the output directory entry"
        );
        assert_eq!(
            live.query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT value FROM marker",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<String>("", "value")
            .unwrap(),
            format!("marker-{alias:?}")
        );
        live.close().await.unwrap();
    }
}

#[tokio::test]
async fn backup_force_to_a_different_file_produces_a_restorable_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("forgekeep.db");
    let database_url = format!("sqlite://{}?mode=rwc", source.display());
    let live = seed_database(&source, "copied-snapshot").await;
    let output = dir.path().join("backup.db");
    std::fs::write(&output, b"old backup that must be replaced").unwrap();

    let backup = run_backup(&database_url, &output);
    let backup_output = command_output(&backup);
    assert!(backup.status.success(), "{backup_output}");

    live.close().await.unwrap();
    let output_url = format!("sqlite://{}?mode=rw", output.display());
    assert_eq!(read_marker(&output_url).await, "copied-snapshot");
}
