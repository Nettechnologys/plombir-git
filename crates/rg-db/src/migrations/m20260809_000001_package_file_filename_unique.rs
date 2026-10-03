//! Migration: make a package filename unique within its version.
//!
//! `add_files_to_version` used to enforce this with a read before the insert.
//! Two concurrent publishes could both pass that read, so the database must be
//! the final arbiter. Existing duplicates are not safe to choose between: the
//! rows may point at different bytes and prior downloads did not establish a
//! deterministic winner. Refuse the upgrade with the exact key instead of
//! silently deleting published metadata or orphaning one of its blobs.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

const UNIQUE_INDEX: &str = "uq_package_files_version_filename";

#[derive(DeriveMigrationName)]
pub struct Migration;

async fn reject_existing_duplicates(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = db.get_database_backend();
    let duplicate = db
        .query_one(Statement::from_string(
            backend,
            "SELECT version_id, filename FROM package_files \
             GROUP BY version_id, filename HAVING COUNT(*) > 1 \
             ORDER BY version_id, filename LIMIT 1"
                .to_string(),
        ))
        .await?;

    if let Some(duplicate) = duplicate {
        let version_id: i64 = duplicate.try_get("", "version_id")?;
        let filename: String = duplicate.try_get("", "filename")?;
        return Err(DbErr::Custom(format!(
            "package_files contains duplicate (version_id, filename) = ({version_id}, {filename:?}); resolve the conflicting published files before retrying the migration"
        )));
    }

    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("package_files").await? {
            return Ok(());
        }

        reject_existing_duplicates(manager).await?;
        if !manager.has_index("package_files", UNIQUE_INDEX).await? {
            manager
                .create_index(
                    Index::create()
                        .name(UNIQUE_INDEX)
                        .table(PackageFiles::Table)
                        .col(PackageFiles::VersionId)
                        .col(PackageFiles::Filename)
                        .unique()
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_index("package_files", UNIQUE_INDEX).await? {
            manager
                .drop_index(
                    Index::drop()
                        .name(UNIQUE_INDEX)
                        .table(PackageFiles::Table)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Iden)]
enum PackageFiles {
    Table,
    VersionId,
    Filename,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{Database, SqlErr};

    /// Optional disposable server database for the cross-backend acceptance
    /// run. Ordinary `cargo test` keeps using an in-memory SQLite database.
    const TEST_DATABASE_URL_ENV: &str = "PLOMBIR_GIT_PACKAGE_FILE_MIGRATION_TEST_DATABASE_URL";

    #[tokio::test]
    async fn one_filename_per_version_is_owned_by_the_database() {
        let server_database_url = std::env::var(TEST_DATABASE_URL_ENV).ok();
        let database_url = server_database_url
            .clone()
            .unwrap_or_else(|| "sqlite::memory:".to_string());
        let db = Database::connect(&database_url).await.unwrap();
        if server_database_url.is_some() {
            // The opt-in URL is documented as disposable. CI runs this after
            // the shared backend smoke, so replacing this one child table
            // cannot hide a failure from a later step.
            db.execute_unprepared("DROP TABLE IF EXISTS package_files")
                .await
                .unwrap();
        }
        db.execute_unprepared(
            "CREATE TABLE package_files (\
                id BIGINT PRIMARY KEY, \
                version_id BIGINT NOT NULL, \
                filename VARCHAR(255) NOT NULL, \
                size BIGINT NOT NULL DEFAULT 0, \
                storage_path VARCHAR(255) NOT NULL, \
                created_at TIMESTAMP NOT NULL\
            );\
            INSERT INTO package_files \
                (id, version_id, filename, size, storage_path, created_at) VALUES \
                (1, 7, 'crate.bin', 3, 'objects/one/crate.bin', CURRENT_TIMESTAMP);",
        )
        .await
        .unwrap();

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        let duplicate = db
            .execute_unprepared(
                "INSERT INTO package_files \
                    (id, version_id, filename, size, storage_path, created_at) VALUES \
                    (2, 7, 'crate.bin', 4, 'objects/two/crate.bin', CURRENT_TIMESTAMP)",
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                duplicate.sql_err(),
                Some(SqlErr::UniqueConstraintViolation(_))
            ),
            "duplicate filename was not rejected as a UNIQUE violation: {duplicate:#}"
        );

        db.execute_unprepared(
            "INSERT INTO package_files \
                (id, version_id, filename, size, storage_path, created_at) VALUES \
                (3, 7, 'sources.bin', 5, 'objects/three/sources.bin', CURRENT_TIMESTAMP), \
                (4, 8, 'crate.bin', 6, 'objects/four/crate.bin', CURRENT_TIMESTAMP)",
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_existing_duplicate_stops_the_upgrade_with_the_conflicting_key() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE package_files (\
                id BIGINT PRIMARY KEY, \
                version_id BIGINT NOT NULL, \
                filename VARCHAR(255) NOT NULL\
            );\
            INSERT INTO package_files (id, version_id, filename) VALUES \
                (1, 9, 'duplicate.bin'), (2, 9, 'duplicate.bin');",
        )
        .await
        .unwrap();

        let error = Migration.up(&SchemaManager::new(&db)).await.unwrap_err();
        assert!(matches!(error, DbErr::Custom(_)));
        let message = error.to_string();
        assert!(message.contains("version_id, filename"), "{message}");
        assert!(message.contains("9"), "{message}");
        assert!(message.contains("duplicate.bin"), "{message}");
    }
}
