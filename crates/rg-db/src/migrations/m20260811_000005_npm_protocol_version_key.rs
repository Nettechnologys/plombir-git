//! Migration: give every node-semver-normalizable npm version one client identity.
//!
//! npm cleans package versions through node-semver before publishing and drops
//! build metadata from the resulting `.version`. The HTTP endpoint also accepts
//! hand-built packuments, so raw spellings such as `1.0.0+a`, `1.0.0+b` and
//! `v01.0.0` may already coexist. Refuse such legacy collisions before writing
//! any key; their tarballs may differ and choosing one would discard data.

use std::collections::HashMap;

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement, Value};

use crate::package_version_key::npm_version_key;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(Debug)]
struct NpmVersionRow {
    id: i64,
    package_id: i64,
    package_name: String,
    version: String,
    protocol_version_key: Option<String>,
}

async fn npm_version_rows(manager: &SchemaManager<'_>) -> Result<Vec<NpmVersionRow>, DbErr> {
    let db = manager.get_connection();
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT pv.id, pv.package_id, p.name AS package_name, pv.version \
             FROM package_versions pv \
             JOIN packages p ON p.id = pv.package_id \
             JOIN package_registry pr ON pr.id = p.package_registry_id \
             WHERE pr.package_type = 'npm' \
             ORDER BY pv.package_id, pv.id"
                .to_string(),
        ))
        .await?;

    rows.into_iter()
        .map(|row| {
            let version: String = row.try_get("", "version")?;
            Ok(NpmVersionRow {
                id: row.try_get("", "id")?,
                package_id: row.try_get("", "package_id")?,
                package_name: row.try_get("", "package_name")?,
                protocol_version_key: npm_version_key(&version),
                version,
            })
        })
        .collect()
}

fn reject_existing_duplicates(rows: &[NpmVersionRow]) -> Result<(), DbErr> {
    let mut seen = HashMap::new();
    for row in rows {
        let Some(key) = row.protocol_version_key.as_deref() else {
            // An invalid historical spelling has no npm resolver identity and
            // remains addressable through its exact raw version.
            continue;
        };
        if let Some((previous_id, previous_version)) =
            seen.insert((row.package_id, key), (row.id, row.version.as_str()))
        {
            return Err(DbErr::Custom(format!(
                "package_versions contains equivalent npm rows for package_id {} package {:?} and protocol_version_key {:?}: id {} spelling {:?}, id {} spelling {:?}; resolve the conflicting published tarballs before retrying the migration",
                row.package_id,
                row.package_name,
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

async fn backfill_keys(manager: &SchemaManager<'_>, rows: &[NpmVersionRow]) -> Result<(), DbErr> {
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

        let rows = npm_version_rows(manager).await?;
        reject_existing_duplicates(&rows)?;
        backfill_keys(manager, &rows).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Clearing npm identities would reopen both sequential and concurrent
        // aliases while the shared column/index remain in use.
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
             CREATE TABLE packages (id BIGINT PRIMARY KEY, package_registry_id BIGINT NOT NULL, name VARCHAR(255) NOT NULL);\
             CREATE TABLE package_versions (id BIGINT PRIMARY KEY, package_id BIGINT NOT NULL, version VARCHAR(255) NOT NULL, protocol_version_key VARCHAR(255) NULL);\
             CREATE UNIQUE INDEX uq_package_versions_package_protocol_version_key ON package_versions (package_id, protocol_version_key);\
             INSERT INTO package_registry (id, package_type) VALUES (1, 'npm'), (2, 'generic');\
             INSERT INTO packages (id, package_registry_id, name) VALUES (10, 1, 'matrix'), (20, 2, 'generic');\
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
    async fn backfills_npm_keys_and_database_owns_the_uniqueness() {
        let db = fixture(
            "INSERT INTO package_versions (id, package_id, version) VALUES \
             (1, 10, 'v01.2.0+Build.7'), \
             (2, 10, 'legacy row'), \
             (3, 20, 'v01.2.0+Build.7');",
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
            Some("1.2.0")
        );
        assert_eq!(string(&rows[1], "protocol_version_key"), None);
        assert_eq!(string(&rows[2], "protocol_version_key"), None);

        let duplicate = db
            .execute_unprepared(
                "INSERT INTO package_versions \
                 (id, package_id, version, protocol_version_key) VALUES \
                 (4, 10, '1.2.0+other', '1.2.0')",
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
             (7, 10, '1.0.0+a'), (8, 10, 'v01.0.0+b');",
        )
        .await;
        let manager = SchemaManager::new(&db);

        let error = Migration.up(&manager).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("package_id 10"), "{message}");
        assert!(message.contains("package \"matrix\""), "{message}");
        assert!(
            message.contains("protocol_version_key \"1.0.0\""),
            "{message}"
        );
        assert!(message.contains("id 7 spelling \"1.0.0+a\""), "{message}");
        assert!(message.contains("id 8 spelling \"v01.0.0+b\""), "{message}");

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
