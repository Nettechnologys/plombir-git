//! `mirrors.password_encrypted` was never encrypted: `create_mirror` /
//! `update_mirror` stored the operator-supplied password verbatim, and nothing
//! ever read the column back (card_c29cb3416941). Every value written before
//! that fix is therefore plaintext — a database dump handed out the passwords
//! for other people's remotes as they were typed.
//!
//! Because the column had exactly one writer and no readers, "everything
//! already in it is plaintext" is a fact, not a guess, so this needs no
//! heuristic to tell ciphertext from cleartext: the whole column is cleared.
//! Nothing functional is lost — the value was never used for anything — and the
//! settings UI reports `has_credentials: false` afterwards, which is the honest
//! state. Operators re-enter the credential once; from then on it is stored as
//! AES-256-GCM ciphertext and actually used by the sync.
//!
//! There is no `down`: restoring the plaintext is exactly what this migration
//! exists to undo, and it is not recoverable from here anyway.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260727_000001_clear_plaintext_mirror_passwords"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("mirrors").await? {
            return Ok(());
        }
        manager
            .exec_stmt(
                Query::update()
                    .table(Alias::new("mirrors"))
                    .value(Alias::new("password_encrypted"), Keyword::Null)
                    .and_where(Expr::col(Alias::new("password_encrypted")).is_not_null())
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
    async fn clears_every_stored_password_and_keeps_the_rest_of_the_row() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE mirrors (id INTEGER PRIMARY KEY, repo_id BIGINT NOT NULL, url TEXT NOT NULL, \
             username TEXT, password_encrypted TEXT);\
             INSERT INTO mirrors (id, repo_id, url, username, password_encrypted) \
             VALUES (1, 7, 'https://example.com/a.git', 'sync-bot', 'hunter2'), \
                    (2, 8, 'https://example.com/b.git', NULL, NULL);",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM mirrors WHERE password_encrypted IS NOT NULL"
                    .to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<i64>("", "n").unwrap(),
            0,
            "a plaintext mirror password survived the migration"
        );

        // The credential is what goes; the mirror itself stays configured.
        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT url, username FROM mirrors WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<String>("", "url").unwrap(),
            "https://example.com/a.git"
        );
        assert_eq!(row.try_get::<String>("", "username").unwrap(), "sync-bot");
    }

    /// A database that never reached the mirror migrations at all must not make
    /// the migration runner fail.
    #[tokio::test]
    async fn is_a_no_op_without_the_mirrors_table() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
    }
}
