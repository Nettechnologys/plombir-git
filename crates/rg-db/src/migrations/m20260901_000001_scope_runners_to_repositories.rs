//! Bind every usable external runner credential to one repository.
//!
//! Existing runners deliberately receive `NULL`: before this migration there
//! is no trustworthy repository to backfill from. Runtime authentication treats
//! that legacy state as revoked and tells the operator to re-register. Guessing
//! from the last assigned job would preserve the very ambient authority this
//! migration removes.

use sea_orm::{ConnectionTrait, DatabaseBackend};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let statement = match manager.get_database_backend() {
            DatabaseBackend::Sqlite => {
                "ALTER TABLE runners ADD COLUMN repo_id INTEGER NULL \
                 REFERENCES repositories(id) ON DELETE CASCADE"
            }
            DatabaseBackend::Postgres => {
                "ALTER TABLE runners ADD COLUMN repo_id BIGINT NULL \
                 REFERENCES repositories(id) ON DELETE CASCADE"
            }
            DatabaseBackend::MySql => {
                "ALTER TABLE runners ADD COLUMN repo_id BIGINT NULL, \
                 ADD CONSTRAINT fk_runners_repo_id FOREIGN KEY (repo_id) \
                 REFERENCES repositories(id) ON DELETE CASCADE"
            }
        };
        manager
            .get_connection()
            .execute_unprepared(statement)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let statement = match manager.get_database_backend() {
            DatabaseBackend::Sqlite | DatabaseBackend::Postgres => {
                "ALTER TABLE runners DROP COLUMN repo_id"
            }
            DatabaseBackend::MySql => {
                "ALTER TABLE runners DROP FOREIGN KEY fk_runners_repo_id, \
                 DROP COLUMN repo_id"
            }
        };
        manager
            .get_connection()
            .execute_unprepared(statement)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};

    use super::*;

    async fn fixture() -> sea_orm::DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared("PRAGMA foreign_keys = ON")
            .await
            .unwrap();
        for statement in [
            "CREATE TABLE repositories (id INTEGER PRIMARY KEY)",
            "CREATE TABLE runners (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "INSERT INTO runners(id, name) VALUES(1, 'legacy')",
        ] {
            db.execute_unprepared(statement).await.unwrap();
        }
        db
    }

    #[tokio::test]
    async fn legacy_tokens_are_not_guessed_into_a_repository_scope() {
        let db = fixture().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let repo_id: Option<i64> = db
            .query_one(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT repo_id FROM runners WHERE id = 1",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "repo_id")
            .unwrap();
        assert_eq!(repo_id, None);
    }

    #[tokio::test]
    async fn repository_scope_is_a_real_foreign_key_with_cascade() {
        let db = fixture().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        db.execute_unprepared("INSERT INTO repositories(id) VALUES(7)")
            .await
            .unwrap();
        db.execute_unprepared("UPDATE runners SET repo_id = 7 WHERE id = 1")
            .await
            .unwrap();
        assert!(db
            .execute_unprepared("UPDATE runners SET repo_id = 8 WHERE id = 1")
            .await
            .is_err());

        db.execute_unprepared("DELETE FROM repositories WHERE id = 7")
            .await
            .unwrap();
        let count: i64 = db
            .query_one(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM runners",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "n")
            .unwrap();
        assert_eq!(count, 0);
    }
}
