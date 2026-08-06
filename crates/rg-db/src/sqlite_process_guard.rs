//! Cross-process exclusion for file-backed SQLite schema changes.
//!
//! SQLite coordinates individual transactions, but it cannot refresh schema
//! state cached by a connection pool in another process. ForgeKeep therefore
//! treats a file-backed SQLite server as an offline-migration deployment: the
//! server holds this lease for its whole lifetime, and a standalone migrator
//! must acquire the same lease before it opens a pool.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// An OS-owned lease on the sidecar belonging to one file-backed SQLite DB.
///
/// The sidecar deliberately persists after this value is dropped. Deleting a
/// locked file would let another process open a new inode and bypass the lease;
/// only the OS lock is transient and it is released automatically on exit.
#[derive(Debug)]
pub struct SqliteProcessGuard {
    _file: File,
}

#[derive(Clone, Copy)]
enum Purpose {
    Server,
    Migration,
    Restore,
}

/// Acquire the lease a ForgeKeep server holds until HTTP and SSH have stopped.
///
/// Returns `None` for non-SQLite and in-memory SQLite URLs: neither shares a
/// file-backed SQLite schema with a separate process.
pub fn acquire_server(database_url: &str) -> Result<Option<SqliteProcessGuard>> {
    acquire(database_url, Purpose::Server)
}

/// Acquire the lease required before a standalone SQLite migration opens.
///
/// A contended lease is an actionable offline-contract failure, not something
/// to wait out while a live server continues accepting requests.
pub fn acquire_migration(database_url: &str) -> Result<Option<SqliteProcessGuard>> {
    acquire(database_url, Purpose::Migration)
}

/// Acquire the lease required before replacing a file-backed SQLite database.
///
/// The guard must be held across removal of the database/WAL/SHM files and the
/// backup copy. Otherwise a live server can keep writing through its old open
/// inode while the restored database already occupies the configured path.
pub fn acquire_restore(database_url: &str) -> Result<Option<SqliteProcessGuard>> {
    acquire(database_url, Purpose::Restore)
}

fn acquire(database_url: &str, purpose: Purpose) -> Result<Option<SqliteProcessGuard>> {
    let Some((database_path, lock_path)) = database_and_lock_path(database_url)? else {
        return Ok(None);
    };

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| {
            format!(
                "open SQLite process-lease sidecar `{}` for database `{}`",
                lock_path.display(),
                database_path.display()
            )
        })?;

    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(SqliteProcessGuard { _file: file })),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => match purpose {
            Purpose::Server => anyhow::bail!(
                "SQLite database `{}` is already in use by another ForgeKeep server or offline \
                 database operation; stop that process before starting this server",
                database_path.display()
            ),
            Purpose::Migration => anyhow::bail!(
                "SQLite migrations require the ForgeKeep server to be stopped; database `{}` is \
                 held by another ForgeKeep process",
                database_path.display()
            ),
            Purpose::Restore => anyhow::bail!(
                "SQLite restore requires the ForgeKeep server to be stopped; database `{}` is \
                 held by another ForgeKeep process",
                database_path.display()
            ),
        },
        Err(error) => Err(error).with_context(|| {
            format!(
                "acquire SQLite process lease `{}` for database `{}`",
                lock_path.display(),
                database_path.display()
            )
        }),
    }
}

fn database_and_lock_path(database_url: &str) -> Result<Option<(PathBuf, PathBuf)>> {
    let Some(path) = sqlite_file_path(database_url) else {
        return Ok(None);
    };
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .context("resolve the working directory for the SQLite process lease")?
            .join(path)
    };

    // Resolve aliases before naming the sidecar. The DB may not exist on first
    // start, so canonicalize its parent in that case and retain the filename.
    let database_path = match absolute.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = absolute.parent().unwrap_or_else(|| Path::new("."));
            let parent = parent.canonicalize().with_context(|| {
                format!(
                    "resolve SQLite database directory `{}` before acquiring its process lease",
                    parent.display()
                )
            })?;
            let file_name = absolute.file_name().ok_or_else(|| {
                anyhow::anyhow!(
                    "SQLite database URL does not name a file: `{}`",
                    database_url
                )
            })?;
            parent.join(file_name)
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "resolve SQLite database path `{}` before acquiring its process lease",
                    absolute.display()
                )
            });
        }
    };

    let file_name = database_path.file_name().ok_or_else(|| {
        anyhow::anyhow!(
            "SQLite database URL does not name a file: `{}`",
            database_url
        )
    })?;
    let mut lock_name = OsString::from(file_name);
    lock_name.push(".forgekeep.lock");
    let lock_path = database_path.with_file_name(lock_name);
    Ok(Some((database_path, lock_path)))
}

fn sqlite_file_path(database_url: &str) -> Option<PathBuf> {
    let rest = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite3://"))
        .or_else(|| database_url.strip_prefix("sqlite:"))
        .or_else(|| database_url.strip_prefix("sqlite3:"))?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let is_memory = matches!(path, "" | ":memory:" | "/:memory:")
        || query.split('&').any(|pair| pair == "mode=memory");
    (!is_memory).then(|| PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_file_backed_sqlite_urls_get_a_process_lease() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/forgekeep.db?mode=rwc", dir.path().display());

        let guard = super::acquire_server(&url).unwrap();
        assert!(guard.is_some());
        assert!(super::acquire_server("sqlite::memory:").unwrap().is_none());
        assert!(
            super::acquire_server("sqlite://named?mode=memory&cache=shared")
                .unwrap()
                .is_none()
        );
        assert!(super::acquire_server("postgres://localhost/forgekeep")
            .unwrap()
            .is_none());
        assert!(super::acquire_migration("postgres://localhost/forgekeep")
            .unwrap()
            .is_none());
        assert!(super::acquire_migration("mysql://localhost/forgekeep")
            .unwrap()
            .is_none());
        assert!(super::acquire_restore("postgres://localhost/forgekeep")
            .unwrap()
            .is_none());
        assert!(super::acquire_restore("mysql://localhost/forgekeep")
            .unwrap()
            .is_none());
    }

    #[test]
    fn aliases_of_one_sqlite_file_contend_on_one_lease() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let direct = format!("sqlite://{}/forgekeep.db?mode=rwc", data.display());
        let alias = format!("sqlite://{}/../data/forgekeep.db?mode=rwc", data.display());

        let server = super::acquire_server(&direct).unwrap().unwrap();
        let message = format!(
            "{:#}",
            super::acquire_migration(&alias)
                .expect_err("the alias must not bypass the live-server lease")
        );
        assert!(message.contains("server to be stopped"), "{message}");

        drop(server);
        assert!(super::acquire_migration(&alias).unwrap().is_some());
    }
}
