//! `import_tasks.auth_token_encrypted` held the user's GitHub/GitLab PAT in
//! plaintext: `start_import` stored the token exactly as it arrived in the
//! request body, and the readers took it back with `as_deref()` — the
//! `_encrypted` suffix was the only encryption there ever was
//! (card_2e259792f27c). A database dump was therefore a set of other people's
//! `repo`-scoped personal access tokens.
//!
//! The token is no longer stored at all: the import worker is started in-process
//! and handed it in memory, so nothing ever read the column back. That makes
//! "everything in it is plaintext" a fact rather than a guess — the column had
//! one writer and no consumer outside the same request — so the whole column is
//! cleared without needing a heuristic to tell ciphertext from cleartext.
//!
//! The column itself is left in place. It is written by nobody and mapped by no
//! entity field from here on, and dropping a column costs more (older SQLite,
//! rollback to a previous binary) than an always-NULL column does.
//!
//! There is no `down`: handing the tokens back is precisely what this migration
//! undoes, and they are not recoverable from here anyway.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260727_000002_clear_import_task_auth_tokens"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("import_tasks").await? {
            return Ok(());
        }
        manager
            .exec_stmt(
                Query::update()
                    .table(Alias::new("import_tasks"))
                    .value(Alias::new("auth_token_encrypted"), Keyword::Null)
                    .and_where(Expr::col(Alias::new("auth_token_encrypted")).is_not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn clears_every_stored_token_and_keeps_the_rest_of_the_task() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE import_tasks (id INTEGER PRIMARY KEY, user_id BIGINT NOT NULL, \
             source_url TEXT NOT NULL, status TEXT NOT NULL, progress INTEGER NOT NULL, \
             auth_token_encrypted TEXT);\
             INSERT INTO import_tasks (id, user_id, source_url, status, progress, auth_token_encrypted) \
             VALUES (1, 7, 'https://github.com/a/b', 'completed', 100, 'ghp_plaintext'), \
                    (2, 8, 'https://gitlab.com/c/d', 'importing', 40, 'glpat-plaintext'), \
                    (3, 9, 'https://example.com/e/f', 'failed', 0, NULL);",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM import_tasks WHERE auth_token_encrypted IS NOT NULL"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<i64>("", "n").unwrap(),
            0,
            "a plaintext import token survived the migration"
        );

        // The credential is what goes; the task and its history stay readable.
        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT source_url, status, progress FROM import_tasks WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<String>("", "source_url").unwrap(),
            "https://github.com/a/b"
        );
        assert_eq!(row.try_get::<String>("", "status").unwrap(), "completed");
        assert_eq!(row.try_get::<i32>("", "progress").unwrap(), 100);
    }

    /// A database that never reached the import migrations at all must not make
    /// the migration runner fail.
    #[tokio::test]
    async fn is_a_no_op_without_the_import_tasks_table() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
    }
}
