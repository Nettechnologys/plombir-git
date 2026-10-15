//! Plombir Git schema history: the migrations, the lock that serialises
//! migrators, and the pure helpers a data migration needs.
//!
//! This crate sits below `rg-db` and never depends on it. Before the split the
//! 145 migrations were three quarters of `rg-db`, so every edit to an op or an
//! entity recompiled them and everything downstream. `rg-db` re-exports what
//! callers use — `rg_db::run_migrations`, `rg_db::migrations`,
//! `rg_db::migration_lock`, `rg_db::package_version_key`, `rg_db::prepare_sql` —
//! so no caller had to change its path.
//!
//! A migration describes the schema as it was on its date. It therefore never
//! reaches for `rg-db`'s live entities: a data migration that needs typed
//! columns declares the few it reads ([`grant_tables`]).

pub mod grant_tables;
pub mod migration_lock;
pub mod migrations;
pub mod package_version_key;
#[doc(hidden)]
pub mod serialized_user_grants;

use anyhow::{Context, Result};
use sea_orm::DatabaseConnection;
use sea_orm_migration::MigratorTrait;

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

/// Run all pending migrations.
///
/// Every caller is serialised against every other process migrating the same
/// database, so the gate cannot be forgotten by the sixth call site the way it
/// would be if each one took it separately. On file-backed SQLite that
/// exclusion is the file lease the caller already holds (`rg_db`'s
/// `sqlite_process_guard`); on PostgreSQL and MySQL it is a lock inside the
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

#[cfg(test)]
mod tests {
    use super::prepare_sql;
    use sea_orm::DatabaseBackend;

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
}
