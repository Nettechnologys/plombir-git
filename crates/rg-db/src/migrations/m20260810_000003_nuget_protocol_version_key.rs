//! Migration: give every parseable NuGet version one database-owned identity.
//!
//! NuGet considers spellings such as `1`, `1.0.0`, and `1.0.0.0+build` to be
//! the same version. The original `(package_id, version)` UNIQUE index only
//! protected the raw text, so concurrent equivalent publishes could both win.
//! Existing collisions are not safe to merge: each row can own different
//! files. Refuse the upgrade with both rows instead of choosing an artifact.

use std::collections::HashMap;

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement, Value};

use crate::package_version_key::NuGetVersion;

const UNIQUE_INDEX: &str = "uq_package_versions_package_protocol_version_key";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(Debug)]
struct NuGetVersionRow {
    id: i64,
    package_id: i64,
    version: String,
    protocol_version_key: Option<String>,
}

async fn nuget_version_rows(manager: &SchemaManager<'_>) -> Result<Vec<NuGetVersionRow>, DbErr> {
    let db = manager.get_connection();
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT pv.id, pv.package_id, pv.version \
             FROM package_versions pv \
             JOIN packages p ON p.id = pv.package_id \
             JOIN package_registry pr ON pr.id = p.package_registry_id \
             WHERE pr.package_type = 'nuget' \
             ORDER BY pv.package_id, pv.id"
                .to_string(),
        ))
        .await?;

    rows.into_iter()
        .map(|row| {
            let version: String = row.try_get("", "version")?;
            Ok(NuGetVersionRow {
                id: row.try_get("", "id")?,
                package_id: row.try_get("", "package_id")?,
                protocol_version_key: NuGetVersion::parse(&version)
                    .map(|version| version.normalized()),
                version,
            })
        })
        .collect()
}

fn reject_existing_duplicates(rows: &[NuGetVersionRow]) -> Result<(), DbErr> {
    let mut seen = HashMap::new();
    for row in rows {
        let Some(key) = row.protocol_version_key.as_deref() else {
            // Unparseable historical values have no valid normalized NuGet
            // identity. They remain addressable by their old raw spelling.
            continue;
        };
        if let Some((previous_id, previous_version)) =
            seen.insert((row.package_id, key), (row.id, row.version.as_str()))
        {
            return Err(DbErr::Custom(format!(
                "package_versions contains equivalent NuGet rows for package_id {} and protocol_version_key {:?}: id {} version {:?}, id {} version {:?}; resolve the conflicting published artifacts before retrying the migration",
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

async fn backfill_keys(manager: &SchemaManager<'_>, rows: &[NuGetVersionRow]) -> Result<(), DbErr> {
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
        {
            return Ok(());
        }

        // Detect unsafe legacy state before the first DDL statement. MySQL DDL
        // auto-commits, so doing this after ADD COLUMN would leave a half-applied
        // migration on the exact upgrade that needs operator intervention.
        let rows = nuget_version_rows(manager).await?;
        reject_existing_duplicates(&rows)?;

        if !manager
            .has_column("package_versions", "protocol_version_key")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PackageVersions::Table)
                        .add_column(
                            ColumnDef::new(PackageVersions::ProtocolVersionKey)
                                .string()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        backfill_keys(manager, &rows).await?;
        if !manager.has_index("package_versions", UNIQUE_INDEX).await? {
            manager
                .create_index(
                    Index::create()
                        .name(UNIQUE_INDEX)
                        .table(PackageVersions::Table)
                        .col(PackageVersions::PackageId)
                        .col(PackageVersions::ProtocolVersionKey)
                        .unique()
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_index("package_versions", UNIQUE_INDEX).await? {
            manager
                .drop_index(
                    Index::drop()
                        .name(UNIQUE_INDEX)
                        .table(PackageVersions::Table)
                        .to_owned(),
                )
                .await?;
        }
        if manager
            .has_column("package_versions", "protocol_version_key")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PackageVersions::Table)
                        .drop_column(PackageVersions::ProtocolVersionKey)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Iden)]
enum PackageVersions {
    Table,
    PackageId,
    ProtocolVersionKey,
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
             CREATE TABLE package_versions (id BIGINT PRIMARY KEY, package_id BIGINT NOT NULL, version VARCHAR(255) NOT NULL);\
             INSERT INTO package_registry (id, package_type) VALUES (1, 'nuget'), (2, 'generic');\
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
    async fn backfills_nuget_keys_and_database_owns_the_uniqueness() {
        let db = fixture(
            "INSERT INTO package_versions (id, package_id, version) VALUES \
             (1, 10, '01.00.000.0-RC+Build.7'), \
             (2, 10, 'legacy-row'), \
             (3, 20, '01.00.000.0-RC+Build.7');",
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
            Some("1.0.0-rc")
        );
        assert_eq!(string(&rows[1], "protocol_version_key"), None);
        assert_eq!(string(&rows[2], "protocol_version_key"), None);

        let duplicate = db
            .execute_unprepared(
                "INSERT INTO package_versions \
                 (id, package_id, version, protocol_version_key) VALUES \
                 (4, 10, '1.0.0-rc+other', '1.0.0-rc')",
            )
            .await
            .unwrap_err();
        assert!(matches!(
            duplicate.sql_err(),
            Some(SqlErr::UniqueConstraintViolation(_))
        ));

        db.execute_unprepared(
            "INSERT INTO package_versions \
             (id, package_id, version, protocol_version_key) VALUES \
             (5, 20, 'other-one', NULL), (6, 20, 'other-two', NULL)",
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn equivalent_legacy_rows_stop_before_schema_mutation() {
        let db = fixture(
            "INSERT INTO package_versions (id, package_id, version) VALUES \
             (7, 10, '1'), (8, 10, '1.0.0+Build.8');",
        )
        .await;
        let manager = SchemaManager::new(&db);

        let error = Migration.up(&manager).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("package_id 10"), "{message}");
        assert!(
            message.contains("protocol_version_key \"1.0.0\""),
            "{message}"
        );
        assert!(message.contains("id 7 version \"1\""), "{message}");
        assert!(
            message.contains("id 8 version \"1.0.0+Build.8\""),
            "{message}"
        );
        assert!(!manager
            .has_column("package_versions", "protocol_version_key")
            .await
            .unwrap());
    }
}
