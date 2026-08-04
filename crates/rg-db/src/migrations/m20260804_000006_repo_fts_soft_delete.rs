//! Make the source row the sole authority for repository metadata FTS.

use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260804_000006_repo_fts_soft_delete"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in statements(manager.get_database_backend()) {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // This is a data-integrity repair. Reinstating triggers that index
        // soft-deleted repositories would make a rollback silently expose
        // stale search rows again.
        Ok(())
    }
}

fn statements(backend: DatabaseBackend) -> Vec<&'static str> {
    match backend {
        // Keep the trigger replacement in one SQLite batch. Sending each DDL
        // statement separately through a pooled connection intermittently let
        // CREATE observe the pre-DROP schema during parallel test migrations.
        DatabaseBackend::Sqlite => vec![r#"
            DROP TRIGGER IF EXISTS repos_fts_insert;
            DROP TRIGGER IF EXISTS repos_fts_update;
            DROP TRIGGER IF EXISTS repos_fts_delete;

            CREATE TRIGGER repos_fts_insert AFTER INSERT ON repositories
            WHEN NEW.deleted_at IS NULL
            BEGIN
              INSERT INTO repos_fts(rowid, name, description)
              VALUES (NEW.id, NEW.name, COALESCE(NEW.description, ''));
            END;

            CREATE TRIGGER repos_fts_update AFTER UPDATE ON repositories
            BEGIN
              DELETE FROM repos_fts WHERE rowid = OLD.id;
              INSERT INTO repos_fts(rowid, name, description)
              SELECT NEW.id, NEW.name, COALESCE(NEW.description, '')
              WHERE NEW.deleted_at IS NULL;
            END;

            CREATE TRIGGER repos_fts_delete AFTER DELETE ON repositories
            BEGIN
              DELETE FROM repos_fts WHERE rowid = OLD.id;
            END;

            DELETE FROM repos_fts;
            INSERT INTO repos_fts(rowid, name, description)
            SELECT id, name, COALESCE(description, '')
            FROM repositories
            WHERE deleted_at IS NULL;
        "#],
        DatabaseBackend::Postgres => vec![
            r#"CREATE OR REPLACE FUNCTION forgekeep_sync_repos_fts() RETURNS TRIGGER AS $$
               BEGIN
                 IF TG_OP = 'DELETE' THEN
                   DELETE FROM repos_fts WHERE rowid = OLD.id;
                 ELSIF NEW.deleted_at IS NOT NULL THEN
                   DELETE FROM repos_fts WHERE rowid = NEW.id;
                 ELSE
                   INSERT INTO repos_fts(rowid, name, description)
                   VALUES (NEW.id, NEW.name, COALESCE(NEW.description, ''))
                   ON CONFLICT (rowid) DO UPDATE
                   SET name = EXCLUDED.name, description = EXCLUDED.description;
                 END IF;
                 RETURN NULL;
               END;
               $$ LANGUAGE plpgsql"#,
            "DROP TRIGGER IF EXISTS repos_fts_ai ON repositories",
            "DROP TRIGGER IF EXISTS repos_fts_ad ON repositories",
            "CREATE TRIGGER repos_fts_ai AFTER INSERT OR UPDATE ON repositories FOR EACH ROW EXECUTE FUNCTION forgekeep_sync_repos_fts()",
            "CREATE TRIGGER repos_fts_ad AFTER DELETE ON repositories FOR EACH ROW EXECUTE FUNCTION forgekeep_sync_repos_fts()",
            "DELETE FROM repos_fts",
            "INSERT INTO repos_fts(rowid, name, description) SELECT id, name, COALESCE(description, '') FROM repositories WHERE deleted_at IS NULL",
        ],
        DatabaseBackend::MySql => vec![
            "DROP TRIGGER IF EXISTS repos_fts_ai",
            "DROP TRIGGER IF EXISTS repos_fts_au",
            "DROP TRIGGER IF EXISTS repos_fts_ad",
            r#"CREATE TRIGGER repos_fts_ai AFTER INSERT ON repositories FOR EACH ROW
               BEGIN
                 IF NEW.deleted_at IS NULL THEN
                   INSERT INTO repos_fts(rowid, name, description)
                   VALUES (NEW.id, NEW.name, COALESCE(NEW.description, ''))
                   ON DUPLICATE KEY UPDATE
                   name = NEW.name, description = COALESCE(NEW.description, '');
                 END IF;
               END"#,
            r#"CREATE TRIGGER repos_fts_au AFTER UPDATE ON repositories FOR EACH ROW
               BEGIN
                 DELETE FROM repos_fts WHERE rowid = OLD.id;
                 IF NEW.deleted_at IS NULL THEN
                   INSERT INTO repos_fts(rowid, name, description)
                   VALUES (NEW.id, NEW.name, COALESCE(NEW.description, ''))
                   ON DUPLICATE KEY UPDATE
                   name = NEW.name, description = COALESCE(NEW.description, '');
                 END IF;
               END"#,
            "CREATE TRIGGER repos_fts_ad AFTER DELETE ON repositories FOR EACH ROW DELETE FROM repos_fts WHERE rowid = OLD.id",
            "DELETE FROM repos_fts",
            "INSERT INTO repos_fts(rowid, name, description) SELECT id, name, COALESCE(description, '') FROM repositories WHERE deleted_at IS NULL ON DUPLICATE KEY UPDATE name = VALUES(name), description = VALUES(description)",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::statements;
    use super::Migration;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
    use sea_orm_migration::{MigrationTrait, SchemaManager};

    #[test]
    fn every_backend_removes_deleted_rows_and_backfills_only_live_repositories() {
        for backend in [
            DatabaseBackend::Sqlite,
            DatabaseBackend::Postgres,
            DatabaseBackend::MySql,
        ] {
            let sql = statements(backend).join("\n");
            assert!(sql.contains("DELETE FROM repos_fts"));
            assert!(sql.contains("WHERE deleted_at IS NULL"));
            assert!(
                sql.contains("NEW.deleted_at IS NULL")
                    || sql.contains("NEW.deleted_at IS NOT NULL")
            );
        }
    }

    #[tokio::test]
    async fn upgrade_removes_stale_deleted_rows_and_the_trigger_tracks_restore() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            r#"
            CREATE TABLE repositories (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                deleted_at TEXT
            );
            CREATE VIRTUAL TABLE repos_fts USING fts5(name, description);
            CREATE TRIGGER repos_fts_insert AFTER INSERT ON repositories BEGIN
                INSERT INTO repos_fts(rowid, name, description)
                VALUES (NEW.id, NEW.name, COALESCE(NEW.description, ''));
            END;
            CREATE TRIGGER repos_fts_update AFTER UPDATE ON repositories BEGIN
                DELETE FROM repos_fts WHERE rowid = OLD.id;
                INSERT INTO repos_fts(rowid, name, description)
                VALUES (NEW.id, NEW.name, COALESCE(NEW.description, ''));
            END;
            CREATE TRIGGER repos_fts_delete AFTER DELETE ON repositories BEGIN
                DELETE FROM repos_fts WHERE rowid = OLD.id;
            END;
            INSERT INTO repositories(id, name, description, deleted_at)
            VALUES (7, 'stale-repo', 'stale description', 'already deleted');
            "#,
        )
        .await
        .unwrap();

        assert_eq!(
            fts_rows(&db).await,
            vec![("stale-repo".into(), "stale description".into())]
        );
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        assert!(fts_rows(&db).await.is_empty());

        db.execute_unprepared(
            "UPDATE repositories \
             SET name = 'restored-repo', description = 'restored description', deleted_at = NULL \
             WHERE id = 7",
        )
        .await
        .unwrap();
        assert_eq!(
            fts_rows(&db).await,
            vec![("restored-repo".into(), "restored description".into())]
        );

        db.execute_unprepared("UPDATE repositories SET deleted_at = 'deleted again' WHERE id = 7")
            .await
            .unwrap();
        assert!(fts_rows(&db).await.is_empty());
    }

    async fn fts_rows(db: &sea_orm::DatabaseConnection) -> Vec<(String, String)> {
        db.query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name, description FROM repos_fts ORDER BY rowid".to_string(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.try_get("", "name").unwrap(),
                row.try_get("", "description").unwrap(),
            )
        })
        .collect()
    }
}
