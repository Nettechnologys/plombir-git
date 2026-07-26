//! Corrective migration: `m20260607_000001_create_mirrors` used a bare
//! `#[derive(Iden)] enum Mirror`, which names the table `mirror` (singular),
//! while `entities::mirror` reads `mirrors`. Every mirror query therefore
//! failed at runtime with `no such table: mirrors` — the whole mirror feature
//! answered 500.
//!
//! This mirrors the package / org / team corrective migrations: rename the
//! broken singular table only when it exists and the plural one does not, so
//! the migration is a no-op on databases created after the `#[iden]` fix.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260726_000001_rename_mirror_table_plural"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("mirror").await? && !manager.has_table("mirrors").await? {
            manager
                .rename_table(
                    Table::rename()
                        .table(Alias::new("mirror"), Alias::new("mirrors"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("mirrors").await? && !manager.has_table("mirror").await? {
            manager
                .rename_table(
                    Table::rename()
                        .table(Alias::new("mirrors"), Alias::new("mirror"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn renames_the_legacy_singular_table_and_keeps_its_rows() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE mirror (id INTEGER PRIMARY KEY, repo_id BIGINT NOT NULL, url TEXT NOT NULL);\
             INSERT INTO mirror (id, repo_id, url) VALUES (3, 42, 'https://example.com/x.git');",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let manager = SchemaManager::new(&db);
        assert!(!manager.has_table("mirror").await.unwrap());
        assert!(manager.has_table("mirrors").await.unwrap());
        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT id, repo_id, url FROM mirrors".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<i64>("", "id").unwrap(), 3);
        assert_eq!(row.try_get::<i64>("", "repo_id").unwrap(), 42);
        assert_eq!(
            row.try_get::<String>("", "url").unwrap(),
            "https://example.com/x.git"
        );
    }

    /// A database created after the `#[iden = "mirrors"]` fix already has the
    /// right name and no `mirror` table at all — the rename must not fire.
    #[tokio::test]
    async fn is_a_no_op_on_a_database_that_already_has_mirrors() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE mirrors (id INTEGER PRIMARY KEY, repo_id BIGINT NOT NULL, url TEXT NOT NULL);\
             INSERT INTO mirrors (id, repo_id, url) VALUES (1, 1, 'https://example.com/y.git');",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let manager = SchemaManager::new(&db);
        assert!(manager.has_table("mirrors").await.unwrap());
        assert!(!manager.has_table("mirror").await.unwrap());
        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM mirrors".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<i64>("", "n").unwrap(), 1);
    }
}
