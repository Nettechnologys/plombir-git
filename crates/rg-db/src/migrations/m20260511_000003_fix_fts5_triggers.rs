use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

/// Fix FTS5 triggers: the 'delete' command in FTS5 only accepts (rowid),
/// not content columns. The original triggers incorrectly passed column values
/// like VALUES('delete', old.id, old.name, ...) which causes "SQL logic error".
///
/// This fix only applies to SQLite (FTS5). For Postgres / MySQL the triggers
/// were already created correctly in `m20260508_000005_create_fts5_indexes`
/// (using `DELETE FROM ... WHERE rowid = ...`) and there is no FTS5 `rebuild`
/// command, so this migration is a no-op on those backends.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260511_000003_fix_fts5_triggers"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        if !matches!(backend, DatabaseBackend::Sqlite) {
            // Postgres / MySQL: nothing to fix — triggers are already correct.
            return Ok(());
        }

        let sql = r#"
            -- Drop all existing FTS triggers (old triggers used FTS5 'delete' command
            -- which requires special VALUES syntax that doesn't work reliably)
            DROP TRIGGER IF EXISTS repos_fts_update;
            DROP TRIGGER IF EXISTS repos_fts_delete;
            DROP TRIGGER IF EXISTS repos_fts_insert;
            DROP TRIGGER IF EXISTS issues_fts_update;
            DROP TRIGGER IF EXISTS issues_fts_delete;
            DROP TRIGGER IF EXISTS issues_fts_insert;
            DROP TRIGGER IF EXISTS wiki_pages_fts_update;
            DROP TRIGGER IF EXISTS wiki_pages_fts_delete;
            DROP TRIGGER IF EXISTS wiki_pages_fts_insert;

            -- repos_fts triggers (use DELETE FROM instead of FTS5 'delete' command)
            CREATE TRIGGER IF NOT EXISTS repos_fts_insert AFTER INSERT ON repositories BEGIN
                INSERT INTO repos_fts(rowid, name, description)
                VALUES (new.id, new.name, COALESCE(new.description, ''));
            END;

            CREATE TRIGGER IF NOT EXISTS repos_fts_delete AFTER DELETE ON repositories BEGIN
                DELETE FROM repos_fts WHERE rowid = old.id;
            END;

            CREATE TRIGGER IF NOT EXISTS repos_fts_update AFTER UPDATE ON repositories BEGIN
                DELETE FROM repos_fts WHERE rowid = old.id;
                INSERT INTO repos_fts(rowid, name, description)
                VALUES (new.id, new.name, COALESCE(new.description, ''));
            END;

            -- issues_fts triggers
            CREATE TRIGGER IF NOT EXISTS issues_fts_insert AFTER INSERT ON issues BEGIN
                INSERT INTO issues_fts(rowid, title, body)
                VALUES (new.id, new.title, COALESCE(new.body, ''));
            END;

            CREATE TRIGGER IF NOT EXISTS issues_fts_delete AFTER DELETE ON issues BEGIN
                DELETE FROM issues_fts WHERE rowid = old.id;
            END;

            CREATE TRIGGER IF NOT EXISTS issues_fts_update AFTER UPDATE ON issues BEGIN
                DELETE FROM issues_fts WHERE rowid = old.id;
                INSERT INTO issues_fts(rowid, title, body)
                VALUES (new.id, new.title, COALESCE(new.body, ''));
            END;

            -- wiki_pages_fts triggers
            CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_insert AFTER INSERT ON wiki_pages BEGIN
                INSERT INTO wiki_pages_fts(rowid, title, content)
                VALUES (new.id, new.title, COALESCE(new.content, ''));
            END;

            CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_delete AFTER DELETE ON wiki_pages BEGIN
                DELETE FROM wiki_pages_fts WHERE rowid = old.id;
            END;

            CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_update AFTER UPDATE ON wiki_pages BEGIN
                DELETE FROM wiki_pages_fts WHERE rowid = old.id;
                INSERT INTO wiki_pages_fts(rowid, title, content)
                VALUES (new.id, new.title, COALESCE(new.content, ''));
            END;

            -- Rebuild FTS indexes
            INSERT INTO repos_fts(repos_fts) VALUES('rebuild');
            INSERT INTO issues_fts(issues_fts) VALUES('rebuild');
            INSERT INTO wiki_pages_fts(wiki_pages_fts) VALUES('rebuild');
        "#;

        manager.get_connection().execute_unprepared(sql).await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Same as up — restore original triggers
        self.up(manager).await
    }
}
