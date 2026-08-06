//! The one place any `forgekeep` subcommand opens the database.
//!
//! `rg_db::connect` reports a failed SQLite open as the bare string `unable to
//! open database file` — no path, no reason — and an unwritable `/data`
//! bind-mount is the single most likely first-boot failure of a container. The
//! diagnostic that turns that into an actionable message used to live inside
//! `serve.rs` and therefore only helped `forgekeep serve`; every other
//! subcommand (`migrate`, `rebuild-fts`, `import`, `index-repo`, `backup-db`,
//! `package list`) failed opaquely on exactly the same misconfiguration. Route
//! all of them through here instead.

use std::path::PathBuf;

use rg_db::DatabaseConnection;

/// A database connection paired with the process lease that makes opening it
/// safe for an offline-only SQLite role.
pub(crate) struct GuardedDatabaseConnection {
    connection: DatabaseConnection,
    _process_guard: Option<rg_db::sqlite_process_guard::SqliteProcessGuard>,
}

impl GuardedDatabaseConnection {
    pub(crate) fn connection(&self) -> &DatabaseConnection {
        &self.connection
    }
}

/// Connect to `db_url`, annotating an unwritable-directory SQLite failure.
pub(crate) async fn connect(db_url: &str) -> anyhow::Result<DatabaseConnection> {
    rg_db::connect(db_url)
        .await
        .map_err(|e| annotate_db_open_error(e, db_url))
}

/// [`connect`] with the `[timeouts]` connect/idle budget the server configures.
pub(crate) async fn connect_server_with_timeouts(
    db_url: &str,
    connect_secs: u64,
    idle_secs: u64,
) -> anyhow::Result<GuardedDatabaseConnection> {
    let process_guard = rg_db::sqlite_process_guard::acquire_server(db_url)?;
    let connection = rg_db::connect_with_timeouts(db_url, connect_secs, idle_secs)
        .await
        .map_err(|e| annotate_db_open_error(e, db_url))?;
    Ok(GuardedDatabaseConnection {
        connection,
        _process_guard: process_guard,
    })
}

/// Open a standalone CLI pool that may apply migrations, after proving a
/// file-backed SQLite server is not alive against the same database.
pub(crate) async fn connect_offline_migration(
    db_url: &str,
) -> anyhow::Result<GuardedDatabaseConnection> {
    let process_guard = rg_db::sqlite_process_guard::acquire_migration(db_url)?;
    let connection = rg_db::connect(db_url)
        .await
        .map_err(|e| annotate_db_open_error(e, db_url))?;
    Ok(GuardedDatabaseConnection {
        connection,
        _process_guard: process_guard,
    })
}

/// Extract the on-disk file a SQLite URL points at, or `None` for a
/// non-SQLite/in-memory URL. Used only to turn an opaque "unable to open
/// database file" into a message naming the directory that has to be writable.
fn sqlite_file_path(db_url: &str) -> Option<PathBuf> {
    let rest = db_url
        .strip_prefix("sqlite://")
        .or_else(|| db_url.strip_prefix("sqlite:"))?;
    let path = rest.split('?').next().unwrap_or("");
    if path.is_empty() || path == ":memory:" {
        return None;
    }
    Some(PathBuf::from(path))
}

/// What a write probe actually established about a directory.
///
/// The point of the enum is the last variant. A `bool` had no room for "the
/// probe could not be carried out", so every `ENOTDIR`, `ENOSPC` and `EIO`
/// answered the question it never got to ask — and the caller told the operator
/// to fix permissions on a directory whose real problem was the device, the
/// disk, or the path not being a directory at all.
enum DirWriteProbe {
    /// A file was created there and removed again.
    Writable,
    /// The filesystem refused access. This is the case the diagnostic exists
    /// for: a `/data` bind-mount owned by the host uid.
    Denied,
    /// The directory itself is not there.
    Missing,
    /// The probe failed for some other reason, so nothing was established
    /// about writability.
    Inconclusive(std::io::Error),
}

/// Try to create (and remove) a file in `dir`, reporting what that proved.
fn probe_dir_writable(dir: &std::path::Path) -> DirWriteProbe {
    let probe = dir.join(".forgekeep_db_write_test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            // Best-effort cleanup: failing to remove the probe says nothing
            // about writability, which is all this function measures — but a
            // leftover file is worth a line in the log rather than nothing.
            if let Err(e) = std::fs::remove_file(&probe) {
                tracing::debug!(probe = %probe.display(), error = %e, "failed to remove the database write-probe file");
            }
            DirWriteProbe::Writable
        }
        Err(error) => match error.kind() {
            std::io::ErrorKind::PermissionDenied => DirWriteProbe::Denied,
            std::io::ErrorKind::NotFound => DirWriteProbe::Missing,
            _ => DirWriteProbe::Inconclusive(error),
        },
    }
}

/// Attach the uid/ownership diagnostic to a SQLite connection failure caused by
/// an unwritable data directory.
///
/// SQLite reports that case as `unable to open database file` with no path and
/// no reason, and it is the single most likely first-boot failure of a
/// container whose `/data` bind-mount is owned by the host uid: the WAL and
/// `-shm` sidecar files need write access to the *directory*, not just the
/// database file. Non-permission failures (corrupt file, bad URL, Postgres,
/// MySQL) are returned untouched.
///
/// Only a *proven* verdict is annotated. A probe that could not run leaves the
/// SQLite error exactly as it was: an operator sent to `chown` a directory
/// whose real problem is a full disk or a path that is not a directory loses
/// more time than one who got no hint at all.
fn annotate_db_open_error(error: anyhow::Error, db_url: &str) -> anyhow::Error {
    let Some(path) = sqlite_file_path(db_url) else {
        return error;
    };
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));

    let mut context = match probe_dir_writable(dir) {
        DirWriteProbe::Writable => return error,
        DirWriteProbe::Inconclusive(probe_error) => {
            tracing::debug!(
                dir = %dir.display(),
                error = %probe_error,
                "could not establish whether the database directory is writable; \
                 leaving the SQLite error unannotated"
            );
            return error;
        }
        DirWriteProbe::Missing => format!(
            "SQLite database `{}` could not be opened: its directory `{}` does not exist — create \
             it (or bind-mount it) before starting the server",
            path.display(),
            dir.display()
        ),
        DirWriteProbe::Denied => format!(
            "SQLite database `{}` could not be opened: `{}` is not writable by the server, and \
             SQLite needs to create the `-wal` / `-shm` sidecar files next to the database",
            path.display(),
            dir.display()
        ),
    };
    if let Some(hint) = rg_core::platform::fs::ownership_hint(dir) {
        context.push_str(&format!("\n  hint: {hint}"));
    }
    error.context(context)
}

#[cfg(test)]
mod tests {
    #[test]
    fn sqlite_urls_resolve_to_the_file_the_directory_check_needs() {
        use std::path::PathBuf;

        assert_eq!(
            super::sqlite_file_path("sqlite:///data/forgekeep.db?mode=rwc"),
            Some(PathBuf::from("/data/forgekeep.db"))
        );
        assert_eq!(
            super::sqlite_file_path("sqlite://./forgekeep.db"),
            Some(PathBuf::from("./forgekeep.db"))
        );
        assert_eq!(super::sqlite_file_path("sqlite::memory:"), None);
        assert_eq!(
            super::sqlite_file_path("postgres://user:pw@localhost/forgekeep"),
            None
        );
    }

    #[tokio::test]
    async fn server_connection_holds_the_sqlite_lease_for_its_lifetime() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/forgekeep.db?mode=rwc", dir.path().display());
        let server = super::connect_server_with_timeouts(&url, 10, 60)
            .await
            .unwrap();

        let message = format!(
            "{:#}",
            super::connect_offline_migration(&url)
                .await
                .err()
                .expect("a live server connection must exclude the migrator")
        );
        assert!(message.contains("server to be stopped"), "{message}");

        drop(server);
        let migration = super::connect_offline_migration(&url).await.unwrap();
        drop(migration);
    }

    /// The `/data` bind-mount case: the directory is unwritable, so the opaque
    /// SQLite failure gains the path plus the uid to `chown` to.
    #[cfg(unix)]
    #[test]
    fn unwritable_sqlite_directory_is_named_in_the_connect_error() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return; // root writes through the mode bits
        }
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o500)).unwrap();
        let url = format!("sqlite://{}/forgekeep.db?mode=rwc", data.display());

        let annotated = format!(
            "{:#}",
            super::annotate_db_open_error(anyhow::anyhow!("unable to open database file"), &url)
        );

        assert!(
            annotated.contains("unable to open database file"),
            "{annotated}"
        );
        assert!(annotated.contains("is not writable"), "{annotated}");
        assert!(
            annotated.contains("this process runs as uid="),
            "{annotated}"
        );
        assert!(
            annotated.contains("chmod") || annotated.contains("chown"),
            "{annotated}"
        );

        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The probe's own failure is not a verdict. Here the "directory" is a
    /// regular file, so the write fails with `ENOTDIR` — nothing whatsoever was
    /// established about permissions, and claiming otherwise sends the operator
    /// to `chown` a path that needs a different fix entirely.
    #[test]
    fn a_probe_that_could_not_run_is_not_reported_as_a_permission_problem() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"i am a file").unwrap();
        let url = format!("sqlite://{}/data/forgekeep.db?mode=rwc", blocker.display());

        let annotated = format!(
            "{:#}",
            super::annotate_db_open_error(anyhow::anyhow!("unable to open database file"), &url)
        );

        assert_eq!(annotated, "unable to open database file");
    }

    /// A directory that is simply not there needs `mkdir`, not `chmod`. Both
    /// used to arrive as "is not writable by the server".
    #[test]
    fn a_missing_directory_is_named_as_missing_rather_than_unwritable() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");
        let url = format!("sqlite://{}/forgekeep.db?mode=rwc", missing.display());

        let annotated = format!(
            "{:#}",
            super::annotate_db_open_error(anyhow::anyhow!("unable to open database file"), &url)
        );

        assert!(
            annotated.contains("unable to open database file"),
            "{annotated}"
        );
        assert!(annotated.contains("does not exist"), "{annotated}");
        assert!(!annotated.contains("is not writable"), "{annotated}");
    }

    /// A corrupt database or a bad URL must not be blamed on permissions.
    #[test]
    fn writable_sqlite_directory_leaves_the_connect_error_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/forgekeep.db?mode=rwc", dir.path().display());

        let annotated = format!(
            "{:#}",
            super::annotate_db_open_error(anyhow::anyhow!("file is not a database"), &url)
        );

        assert_eq!(annotated, "file is not a database");
    }
}
