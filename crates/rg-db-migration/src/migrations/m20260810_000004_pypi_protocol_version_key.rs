//! Migration: give every parseable PyPI version one database-owned identity.
//!
//! PEP 440 declares spellings such as `v1.0`, `1.0.0`, `1.0-1` and `1.0.post1`
//! to be the same public version, and requires a public version to be unique
//! inside a distribution. The `(package_id, version)` UNIQUE index only ever
//! protected the raw text, so equivalent spellings could each take a row.
//!
//! The column and its UNIQUE index already exist — `m20260810_000003` added
//! them for NuGet. What is left is the PyPI half of the backfill, and the same
//! refusal to guess: two equivalent legacy rows can own different distribution
//! files, so the upgrade names both rather than merging them.

use std::collections::HashMap;

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement, Value};

use crate::package_version_key::Pep440Version;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(Debug)]
struct PyPIVersionRow {
    id: i64,
    package_id: i64,
    version: String,
    protocol_version_key: Option<String>,
}

async fn pypi_version_rows(manager: &SchemaManager<'_>) -> Result<Vec<PyPIVersionRow>, DbErr> {
    let db = manager.get_connection();
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT pv.id, pv.package_id, pv.version \
             FROM package_versions pv \
             JOIN packages p ON p.id = pv.package_id \
             JOIN package_registry pr ON pr.id = p.package_registry_id \
             WHERE pr.package_type = 'pypi' \
             ORDER BY pv.package_id, pv.id"
                .to_string(),
        ))
        .await?;

    rows.into_iter()
        .map(|row| {
            let version: String = row.try_get("", "version")?;
            Ok(PyPIVersionRow {
                id: row.try_get("", "id")?,
                package_id: row.try_get("", "package_id")?,
                protocol_version_key: Pep440Version::parse(&version)
                    .map(|version| version.canonical()),
                version,
            })
        })
        .collect()
}

fn reject_existing_duplicates(rows: &[PyPIVersionRow]) -> Result<(), DbErr> {
    let mut seen = HashMap::new();
    for row in rows {
        let Some(key) = row.protocol_version_key.as_deref() else {
            // Unparseable historical values have no valid PEP 440 identity.
            // They remain addressable by their old raw spelling.
            continue;
        };
        if let Some((previous_id, previous_version)) =
            seen.insert((row.package_id, key), (row.id, row.version.as_str()))
        {
            return Err(DbErr::Custom(format!(
                "package_versions contains equivalent PyPI rows for package_id {} and protocol_version_key {:?}: id {} version {:?}, id {} version {:?}; resolve the conflicting published distributions before retrying the migration",
                row.package_id,
                key,
                previous_id,
                previous_version,
                row.id,
                row.version
            )));
        }
    }
    Ok(())
}

async fn backfill_keys(manager: &SchemaManager<'_>, rows: &[PyPIVersionRow]) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = db.get_database_backend();
    for row in rows {
        let Some(key) = row.protocol_version_key.as_deref() else {
            continue;
        };
        db.execute(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(
                backend,
                "UPDATE package_versions SET protocol_version_key = ? WHERE id = ?",
            ),
            [Value::from(key), Value::from(row.id)],
        ))
        .await?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("package_versions").await?
            || !manager.has_table("packages").await?
            || !manager.has_table("package_registry").await?
            || !manager
                .has_column("package_versions", "protocol_version_key")
                .await?
        {
            return Ok(());
        }

        let rows = pypi_version_rows(manager).await?;
        reject_existing_duplicates(&rows)?;
        backfill_keys(manager, &rows).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The column and its index belong to the migration that created them.
        // Clearing the keys back to NULL would only re-open the window this
        // migration closed, so the reverse of a backfill is to leave it be.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{
        Database, DatabaseBackend, DatabaseConnection, QueryResult, SqlErr,
    };

    async fn fixture(rows: &str) -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(&format!(
            "CREATE TABLE package_registry (id BIGINT PRIMARY KEY, package_type VARCHAR(255) NOT NULL);\
             CREATE TABLE packages (id BIGINT PRIMARY KEY, package_registry_id BIGINT NOT NULL);\
             CREATE TABLE package_versions (id BIGINT PRIMARY KEY, package_id BIGINT NOT NULL, version VARCHAR(255) NOT NULL, protocol_version_key VARCHAR(255) NULL);\
             CREATE UNIQUE INDEX uq_package_versions_package_protocol_version_key ON package_versions (package_id, protocol_version_key);\
             INSERT INTO package_registry (id, package_type) VALUES (1, 'pypi'), (2, 'generic');\
             INSERT INTO packages (id, package_registry_id) VALUES (10, 1), (20, 2);\
             {rows}"
        ))
        .await
        .unwrap();
        db
    }

    fn string(row: &QueryResult, column: &str) -> Option<String> {
        row.try_get("", column).unwrap()
    }

    #[tokio::test]
    async fn backfills_pypi_keys_and_database_owns_the_uniqueness() {
        let db = fixture(
            "INSERT INTO package_versions (id, package_id, version) VALUES \
             (1, 10, 'V01.0.0-Beta.1'), \
             (2, 10, 'legacy row'), \
             (3, 20, 'V01.0.0-Beta.1');",
        )
        .await;
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT id, protocol_version_key FROM package_versions ORDER BY id".to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            string(&rows[0], "protocol_version_key").as_deref(),
            Some("1b1")
        );
        assert_eq!(string(&rows[1], "protocol_version_key"), None);
        assert_eq!(
            string(&rows[2], "protocol_version_key"),
            None,
            "a non-PyPI registry keeps its exact-text behavior"
        );

        let duplicate = db
            .execute_unprepared(
                "INSERT INTO package_versions \
                 (id, package_id, version, protocol_version_key) VALUES \
                 (4, 10, '1.0.0b1', '1b1')",
            )
            .await
            .unwrap_err();
        assert!(matches!(
            duplicate.sql_err(),
            Some(SqlErr::UniqueConstraintViolation(_))
        ));
    }

    #[tokio::test]
    async fn equivalent_legacy_rows_stop_before_any_write() {
        let db = fixture(
            "INSERT INTO package_versions (id, package_id, version) VALUES \
             (7, 10, '1.0'), (8, 10, 'v1.0.0');",
        )
        .await;
        let manager = SchemaManager::new(&db);

        let error = Migration.up(&manager).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("package_id 10"), "{message}");
        assert!(message.contains("protocol_version_key \"1\""), "{message}");
        assert!(message.contains("id 7 version \"1.0\""), "{message}");
        assert!(message.contains("id 8 version \"v1.0.0\""), "{message}");

        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT id, protocol_version_key FROM package_versions ORDER BY id".to_string(),
            ))
            .await
            .unwrap();
        assert!(
            rows.iter()
                .all(|row| string(row, "protocol_version_key").is_none()),
            "a refused migration wrote a key anyway"
        );
    }
}
