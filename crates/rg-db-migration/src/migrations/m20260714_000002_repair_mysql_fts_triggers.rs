//! Restore MySQL FTS triggers that can be omitted by table/data-only database moves.

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

use super::fts_safety;

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

        fts_safety::mysql_fts_maintenance(
            manager,
            "repositories WRITE, issues WRITE, wiki_pages WRITE, repos_fts WRITE, issues_fts WRITE, wiki_pages_fts WRITE",
            &mysql_trigger_repair_statements(),
            &mysql_reconcile_statements(),
        )
        .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // This is a data-integrity repair. Rolling it back must not remove the
        // synchronization triggers and make search indexes stale again.
        Ok(())
    }
}

fn mysql_trigger_repair_statements() -> Vec<String> {
    let definitions = [
        (
            "repos_fts_ai",
            "fk_mig_repos_ai_guard",
            "AFTER INSERT ON repositories FOR EACH ROW INSERT INTO repos_fts(rowid, name, description) VALUES (NEW.id, NEW.name, COALESCE(NEW.description,'')) ON DUPLICATE KEY UPDATE name = NEW.name, description = COALESCE(NEW.description,'')",
        ),
        (
            "repos_fts_au",
            "fk_mig_repos_au_guard",
            "AFTER UPDATE ON repositories FOR EACH ROW INSERT INTO repos_fts(rowid, name, description) VALUES (NEW.id, NEW.name, COALESCE(NEW.description,'')) ON DUPLICATE KEY UPDATE name = NEW.name, description = COALESCE(NEW.description,'')",
        ),
        (
            "repos_fts_ad",
            "fk_mig_repos_ad_guard",
            "AFTER DELETE ON repositories FOR EACH ROW DELETE FROM repos_fts WHERE rowid = OLD.id",
        ),
        (
            "issues_fts_ai",
            "fk_mig_issues_ai_guard",
            "AFTER INSERT ON issues FOR EACH ROW INSERT INTO issues_fts(rowid, title, body) VALUES (NEW.id, NEW.title, COALESCE(NEW.body,'')) ON DUPLICATE KEY UPDATE title = NEW.title, body = COALESCE(NEW.body,'')",
        ),
        (
            "issues_fts_au",
            "fk_mig_issues_au_guard",
            "AFTER UPDATE ON issues FOR EACH ROW INSERT INTO issues_fts(rowid, title, body) VALUES (NEW.id, NEW.title, COALESCE(NEW.body,'')) ON DUPLICATE KEY UPDATE title = NEW.title, body = COALESCE(NEW.body,'')",
        ),
        (
            "issues_fts_ad",
            "fk_mig_issues_ad_guard",
            "AFTER DELETE ON issues FOR EACH ROW DELETE FROM issues_fts WHERE rowid = OLD.id",
        ),
        (
            "wiki_pages_fts_ai",
            "fk_mig_wiki_ai_guard",
            "AFTER INSERT ON wiki_pages FOR EACH ROW INSERT INTO wiki_pages_fts(rowid, title, content) VALUES (NEW.id, NEW.title, COALESCE(NEW.content,'')) ON DUPLICATE KEY UPDATE title = NEW.title, content = COALESCE(NEW.content,'')",
        ),
        (
            "wiki_pages_fts_au",
            "fk_mig_wiki_au_guard",
            "AFTER UPDATE ON wiki_pages FOR EACH ROW INSERT INTO wiki_pages_fts(rowid, title, content) VALUES (NEW.id, NEW.title, COALESCE(NEW.content,'')) ON DUPLICATE KEY UPDATE title = NEW.title, content = COALESCE(NEW.content,'')",
        ),
        (
            "wiki_pages_fts_ad",
            "fk_mig_wiki_ad_guard",
            "AFTER DELETE ON wiki_pages FOR EACH ROW DELETE FROM wiki_pages_fts WHERE rowid = OLD.id",
        ),
    ];

    definitions
        .into_iter()
        .flat_map(|(canonical, guard, definition)| {
            [
                format!("CREATE TRIGGER IF NOT EXISTS {guard} {definition}"),
                format!("DROP TRIGGER IF EXISTS {canonical}"),
                format!("CREATE TRIGGER {canonical} {definition}"),
                format!("DROP TRIGGER IF EXISTS {guard}"),
            ]
        })
        .collect()
}

fn mysql_reconcile_statements() -> Vec<String> {
    vec![
        "DELETE FROM repos_fts WHERE NOT EXISTS (SELECT 1 FROM repositories WHERE repositories.id = repos_fts.rowid)".into(),
        "INSERT INTO repos_fts(rowid, name, description) SELECT id, name, COALESCE(description,'') FROM repositories ON DUPLICATE KEY UPDATE name = VALUES(name), description = VALUES(description)".into(),
        "DELETE FROM issues_fts WHERE NOT EXISTS (SELECT 1 FROM issues WHERE issues.id = issues_fts.rowid)".into(),
        "INSERT INTO issues_fts(rowid, title, body) SELECT id, title, COALESCE(body,'') FROM issues ON DUPLICATE KEY UPDATE title = VALUES(title), body = VALUES(body)".into(),
        "DELETE FROM wiki_pages_fts WHERE NOT EXISTS (SELECT 1 FROM wiki_pages WHERE wiki_pages.id = wiki_pages_fts.rowid)".into(),
        "INSERT INTO wiki_pages_fts(rowid, title, content) SELECT id, title, COALESCE(content,'') FROM wiki_pages ON DUPLICATE KEY UPDATE title = VALUES(title), content = VALUES(content)".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::{mysql_reconcile_statements, mysql_trigger_repair_statements};

    #[test]
    fn repair_recreates_all_sync_triggers_and_backfills_indexes() {
        let ddl = mysql_trigger_repair_statements();
        let reconcile = mysql_reconcile_statements();
        for table in ["repos_fts", "issues_fts", "wiki_pages_fts"] {
            assert!(ddl
                .iter()
                .any(|statement| statement.starts_with("CREATE TRIGGER ")
                    && statement.contains(table)));
            assert!(
                reconcile
                    .iter()
                    .any(|statement| statement.starts_with("DELETE FROM")
                        && statement.contains(table))
            );
            assert!(
                reconcile
                    .iter()
                    .any(|statement| statement.starts_with("INSERT INTO")
                        && statement.contains(table))
            );
        }
        assert_eq!(
            ddl.iter()
                .filter(|statement| {
                    statement.starts_with("CREATE TRIGGER ")
                        && !statement.starts_with("CREATE TRIGGER IF NOT EXISTS")
                })
                .count(),
            9
        );
        assert_eq!(
            ddl.iter()
                .filter(|statement| statement.starts_with("CREATE TRIGGER IF NOT EXISTS"))
                .count(),
            9
        );
    }
}
