//! Cross-process exclusion for schema changes on a **server** database.
//!
//! [`crate::sqlite_process_guard`] is the same gate for file-backed SQLite, and
//! it does two jobs at once: it serialises the processes that may apply
//! migrations, and it enforces the offline contract that a live server's pool
//! must not be left holding a schema it can no longer refresh. Only the second
//! job is SQLite-specific — and because the whole gate is a lock on the
//! database *file*, the first job disappeared along with it for PostgreSQL and
//! MySQL, where `acquire_*` correctly returns `None`.
//!
//! It is not a theoretical gap. [`sea_orm_migration::MigratorTrait::up`] is not
//! idempotent with respect to a concurrent copy of itself on a server backend:
//! two ForgeKeep processes migrating one PostgreSQL database race inside
//! `CREATE TABLE` and one of them dies with
//! `duplicate key value violates unique constraint "pg_type_typname_nsp_index"`
//! — an error that names a PostgreSQL system index and tells the operator
//! neither the cause nor the remedy. Two replicas, a `docker compose up
//! --scale`, a restart overlapping a still-running old process, or an operator
//! running `forgekeep migrate` while the server boots all produce it.
//!
//! So take the lock inside the database instead of in the filesystem:
//! `pg_advisory_lock` on PostgreSQL, `GET_LOCK` on MySQL. Both are held by a
//! *session*, which means the database releases them by itself when the process
//! that held one dies — no lease file to clean up, no external coordinator, and
//! no way for a crashed migrator to wedge the next boot.
//!
//! # The loser waits — deliberately, and unlike SQLite
//!
//! On file-backed SQLite a contended lease is an **error**: the loser cannot
//! proceed at all, because the live server's pool has cached a schema no other
//! process can refresh, so the only correct answer is "stop that server first".
//! A server backend has no such cache — every connection sees the committed
//! schema — so the loser has something useful to do: wait, then apply whatever
//! the winner did not. It gets [`DEFAULT_WAIT`] to do it, after which it fails
//! with a message naming ForgeKeep and the action, because at that point the
//! holder is more likely stuck than slow.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sea_orm::sqlx::mysql::MySqlConnection;
use sea_orm::sqlx::postgres::PgConnection;
use sea_orm::sqlx::Connection;
use sea_orm::DatabaseConnection;

/// How long a migrator waits for the current holder before giving up.
///
/// Generous on purpose: the thing being waited for is a full migration run of a
/// database that may be large, and the cost of waiting too long (a slow boot,
/// with a log line saying why) is far below the cost of giving up too early (a
/// replica that refuses to start while its sibling is mid-upgrade).
pub const DEFAULT_WAIT: Duration = Duration::from_secs(300);

/// How long the lock's own dedicated connection may take to open.
///
/// The pool this borrows its settings from has already connected, so anything
/// beyond this is a network or server fault rather than a slow handshake, and
/// hanging here would stall a server boot with no explanation.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How often PostgreSQL is re-asked for the lock while waiting.
///
/// `pg_advisory_lock` blocks server-side, which would be cheaper, but a blocked
/// statement cannot be interrupted from here without leaving the connection in
/// an indeterminate state; polling `pg_try_advisory_lock` keeps the deadline in
/// this process and every failure a completed statement.
const POSTGRES_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The advisory-lock key ForgeKeep migrations use: `FORGEKEP` in ASCII.
///
/// PostgreSQL advisory locks are scoped to the current *database*, so two
/// ForgeKeep databases in one cluster do not contend on this constant.
const POSTGRES_ADVISORY_KEY: i64 = 0x464F_5247_454B_4550;

/// Prefix of the MySQL user-level lock name.
const MYSQL_LOCK_PREFIX: &str = "forgekeep_migrations";

/// MySQL rejects a user-level lock name longer than this.
const MYSQL_LOCK_NAME_MAX: usize = 64;

/// The right to apply migrations to one database, for as long as this is alive.
///
/// Dropping it without calling [`MigrationLock::release`] is safe: the value
/// owns the connection that holds the lock, and both PostgreSQL and MySQL
/// release a session's locks when its session ends. `release` exists so the
/// normal path hands the lock back promptly and by name rather than relying on
/// a socket closing.
pub struct MigrationLock {
    held: Option<Held>,
}

enum Held {
    /// A dedicated PostgreSQL session holding [`POSTGRES_ADVISORY_KEY`].
    Postgres(Box<PgConnection>),
    /// A dedicated MySQL session holding the named user-level lock.
    MySql {
        connection: Box<MySqlConnection>,
        name: String,
    },
}

/// Written by hand rather than derived: the interesting part is *what* is held,
/// and a derive would print the driver's connection internals instead.
impl std::fmt::Debug for MigrationLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.held {
            Some(Held::Postgres(_)) => write!(
                f,
                "MigrationLock(PostgreSQL advisory lock {POSTGRES_ADVISORY_KEY})"
            ),
            Some(Held::MySql { name, .. }) => write!(f, "MigrationLock(MySQL lock `{name}`)"),
            None => f.write_str("MigrationLock(no server-side lock)"),
        }
    }
}

/// Take the migration lock for `db`, waiting up to `wait` for a current holder.
///
/// A SQLite (or otherwise non-server) handle yields a lock that holds nothing:
/// its exclusion lives in [`crate::sqlite_process_guard`], and an in-memory
/// database has no second process to exclude.
pub async fn acquire(db: &DatabaseConnection, wait: Duration) -> Result<MigrationLock> {
    // Matched on the variant rather than `get_database_backend`, which panics on
    // a disconnected handle — the same reason `connect_options_handle` does.
    match db {
        DatabaseConnection::SqlxPostgresPoolConnection(_) => {
            let options = db.get_postgres_connection_pool().connect_options();
            let mut connection =
                tokio::time::timeout(CONNECT_TIMEOUT, PgConnection::connect_with(&options))
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!(
                    "opening the PostgreSQL session for the ForgeKeep migration lock timed out \
                     after {}s",
                    CONNECT_TIMEOUT.as_secs()
                )
                    })?
                    .context("open a PostgreSQL session for the ForgeKeep migration lock")?;
            acquire_postgres(&mut connection, wait).await?;
            Ok(MigrationLock {
                held: Some(Held::Postgres(Box::new(connection))),
            })
        }
        DatabaseConnection::SqlxMySqlPoolConnection(_) => {
            let options = db.get_mysql_connection_pool().connect_options();
            let mut connection =
                tokio::time::timeout(CONNECT_TIMEOUT, MySqlConnection::connect_with(&options))
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!(
                    "opening the MySQL session for the ForgeKeep migration lock timed out after \
                     {}s",
                    CONNECT_TIMEOUT.as_secs()
                )
                    })?
                    .context("open a MySQL session for the ForgeKeep migration lock")?;
            let name = acquire_mysql(&mut connection, wait).await?;
            Ok(MigrationLock {
                held: Some(Held::MySql {
                    connection: Box::new(connection),
                    name,
                }),
            })
        }
        _ => Ok(MigrationLock { held: None }),
    }
}

impl MigrationLock {
    /// Hand the lock back and close the session that held it.
    ///
    /// Best-effort by construction: the caller is finishing a migration run and
    /// a failure to unlock says nothing about whether that run succeeded, so
    /// this reports rather than returns. Closing the connection releases the
    /// lock regardless, which is what makes that safe.
    pub async fn release(mut self) {
        match self.held.take() {
            Some(Held::Postgres(connection)) => {
                let mut connection = *connection;
                if let Err(error) = sea_orm::sqlx::query("SELECT pg_advisory_unlock($1)")
                    .bind(POSTGRES_ADVISORY_KEY)
                    .execute(&mut connection)
                    .await
                {
                    tracing::warn!(
                        %error,
                        "failed to release the ForgeKeep migration advisory lock; closing its \
                         PostgreSQL session instead"
                    );
                }
                close_quietly(connection.close().await);
            }
            Some(Held::MySql { connection, name }) => {
                let mut connection = *connection;
                if let Err(error) = sea_orm::sqlx::query("SELECT RELEASE_LOCK(?)")
                    .bind(&name)
                    .execute(&mut connection)
                    .await
                {
                    tracing::warn!(
                        %error,
                        lock = %name,
                        "failed to release the ForgeKeep migration lock; closing its MySQL \
                         session instead"
                    );
                }
                close_quietly(connection.close().await);
            }
            None => {}
        }
    }
}

/// A close that failed changed nothing the caller can act on — the session ends
/// either way, and with it the lock.
fn close_quietly(result: Result<(), sea_orm::sqlx::Error>) {
    if let Err(error) = result {
        tracing::debug!(
            %error,
            "closing the ForgeKeep migration lock session reported an error"
        );
    }
}

/// Poll `pg_try_advisory_lock` until it is granted or `wait` runs out.
async fn acquire_postgres(connection: &mut PgConnection, wait: Duration) -> Result<()> {
    let deadline = Instant::now() + wait;
    let mut announced = false;

    loop {
        let granted: bool = sea_orm::sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(POSTGRES_ADVISORY_KEY)
            .fetch_one(&mut *connection)
            .await
            .context("ask PostgreSQL for the ForgeKeep migration lock")?;
        if granted {
            if announced {
                tracing::info!("Acquired the ForgeKeep migration lock");
            }
            return Ok(());
        }

        let Some(remaining) = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| {
                // `checked_duration_since` measures the wrong direction for a passed
                // deadline; a zero remainder is equally out of budget.
                !left.is_zero()
            })
        else {
            anyhow::bail!(
                "another ForgeKeep process has held the migration lock on this PostgreSQL \
                 database for more than {}s; wait for that migration to finish or stop that \
                 process, then run this again",
                wait.as_secs()
            );
        };

        if !announced {
            announced = true;
            tracing::info!(
                wait_secs = wait.as_secs(),
                "Another ForgeKeep process is migrating this PostgreSQL database; waiting for it \
                 to finish"
            );
        }
        tokio::time::sleep(POSTGRES_POLL_INTERVAL.min(remaining)).await;
    }
}

/// Take the MySQL user-level lock, returning the name it was taken under.
async fn acquire_mysql(connection: &mut MySqlConnection, wait: Duration) -> Result<String> {
    let schema: Option<String> = sea_orm::sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(&mut *connection)
        .await
        .context("read the current schema name for the ForgeKeep migration lock")?;
    let name = mysql_lock_name(schema.as_deref().unwrap_or_default());

    // A zero-second attempt first, purely so a wait can be announced before it
    // happens rather than explained after it: `GET_LOCK` blocks server-side and
    // this connection has nothing to say while it does.
    if get_lock(connection, &name, 0).await? {
        return Ok(name);
    }
    tracing::info!(
        wait_secs = wait.as_secs(),
        lock = %name,
        "Another ForgeKeep process is migrating this MySQL database; waiting for it to finish"
    );

    // `GET_LOCK` takes whole seconds and treats a negative timeout as "wait
    // forever", which is precisely the outcome this budget exists to prevent.
    let timeout = i64::try_from(wait.as_secs()).unwrap_or(i64::MAX);
    if get_lock(connection, &name, timeout).await? {
        tracing::info!(lock = %name, "Acquired the ForgeKeep migration lock");
        return Ok(name);
    }

    anyhow::bail!(
        "another ForgeKeep process has held the migration lock `{name}` on this MySQL server for \
         more than {}s; wait for that migration to finish or stop that process, then run this \
         again",
        wait.as_secs()
    )
}

/// One `GET_LOCK` attempt: `true` granted, `false` timed out.
async fn get_lock(connection: &mut MySqlConnection, name: &str, timeout: i64) -> Result<bool> {
    let outcome: Option<i64> = sea_orm::sqlx::query_scalar("SELECT GET_LOCK(?, ?)")
        .bind(name)
        .bind(timeout)
        .fetch_one(&mut *connection)
        .await
        .context("ask MySQL for the ForgeKeep migration lock")?;
    match outcome {
        Some(1) => Ok(true),
        Some(0) => Ok(false),
        // NULL is MySQL reporting that it could not arbitrate at all (an error
        // or a killed session). Waiting longer would not help, and proceeding
        // would migrate unserialised — which is the whole thing being prevented.
        other => anyhow::bail!(
            "MySQL could not arbitrate the ForgeKeep migration lock `{name}` (GET_LOCK returned \
             {}); migrations were not applied",
            other.map_or_else(|| "NULL".to_string(), |value| value.to_string())
        ),
    }
}

/// The user-level lock name for a MySQL schema.
///
/// `GET_LOCK` names are scoped to the *server*, not the schema, so the schema
/// name has to be part of the name or two unrelated ForgeKeep databases on one
/// MySQL server would serialise their migrations against each other. MySQL
/// rejects a name over [`MYSQL_LOCK_NAME_MAX`] characters outright, and a schema
/// name may occupy all 64 of them by itself, so an over-long one is folded into
/// a digest rather than sent as-is and refused.
fn mysql_lock_name(schema: &str) -> String {
    let full = format!("{MYSQL_LOCK_PREFIX}:{schema}");
    if full.len() <= MYSQL_LOCK_NAME_MAX {
        return full;
    }

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(schema.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("{MYSQL_LOCK_PREFIX}:{}", &digest[..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_schema_name_is_readable_in_the_lock_name() {
        assert_eq!(
            mysql_lock_name("forgekeep"),
            "forgekeep_migrations:forgekeep"
        );
    }

    #[test]
    fn a_schema_name_that_would_overflow_the_lock_name_is_folded_into_a_digest() {
        let schema = "s".repeat(64);
        let name = mysql_lock_name(&schema);

        assert!(
            name.len() <= MYSQL_LOCK_NAME_MAX,
            "MySQL refuses a lock name over {MYSQL_LOCK_NAME_MAX} chars, got {}: {name}",
            name.len()
        );
        assert!(name.starts_with(MYSQL_LOCK_PREFIX));
    }

    #[test]
    fn folded_lock_names_still_separate_two_schemas() {
        let first = mysql_lock_name(&"a".repeat(64));
        let second = mysql_lock_name(&format!("{}b", "a".repeat(63)));

        assert_ne!(
            first, second,
            "two schemas must not share one migration lock"
        );
    }

    #[tokio::test]
    async fn a_sqlite_handle_locks_nothing() {
        let db = crate::connect_with_pool(
            "sqlite::memory:",
            crate::TEST_CONNECT_TIMEOUT_SECS,
            crate::DEFAULT_IDLE_TIMEOUT_SECS,
            crate::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .expect("connect to an in-memory SQLite database");

        let lock = acquire(&db, Duration::from_secs(1))
            .await
            .expect("a SQLite handle has no server-side lock to take");

        assert!(
            lock.held.is_none(),
            "SQLite exclusion belongs to sqlite_process_guard, not to this module"
        );
        lock.release().await;
    }
}
