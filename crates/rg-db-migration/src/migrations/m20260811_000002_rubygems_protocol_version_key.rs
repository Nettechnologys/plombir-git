//! Migration: give every RubyGems `(version, platform)` one database identity.
//!
//! `Gem::Version` compares canonical numeric/text segments, so raw spellings
//! such as `1.0` and `1.0.0` are aliases. Platform is the other half of the
//! release identity: `1.0.0-ruby` and `1.0.0-java` must remain distinct.
//!
//! The protocol key column already exists, but the original UNIQUE index on
//! `(package_id, version)` cannot represent two platform builds with the same
//! raw number. This migration backfills the composite protocol identity and
//! replaces that raw index with `(package_id, version, protocol_variant_key)`.
//! Existing collisions or rows whose platform cannot be recovered are refused
//! before DDL, because choosing `ruby` could merge a native artifact silently.

use std::collections::HashMap;

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement, Value};

use crate::package_version_key::{rubygems_platform_from_metadata, rubygems_version_key};

const OLD_RAW_UNIQUE_INDEX: &str = "idx_package_version";
const RAW_VARIANT_UNIQUE_INDEX: &str = "uq_package_versions_package_version_variant";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(Debug)]
struct RubyGemsVersionRow {
    id: i64,
    package_id: i64,
    version: String,
    platform: String,
    protocol_version_key: Option<String>,
}

fn platform_from_filename(filename: &str, package_name: &str, version: &str) -> Option<String> {
    let stem = filename.strip_suffix(".gem")?;
    let base = format!("{package_name}-{version}");
    let suffix = stem.strip_prefix(&base)?;
    if suffix.is_empty() {
        return Some("ruby".to_string());
    }
    suffix
        .strip_prefix('-')
        .filter(|platform| !platform.is_empty())
        .map(str::to_string)
}

async fn rubygems_version_rows(
    manager: &SchemaManager<'_>,
) -> Result<Vec<RubyGemsVersionRow>, DbErr> {
    let db = manager.get_connection();
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT pv.id, pv.package_id, p.name AS package_name, pv.version, pv.metadata, \
                    (SELECT pf.filename FROM package_files pf \
                     WHERE pf.version_id = pv.id AND pf.filename LIKE '%.gem' \
                     ORDER BY pf.id LIMIT 1) AS filename \
             FROM package_versions pv \
             JOIN packages p ON p.id = pv.package_id \
             JOIN package_registry pr ON pr.id = p.package_registry_id \
             WHERE pr.package_type = 'rubygems' \
             ORDER BY pv.package_id, pv.id"
                .to_string(),
        ))
        .await?;

    rows.into_iter()
        .map(|row| {
            let id: i64 = row.try_get("", "id")?;
            let package_id: i64 = row.try_get("", "package_id")?;
            let package_name: String = row.try_get("", "package_name")?;
            let version: String = row.try_get("", "version")?;
            let metadata: Option<String> = row.try_get("", "metadata")?;
            let filename: Option<String> = row.try_get("", "filename")?;
            let platform = metadata
                .as_deref()
                .and_then(rubygems_platform_from_metadata)
                .or_else(|| {
                    filename.as_deref().and_then(|filename| {
                        platform_from_filename(filename, &package_name, &version)
                    })
                })
                .ok_or_else(|| {
                    DbErr::Custom(format!(
                        "cannot determine the RubyGems platform for package_version id {id}, \
                         package_id {package_id}, package {package_name:?}, version {version:?}; \
                         restore readable protocol metadata or a canonical .gem filename before \
                         retrying the migration"
                    ))
                })?;
            let protocol_version_key = rubygems_version_key(&version, &platform);
            Ok(RubyGemsVersionRow {
                id,
                package_id,
                version,
                platform,
                protocol_version_key,
            })
        })
        .collect()
}

fn reject_existing_duplicates(rows: &[RubyGemsVersionRow]) -> Result<(), DbErr> {
    let mut seen = HashMap::new();
    for row in rows {
        let Some(key) = row.protocol_version_key.as_deref() else {
            // An invalid historical version is not a Gem::Version identity. It
            // retains exact-text uniqueness inside its recovered platform.
            continue;
        };
        if let Some((previous_id, previous_version, previous_platform)) = seen.insert(
            (row.package_id, key),
            (row.id, row.version.as_str(), row.platform.as_str()),
        ) {
            return Err(DbErr::Custom(format!(
                "package_versions contains equivalent RubyGems rows for package_id {} and \
                 protocol_version_key {:?}: id {} version {:?} platform {:?}, id {} version \
                 {:?} platform {:?}; resolve the conflicting published gems before retrying \
                 the migration",
                row.package_id,
                key,
                previous_id,
                previous_version,
                previous_platform,
                row.id,
                row.version,
                row.platform
            )));
        }
    }
    Ok(())
}

async fn backfill_keys(
    manager: &SchemaManager<'_>,
    rows: &[RubyGemsVersionRow],
) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = db.get_database_backend();
    for row in rows {
        db.execute(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(
                backend,
                "UPDATE package_versions \
                 SET protocol_version_key = ?, protocol_variant_key = ? WHERE id = ?",
            ),
            [
                Value::from(row.protocol_version_key.clone()),
                Value::from(row.platform.as_str()),
                Value::from(row.id),
            ],
        ))
        .await?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("package_versions").await?
            || !manager.has_table("package_files").await?
            || !manager.has_table("packages").await?
            || !manager.has_table("package_registry").await?
            || !manager
                .has_column("package_versions", "protocol_version_key")
                .await?
        {
            return Ok(());
        }

        // MySQL DDL auto-commits. Resolve every platform and detect unsafe
        // canonical collisions before adding the first column.
        let rows = rubygems_version_rows(manager).await?;
        reject_existing_duplicates(&rows)?;

        if !manager
            .has_column("package_versions", "protocol_variant_key")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(PackageVersions::Table)
                        .add_column(
                            ColumnDef::new(PackageVersions::ProtocolVariantKey)
                                .string()
                                .not_null()
                                .default(""),
                        )
                        .to_owned(),
                )
                .await?;
        }

        backfill_keys(manager, &rows).await?;
        if !manager
            .has_index("package_versions", RAW_VARIANT_UNIQUE_INDEX)
            .await?
        {
            manager
                .create_index(
                    Index::create()
                        .name(RAW_VARIANT_UNIQUE_INDEX)
                        .table(PackageVersions::Table)
                        .col(PackageVersions::PackageId)
                        .col(PackageVersions::Version)
                        .col(PackageVersions::ProtocolVariantKey)
                        .unique()
                        .to_owned(),
                )
                .await?;
        }
        if manager
            .has_index("package_versions", OLD_RAW_UNIQUE_INDEX)
            .await?
        {
            manager
                .drop_index(
                    Index::drop()
                        .name(OLD_RAW_UNIQUE_INDEX)
                        .table(PackageVersions::Table)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // A platform sibling published after this migration makes the old raw
        // UNIQUE index impossible to restore. Leaving the stronger composite
        // identities in place is safer than a lossy or partially-applied down.
        Ok(())
    }
}

#[derive(Iden)]
enum PackageVersions {
    Table,
    PackageId,
    Version,
    ProtocolVariantKey,
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
             CREATE TABLE package_versions (id BIGINT PRIMARY KEY, package_id BIGINT NOT NULL, version VARCHAR(255) NOT NULL, metadata TEXT NULL, protocol_version_key VARCHAR(255) NULL);\
             CREATE TABLE package_files (id BIGINT PRIMARY KEY, version_id BIGINT NOT NULL, filename VARCHAR(255) NOT NULL);\
             CREATE UNIQUE INDEX idx_package_version ON package_versions (package_id, version);\
             CREATE UNIQUE INDEX uq_package_versions_package_protocol_version_key ON package_versions (package_id, protocol_version_key);\
             INSERT INTO package_registry (id, package_type) VALUES (1, 'rubygems'), (2, 'generic');\
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
    async fn backfills_composite_keys_and_allows_platform_siblings() {
        let db = fixture(
            r#"INSERT INTO package_versions (id, package_id, version, metadata) VALUES
               (1, 10, '1.0.0', '{"dependencies":[]}'),
               (2, 10, '2.0.0', '{"platform":"java"}'),
               (3, 10, '3.0.0', NULL),
               (4, 20, '1.0.0', NULL);
               INSERT INTO package_files (id, version_id, filename) VALUES
               (30, 3, 'matrix-3.0.0-x86_64-linux.gem');"#,
        )
        .await;
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT id, protocol_version_key, protocol_variant_key \
                 FROM package_versions ORDER BY id"
                    .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            string(&rows[0], "protocol_version_key").as_deref(),
            Some("n1|p:ruby")
        );
        assert_eq!(
            string(&rows[0], "protocol_variant_key").as_deref(),
            Some("ruby")
        );
        assert_eq!(
            string(&rows[1], "protocol_version_key").as_deref(),
            Some("n2|p:java")
        );
        assert_eq!(
            string(&rows[1], "protocol_variant_key").as_deref(),
            Some("java")
        );
        assert_eq!(
            string(&rows[2], "protocol_variant_key").as_deref(),
            Some("x86_64-linux")
        );
        assert_eq!(string(&rows[3], "protocol_version_key"), None);
        assert_eq!(
            string(&rows[3], "protocol_variant_key").as_deref(),
            Some("")
        );
        assert!(!manager
            .has_index("package_versions", OLD_RAW_UNIQUE_INDEX)
            .await
            .unwrap());
        assert!(manager
            .has_index("package_versions", RAW_VARIANT_UNIQUE_INDEX)
            .await
            .unwrap());

        // Same raw version, different platform: two legitimate releases.
        db.execute_unprepared(
            r#"INSERT INTO package_versions
               (id, package_id, version, metadata, protocol_version_key, protocol_variant_key)
               VALUES (5, 10, '1.0.0', '{"platform":"java"}', 'n1|p:java', 'java')"#,
        )
        .await
        .unwrap();

        let alias = db
            .execute_unprepared(
                r#"INSERT INTO package_versions
                   (id, package_id, version, metadata, protocol_version_key, protocol_variant_key)
                   VALUES (6, 10, '1.0', '{}', 'n1|p:ruby', 'ruby')"#,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            alias.sql_err(),
            Some(SqlErr::UniqueConstraintViolation(_))
        ));

        let raw_duplicate = db
            .execute_unprepared(
                "INSERT INTO package_versions \
                 (id, package_id, version, metadata, protocol_version_key, protocol_variant_key) \
                 VALUES (7, 20, '1.0.0', NULL, NULL, '')",
            )
            .await
            .unwrap_err();
        assert!(matches!(
            raw_duplicate.sql_err(),
            Some(SqlErr::UniqueConstraintViolation(_))
        ));
    }

    #[tokio::test]
    async fn equivalent_legacy_rows_stop_before_schema_mutation() {
        let db = fixture(
            r#"INSERT INTO package_versions (id, package_id, version, metadata) VALUES
               (7, 10, '1.0', '{}'), (8, 10, '1.0.0', '{}');"#,
        )
        .await;
        let manager = SchemaManager::new(&db);

        let error = Migration.up(&manager).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("package_id 10"), "{message}");
        assert!(
            message.contains("protocol_version_key \"n1|p:ruby\""),
            "{message}"
        );
        assert!(
            message.contains("id 7 version \"1.0\" platform \"ruby\""),
            "{message}"
        );
        assert!(
            message.contains("id 8 version \"1.0.0\" platform \"ruby\""),
            "{message}"
        );
        assert!(!manager
            .has_column("package_versions", "protocol_variant_key")
            .await
            .unwrap());
        assert!(manager
            .has_index("package_versions", OLD_RAW_UNIQUE_INDEX)
            .await
            .unwrap());

        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT protocol_version_key FROM package_versions".to_string(),
            ))
            .await
            .unwrap();
        assert!(rows
            .iter()
            .all(|row| string(row, "protocol_version_key").is_none()));
    }

    #[tokio::test]
    async fn unknown_legacy_platform_stops_before_schema_mutation() {
        let db = fixture(
            "INSERT INTO package_versions (id, package_id, version, metadata) \
             VALUES (9, 10, '4.0.0', NULL); \
             INSERT INTO package_files (id, version_id, filename) \
             VALUES (90, 9, 'package');",
        )
        .await;
        let manager = SchemaManager::new(&db);

        let error = Migration.up(&manager).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("package_version id 9"), "{message}");
        assert!(message.contains("version \"4.0.0\""), "{message}");
        assert!(!manager
            .has_column("package_versions", "protocol_variant_key")
            .await
            .unwrap());
    }
}
