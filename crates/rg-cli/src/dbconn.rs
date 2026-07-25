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

/// Connect to `db_url`, annotating an unwritable-directory SQLite failure.
pub(crate) async fn connect(db_url: &str) -> anyhow::Result<DatabaseConnection> {
    rg_db::connect(db_url)
        .await
        .map_err(|e| annotate_db_open_error(e, db_url))
}

/// [`connect`] with the `[timeouts]` connect/idle budget the server configures.
pub(crate) async fn connect_with_timeouts(
    db_url: &str,
    connect_secs: u64,
    idle_secs: u64,
) -> anyhow::Result<DatabaseConnection> {
    rg_db::connect_with_timeouts(db_url, connect_secs, idle_secs)
        .await
        .map_err(|e| annotate_db_open_error(e, db_url))
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

/// True when the process can create a file in `dir` right now.
fn dir_is_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".forgekeep_db_write_test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            // Best-effort cleanup: failing to remove the probe says nothing
            // about writability, which is all this function measures — but a
            // leftover file is worth a line in the log rather than nothing.
            if let Err(e) = std::fs::remove_file(&probe) {
                tracing::debug!(probe = %probe.display(), error = %e, "failed to remove the database write-probe file");
            }
            true
        }
        Err(_) => false,
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
fn annotate_db_open_error(error: anyhow::Error, db_url: &str) -> anyhow::Error {
    let Some(path) = sqlite_file_path(db_url) else {
        return error;
    };
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    if dir_is_writable(dir) {
        return error;
    }

    let mut context = format!(
        "SQLite database `{}` could not be opened: `{}` is not writable by the server, and SQLite \
         needs to create the `-wal` / `-shm` sidecar files next to the database",
        path.display(),
        dir.display()
    );
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
