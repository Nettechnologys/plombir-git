//! ForgeKeep database layer — SeaORM + SQLite.
//!
//! # Usage
//!
//! ```rust,no_run
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let db = rg_db::connect("sqlite:///tmp/forgekeep/forgekeep.db?mode=rwc").await?;
//!     rg_db::run_migrations(&db).await?;
//!     Ok(())
//! }
//! ```

pub mod contention;
pub mod entities;
#[cfg(test)]
mod entity_schema_guard;
#[cfg(test)]
mod fts_rebuild_tests;
pub mod migration_lock;
pub mod migrations;
pub mod ops;
pub mod package_version_key;
mod serialized_user_grants;
pub mod sqlite_process_guard;
pub mod user_grants;

use std::any::Any;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use sea_orm::sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use sea_orm::SqlxSqliteConnector;
use sea_orm_migration::MigratorTrait;

pub use sea_orm;
pub use sea_orm::DatabaseConnection;

/// Which database backend a `database_url` selects, inferred from its scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbBackend {
    /// SQLite (embedded, zero-dependency, default).
    Sqlite,
    /// PostgreSQL.
    Postgres,
    /// MySQL / MariaDB.
    MySql,
}

/// Redact a password embedded in a database URL before logging or returning
/// the URL in an error. Usernames, hosts, ports, database names and query
/// options remain visible for diagnostics.
pub fn redact_database_url(db_url: &str) -> String {
    let Some(scheme_end) = db_url.find("://") else {
        return "<invalid database URL>".to_string();
    };
    let authority_start = scheme_end + 3;
    let authority_end = db_url[authority_start..]
        .find(['/', '?', '#'])
        .map(|offset| authority_start + offset)
        .unwrap_or(db_url.len());
    let authority = &db_url[authority_start..authority_end];
    let Some(at) = authority.rfind('@') else {
        return db_url.to_string();
    };
    let user_info = &authority[..at];
    let Some(password_separator) = user_info.find(':') else {
        return db_url.to_string();
    };

    format!(
        "{}{}:***@{}",
        &db_url[..authority_start],
        &user_info[..password_separator],
        &db_url[authority_start + at + 1..]
    )
}

/// Infer the backend from a `database_url` scheme.
///
/// Accepted schemes: `sqlite://` (`sqlite3://`), `postgres://` / `postgresql://`,
/// `mysql://`. Anything else fails loudly so misconfiguration is caught early.
pub fn detect_backend(db_url: &str) -> Result<DbBackend> {
    if db_url.starts_with("sqlite:") || db_url.starts_with("sqlite3:") {
        Ok(DbBackend::Sqlite)
    } else if db_url.starts_with("postgres:") || db_url.starts_with("postgresql:") {
        Ok(DbBackend::Postgres)
    } else if db_url.starts_with("mysql:") {
        Ok(DbBackend::MySql)
    } else {
        anyhow::bail!(
            "unsupported database_url scheme in '{}': expected sqlite://, postgres://, or mysql://",
            redact_database_url(db_url)
        )
    }
}

/// True when this database error is the backend's UNIQUE-constraint violation.
///
/// Answered from the backend's own error code — SQLite 1555/2067, PostgreSQL
/// 23505, MySQL 1062 — via [`sea_orm::DbErr::sql_err`], not by looking for the
/// word "unique" in the message: the wording differs per backend and per
/// version, and a substring match also claims an error that merely *mentions* a
/// uniquely-named index.
///
/// Narrow on purpose. A caller uses this to turn one specific loss — someone
/// else inserted the row I was about to insert — into a normal outcome, so
/// every other failure (foreign key, check constraint, disk, outage) must stay
/// an error rather than become a fabricated success.
pub fn is_unique_violation(error: &sea_orm::DbErr) -> bool {
    matches!(
        error.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    )
}

/// [`is_unique_violation`] for a `DbErr` a higher layer wrapped in `anyhow`
/// context.
///
/// `rg_db::ops::user_ops` returns `anyhow::Result` and attaches a `db: ...`
/// context to every call, so its callers never see the `DbErr` directly. The
/// whole chain is walked rather than just the source: context can be added more
/// than once on the way up.
pub fn is_unique_violation_anyhow(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<sea_orm::DbErr>()
            .is_some_and(is_unique_violation)
    })
}

/// True when the backend reports that an entire transaction may be retried.
///
/// These are concurrency outcomes, not connection failures: SQLite could not
/// promote/hold a WAL write lock, PostgreSQL aborted on serialization/deadlock,
/// or MySQL chose a deadlock/lock-wait victim. Callers must restart the *whole*
/// transaction from a fresh read; retrying only the failed statement can reuse
/// stale decisions.
pub fn is_retryable_transaction_error(error: &sea_orm::DbErr) -> bool {
    use sea_orm::{RuntimeErr, SqlxError};

    let runtime = match error {
        sea_orm::DbErr::Conn(runtime)
        | sea_orm::DbErr::Exec(runtime)
        | sea_orm::DbErr::Query(runtime) => runtime,
        _ => return false,
    };
    let RuntimeErr::SqlxError(SqlxError::Database(database_error)) = runtime else {
        return false;
    };

    matches!(
        database_error.code().as_deref(),
        // SQLite: BUSY, LOCKED and their WAL/shared-cache/timeout variants.
        Some("5" | "6" | "261" | "262" | "517" | "518" | "773")
            // PostgreSQL: serialization failure and deadlock detected.
            | Some("40001" | "40P01")
            // MySQL/MariaDB: lock wait timeout and deadlock victim.
            | Some("1205" | "1213")
    )
}

/// [`is_retryable_transaction_error`] for a `DbErr` wrapped in `anyhow` context.
pub fn is_retryable_transaction_error_anyhow(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<sea_orm::DbErr>()
            .is_some_and(is_retryable_transaction_error)
    })
}

/// Convert portable `?` bind markers in raw SQL to the backend's syntax.
///
/// SeaORM does not rewrite placeholders in `Statement::from_sql_and_values`:
/// PostgreSQL requires `$1`, `$2`, ... while SQLite and MySQL use `?`. Keep
/// raw SQL readable and portable by passing it through this helper first.
/// Question marks inside quoted strings or identifiers are left untouched.
pub fn prepare_sql(backend: sea_orm::DatabaseBackend, sql: &str) -> String {
    if backend != sea_orm::DatabaseBackend::Postgres {
        return sql.to_string();
    }

    let mut result = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut quote = None;
    let mut parameter = 1;

    while let Some(ch) = chars.next() {
        if let Some(active_quote) = quote {
            result.push(ch);
            if ch == active_quote {
                if chars.peek() == Some(&active_quote) {
                    result.push(chars.next().unwrap());
                } else {
                    quote = None;
                }
            }
            continue;
        }

        match ch {
            '\'' | '"' | '`' => {
                quote = Some(ch);
                result.push(ch);
            }
            '?' => {
                result.push('$');
                result.push_str(&parameter.to_string());
                parameter += 1;
            }
            _ => result.push(ch),
        }
    }

    result
}

// ── Identity of a live database instance ────────────────────────────────

/// Identity of one live database instance, as returned by [`instance_id`].
///
/// Opaque and only ever compared for equality — the numbering is an internal
/// allocation order, not something to persist or expose.
pub type InstanceId = u64;

/// The identity of the database instance `db` talks to.
///
/// Callers that keep process-wide state keyed by database row ids need this:
/// a row id is only unique *within* one database, so a cache keyed by id alone
/// silently merges two databases that both start their autoincrement at 1.
/// That is not hypothetical — the test suite compiles into one binary per
/// crate and every test opens its own database, so `(repo_id = 1, user_id = 2)`
/// means something different in each of them.
///
/// Guarantees:
/// - Every handle onto the same pool — including clones of the
///   `DatabaseConnection` — reports the same id.
/// - Two separate pools never share an id, *even when their URLs are
///   identical*. `sqlite::memory:` is the case that matters: each connect
///   creates a private database behind the same URL, so the URL cannot be the
///   identity.
/// - `None` for a handle with no pool to identify (mock, proxy, disconnected).
///   Such a handle answers no queries, so callers should treat it as
///   "not cacheable" rather than lumping it in with a real database.
///
/// Cheap enough for a per-request call: a read-locked hash lookup on the
/// established path.
pub fn instance_id(db: &DatabaseConnection) -> Option<InstanceId> {
    let handle = connect_options_handle(db)?;
    // The address of the pool's connect-options allocation. Stable for the
    // pool's whole life, and unique among live pools — see `InstanceEntry` for
    // why a *dead* pool can never lend its address to a new one unnoticed.
    let key = Arc::as_ptr(&handle) as *const () as usize;

    let registry = instance_registry();
    if let Some(entry) = registry
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        return Some(entry.id);
    }

    let mut registry = registry
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Lost the race to another thread registering the same pool.
    if let Some(entry) = registry.get(&key) {
        return Some(entry.id);
    }
    // Release the addresses of pools that have since been dropped, so the
    // registry tracks live databases rather than every one the process ever
    // opened. Safe to do here: `handle` is alive, so its own entry is never
    // the one being released.
    registry.retain(|_, entry| entry.guard.strong_count() > 0);

    let id = NEXT_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
    registry.insert(
        key,
        InstanceEntry {
            id,
            guard: Arc::downgrade(&handle),
        },
    );
    Some(id)
}

/// A pool's address, and the id handed out for it.
struct InstanceEntry {
    id: InstanceId,
    /// Pins the allocation the map is keyed by. Never upgraded: a `Weak` keeps
    /// the allocation reserved after the value inside it is dropped, so while
    /// this entry exists no other `Arc` can be handed that address — which is
    /// what makes the address a durable identity instead of a coincidence.
    /// Dropping the entry and this guard together (see the `retain` above) is
    /// therefore the only way an address is ever recycled, and it takes the
    /// stale id with it.
    guard: Weak<dyn Any + Send + Sync>,
}

static INSTANCE_REGISTRY: OnceLock<RwLock<HashMap<usize, InstanceEntry>>> = OnceLock::new();
static NEXT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

fn instance_registry() -> &'static RwLock<HashMap<usize, InstanceEntry>> {
    INSTANCE_REGISTRY.get_or_init(Default::default)
}

/// The connect options behind `db`'s pool, type-erased.
///
/// Matched on the variant rather than [`sea_orm::ConnectionTrait::get_database_backend`]
/// because that panics on a disconnected handle, and the `get_*_connection_pool`
/// accessors panic on a mock one.
fn connect_options_handle(db: &DatabaseConnection) -> Option<Arc<dyn Any + Send + Sync>> {
    match db {
        DatabaseConnection::SqlxSqlitePoolConnection(_) => {
            Some(db.get_sqlite_connection_pool().connect_options())
        }
        DatabaseConnection::SqlxPostgresPoolConnection(_) => {
            Some(db.get_postgres_connection_pool().connect_options())
        }
        DatabaseConnection::SqlxMySqlPoolConnection(_) => {
            Some(db.get_mysql_connection_pool().connect_options())
        }
        _ => None,
    }
}

/// Default DB connect (acquire) timeout (seconds).
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Connect (acquire) timeout for the throwaway databases the test suites build.
///
/// Deliberately far longer than [`DEFAULT_CONNECT_TIMEOUT_SECS`]. The timeout is
/// sqlx's *acquire* timeout, and [`connect_with_pool`] opens `min_connections`
/// eagerly, so it also bounds the very first connect: creating the file, running
/// the PRAGMAs and writing the WAL header must all finish inside it. A test
/// process is the one place where that budget competes with dozens of sibling
/// test processes doing the same thing on the same disk — so the short
/// production value turns machine load into a `pool timed out while waiting for
/// an open connection` panic in *setup*, which reads like a broken test rather
/// than a busy disk.
///
/// A server has the opposite need: a connect that is not answered within seconds
/// is an outage worth reporting, not something to wait out. Hence two values.
pub const TEST_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Default DB idle timeout (seconds).
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 600;
/// Default max pool connections.
///
/// In WAL mode SQLite allows many concurrent readers alongside a single
/// writer, so a small pool lets read-heavy API/UI traffic run in parallel
/// instead of serialising on one connection. Writers still serialise at the
/// SQLite level; `busy_timeout` (set per connection) absorbs brief overlaps.
/// For Postgres/MySQL the pool is shared by a single server, so a small pool
/// is still appropriate for an embedded-style deployment.
pub const DEFAULT_MAX_CONNECTIONS: u32 = 5;

/// Connect to the database selected by `db_url`'s scheme (SQLite / Postgres / MySQL).
/// URL example: `sqlite:///path/to/db?mode=rwc`, `postgres://user@localhost/forgekeep`.
pub async fn connect(db_url: &str) -> Result<DatabaseConnection> {
    connect_with_timeouts(
        db_url,
        DEFAULT_CONNECT_TIMEOUT_SECS,
        DEFAULT_IDLE_TIMEOUT_SECS,
    )
    .await
}

/// Connect with configurable connect/idle timeouts and the default pool size.
pub async fn connect_with_timeouts(
    db_url: &str,
    connect_secs: u64,
    idle_secs: u64,
) -> Result<DatabaseConnection> {
    connect_with_pool(db_url, connect_secs, idle_secs, DEFAULT_MAX_CONNECTIONS).await
}

/// Connect to the database with full pool control, dispatching on the URL scheme.
///
/// SQLite applies per-connection PRAGMAs (see [`connect_sqlite`]). Postgres/MySQL
/// use their own connect options, gated behind the `db-postgres` / `db-mysql`
/// cargo features so the default binary stays SQLite-only.
pub async fn connect_with_pool(
    db_url: &str,
    connect_secs: u64,
    idle_secs: u64,
    max_connections: u32,
) -> Result<DatabaseConnection> {
    let max_connections = max_connections.max(1);
    let backend = detect_backend(db_url)?;
    tracing::info!(url = %redact_database_url(db_url), ?backend, connect_secs, idle_secs, max_connections, "Connecting to database");

    match backend {
        DbBackend::Sqlite => connect_sqlite(db_url, connect_secs, idle_secs, max_connections).await,
        DbBackend::Postgres => {
            connect_postgres(db_url, connect_secs, idle_secs, max_connections).await
        }
        DbBackend::MySql => connect_mysql(db_url, connect_secs, idle_secs, max_connections).await,
    }
}

/// Connect to SQLite with PRAGMA optimization.
///
/// PRAGMAs are attached to the sqlx [`SqliteConnectOptions`] so they are applied
/// to **every** physical connection the pool opens — including reconnections
/// after an idle timeout. (The previous approach ran the PRAGMAs once after
/// connect, which silently lost per-connection settings such as `foreign_keys`
/// and `busy_timeout` whenever the pool re-established a connection.)
async fn connect_sqlite(
    db_url: &str,
    connect_secs: u64,
    idle_secs: u64,
    max_connections: u32,
) -> Result<DatabaseConnection> {
    // Per-connection options — applied on every connect by sqlx.
    let conn_opts = SqliteConnectOptions::from_str(db_url)
        .with_context(|| format!("invalid sqlite url: {db_url}"))?
        .create_if_missing(true) // honour `?mode=rwc`
        .journal_mode(SqliteJournalMode::Wal) // WAL for reader/writer concurrency
        .synchronous(SqliteSynchronous::Normal) // good balance for WAL
        .busy_timeout(Duration::from_secs(5)) // wait instead of immediate SQLITE_BUSY
        .foreign_keys(true) // enforce FK constraints
        .pragma("cache_size", "-64000") // 64MB page cache
        .pragma("temp_store", "MEMORY") // temp tables in RAM
        .pragma("mmap_size", "268435456"); // 256MB memory-mapped I/O

    // WAL allows concurrent readers; writers serialise (busy_timeout absorbs
    // brief overlaps). Keep min_connections low to stay light on idle.
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(connect_secs))
        .idle_timeout(Duration::from_secs(idle_secs))
        .connect_with(conn_opts)
        .await
        .with_context(|| format!("failed to connect to database: {db_url}"))?;

    tracing::info!("Applied SQLite PRAGMAs via per-connection options");
    Ok(SqlxSqliteConnector::from_sqlx_sqlite_pool(pool))
}

/// Connect to PostgreSQL. The sqlx Postgres driver is always linked (SeaORM's
/// default features include it), so no cargo feature gate is required.
async fn connect_postgres(
    db_url: &str,
    connect_secs: u64,
    idle_secs: u64,
    max_connections: u32,
) -> Result<DatabaseConnection> {
    use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use sea_orm::SqlxPostgresConnector;

    let conn_opts = PgConnectOptions::from_str(db_url)
        .with_context(|| format!("invalid postgres url: {}", redact_database_url(db_url)))?;
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(connect_secs))
        .idle_timeout(Duration::from_secs(idle_secs))
        .connect_with(conn_opts)
        .await
        .with_context(|| {
            format!(
                "failed to connect to postgres: {}",
                redact_database_url(db_url)
            )
        })?;
    tracing::info!("Connected to PostgreSQL");
    Ok(SqlxPostgresConnector::from_sqlx_postgres_pool(pool))
}

/// Connect to MySQL / MariaDB. The sqlx MySQL driver is always linked (SeaORM's
/// default features include it), so no cargo feature gate is required.
async fn connect_mysql(
    db_url: &str,
    connect_secs: u64,
    idle_secs: u64,
    max_connections: u32,
) -> Result<DatabaseConnection> {
    use sea_orm::sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
    use sea_orm::SqlxMySqlConnector;

    let conn_opts = MySqlConnectOptions::from_str(db_url)
        .with_context(|| format!("invalid mysql url: {}", redact_database_url(db_url)))?;
    let pool = MySqlPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(connect_secs))
        .idle_timeout(Duration::from_secs(idle_secs))
        .connect_with(conn_opts)
        .await
        .with_context(|| {
            format!(
                "failed to connect to mysql: {}",
                redact_database_url(db_url)
            )
        })?;
    tracing::info!("Connected to MySQL");
    Ok(SqlxMySqlConnector::from_sqlx_mysql_pool(pool))
}

/// Run all pending migrations.
///
/// Every caller is serialised against every other process migrating the same
/// database, so the gate cannot be forgotten by the sixth call site the way it
/// would be if each one took it separately. On file-backed SQLite that
/// exclusion is the file lease the caller already holds
/// ([`sqlite_process_guard`]); on PostgreSQL and MySQL it is a lock inside the
/// database itself ([`migration_lock`]), because `Migrator::up` is not
/// idempotent with respect to a concurrent copy of itself there.
pub async fn run_migrations(db: &DatabaseConnection) -> Result<()> {
    let lock = migration_lock::acquire(db, migration_lock::DEFAULT_WAIT).await?;
    let outcome = run_migrations_locked(db).await;
    // Released explicitly on both paths so the next migrator is not left waiting
    // on a socket that has not been noticed yet.
    lock.release().await;
    outcome
}

/// [`run_migrations`] with the lock already held.
async fn run_migrations_locked(db: &DatabaseConnection) -> Result<()> {
    tracing::info!("Running database migrations");
    migrations::Migrator::up(db, None)
        .await
        .context("migration failed")?;
    if matches!(db, DatabaseConnection::SqlxSqlitePoolConnection(_)) {
        migrations::refresh_sqlite_pool_after_schema_change(
            db.get_sqlite_connection_pool(),
            "database migrations",
        )
        .await
        .context("failed to refresh SQLite connections after migrations")?;
    }
    Ok(())
}

/// Rebuild full-text search indexes from the main tables.
///
/// * **SQLite** — the FTS5 tables are independent virtual tables, so we
///   `DELETE` + re-`INSERT` from the source rows (mirroring the original FTS5
///   `rebuild` command).
/// * **Postgres / MySQL** — the FTS columns/tables are maintained
///   automatically (generated `tsvector` column / `FULLTEXT` index), so a
///   manual rebuild is unnecessary; we just refresh statistics.
pub async fn rebuild_fts_indexes(db: &DatabaseConnection) -> Result<()> {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let backend = db.get_database_backend();
    tracing::info!(?backend, "Rebuilding FTS indexes...");

    match backend {
        DatabaseBackend::Sqlite => {
            rebuild_sqlite_fts_indexes(db, |_| async { Ok(()) }).await?;
        }
        DatabaseBackend::Postgres => {
            for t in ["repos_fts", "issues_fts", "wiki_pages_fts"] {
                db.execute(Statement::from_string(backend, format!("ANALYZE {t}")))
                    .await?;
            }
            tracing::info!(
                "Postgres FTS columns are generated and self-maintaining; statistics refreshed."
            );
        }
        DatabaseBackend::MySql => {
            db.execute(Statement::from_string(
                backend,
                "OPTIMIZE TABLE repos_fts, issues_fts, wiki_pages_fts",
            ))
            .await?;
            tracing::info!("MySQL FULLTEXT indexes optimized.");
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqliteFtsTable {
    Repositories,
    Issues,
    WikiPages,
}

/// Rebuild every SQLite FTS table under one write transaction.
///
/// The first `DELETE` acquires SQLite's single-writer lock. Source-table
/// writers (and therefore their FTS triggers) wait until the complete rebuild
/// commits, while readers keep seeing the previous committed snapshot. The
/// callback is a private test seam used to pause or fail after a clear without
/// relying on scheduler timing in the regression tests.
async fn rebuild_sqlite_fts_indexes<F, Fut>(db: &DatabaseConnection, after_clear: F) -> Result<()>
where
    F: Fn(SqliteFtsTable) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};

    let backend = DatabaseBackend::Sqlite;
    let transaction = db
        .begin()
        .await
        .context("begin atomic SQLite FTS rebuild")?;

    let rebuild_result: Result<()> = async {
        tracing::info!("  Rebuilding repos_fts...");
        transaction
            .execute(Statement::from_sql_and_values(
                backend,
                "DELETE FROM repos_fts",
                [],
            ))
            .await
            .context("clear repos_fts")?;
        after_clear(SqliteFtsTable::Repositories).await?;
        transaction
            .execute(Statement::from_sql_and_values(
                backend,
                "INSERT INTO repos_fts(rowid, name, description) SELECT id, name, description FROM repositories WHERE deleted_at IS NULL",
                [],
            ))
            .await
            .context("repopulate repos_fts")?;

        tracing::info!("  Rebuilding issues_fts...");
        transaction
            .execute(Statement::from_sql_and_values(
                backend,
                "DELETE FROM issues_fts",
                [],
            ))
            .await
            .context("clear issues_fts")?;
        after_clear(SqliteFtsTable::Issues).await?;
        transaction
            .execute(Statement::from_sql_and_values(
                backend,
                "INSERT INTO issues_fts(rowid, title, body) SELECT id, title, COALESCE(body, '') FROM issues",
                [],
            ))
            .await
            .context("repopulate issues_fts")?;

        tracing::info!("  Rebuilding wiki_pages_fts...");
        transaction
            .execute(Statement::from_sql_and_values(
                backend,
                "DELETE FROM wiki_pages_fts",
                [],
            ))
            .await
            .context("clear wiki_pages_fts")?;
        after_clear(SqliteFtsTable::WikiPages).await?;
        transaction
            .execute(Statement::from_sql_and_values(
                backend,
                "INSERT INTO wiki_pages_fts(rowid, title, content) SELECT id, title, content FROM wiki_pages",
                [],
            ))
            .await
            .context("repopulate wiki_pages_fts")?;

        Ok(())
    }
    .await;

    match rebuild_result {
        Ok(()) => {
            transaction
                .commit()
                .await
                .context("commit atomic SQLite FTS rebuild")?;
            tracing::info!("FTS5 indexes rebuilt successfully");
            Ok(())
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                return Err(error).context(format!(
                    "SQLite FTS rebuild failed and its transaction could not be rolled back: \
                     {rollback_error}"
                ));
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        clippy::let_underscore_must_use,
        reason = "SQLite test files may already be absent before or after the run; cleanup must not mask the assertion under test."
    )]
    fn discard_sqlite_test_file(path: impl AsRef<std::path::Path>) {
        let _ = std::fs::remove_file(path);
    }

    fn discard_sqlite_test_files(path: &std::path::Path) {
        discard_sqlite_test_file(path);
        for suffix in ["-wal", "-shm"] {
            discard_sqlite_test_file(std::path::PathBuf::from(format!(
                "{}{}",
                path.display(),
                suffix
            )));
        }
    }
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    /// The identity has to survive being asked twice and being asked through a
    /// clone, or process-wide state keyed on it would split per call site.
    #[tokio::test]
    async fn one_database_has_one_identity() {
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("connect");

        let id = instance_id(&db).expect("a pooled connection has an identity");
        assert_eq!(Some(id), instance_id(&db), "asking twice must not renumber");
        assert_eq!(
            Some(id),
            instance_id(&db.clone()),
            "a clone shares the pool, so it shares the identity"
        );
    }

    /// The case the callers actually need: two databases behind one URL.
    /// `sqlite::memory:` gives each connect a private database, so anything that
    /// identified a database by its URL would merge the two.
    #[tokio::test]
    async fn two_databases_behind_the_same_url_have_different_identities() {
        let first = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("connect first");
        let second = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("connect second");

        assert_ne!(instance_id(&first), instance_id(&second));
        assert!(instance_id(&first).is_some());
    }

    /// A closed database must not bequeath its identity to the next one. The
    /// allocator will happily hand the fresh pool the address the dead one had;
    /// what stops the id coming with it is the guard held in the registry.
    #[tokio::test]
    async fn a_new_database_never_inherits_a_dropped_one_s_identity() {
        let mut retired = Vec::new();
        for _ in 0..8 {
            let db = sea_orm::Database::connect("sqlite::memory:")
                .await
                .expect("connect");
            retired.push(instance_id(&db).expect("identity"));
            drop(db);
        }

        let live = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("connect live");
        let live_id = instance_id(&live).expect("identity");
        assert!(
            !retired.contains(&live_id),
            "identity {live_id} was already handed out to a closed database"
        );
    }

    #[test]
    fn a_handle_with_no_pool_has_no_identity() {
        // Nothing to identify, and no queries to cache the answers of either.
        assert_eq!(instance_id(&DatabaseConnection::Disconnected), None);
    }

    #[test]
    fn database_urls_are_redacted_before_diagnostics() {
        assert_eq!(
            redact_database_url(
                "postgres://forgekeep:super-secret@db.internal:5432/forgekeep?sslmode=require"
            ),
            "postgres://forgekeep:***@db.internal:5432/forgekeep?sslmode=require"
        );
        assert_eq!(
            redact_database_url("mysql://root:p%40ss%3Aword@127.0.0.1:3306/forgekeep"),
            "mysql://root:***@127.0.0.1:3306/forgekeep"
        );
        assert_eq!(
            redact_database_url("postgres://forgekeep@db.internal/forgekeep"),
            "postgres://forgekeep@db.internal/forgekeep"
        );
        assert_eq!(
            redact_database_url("sqlite:///tmp/forgekeep.db?mode=rwc"),
            "sqlite:///tmp/forgekeep.db?mode=rwc"
        );

        let error = detect_backend("custom://root:top-secret@db/forgekeep")
            .expect_err("unsupported scheme")
            .to_string();
        assert!(!error.contains("top-secret"));
        assert!(error.contains("root:***@db"));
    }

    #[test]
    fn postgres_raw_sql_placeholders_are_numbered() {
        assert_eq!(
            prepare_sql(
                DatabaseBackend::Postgres,
                "SELECT '?' AS literal, id FROM t WHERE a = ? AND b = ?"
            ),
            "SELECT '?' AS literal, id FROM t WHERE a = $1 AND b = $2"
        );
        assert_eq!(
            prepare_sql(DatabaseBackend::MySql, "SELECT * FROM t WHERE id = ?"),
            "SELECT * FROM t WHERE id = ?"
        );
    }

    /// Regression guard: every connection handed out by the pool must have the
    /// per-connection PRAGMAs applied. An opener that loses `foreign_keys`
    /// (e.g. by running PRAGMAs once instead of per-connection) silently
    /// disables FK enforcement — this asserts it stays on.
    #[tokio::test]
    async fn connect_applies_per_connection_pragmas() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("forgekeep_pragma_test_{}.db", std::process::id()));
        discard_sqlite_test_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());

        let db = connect_with_pool(
            &url,
            TEST_CONNECT_TIMEOUT_SECS,
            DEFAULT_IDLE_TIMEOUT_SECS,
            DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .expect("connect");

        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Sqlite,
                "PRAGMA foreign_keys".to_string(),
            ))
            .await
            .expect("query foreign_keys")
            .expect("one row");
        let fk: i32 = row.try_get_by_index(0).expect("fk value");
        assert_eq!(fk, 1, "foreign_keys must be ON for every connection");

        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Sqlite,
                "PRAGMA journal_mode".to_string(),
            ))
            .await
            .expect("query journal_mode")
            .expect("one row");
        let mode: String = row.try_get_by_index(0).expect("journal mode value");
        assert_eq!(mode.to_lowercase(), "wal", "journal_mode must be WAL");

        discard_sqlite_test_file(&path);
    }

    /// Stands in for a load test: hammer the multi-connection pool with
    /// concurrent writers and readers and assert nothing errors and no write
    /// is lost. Proves WAL + per-connection busy_timeout safely absorb the
    /// write contention enabled by `max_connections > 1`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_reads_and_writes_do_not_error() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "forgekeep_concurrency_test_{}.db",
            std::process::id()
        ));
        discard_sqlite_test_files(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());

        let db = connect_with_pool(
            &url,
            TEST_CONNECT_TIMEOUT_SECS,
            DEFAULT_IDLE_TIMEOUT_SECS,
            DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .expect("connect");
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER NOT NULL)".to_string(),
        ))
        .await
        .expect("create table");

        const TASKS: i64 = 32;
        let mut handles = Vec::new();
        for n in 0..TASKS {
            let db = db.clone();
            handles.push(tokio::spawn(async move {
                // Write
                db.execute(Statement::from_string(
                    DatabaseBackend::Sqlite,
                    format!("INSERT INTO t (n) VALUES ({n})"),
                ))
                .await
                .expect("concurrent insert should not error");
                // Read
                db.query_one(Statement::from_string(
                    DatabaseBackend::Sqlite,
                    "SELECT COUNT(*) FROM t".to_string(),
                ))
                .await
                .expect("concurrent read should not error");
            }));
        }
        for h in handles {
            h.await.expect("task panicked");
        }

        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT COUNT(*) FROM t".to_string(),
            ))
            .await
            .expect("final count")
            .expect("one row");
        let count: i64 = row.try_get_by_index(0).expect("count value");
        assert_eq!(count, TASKS, "every concurrent write must be persisted");

        discard_sqlite_test_files(&path);
    }
}
