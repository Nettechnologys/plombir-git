//! Drop three storage metadata columns with writers but no readers.
//!
//! `lfs_objects.compression` and `compressed_size` duplicated facts already
//! encoded by the blob key. The download path probes the compressed key first
//! and carries that result alongside the bytes; no production path consulted
//! either column. `ci_cache_entries.last_accessed_at` duplicated the timestamp
//! used to extend `expires_at`, while retention selects only by `expires_at`.
//!
//! `down` can restore the old shape, but not values nothing consumed. Existing
//! cache rows receive a fixed non-null timestamp so the restored SeaORM entity
//! remains readable on every supported backend.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260810_000001_drop_unused_storage_metadata"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("lfs_objects", "compression").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(LfsObjects::Table)
                        .drop_column(LfsObjects::Compression)
                        .to_owned(),
                )
                .await?;
        }
        if manager.has_column("lfs_objects", "compressed_size").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(LfsObjects::Table)
                        .drop_column(LfsObjects::CompressedSize)
                        .to_owned(),
                )
                .await?;
        }
        if manager
            .has_column("ci_cache_entries", "last_accessed_at")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(CiCacheEntries::Table)
                        .drop_column(CiCacheEntries::LastAccessedAt)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("lfs_objects", "compression").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(LfsObjects::Table)
                        .add_column(ColumnDef::new(LfsObjects::Compression).string().null())
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column("lfs_objects", "compressed_size").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(LfsObjects::Table)
                        .add_column(
                            ColumnDef::new(LfsObjects::CompressedSize)
                                .big_integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("ci_cache_entries", "last_accessed_at")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(CiCacheEntries::Table)
                        .add_column(
                            ColumnDef::new(CiCacheEntries::LastAccessedAt)
                                .timestamp_with_time_zone()
                                .not_null()
                                .default("1970-01-01 00:00:00+00:00"),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum LfsObjects {
    Table,
    Compression,
    CompressedSize,
}

#[derive(DeriveIden)]
enum CiCacheEntries {
    Table,
    LastAccessedAt,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    const SCHEMA: &str = "\
        CREATE TABLE lfs_objects (\
            id INTEGER PRIMARY KEY, oid TEXT NOT NULL, compression TEXT, compressed_size BIGINT\
        );\
        INSERT INTO lfs_objects (id, oid, compression, compressed_size)\
            VALUES (1, 'abc', 'zstd', 7);\
        CREATE TABLE ci_cache_entries (\
            id INTEGER PRIMARY KEY, key_hash TEXT NOT NULL,\
            last_accessed_at TEXT NOT NULL, expires_at TEXT NOT NULL\
        );\
        INSERT INTO ci_cache_entries (id, key_hash, last_accessed_at, expires_at)\
            VALUES (1, 'cache', '2026-08-10T00:00:00Z', '2026-08-17T00:00:00Z');";

    async fn fixture() -> sea_orm_migration::sea_orm::DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(SCHEMA).await.unwrap();
        db
    }

    #[tokio::test]
    async fn drops_all_three_columns_without_losing_rows() {
        let db = fixture().await;
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        assert!(!manager
            .has_column("lfs_objects", "compression")
            .await
            .unwrap());
        assert!(!manager
            .has_column("lfs_objects", "compressed_size")
            .await
            .unwrap());
        assert!(!manager
            .has_column("ci_cache_entries", "last_accessed_at")
            .await
            .unwrap());

        let lfs = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT oid FROM lfs_objects WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lfs.try_get::<String>("", "oid").unwrap(), "abc");

        let cache = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT key_hash FROM ci_cache_entries WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cache.try_get::<String>("", "key_hash").unwrap(), "cache");
    }

    #[tokio::test]
    async fn down_restores_a_readable_shape_for_existing_rows() {
        let db = fixture().await;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();

        Migration.down(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();

        assert!(manager
            .has_column("lfs_objects", "compression")
            .await
            .unwrap());
        assert!(manager
            .has_column("lfs_objects", "compressed_size")
            .await
            .unwrap());
        assert!(manager
            .has_column("ci_cache_entries", "last_accessed_at")
            .await
            .unwrap());

        let cache = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS present FROM ci_cache_entries \
                 WHERE last_accessed_at IS NOT NULL"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cache.try_get::<i64>("", "present").unwrap(), 1);
    }
}
