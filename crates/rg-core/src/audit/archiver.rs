//! Audit log archival — periodically exports old audit logs to compressed NDJSON
//! files and purges them from the database only after durable file creation.

use crate::platform::fs::{discard_file, discard_file_async};
use crate::task_tracker::wait_optional_shutdown;
use chrono::{Duration, Utc};
use sea_orm::DatabaseConnection;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tokio::sync::watch;
use tokio::time;

/// Days a row stays in the database before it is archived, without a configured
/// `[audit].archive_after_days`.
///
/// Named here rather than written into the `unwrap_or` at the resolution site
/// for the reason `[mirror]` already is: a number that exists only inside one
/// `unwrap_or` is a number no contract can reach, so nothing ties it to the
/// `archive_after_days = 90` line `plombir-git.example.toml` shows the operator.
pub const DEFAULT_ARCHIVE_AFTER_DAYS: i64 = 90;

/// Minutes between archival passes, without a configured
/// `[audit].interval_minutes`.
pub const DEFAULT_INTERVAL_MINUTES: u64 = 60;

/// Rows exported per pass, without a configured `[audit].batch_size`.
pub const DEFAULT_BATCH_SIZE: u64 = 1_000;

#[derive(Clone, Debug)]
pub struct AuditArchiveConfig {
    pub archive_dir: PathBuf,
    pub archive_after_days: i64,
    pub interval_minutes: u64,
    pub batch_size: u64,
}

impl AuditArchiveConfig {
    pub fn with_archive_dir(archive_dir: PathBuf) -> Self {
        Self {
            archive_dir,
            archive_after_days: DEFAULT_ARCHIVE_AFTER_DAYS,
            interval_minutes: DEFAULT_INTERVAL_MINUTES,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.archive_after_days > 0,
            "archive_after_days must be positive"
        );
        anyhow::ensure!(
            self.interval_minutes > 0,
            "interval_minutes must be positive"
        );
        anyhow::ensure!(self.batch_size > 0, "batch_size must be positive");
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ArchiveResult {
    pub path: PathBuf,
    pub count: usize,
}

/// Remediation appended to every `archive_dir` failure. The escape hatch is
/// named on purpose: an operator who does not want audit archival at all should
/// turn it off explicitly rather than leave a directory the server cannot use.
const ARCHIVE_DIR_HINT: &str =
    "point `[audit].archive_dir` at a directory the server can create and write into, or set \
     `[audit].enabled = false` to turn audit-log archival off";

/// One actionable line for any filesystem failure on the archive path.
///
/// [`describe_path_error`](crate::platform::fs::describe_path_error) trades the
/// caller's remedy for the uid diagnostic on a permission error; here both
/// matter. A permission failure now refuses the whole server start, so the
/// operator has to be told which knob to point elsewhere — or how to turn
/// archival off — and not just which directory to `chown`.
fn archive_path_error(what: &str, path: &Path, error: &std::io::Error) -> anyhow::Error {
    let described = crate::platform::fs::describe_path_error(what, path, error, "");
    anyhow::anyhow!("{described}\n  hint: {ARCHIVE_DIR_HINT}")
}

/// Create `archive_dir` and prove it is writable — at startup, not an hour later.
///
/// Without this the first filesystem contact happens inside the archiver loop,
/// once enough rows have aged past the retention cutoff, and its only failure
/// channel is a warning nobody reads. An `archive_dir` the server cannot write
/// (the usual case: a bind-mounted host directory owned by another uid) would
/// therefore mean the audit log is never trimmed, and the operator finds out
/// from a full disk rather than from the server.
pub fn ensure_archive_dir(archive_dir: &Path) -> anyhow::Result<()> {
    // Owner-only, not `create_dir_all`: this directory does not exist yet on a
    // first start, and a directory the server itself creates has no reason to
    // inherit the `umask` of whoever started it — the warning below is for the
    // directory an operator made, not for one made a syscall ago.
    crate::platform::fs::create_dir_all_owner_only(archive_dir)
        .map_err(|error| archive_path_error("audit archive_dir", archive_dir, &error))?;

    // `create_dir_all` is happy with an existing directory the process cannot
    // write into — which is exactly the bind-mount-owned-by-another-uid case —
    // so prove writability with the same kind of temp file the archiver uses.
    let probe = temporary_path(archive_dir, uuid::Uuid::new_v4());
    std::fs::write(&probe, b"")
        .map_err(|error| archive_path_error("audit archive_dir", archive_dir, &error))?;
    discard_file("audit archive_dir writability probe", &probe);

    // The archive is the audit log itself — actor, IP address and target of
    // every administrative action — moved out of the database and onto disk.
    // Like `[backup].dir` it defaults to a *sibling* of `[server].repo_root`,
    // so it is not covered by the mode of the repository root.
    crate::platform::fs::warn_if_others_can_reach("audit archive_dir", archive_dir);
    Ok(())
}

/// Start the background audit archive task, optionally wired to a graceful
/// shutdown signal. When `shutdown_rx` flips to `true`, the archiver stops at
/// the next idle point rather than being aborted mid-run — its writes are
/// already atomic (temp file + rename, DB purge only after durable write), so
/// there is nothing to flush, only a clean loop exit.
pub fn spawn_archiver_with_shutdown(
    db: DatabaseConnection,
    config: AuditArchiveConfig,
    shutdown_rx: Option<watch::Receiver<bool>>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    config.validate()?;
    ensure_archive_dir(&config.archive_dir)?;
    Ok(tokio::spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        let mut interval = time::interval(time::Duration::from_secs(
            config.interval_minutes.saturating_mul(60),
        ));
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = wait_optional_shutdown(&mut shutdown_rx) => {
                    tracing::info!("audit log archiver received shutdown, stopping");
                    break;
                }
            }
            loop {
                match run_archive_once(&db, &config).await {
                    Ok(Some(result)) => {
                        tracing::info!(
                            count = result.count,
                            path = %result.path.display(),
                            "archived audit logs"
                        );
                        if result.count < config.batch_size as usize {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        // Name the directory and unwind the whole `anyhow`
                        // chain (`{:#}` — a bare `%error` prints only the top
                        // context), and say what the failure costs: an
                        // archiver that never succeeds means the audit log
                        // grows unbounded.
                        tracing::warn!(
                            archive_dir = %config.archive_dir.display(),
                            "audit log archive run failed, audit rows are not being trimmed: {error:#}"
                        );
                        break;
                    }
                }
            }
        }
    }))
}

/// Archive one bounded batch. Returns `None` when no eligible entries exist.
pub async fn run_archive_once(
    db: &DatabaseConnection,
    config: &AuditArchiveConfig,
) -> anyhow::Result<Option<ArchiveResult>> {
    config.validate()?;

    // Before anything else, and unconditionally: a run that finds nothing to
    // archive is exactly when a spool a killed run left behind has been sitting
    // longest. See `prune_stale_spools`.
    prune_stale_spools(&config.archive_dir).await;

    let cutoff = Utc::now() - Duration::days(config.archive_after_days);
    let old_entries =
        rg_db::ops::audit_log_ops::list_before_limit(db, cutoff, config.batch_size).await?;
    if old_entries.is_empty() {
        return Ok(None);
    }

    // Re-created on every run rather than only at startup: the directory can be
    // removed or unmounted while the server is up, and the error has to name it
    // either way.
    crate::platform::fs::create_dir_all_owner_only_async(&config.archive_dir)
        .await
        .map_err(|error| archive_path_error("audit archive_dir", &config.archive_dir, &error))?;
    let archive_id = uuid::Uuid::new_v4();
    let filename = format!(
        "audit-{}-{}.ndjson.zst",
        Utc::now().format("%Y%m%dT%H%M%S"),
        archive_id
    );
    let path = config.archive_dir.join(filename);
    let temp_path = temporary_path(&config.archive_dir, archive_id);

    let mut ndjson = Vec::new();
    for entry in &old_entries {
        serde_json::to_writer(&mut ndjson, entry)?;
        ndjson.push(b'\n');
    }
    let compressed =
        tokio::task::spawn_blocking(move || zstd::stream::encode_all(Cursor::new(ndjson), 3))
            .await??;

    if let Err(error) = write_archive_atomically(&temp_path, &path, &compressed).await {
        discard_file_async("partial audit archive", &temp_path).await;
        return Err(archive_path_error("audit archive file", &path, &error));
    }

    let ids = old_entries.iter().map(|entry| entry.id).collect::<Vec<_>>();
    rg_db::ops::audit_log_ops::delete_by_ids(db, &ids).await?;

    Ok(Some(ArchiveResult {
        path,
        count: old_entries.len(),
    }))
}

/// Retire archive spools that a stop running no destructors left behind.
///
/// The archiver publishes with a same-directory rename, so its spool is retired
/// by the error path of the run that created it — which a `SIGKILL`, the OOM
/// killer and a container restart all skip. This is the reverse arc, and it
/// belongs to the archiver rather than to the startup sweep in
/// `rg_core::staging` for the same reason `backup::prune_snapshots` belongs to
/// the backup scheduler: `[audit].archive_dir` is configured separately from
/// `repo_root` and defaults to a *sibling* of it, so nothing else knows where
/// to look.
///
/// Best-effort throughout — a directory that is not there yet is not a problem,
/// and a spool that cannot be removed is a warning, never a reason to skip the
/// archiving this run was called for.
async fn prune_stale_spools(archive_dir: &Path) {
    let report =
        crate::staging::sweep_stale_sibling_spools(archive_dir, crate::staging::STALE_SPOOL_AGE)
            .await;
    if report != crate::staging::SweepReport::default() {
        tracing::info!(
            dir = %archive_dir.display(),
            removed = report.removed,
            retained = report.retained,
            failed = report.failed,
            "swept audit archive spools left behind by a previous run"
        );
    }
}

fn temporary_path(archive_dir: &Path, archive_id: uuid::Uuid) -> PathBuf {
    archive_dir.join(crate::staging::audit_archive_spool_name(archive_id))
}

/// Returns the raw [`std::io::Error`] rather than an `anyhow::Error` so the
/// caller can hand it to `describe_path_error`, which keys the uid/ownership
/// diagnostic off `ErrorKind::PermissionDenied`.
async fn write_archive_atomically(
    temp_path: &Path,
    path: &Path,
    data: &[u8],
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    // The archive is who did what from which IP, so it is born owner-only
    // rather than narrowed a statement later: `File::create` takes the ambient
    // umask, and a crash between that and a `chmod` would leave the record
    // world-readable for good. This file is ours to open, so the mode goes on
    // the open (card_d55ad81ab8bb).
    let mut file = crate::platform::fs::create_new_owner_only_async(temp_path).await?;
    file.write_all(data).await?;
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(temp_path, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ensure_archive_dir, run_archive_once, AuditArchiveConfig};
    use chrono::{Duration, Utc};
    use sea_orm::{NotSet, Set};
    use std::io::Cursor;

    /// Connect a throwaway database through the production path.
    ///
    /// `rg_db::connect_with_pool` applies the PRAGMAs the server runs with
    /// (WAL journalling, `synchronous = NORMAL`, `busy_timeout`). A bare
    /// `Database::connect` would leave sqlx's defaults in place —
    /// `journal_mode = DELETE` and `synchronous = FULL` — which is both a
    /// different configuration from production and an fsync on each of the
    /// ~75 migration commits.
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

    #[test]
    fn ensure_archive_dir_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("nested").join("audit-archive");

        ensure_archive_dir(&archive_dir).unwrap();

        assert!(archive_dir.is_dir());
        // The write probe must not survive as a stray file in the archive dir.
        assert_eq!(std::fs::read_dir(&archive_dir).unwrap().count(), 0);
    }

    /// The failure this whole preflight exists for: a bind-mounted host
    /// directory owned by another uid. `create_dir_all` cannot see it (the
    /// directory already exists), so only the write probe catches it — and the
    /// message has to carry the path and the uid diagnostic, because the
    /// alternative is an hourly warning nobody reads.
    #[cfg(unix)]
    #[test]
    fn ensure_archive_dir_rejects_an_existing_unwritable_directory() {
        if running_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("audit-archive");
        std::fs::create_dir(&archive_dir).unwrap();
        make_read_only(&archive_dir);

        let error = ensure_archive_dir(&archive_dir).unwrap_err();
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&archive_dir.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("this process runs as uid="), "{rendered}");
        assert!(rendered.contains("[audit].enabled = false"), "{rendered}");
    }

    /// The same failure one level up: the directory does not exist yet and its
    /// parent denies creation.
    #[cfg(unix)]
    #[test]
    fn ensure_archive_dir_rejects_an_uncreatable_directory() {
        if running_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("data");
        std::fs::create_dir(&parent).unwrap();
        make_read_only(&parent);
        let archive_dir = parent.join("audit-archive");

        let error = ensure_archive_dir(&archive_dir).unwrap_err();
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&archive_dir.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("this process runs as uid="), "{rendered}");
    }

    /// A misconfigured `archive_dir` must abort the *spawn*, not degrade into a
    /// background task that warns once an hour forever.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_refuses_to_start_with_an_unwritable_archive_dir() {
        if running_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let db = connect_test_db(&db_url).await;
        let archive_dir = dir.path().join("audit-archive");
        std::fs::create_dir(&archive_dir).unwrap();
        make_read_only(&archive_dir);

        let error = super::spawn_archiver_with_shutdown(
            db,
            AuditArchiveConfig::with_archive_dir(archive_dir.clone()),
            None,
        )
        .unwrap_err();
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&archive_dir.display().to_string()),
            "{rendered}"
        );
    }

    /// The archive directory can lose write access while the server is up (a
    /// remount, a chown, a full-disk-triggered permission change), and that
    /// failure reaches the operator only through the loop's warning — so the
    /// error itself has to name the directory rather than say `os error 13`.
    #[cfg(unix)]
    #[tokio::test]
    async fn run_archive_once_names_the_directory_when_the_write_fails() {
        if running_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let db = connect_test_db(&db_url).await;
        rg_db::run_migrations(&db).await.unwrap();
        insert_audit_row(&db, "old.action", Utc::now() - Duration::days(91)).await;

        let archive_dir = dir.path().join("audit-archive");
        std::fs::create_dir(&archive_dir).unwrap();
        make_read_only(&archive_dir);
        let config = AuditArchiveConfig::with_archive_dir(archive_dir.clone());

        let error = run_archive_once(&db, &config).await.unwrap_err();
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&archive_dir.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("this process runs as uid="), "{rendered}");

        // The row must still be there: nothing is purged unless the archive
        // was written durably.
        let remaining = rg_db::ops::audit_log_ops::list_before_limit(&db, Utc::now(), 100)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
    }

    /// An audit archive is who did what from which IP, so it must not be born
    /// at the `umask` of whoever started the server.
    ///
    /// Same shape as the backup snapshot's twin, and the same reason the
    /// directory policy cannot answer it: an `[audit].archive_dir` an operator
    /// created keeps the mode they chose, and a world-readable archive inside a
    /// `0755` directory is the exposure. Here the file is ours to open, so the
    /// mode goes on the open (card_d55ad81ab8bb).
    #[cfg(unix)]
    #[tokio::test]
    async fn an_archive_is_owner_only_whatever_the_umask_was() {
        use std::os::unix::fs::PermissionsExt;

        let _umask = crate::platform::fs::test_umask::WideUmask::hold();
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let db = connect_test_db(&db_url).await;
        rg_db::run_migrations(&db).await.unwrap();
        insert_audit_row(&db, "old.action", Utc::now() - Duration::days(91)).await;

        let archive_dir = dir.path().join("operator-archive");
        std::fs::create_dir(&archive_dir).unwrap();
        std::fs::set_permissions(&archive_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result = run_archive_once(
            &db,
            &AuditArchiveConfig::with_archive_dir(archive_dir.clone()),
        )
        .await
        .unwrap()
        .unwrap();

        let mode = std::fs::metadata(&result.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            mode, 0o600,
            "the archive was written {mode:04o}, so every other local account on the host can \
             read who did what from which IP"
        );
        assert_eq!(
            std::fs::metadata(&archive_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o755,
            "an operator's directory must not be narrowed underneath them"
        );
    }

    async fn insert_audit_row(
        db: &sea_orm::DatabaseConnection,
        action: &str,
        created_at: chrono::DateTime<Utc>,
    ) {
        rg_db::ops::audit_log_ops::insert(
            db,
            rg_db::entities::audit_log::ActiveModel {
                id: NotSet,
                user_id: Set(None),
                username: Set(None),
                action: Set(action.to_string()),
                resource_type: Set(None),
                resource_id: Set(None),
                resource_name: Set(None),
                ip_address: Set(None),
                user_agent: Set(None),
                details: Set(None),
                created_at: Set(created_at),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn archives_only_expired_rows_as_compressed_ndjson() {
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let db = connect_test_db(&db_url).await;
        rg_db::run_migrations(&db).await.unwrap();

        for (action, created_at) in [
            ("old.action.1", Utc::now() - Duration::days(92)),
            ("old.action.2", Utc::now() - Duration::days(91)),
            ("new.action", Utc::now() - Duration::days(1)),
        ] {
            rg_db::ops::audit_log_ops::insert(
                &db,
                rg_db::entities::audit_log::ActiveModel {
                    id: NotSet,
                    user_id: Set(None),
                    username: Set(None),
                    action: Set(action.to_string()),
                    resource_type: Set(None),
                    resource_id: Set(None),
                    resource_name: Set(None),
                    ip_address: Set(None),
                    user_agent: Set(None),
                    details: Set(None),
                    created_at: Set(created_at),
                },
            )
            .await
            .unwrap();
        }

        let config = AuditArchiveConfig {
            archive_dir: dir.path().join("archive"),
            archive_after_days: 90,
            interval_minutes: 60,
            batch_size: 1,
        };
        let first = run_archive_once(&db, &config).await.unwrap().unwrap();
        let second = run_archive_once(&db, &config).await.unwrap().unwrap();
        assert_eq!(first.count, 1);
        assert_eq!(second.count, 1);
        assert_ne!(first.path, second.path);
        assert!(first.path.extension().is_some_and(|ext| ext == "zst"));
        let mut archived_actions = Vec::new();
        for result in [first, second] {
            let compressed = tokio::fs::read(&result.path).await.unwrap();
            let decoded = zstd::stream::decode_all(Cursor::new(compressed)).unwrap();
            let line: serde_json::Value = serde_json::from_slice(decoded.trim_ascii()).unwrap();
            archived_actions.push(line["action"].as_str().unwrap().to_string());
        }
        assert_eq!(archived_actions, ["old.action.1", "old.action.2"]);

        let remaining =
            rg_db::ops::audit_log_ops::list_before_limit(&db, Utc::now() + Duration::days(1), 100)
                .await
                .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].action, "new.action");
        assert!(run_archive_once(&db, &config).await.unwrap().is_none());
    }

    /// The reverse arc for the archiver's own spool: a `SIGKILL` between
    /// creating `.audit-<id>.tmp` and renaming it runs no destructor, and until
    /// this prune existed nothing ever read that directory looking for one.
    ///
    /// The run below archives nothing — which is the case that matters, because
    /// a quiet instance is where a leaked spool sits longest, and the prune has
    /// to happen before the "no eligible entries" early return rather than
    /// after it.
    ///
    /// The second half is what makes the first mean anything: a spool young
    /// enough to belong to a run in flight beside this one stays, and so does
    /// the finished archive next to it.
    #[tokio::test]
    async fn a_stale_archive_spool_is_pruned_and_a_fresh_one_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let db = connect_test_db(&db_url).await;
        rg_db::run_migrations(&db).await.unwrap();
        let archive_dir = dir.path().join("audit-archive");
        std::fs::create_dir_all(&archive_dir).unwrap();
        let config = AuditArchiveConfig::with_archive_dir(archive_dir.clone());

        let stale = super::temporary_path(&archive_dir, uuid::Uuid::new_v4());
        let fresh = super::temporary_path(&archive_dir, uuid::Uuid::new_v4());
        let finished = archive_dir.join("audit-20260908T101500-done.ndjson.zst");
        for path in [&stale, &fresh, &finished] {
            std::fs::write(path, b"zstd bytes").unwrap();
        }
        let backdated = std::time::SystemTime::now()
            - (crate::staging::STALE_SPOOL_AGE + std::time::Duration::from_secs(60));
        for path in [&stale, &finished] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(backdated))
                .unwrap();
        }

        assert!(
            run_archive_once(&db, &config).await.unwrap().is_none(),
            "there is nothing to archive — the prune must not depend on there being something"
        );

        assert!(
            !stale.exists(),
            "the spool of a killed archive run survived the next run"
        );
        assert!(
            fresh.exists(),
            "a spool young enough to belong to a run in flight was deleted"
        );
        assert!(finished.exists(), "a finished archive was deleted");
    }
}
