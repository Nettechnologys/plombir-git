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

/// What a subcommand does when the file-backed SQLite database it resolved is
/// not there.
#[derive(Clone, Copy)]
pub(crate) enum MissingDatabase {
    /// Create it, after saying so. Reserved for the two commands whose job is
    /// to bring an instance into existence: `serve` on first boot and `migrate`
    /// on install.
    Create,
    /// Refuse before opening anything. Every other subcommand operates on an
    /// instance that already exists, so a database that is not there is a
    /// mistake about *which* database — never an invitation to make one.
    Refuse,
}

/// Why a one-shot command may open an ordinary pool against a live instance,
/// instead of taking one of the leases above.
///
/// The vocabulary is closed on purpose. Every command here holds SQLite's
/// single write lock for *something*, and the question "for how long, and does
/// a user write meet it" has to be answered per command rather than defaulted.
/// It was defaulted once already: `card_e069d60b4142` recognised the shape and
/// fixed `migrate`, nobody swept the neighbours, and `rebuild-fts` and
/// `rotate-encryption-key` sat on an ordinary pool for months afterwards —
/// found sideways, not by a gate (card_74c8b8754e97).
///
/// So the reason is a parameter rather than a comment: a new subcommand cannot
/// reach an unleased pool without naming which of these answers applies to it,
/// and adding a fourth answer is an edit to this enum that a reviewer reads.
/// The comment version of this rule was the one that failed — prose next to a
/// call site can say anything, and nothing rereads it when a command is copied.
#[derive(Clone, Copy)]
pub(crate) enum OnlineAccess {
    /// The command writes a single row through one statement. That is an
    /// ordinary write, not a whole-database pass, and demanding a stopped
    /// server for it would be a contract with nothing behind it.
    SingleRowWrite,
    /// The command runs exactly what a live HTTP handler runs on request. A
    /// contract requiring a stopped server would forbid from the CLI what the
    /// server itself does on demand, and would say nothing about the hazard,
    /// since the endpoint stays there either way.
    SameWorkAsALiveHandler,
    /// The command never asks for the source's write lock at all — it reads the
    /// database and writes somewhere else.
    NoWriteLockOnTheSource,
}

impl OnlineAccess {
    /// Logged when the pool is opened, so the claim is visible to whoever is
    /// looking at a contended instance rather than only to whoever reads
    /// `dbconn.rs`.
    fn as_str(self) -> &'static str {
        match self {
            Self::SingleRowWrite => "writes a single row",
            Self::SameWorkAsALiveHandler => "same work as a live handler",
            Self::NoWriteLockOnTheSource => "takes no write lock on the source",
        }
    }
}

/// Connect to an existing `db_url` on an ordinary pool, annotating an
/// unwritable-directory SQLite failure.
///
/// The opener with no lease, and `access` is what keeps that from being the
/// path of least resistance — see [`OnlineAccess`].
pub(crate) async fn connect_online(
    db_url: &str,
    operation: &str,
    access: OnlineAccess,
) -> anyhow::Result<DatabaseConnection> {
    check_database_presence(db_url, operation, MissingDatabase::Refuse)?;
    tracing::debug!(
        operation,
        online_because = access.as_str(),
        "opening an ordinary pool against a possibly live instance"
    );
    rg_db::connect(db_url)
        .await
        .map_err(|e| annotate_db_open_error(e, db_url))
}

/// Refuse — or, for the two commands allowed to create one, announce — a
/// file-backed SQLite database that does not exist yet.
///
/// [`crate::config::DEFAULT_DB_URL`] is *relative* (`sqlite://./forgekeep.db`),
/// and the missing file is created rather than reported: `rg_db::connect_sqlite`
/// sets `create_if_missing` unconditionally, so demoting the URL to `?mode=rw`
/// would not hold this line either. Together that means any subcommand started
/// from the wrong directory — `docker exec` without `-w`, a cron entry, another
/// shell — silently addresses a brand new empty database instead of the
/// instance the operator meant, and nothing downstream can tell the difference:
/// the connection opens, the schema is simply empty. `backup-db` then VACUUMs
/// that empty database into the backup file and prints `Backup written`, which
/// is discovered at the one moment the backup was for (card_8baddb74fa82).
///
/// Hence presence is established here, in the one place subcommands open the
/// database, and by asking the filesystem rather than by trusting a URL flag.
pub(crate) fn check_database_presence(
    db_url: &str,
    operation: &str,
    missing: MissingDatabase,
) -> anyhow::Result<()> {
    let Some(path) = rg_db::sqlite_database_file(db_url) else {
        return Ok(());
    };
    match path.try_exists() {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => {
            // The probe failed, so nothing was established either way. Opening
            // the database is about to fail for the same reason and will say so
            // with SQLite's own words; inventing a "does not exist" here would
            // send the operator after the wrong thing.
            tracing::debug!(
                database = %path.display(),
                %error,
                "could not establish whether the database file exists; leaving the open path to report it"
            );
            return Ok(());
        }
    }

    let resolved = absolute_database_path(&path);
    match missing {
        MissingDatabase::Create => {
            tracing::warn!(
                database = %resolved.display(),
                "creating a NEW, empty SQLite database — if this instance already has one, stop \
                 now and point `--db-url` / `--config` at it"
            );
            Ok(())
        }
        MissingDatabase::Refuse => anyhow::bail!(
            "`{operation}` was pointed at the SQLite database `{}`, which does not exist \
             (resolved to `{}`).\n  A relative database URL resolves against the current \
             directory, so a command started somewhere else — `docker exec` without `-w`, a cron \
             entry, another shell — addresses a database that is not there. Nothing was created: \
             only `forgekeep serve` and `forgekeep migrate` may bring a database into \
             existence.\n  hint: pass `--db-url` or `--config`, or run from the data directory \
             (`docker exec -w /data ...`)",
            path.display(),
            resolved.display()
        ),
    }
}

/// The path as the filesystem sees it, so the message names the database that
/// was actually addressed and not just the relative spelling the operator can
/// already read on their own command line.
///
/// `.` components are dropped rather than kept, because the default URL starts
/// with one and `/opt/./forgekeep.db` reads like a typo in the diagnostic
/// rather than the answer to "which file did it mean".
fn absolute_database_path(path: &std::path::Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return path.to_path_buf(),
        }
    };
    absolute
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect()
}

/// [`connect`] with the `[timeouts]` connect/idle budget the server configures.
pub(crate) async fn connect_server_with_timeouts(
    db_url: &str,
    connect_secs: u64,
    idle_secs: u64,
) -> anyhow::Result<GuardedDatabaseConnection> {
    check_database_presence(db_url, "forgekeep serve", MissingDatabase::Create)?;
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
///
/// `missing` is a parameter rather than a constant because this is the one
/// opener with callers on both sides of it: `migrate` installs an instance and
/// may create the database, while `import` and `package list` merely bring the
/// schema forward on an instance that must already exist.
pub(crate) async fn connect_offline_migration(
    db_url: &str,
    operation: &str,
    missing: MissingDatabase,
) -> anyhow::Result<GuardedDatabaseConnection> {
    check_database_presence(db_url, operation, missing)?;
    let process_guard = rg_db::sqlite_process_guard::acquire_migration(db_url)?;
    let connection = rg_db::connect(db_url)
        .await
        .map_err(|e| annotate_db_open_error(e, db_url))?;
    Ok(GuardedDatabaseConnection {
        connection,
        _process_guard: process_guard,
    })
}

/// Open a standalone CLI pool for a whole-database maintenance pass, after
/// proving a file-backed SQLite server is not alive against the same database.
///
/// The sibling of [`connect_offline_migration`], and refused for a different
/// reason: not that the schema is about to change under a pool somebody else
/// cached, but that the pass keeps SQLite's single write lock from its first
/// statement to its commit. See
/// [`rg_db::sqlite_process_guard::acquire_maintenance`].
pub(crate) async fn connect_offline_maintenance(
    db_url: &str,
    operation: &'static str,
) -> anyhow::Result<GuardedDatabaseConnection> {
    check_database_presence(db_url, operation, MissingDatabase::Refuse)?;
    let process_guard = rg_db::sqlite_process_guard::acquire_maintenance(db_url, operation)?;
    let connection = rg_db::connect(db_url)
        .await
        .map_err(|e| annotate_db_open_error(e, db_url))?;
    Ok(GuardedDatabaseConnection {
        connection,
        _process_guard: process_guard,
    })
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
    let Some(path) = rg_db::sqlite_database_file(db_url) else {
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
            rg_db::sqlite_database_file("sqlite:///data/forgekeep.db?mode=rwc"),
            Some(PathBuf::from("/data/forgekeep.db"))
        );
        assert_eq!(
            rg_db::sqlite_database_file("sqlite://./forgekeep.db"),
            Some(PathBuf::from("./forgekeep.db"))
        );
        assert_eq!(rg_db::sqlite_database_file("sqlite::memory:"), None);
        assert_eq!(
            rg_db::sqlite_database_file("postgres://user:pw@localhost/forgekeep"),
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
            super::connect_offline_migration(
                &url,
                "forgekeep migrate",
                super::MissingDatabase::Create
            )
            .await
            .err()
            .expect("a live server connection must exclude the migrator")
        );
        assert!(message.contains("server to be stopped"), "{message}");

        drop(server);
        let migration = super::connect_offline_migration(
            &url,
            "forgekeep migrate",
            super::MissingDatabase::Create,
        )
        .await
        .unwrap();
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
