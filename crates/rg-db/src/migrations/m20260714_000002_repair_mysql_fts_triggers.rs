//! Restore MySQL FTS triggers that can be omitted by table/data-only database moves.

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260714_000002_repair_mysql_fts_triggers"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.get_database_backend() != DatabaseBackend::MySql {
            return Ok(());
        }

        for statement in mysql_repair_statements() {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // This is a data-integrity repair. Rolling it back must not remove the
        // synchronization triggers and make search indexes stale again.
        Ok(())
    }
}

fn mysql_repair_statements() -> Vec<&'static str> {
    vec![
        "DROP TRIGGER IF EXISTS repos_fts_ai",
        "DROP TRIGGER IF EXISTS repos_fts_au",
        "DROP TRIGGER IF EXISTS repos_fts_ad",
        "DROP TRIGGER IF EXISTS issues_fts_ai",
        "DROP TRIGGER IF EXISTS issues_fts_au",
        "DROP TRIGGER IF EXISTS issues_fts_ad",
        "DROP TRIGGER IF EXISTS wiki_pages_fts_ai",
        "DROP TRIGGER IF EXISTS wiki_pages_fts_au",
        "DROP TRIGGER IF EXISTS wiki_pages_fts_ad",
        "CREATE TRIGGER repos_fts_ai AFTER INSERT ON repositories FOR EACH ROW INSERT INTO repos_fts(rowid, name, description) VALUES (NEW.id, NEW.name, COALESCE(NEW.description,'')) ON DUPLICATE KEY UPDATE name = NEW.name, description = COALESCE(NEW.description,'')",
        "CREATE TRIGGER repos_fts_au AFTER UPDATE ON repositories FOR EACH ROW INSERT INTO repos_fts(rowid, name, description) VALUES (NEW.id, NEW.name, COALESCE(NEW.description,'')) ON DUPLICATE KEY UPDATE name = NEW.name, description = COALESCE(NEW.description,'')",
        "CREATE TRIGGER repos_fts_ad AFTER DELETE ON repositories FOR EACH ROW DELETE FROM repos_fts WHERE rowid = OLD.id",
        "CREATE TRIGGER issues_fts_ai AFTER INSERT ON issues FOR EACH ROW INSERT INTO issues_fts(rowid, title, body) VALUES (NEW.id, NEW.title, COALESCE(NEW.body,'')) ON DUPLICATE KEY UPDATE title = NEW.title, body = COALESCE(NEW.body,'')",
        "CREATE TRIGGER issues_fts_au AFTER UPDATE ON issues FOR EACH ROW INSERT INTO issues_fts(rowid, title, body) VALUES (NEW.id, NEW.title, COALESCE(NEW.body,'')) ON DUPLICATE KEY UPDATE title = NEW.title, body = COALESCE(NEW.body,'')",
        "CREATE TRIGGER issues_fts_ad AFTER DELETE ON issues FOR EACH ROW DELETE FROM issues_fts WHERE rowid = OLD.id",
        "CREATE TRIGGER wiki_pages_fts_ai AFTER INSERT ON wiki_pages FOR EACH ROW INSERT INTO wiki_pages_fts(rowid, title, content) VALUES (NEW.id, NEW.title, COALESCE(NEW.content,'')) ON DUPLICATE KEY UPDATE title = NEW.title, content = COALESCE(NEW.content,'')",
        "CREATE TRIGGER wiki_pages_fts_au AFTER UPDATE ON wiki_pages FOR EACH ROW INSERT INTO wiki_pages_fts(rowid, title, content) VALUES (NEW.id, NEW.title, COALESCE(NEW.content,'')) ON DUPLICATE KEY UPDATE title = NEW.title, content = COALESCE(NEW.content,'')",
        "CREATE TRIGGER wiki_pages_fts_ad AFTER DELETE ON wiki_pages FOR EACH ROW DELETE FROM wiki_pages_fts WHERE rowid = OLD.id",
        "INSERT INTO repos_fts(rowid, name, description) SELECT id, name, COALESCE(description,'') FROM repositories ON DUPLICATE KEY UPDATE name = VALUES(name), description = VALUES(description)",
        "INSERT INTO issues_fts(rowid, title, body) SELECT id, title, COALESCE(body,'') FROM issues ON DUPLICATE KEY UPDATE title = VALUES(title), body = VALUES(body)",
        "INSERT INTO wiki_pages_fts(rowid, title, content) SELECT id, title, COALESCE(content,'') FROM wiki_pages ON DUPLICATE KEY UPDATE title = VALUES(title), content = VALUES(content)",
    ]
}

#[cfg(test)]
mod tests {
    use super::mysql_repair_statements;

    #[test]
    fn repair_recreates_all_sync_triggers_and_backfills_indexes() {
        let statements = mysql_repair_statements();
        for table in ["repos_fts", "issues_fts", "wiki_pages_fts"] {
            assert!(statements
                .iter()
                .any(|statement| statement.starts_with("CREATE TRIGGER")
                    && statement.contains(table)));
            assert!(
                statements
                    .iter()
                    .any(|statement| statement.starts_with("INSERT INTO")
                        && statement.contains(table))
            );
        }
        assert_eq!(
            statements
                .iter()
                .filter(|statement| statement.starts_with("CREATE TRIGGER"))
                .count(),
            9
        );
    }
}
