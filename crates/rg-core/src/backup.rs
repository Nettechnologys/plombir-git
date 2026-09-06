//! Scheduled database backups — periodically snapshots the SQLite database with
//! `VACUUM INTO` and rotates old snapshots, so "are there backups?" is answered
//! by the config file rather than by whether someone remembered to write a cron
//! entry on one particular host.
//!
//! Modelled on [`crate::audit::archiver`], and for the same reasons: the
//! directory is created and write-probed at *startup* rather than on first use,
//! and every entry point returns its error to the caller instead of degrading
//! into a background task that warns into a log nobody reads.
//!
//! Running inside the server also removes the sharpest edge of the manual
//! `forgekeep backup-db`: the scheduler snapshots the pool the server itself is
//! using, so it cannot address a different database than the running instance —
//! the failure mode where `backup-db` without `--config`/`--db-url` backs up a
//! freshly-created empty file and reports success.

use crate::platform::fs::{discard_file, discard_file_async};
use crate::task_tracker::wait_optional_shutdown;
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::sync::watch;
use tokio::time;

/// Snapshot file names are `forgekeep-<UTC timestamp>-<uuid>.db`.
///
/// The shape is load-bearing for rotation: only files that parse back into
/// exactly this form are candidates for deletion, so a hand-made backup dropped
/// into the same directory (the `forgekeep-$(date +%Y%m%d-%H%M%S).db` from the
/// deployment guide, say) is never rotated away by the server.
const SNAPSHOT_PREFIX: &str = "forgekeep-";
const SNAPSHOT_SUFFIX: &str = ".db";
const SNAPSHOT_TIMESTAMP_FORMAT: &str = "%Y%m%dT%H%M%S";

/// Minimum wait before the first snapshot of a process, however overdue the
/// backup is. See [`spawn_db_backup_with_shutdown`].
const STARTUP_GRACE: Duration = Duration::from_secs(60);

/// Hours between snapshots, without a configured `[backup].interval_hours`.
///
/// Named here rather than written into the `unwrap_or` at the resolution site:
/// the same number is printed at the operator in `forgekeep.example.toml`,
/// `deploy/forgekeep.docker.toml` and `deploy/README.md`, and a value that has
/// no name in the code is a value no doc-versus-code check can reach.
pub const DEFAULT_INTERVAL_HOURS: u64 = 24;

/// Snapshots kept before the oldest is rotated out, without a configured
/// `[backup].keep_last`.
pub const DEFAULT_KEEP_LAST: usize = 7;

#[derive(Clone, Debug)]
pub struct DbBackupConfig {
    /// Directory the snapshots are written into.
    pub dir: PathBuf,
    /// Hours between snapshots.
    pub interval_hours: u64,
    /// How many snapshots to keep; older ones are deleted after a successful run.
    pub keep_last: usize,
}

impl DbBackupConfig {
    pub fn with_dir(dir: PathBuf) -> Self {
        Self {
            dir,
            interval_hours: DEFAULT_INTERVAL_HOURS,
            keep_last: DEFAULT_KEEP_LAST,
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.interval_hours > 0,
            "`[backup].interval_hours` must be positive"
        );
        // Zero would mean "delete every snapshot after taking it", i.e. a
        // backup task whose only observable effect is disk churn.
        anyhow::ensure!(self.keep_last > 0, "`[backup].keep_last` must be positive");
        Ok(())
    }

    fn period(&self) -> Duration {
        Duration::from_secs(self.interval_hours.saturating_mul(3_600))
    }
}

#[derive(Clone, Debug)]
pub struct BackupResult {
    /// The snapshot that was written.
    pub path: PathBuf,
    /// Its size on disk.
    pub bytes: u64,
    /// Older snapshots deleted by the `keep_last` rotation.
    pub pruned: usize,
    /// Snapshots that matched the rotation rule but could not be deleted. Kept
    /// separate from `pruned` so a rotation that silently stops working (and
    /// therefore fills the disk) is visible next to a successful snapshot
    /// rather than hidden behind it.
    pub prune_failures: usize,
}

/// Remediation appended to every `[backup].dir` failure. As with the audit
/// archiver, the escape hatch is named on purpose: an operator who does not want
/// scheduled backups should turn them off explicitly rather than leave the
/// server pointed at a directory it cannot use.
const BACKUP_DIR_HINT: &str =
    "point `[backup].dir` at a directory the server can create and write into, or set \
     `[backup].enabled = false` to turn scheduled database backups off";

/// One actionable line for any filesystem failure on the backup path.
///
/// [`describe_path_error`](crate::platform::fs::describe_path_error) trades the
/// caller's remedy for the uid diagnostic on a permission error; here both
/// matter, because a permission failure refuses the whole server start.
fn backup_path_error(what: &str, path: &Path, error: &std::io::Error) -> anyhow::Error {
    let described = crate::platform::fs::describe_path_error(what, path, error, "");
    anyhow::anyhow!("{described}\n  hint: {BACKUP_DIR_HINT}")
}

/// Create `dir` and prove it is writable — at startup, not a day later.
///
/// Without this the first filesystem contact happens inside the scheduler loop,
/// `interval_hours` after the start, and its only failure channel is a warning.
/// A backup directory the server cannot write (the usual case: a bind-mounted
/// host directory owned by another uid) would therefore mean no backups at all,
/// and the operator would find out at the moment a backup was needed.
pub fn ensure_backup_dir(dir: &Path) -> anyhow::Result<()> {
    // Owner-only, not `create_dir_all`: on a first start this directory is
    // created here, and what lands in it is the whole database — `VACUUM INTO`
    // writes the snapshot `0644` with no say in the matter, so the directory is
    // the only place the question can be answered.
    crate::platform::fs::create_dir_all_owner_only(dir)
        .map_err(|error| backup_path_error("backup dir", dir, &error))?;

    // `create_dir_all` is happy with an existing directory the process cannot
    // write into — exactly the bind-mount-owned-by-another-uid case — so prove
    // writability with the same kind of temp file the scheduler uses.
    let probe = temporary_path(dir, uuid::Uuid::new_v4());
    std::fs::write(&probe, b"").map_err(|error| backup_path_error("backup dir", dir, &error))?;
    discard_file("backup dir writability probe", &probe);

    // A snapshot is the whole database — argon2 password hashes, e-mail
    // addresses, issue bodies, sealed secrets — and `VACUUM INTO` writes it
    // `0644` with no say in the matter. The default `dir` is a *sibling* of
    // `[server].repo_root` rather than a child, so narrowing the repository
    // root does not narrow this; it has to be asked about on its own.
    crate::platform::fs::warn_if_others_can_reach("backup dir", dir);
    Ok(())
}

/// Refuse to schedule backups on a backend `VACUUM INTO` cannot snapshot.
///
/// Enabled-but-impossible is the failure this module exists to prevent: on
/// PostgreSQL/MySQL the loop could only fail once per interval into a warning,
/// and the instance would look backed up while it was not.
pub fn ensure_sqlite_backend(db: &DatabaseConnection) -> anyhow::Result<()> {
    refuse_non_sqlite(db.get_database_backend())
}

/// Split out from [`ensure_sqlite_backend`] so the refusal can be tested without
/// a live PostgreSQL server — `DatabaseConnection` offers no way to fabricate
/// one, and the branch that matters is the message, not the handle.
fn refuse_non_sqlite(backend: DatabaseBackend) -> anyhow::Result<()> {
    anyhow::ensure!(
        backend == DatabaseBackend::Sqlite,
        "scheduled database backups use SQLite `VACUUM INTO` and cannot run on the {backend:?} \
         backend; schedule your server's native dump tool (pg_dump / mysqldump) instead and set \
         `[backup].enabled = false`"
    );
    Ok(())
}

/// Start the background backup task, optionally wired to a graceful shutdown
/// signal. When `shutdown_rx` flips to `true` the scheduler stops at the next
/// idle point; a snapshot in flight is never interrupted mid-`VACUUM`, and the
/// final name only ever appears after a complete write (temp file + rename).
pub fn spawn_db_backup_with_shutdown(
    db: DatabaseConnection,
    config: DbBackupConfig,
    shutdown_rx: Option<watch::Receiver<bool>>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    config.validate()?;
    ensure_sqlite_backend(&db)?;
    ensure_backup_dir(&config.dir)?;

    let period = config.period();
    // Never snapshot during the first minute of a start, even when a backup is
    // overdue: the process is still opening listeners and installing the metrics
    // observers this task reports through, and a `VACUUM INTO` of the whole
    // database is not what a server should be doing while it comes up.
    let first_delay = initial_delay(&config.dir, period).max(STARTUP_GRACE);
    tracing::info!(
        next_snapshot_in_secs = first_delay.as_secs(),
        "Scheduled database backup armed"
    );

    Ok(tokio::spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        let mut interval = time::interval_at(time::Instant::now() + first_delay, period);
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = wait_optional_shutdown(&mut shutdown_rx) => {
                    tracing::info!("database backup scheduler received shutdown, stopping");
                    break;
                }
            }
            match run_backup_once(&db, &config).await {
                Ok(result) => {
                    tracing::info!(
                        path = %result.path.display(),
                        bytes = result.bytes,
                        pruned = result.pruned,
                        "database backup written"
                    );
                    if result.prune_failures > 0 {
                        tracing::warn!(
                            dir = %config.dir.display(),
                            failures = result.prune_failures,
                            "database backup rotation could not delete {} old snapshot(s); the \
                             backup directory will keep growing",
                            result.prune_failures
                        );
                    }
                    crate::metrics_hook::record_db_backup(true);
                }
                Err(error) => {
                    // Unwind the whole `anyhow` chain (`{:#}` — a bare `%error`
                    // prints only the top context) and say what the failure
                    // costs, because this warning is the only channel between a
                    // backup that stopped working and the operator.
                    tracing::warn!(
                        dir = %config.dir.display(),
                        "database backup run failed, this instance is NOT being backed up: {error:#}"
                    );
                    crate::metrics_hook::record_db_backup(false);
                }
            }
        }
    }))
}

/// How long to wait before the first snapshot.
///
/// Neither fixed answer is right on its own. Firing immediately means a
/// crash-looping server takes a snapshot on every boot and rotates the good ones
/// out within minutes; waiting a full `interval_hours` means a server that is
/// restarted more often than that never backs up at all. So the delay is derived
/// from the newest snapshot already on disk: due now if there is none or it has
/// aged past the interval, otherwise the remainder of the interval.
fn initial_delay(dir: &Path, period: Duration) -> Duration {
    match newest_snapshot_age(dir) {
        Some(age) if age < period => period - age,
        _ => Duration::ZERO,
    }
}

/// Age of the most recent snapshot in `dir`, or `None` when there is none (or
/// the filesystem will not say — an unreadable mtime is treated as "no usable
/// snapshot", which errs towards taking a backup rather than skipping one).
fn newest_snapshot_age(dir: &Path) -> Option<Duration> {
    let now = SystemTime::now();
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| is_scheduled_snapshot(&entry.file_name().to_string_lossy()))
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .filter_map(|modified| now.duration_since(modified).ok())
        .min()
}

/// Take one snapshot and rotate old ones.
pub async fn run_backup_once(
    db: &DatabaseConnection,
    config: &DbBackupConfig,
) -> anyhow::Result<BackupResult> {
    run_backup_once_with_pruner(db, config, prune_snapshots).await
}

async fn run_backup_once_with_pruner(
    db: &DatabaseConnection,
    config: &DbBackupConfig,
    prune: impl FnOnce(&Path, usize, Duration) -> (usize, usize),
) -> anyhow::Result<BackupResult> {
    config.validate()?;
    ensure_sqlite_backend(db)?;

    // Re-created on every run rather than only at startup: the directory can be
    // removed or unmounted while the server is up, and the error has to name it
    // either way.
    crate::platform::fs::create_dir_all_owner_only_async(&config.dir)
        .await
        .map_err(|error| backup_path_error("backup dir", &config.dir, &error))?;

    let snapshot_id = uuid::Uuid::new_v4();
    let filename = format!(
        "{SNAPSHOT_PREFIX}{}-{snapshot_id}{SNAPSHOT_SUFFIX}",
        Utc::now().format(SNAPSHOT_TIMESTAMP_FORMAT)
    );
    let path = config.dir.join(filename);
    let temp_path = temporary_path(&config.dir, snapshot_id);
    let temp_str = temp_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("backup path is not valid UTF-8: {}", temp_path.display()))?
        .to_owned();

    // `VACUUM INTO` a temp name and rename: the final name therefore only ever
    // exists on a complete snapshot, so a run killed halfway cannot leave a
    // truncated file that looks like a backup.
    if let Err(error) = db
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "VACUUM INTO ?",
            [temp_str.into()],
        ))
        .await
    {
        discard_file_async("partial database backup", &temp_path).await;
        return Err(anyhow::Error::new(error)
            .context(format!("SQLite VACUUM INTO {} failed", temp_path.display())));
    }

    let bytes = tokio::fs::metadata(&temp_path)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    if let Err(error) = tokio::fs::rename(&temp_path, &path).await {
        discard_file_async("partial database backup", &temp_path).await;
        return Err(backup_path_error("backup file", &path, &error));
    }

    let (pruned, prune_failures) = prune(&config.dir, config.keep_last, config.period());
    Ok(BackupResult {
        path,
        bytes,
        pruned,
        prune_failures,
    })
}

/// Delete everything but the newest `keep_last` snapshots, plus any leftover
/// temp file from a run that was killed mid-`VACUUM`.
///
/// Returns `(deleted, failed)`. Deletion failures are counted rather than
/// returned as an error: the snapshot itself is already durable, and losing that
/// good news behind a rotation error would be the worse trade. The caller warns
/// on a non-zero failure count.
fn prune_snapshots(dir: &Path, keep_last: usize, period: Duration) -> (usize, usize) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                dir = %dir.display(),
                "cannot list the backup directory to rotate old snapshots: {error}"
            );
            return (0, 1);
        }
    };

    prune_snapshot_entries(dir, entries, keep_last, period)
}

fn prune_snapshot_entries(
    dir: &Path,
    entries: impl IntoIterator<Item = std::io::Result<std::fs::DirEntry>>,
    keep_last: usize,
    period: Duration,
) -> (usize, usize) {
    let mut snapshots: Vec<PathBuf> = Vec::new();
    let mut stale_temps: Vec<PathBuf> = Vec::new();
    let mut failures = 0;
    let now = SystemTime::now();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    dir = %dir.display(),
                    error = %error,
                    "cannot read an entry in the backup directory while rotating snapshots"
                );
                failures += 1;
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_scheduled_snapshot(&name) {
            snapshots.push(entry.path());
        } else if is_temp_snapshot(&name) {
            match older_than(&entry, now, period) {
                Ok(true) => {
                    // A temp file that has outlived a whole backup interval cannot be
                    // an in-flight `VACUUM`; it is the residue of a killed run, and
                    // leaving it would grow the directory by a full database copy each
                    // time the server is killed at the wrong moment.
                    stale_temps.push(entry.path());
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        dir = %dir.display(),
                        path = %entry.path().display(),
                        error = %error,
                        "cannot inspect backup temp-file metadata while rotating snapshots"
                    );
                    failures += 1;
                }
            }
        }
    }

    // The timestamp is fixed-width and leads the name, so lexicographic
    // descending order is newest-first.
    snapshots.sort_unstable();
    snapshots.reverse();

    let doomed = snapshots.into_iter().skip(keep_last).chain(stale_temps);
    let mut pruned = 0;
    for path in doomed {
        match std::fs::remove_file(&path) {
            Ok(()) => pruned += 1,
            Err(error) => {
                tracing::warn!(path = %path.display(), "cannot delete old database backup: {error}");
                failures += 1;
            }
        }
    }
    (pruned, failures)
}

fn older_than(entry: &std::fs::DirEntry, now: SystemTime, age: Duration) -> std::io::Result<bool> {
    let modified = entry.metadata()?.modified()?;
    Ok(now
        .duration_since(modified)
        .ok()
        .is_some_and(|elapsed| elapsed > age))
}

/// Whether `name` is a snapshot *this scheduler* wrote.
///
/// Deliberately strict — both halves of the name have to parse back — because
/// this predicate is what stands between the rotation and someone else's file.
/// The deployment guide's manual recipe (`forgekeep-20260803-101500.db`) fails
/// it, and so does anything else that merely starts with `forgekeep-`.
fn is_scheduled_snapshot(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(SNAPSHOT_PREFIX)
        .and_then(|rest| rest.strip_suffix(SNAPSHOT_SUFFIX))
    else {
        return false;
    };
    let Some((timestamp, id)) = rest.split_once('-') else {
        return false;
    };
    chrono::NaiveDateTime::parse_from_str(timestamp, SNAPSHOT_TIMESTAMP_FORMAT).is_ok()
        && uuid::Uuid::parse_str(id).is_ok()
}

fn is_temp_snapshot(name: &str) -> bool {
    name.strip_prefix(".forgekeep-backup-")
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

fn temporary_path(dir: &Path, snapshot_id: uuid::Uuid) -> PathBuf {
    dir.join(format!(".forgekeep-backup-{snapshot_id}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_backup_dir, initial_delay, is_scheduled_snapshot, prune_snapshot_entries,
        prune_snapshots, run_backup_once, run_backup_once_with_pruner, DbBackupConfig,
    };
    use sea_orm::ConnectionTrait;
    use std::time::Duration;

    /// Connect a throwaway database through the production path, so the
    /// PRAGMAs (WAL, `synchronous = NORMAL`, `busy_timeout`) match the server's.
    async fn connect_test_db(db_url: &str) -> sea_orm::DatabaseConnection {
        rg_db::connect_with_pool(db_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
            .await
            .unwrap()
    }

    /// Permission bits mean nothing to uid 0, so the read-only-directory tests
    /// would see a successful write and fail for the wrong reason.
    #[cfg(unix)]
    fn running_as_root() -> bool {
        // SAFETY: `geteuid` reads the calling process's own credentials.
        unsafe { libc::geteuid() == 0 }
    }

    #[cfg(unix)]
    fn make_read_only(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555)).unwrap();
    }

    async fn seed_marker_table(db: &sea_orm::DatabaseConnection, value: &str) {
        db.execute_unprepared("CREATE TABLE IF NOT EXISTS marker (v TEXT)")
            .await
            .unwrap();
        db.execute_unprepared(&format!("INSERT INTO marker (v) VALUES ('{value}')"))
            .await
            .unwrap();
    }

    #[test]
    fn ensure_backup_dir_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("nested").join("backups");

        ensure_backup_dir(&backup_dir).unwrap();

        assert!(backup_dir.is_dir());
        // The write probe must not survive as a stray file.
        assert_eq!(std::fs::read_dir(&backup_dir).unwrap().count(), 0);
    }

    /// The failure the preflight exists for: a bind-mounted host directory owned
    /// by another uid. `create_dir_all` cannot see it, so only the write probe
    /// catches it — and the message has to carry the path, the uid diagnostic
    /// and the knob to turn the feature off.
    #[cfg(unix)]
    #[test]
    fn ensure_backup_dir_rejects_an_existing_unwritable_directory() {
        if running_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir(&backup_dir).unwrap();
        make_read_only(&backup_dir);

        let error = ensure_backup_dir(&backup_dir).unwrap_err();
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&backup_dir.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("this process runs as uid="), "{rendered}");
        assert!(rendered.contains("[backup].enabled = false"), "{rendered}");
    }

    /// A misconfigured `dir` must abort the *spawn*, not degrade into a
    /// background task that warns once a day forever.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_refuses_to_start_with_an_unwritable_backup_dir() {
        if running_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let db = connect_test_db(&db_url).await;
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir(&backup_dir).unwrap();
        make_read_only(&backup_dir);

        let error = super::spawn_db_backup_with_shutdown(
            db,
            DbBackupConfig::with_dir(backup_dir.clone()),
            None,
        )
        .unwrap_err();
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&backup_dir.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("[backup].enabled = false"), "{rendered}");
    }

    /// A snapshot has to be a *usable database*, not just a file that exists —
    /// which is the only difference between a backup and the appearance of one.
    #[tokio::test]
    async fn a_snapshot_reopens_as_a_database_with_the_same_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let db = connect_test_db(&db_url).await;
        seed_marker_table(&db, "before-backup").await;

        let config = DbBackupConfig::with_dir(dir.path().join("backups"));
        let result = run_backup_once(&db, &config).await.unwrap();

        assert!(result.path.is_file());
        assert!(result.bytes > 0);
        assert!(is_scheduled_snapshot(
            &result.path.file_name().unwrap().to_string_lossy()
        ));

        // Opened `rw`, not `rwc`: `mode=rwc` would happily create an empty file
        // if the snapshot were missing, which is the exact class of "the backup
        // succeeded" lie this test exists to rule out.
        let restored =
            connect_test_db(&format!("sqlite://{}?mode=rw", result.path.display())).await;
        let rows = restored
            .query_all(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT v FROM marker",
            ))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].try_get::<String>("", "v").unwrap(), "before-backup");
    }

    /// `keep_last` is the whole rotation contract: the N+1-th snapshot must
    /// evict the oldest, and nothing else in the directory may be touched.
    #[test]
    fn rotation_keeps_the_newest_and_spares_foreign_files() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path();
        let id = uuid::Uuid::new_v4();
        for stamp in [
            "20260101T000000",
            "20260102T000000",
            "20260103T000000",
            "20260104T000000",
        ] {
            std::fs::write(backup_dir.join(format!("forgekeep-{stamp}-{id}.db")), b"x").unwrap();
        }
        // A hand-made backup from the deployment guide's recipe, and an
        // unrelated file. Neither is ours to delete.
        std::fs::write(backup_dir.join("forgekeep-20250101-000000.db"), b"x").unwrap();
        std::fs::write(backup_dir.join("notes.txt"), b"x").unwrap();

        let (pruned, failures) = prune_snapshots(backup_dir, 2, Duration::from_secs(3_600));

        assert_eq!((pruned, failures), (2, 0));
        assert!(backup_dir
            .join(format!("forgekeep-20260104T000000-{id}.db"))
            .exists());
        assert!(backup_dir
            .join(format!("forgekeep-20260103T000000-{id}.db"))
            .exists());
        assert!(!backup_dir
            .join(format!("forgekeep-20260101T000000-{id}.db"))
            .exists());
        assert!(backup_dir.join("forgekeep-20250101-000000.db").exists());
        assert!(backup_dir.join("notes.txt").exists());
    }

    /// `ReadDir` can successfully open a directory and still fail on one child.
    /// That child must qualify the aggregate without preventing healthy entries
    /// from being rotated in the same pass.
    #[test]
    fn an_unreadable_directory_entry_is_counted_without_stopping_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path();
        let snapshot = backup_dir.join(format!(
            "forgekeep-20260101T000000-{}.db",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&snapshot, b"x").unwrap();
        let healthy_entry = std::fs::read_dir(backup_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let unreadable = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "synthetic unreadable directory entry",
        );
        let (logs, _guard) = crate::test_support::CapturedLogs::capture();

        let (pruned, failures) = prune_snapshot_entries(
            backup_dir,
            [Err(unreadable), Ok(healthy_entry)],
            0,
            Duration::from_secs(3_600),
        );

        assert_eq!((pruned, failures), (1, 1));
        assert!(!snapshot.exists());
        let rendered = logs.rendered();
        assert!(
            rendered.contains("cannot read an entry in the backup directory"),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("dir={}", backup_dir.display())),
            "{rendered}"
        );
        assert!(
            rendered.contains("error=synthetic unreadable directory entry"),
            "{rendered}"
        );
    }

    /// `DirEntry` is obtained before metadata is read. Removing that exact entry
    /// between the two operations deterministically exercises the filesystem
    /// error instead of relying on permissions (which uid 0 bypasses).
    #[test]
    fn an_unreadable_temp_mtime_is_counted_and_logged_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let backup_dir = dir.path();
        let unreadable_temp =
            backup_dir.join(format!(".forgekeep-backup-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&unreadable_temp, b"partial snapshot").unwrap();
        let entry = std::fs::read_dir(backup_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        std::fs::remove_file(&unreadable_temp).unwrap();
        let (logs, _guard) = crate::test_support::CapturedLogs::capture();

        let (pruned, failures) =
            prune_snapshot_entries(backup_dir, [Ok(entry)], 1, Duration::from_secs(3_600));

        assert_eq!((pruned, failures), (0, 1));
        let rendered = logs.rendered();
        assert!(
            rendered.contains("cannot inspect backup temp-file metadata"),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("dir={}", backup_dir.display())),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("path={}", unreadable_temp.display())),
            "{rendered}"
        );
    }

    /// Rotation is best-effort only after the new snapshot is durable. A prune
    /// failure therefore qualifies the successful result instead of hiding the
    /// usable snapshot or being reported as a clean rotation.
    #[tokio::test]
    async fn a_prune_failure_qualifies_the_successful_backup_result() {
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let db = connect_test_db(&db_url).await;
        seed_marker_table(&db, "before-backup").await;
        let config = DbBackupConfig::with_dir(dir.path().join("backups"));

        let result = run_backup_once_with_pruner(&db, &config, |backup_dir, keep_last, _| {
            assert_eq!(keep_last, config.keep_last);
            assert!(std::fs::read_dir(backup_dir).unwrap().any(|entry| {
                entry.is_ok_and(|entry| {
                    is_scheduled_snapshot(&entry.file_name().to_string_lossy())
                        && entry.path().is_file()
                })
            }));
            (0, 1)
        })
        .await
        .unwrap();

        assert!(result.path.is_file());
        assert!(result.bytes > 0);
        assert_eq!((result.pruned, result.prune_failures), (0, 1));
    }

    /// A server restarted more often than `interval_hours` must still back up
    /// (no snapshot on disk ⇒ due now), and a crash-looping one must not take a
    /// snapshot per boot and rotate the good ones out (fresh snapshot ⇒ wait).
    #[test]
    fn the_first_snapshot_is_due_from_the_directory_not_from_the_boot() {
        let dir = tempfile::tempdir().unwrap();
        let period = Duration::from_secs(24 * 3_600);

        assert_eq!(initial_delay(dir.path(), period), Duration::ZERO);

        std::fs::write(
            dir.path().join(format!(
                "forgekeep-20260101T000000-{}.db",
                uuid::Uuid::new_v4()
            )),
            b"x",
        )
        .unwrap();
        let delay = initial_delay(dir.path(), period);
        assert!(delay > Duration::ZERO, "{delay:?}");
        assert!(delay <= period, "{delay:?}");
    }

    /// `VACUUM INTO` cannot snapshot a PostgreSQL/MySQL instance, so
    /// `[backup].enabled = true` there has to fail the start rather than warn
    /// once a day while the instance looks backed up.
    #[test]
    fn a_non_sqlite_backend_is_refused_by_name() {
        for backend in [
            sea_orm::DatabaseBackend::Postgres,
            sea_orm::DatabaseBackend::MySql,
        ] {
            let error = super::refuse_non_sqlite(backend).unwrap_err();
            let rendered = format!("{error:#}");
            assert!(rendered.contains(&format!("{backend:?}")), "{rendered}");
            assert!(rendered.contains("pg_dump"), "{rendered}");
            assert!(rendered.contains("[backup].enabled = false"), "{rendered}");
        }
        super::refuse_non_sqlite(sea_orm::DatabaseBackend::Sqlite).unwrap();
    }

    #[test]
    fn zero_valued_knobs_are_refused_instead_of_silently_disabling_backups() {
        let mut config = DbBackupConfig::with_dir(std::path::PathBuf::from("/tmp"));
        config.keep_last = 0;
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("keep_last"));

        let mut config = DbBackupConfig::with_dir(std::path::PathBuf::from("/tmp"));
        config.interval_hours = 0;
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("interval_hours"));
    }
}
