use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260508_000005_create_fts5_indexes"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let stmts: Vec<String> = match backend {
            DatabaseBackend::Sqlite => sqlite_stmts(),
            DatabaseBackend::Postgres => postgres_stmts(),
            DatabaseBackend::MySql => mysql_stmts(),
        };
        for s in stmts {
            manager.get_connection().execute_unprepared(&s).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let stmts: Vec<String> = match backend {
            DatabaseBackend::Sqlite => vec![
                "DROP TRIGGER IF EXISTS repos_fts_update".into(),
                "DROP TRIGGER IF EXISTS repos_fts_delete".into(),
                "DROP TRIGGER IF EXISTS repos_fts_insert".into(),
                "DROP TRIGGER IF EXISTS issues_fts_update".into(),
                "DROP TRIGGER IF EXISTS issues_fts_delete".into(),
                "DROP TRIGGER IF EXISTS issues_fts_insert".into(),
                "DROP TRIGGER IF EXISTS wiki_pages_fts_update".into(),
                "DROP TRIGGER IF EXISTS wiki_pages_fts_delete".into(),
                "DROP TRIGGER IF EXISTS wiki_pages_fts_insert".into(),
                "DROP TABLE IF EXISTS repos_fts".into(),
                "DROP TABLE IF EXISTS issues_fts".into(),
                "DROP TABLE IF EXISTS wiki_pages_fts".into(),
            ],
            DatabaseBackend::Postgres => vec![
                "DROP TRIGGER IF EXISTS repos_fts_ai ON repositories".into(),
                "DROP TRIGGER IF EXISTS repos_fts_ad ON repositories".into(),
                "DROP TRIGGER IF EXISTS issues_fts_ai ON issues".into(),
                "DROP TRIGGER IF EXISTS issues_fts_ad ON issues".into(),
                "DROP TRIGGER IF EXISTS wiki_pages_fts_ai ON wiki_pages".into(),
                "DROP TRIGGER IF EXISTS wiki_pages_fts_ad ON wiki_pages".into(),
                "DROP FUNCTION IF EXISTS ironforge_sync_repos_fts".into(),
                "DROP FUNCTION IF EXISTS ironforge_sync_issues_fts".into(),
                "DROP FUNCTION IF EXISTS ironforge_sync_wiki_fts".into(),
                "DROP TABLE IF EXISTS repos_fts".into(),
                "DROP TABLE IF EXISTS issues_fts".into(),
                "DROP TABLE IF EXISTS wiki_pages_fts".into(),
            ],
            DatabaseBackend::MySql => vec![
                "DROP TRIGGER IF EXISTS repos_fts_ai".into(),
                "DROP TRIGGER IF EXISTS repos_fts_au".into(),
                "DROP TRIGGER IF EXISTS repos_fts_ad".into(),
                "DROP TRIGGER IF EXISTS issues_fts_ai".into(),
                "DROP TRIGGER IF EXISTS issues_fts_au".into(),
                "DROP TRIGGER IF EXISTS issues_fts_ad".into(),
                "DROP TRIGGER IF EXISTS wiki_pages_fts_ai".into(),
                "DROP TRIGGER IF EXISTS wiki_pages_fts_au".into(),
                "DROP TRIGGER IF EXISTS wiki_pages_fts_ad".into(),
                "DROP TABLE IF EXISTS repos_fts".into(),
                "DROP TABLE IF EXISTS issues_fts".into(),
                "DROP TABLE IF EXISTS wiki_pages_fts".into(),
            ],
        };
        for s in stmts {
            manager.get_connection().execute_unprepared(&s).await?;
        }
        Ok(())
    }
}

/// SQLite: FTS5 virtual tables + triggers (unchanged from original).
fn sqlite_stmts() -> Vec<String> {
    vec![r#"
        -- Clean up any previous partial state
        DROP TRIGGER IF EXISTS repos_fts_update;
        DROP TRIGGER IF EXISTS repos_fts_delete;
        DROP TRIGGER IF EXISTS repos_fts_insert;
        DROP TRIGGER IF EXISTS issues_fts_update;
        DROP TRIGGER IF EXISTS issues_fts_delete;
        DROP TRIGGER IF EXISTS issues_fts_insert;
        DROP TRIGGER IF EXISTS wiki_pages_fts_update;
        DROP TRIGGER IF EXISTS wiki_pages_fts_delete;
        DROP TRIGGER IF EXISTS wiki_pages_fts_insert;
        DROP TABLE IF EXISTS repos_fts;
        DROP TABLE IF EXISTS issues_fts;
        DROP TABLE IF EXISTS wiki_pages_fts;

        -- Recreate FTS5 tables (no content= mode; we sync via triggers)
        CREATE VIRTUAL TABLE repos_fts USING fts5(name, description);
        CREATE VIRTUAL TABLE issues_fts USING fts5(title, body);
        CREATE VIRTUAL TABLE wiki_pages_fts USING fts5(title, content);

        -- Triggers for repos_fts
        CREATE TRIGGER IF NOT EXISTS repos_fts_insert AFTER INSERT ON repositories BEGIN
            INSERT INTO repos_fts(rowid, name, description)
            VALUES (new.id, new.name, COALESCE(new.description, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS repos_fts_delete AFTER DELETE ON repositories BEGIN
            INSERT INTO repos_fts(repos_fts, rowid, name, description)
            VALUES('delete', old.id, old.name, COALESCE(old.description, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS repos_fts_update AFTER UPDATE ON repositories BEGIN
            INSERT INTO repos_fts(repos_fts, rowid, name, description)
            VALUES('delete', old.id, old.name, COALESCE(old.description, ''));
            INSERT INTO repos_fts(rowid, name, description)
            VALUES (new.id, new.name, COALESCE(new.description, ''));
        END;

        -- Triggers for issues_fts
        CREATE TRIGGER IF NOT EXISTS issues_fts_insert AFTER INSERT ON issues BEGIN
            INSERT INTO issues_fts(rowid, title, body)
            VALUES (new.id, new.title, COALESCE(new.body, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS issues_fts_delete AFTER DELETE ON issues BEGIN
            INSERT INTO issues_fts(issues_fts, rowid, title, body)
            VALUES('delete', old.id, old.title, COALESCE(old.body, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS issues_fts_update AFTER UPDATE ON issues BEGIN
            INSERT INTO issues_fts(issues_fts, rowid, title, body)
            VALUES('delete', old.id, old.title, COALESCE(old.body, ''));
            INSERT INTO issues_fts(rowid, title, body)
            VALUES (new.id, new.title, COALESCE(new.body, ''));
        END;

        -- Triggers for wiki_pages_fts
        CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_insert AFTER INSERT ON wiki_pages BEGIN
            INSERT INTO wiki_pages_fts(rowid, title, content)
            VALUES (new.id, new.title, COALESCE(new.content, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_delete AFTER DELETE ON wiki_pages BEGIN
            INSERT INTO wiki_pages_fts(wiki_pages_fts, rowid, title, content)
            VALUES('delete', old.id, old.title, COALESCE(old.content, ''));
        END;

        CREATE TRIGGER IF NOT EXISTS wiki_pages_fts_update AFTER UPDATE ON wiki_pages BEGIN
            INSERT INTO wiki_pages_fts(wiki_pages_fts, rowid, title, content)
            VALUES('delete', old.id, old.title, COALESCE(old.content, ''));
            INSERT INTO wiki_pages_fts(rowid, title, content)
            VALUES (new.id, new.title, COALESCE(new.content, ''));
        END;

        -- Rebuild FTS indexes from existing data
        INSERT INTO repos_fts(repos_fts) VALUES('rebuild');
        INSERT INTO issues_fts(issues_fts) VALUES('rebuild');
        INSERT INTO wiki_pages_fts(wiki_pages_fts) VALUES('rebuild');
    "#
    .into()]
}

/// Postgres: regular tables with a `tsvector` generated column + GIN index,
/// kept in sync by triggers on the source tables.
fn postgres_stmts() -> Vec<String> {
    vec![
        "DROP TABLE IF EXISTS repos_fts".into(),
        "DROP TABLE IF EXISTS issues_fts".into(),
        "DROP TABLE IF EXISTS wiki_pages_fts".into(),
        "CREATE TABLE repos_fts (rowid BIGINT PRIMARY KEY, name TEXT, description TEXT, tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', coalesce(name,'') || ' ' || coalesce(description,''))) STORED)".into(),
        "CREATE INDEX repos_fts_tsv_idx ON repos_fts USING GIN(tsv)".into(),
        "CREATE TABLE issues_fts (rowid BIGINT PRIMARY KEY, title TEXT, body TEXT, tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', coalesce(title,'') || ' ' || coalesce(body,''))) STORED)".into(),
        "CREATE INDEX issues_fts_tsv_idx ON issues_fts USING GIN(tsv)".into(),
        "CREATE TABLE wiki_pages_fts (rowid BIGINT PRIMARY KEY, title TEXT, content TEXT, tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', coalesce(title,'') || ' ' || coalesce(content,''))) STORED)".into(),
        "CREATE INDEX wiki_pages_fts_tsv_idx ON wiki_pages_fts USING GIN(tsv)".into(),
        "CREATE OR REPLACE FUNCTION ironforge_sync_repos_fts() RETURNS TRIGGER AS $$
         BEGIN
           IF TG_OP = 'DELETE' THEN
             DELETE FROM repos_fts WHERE rowid = OLD.id;
           ELSE
             INSERT INTO repos_fts(rowid, name, description) VALUES (NEW.id, NEW.name, COALESCE(NEW.description,''))
               ON CONFLICT (rowid) DO UPDATE SET name = EXCLUDED.name, description = EXCLUDED.description;
           END IF;
           RETURN NULL;
         END;
         $$ LANGUAGE plpgsql".into(),
        "CREATE TRIGGER repos_fts_ai AFTER INSERT OR UPDATE ON repositories FOR EACH ROW EXECUTE FUNCTION ironforge_sync_repos_fts()".into(),
        "CREATE TRIGGER repos_fts_ad AFTER DELETE ON repositories FOR EACH ROW EXECUTE FUNCTION ironforge_sync_repos_fts()".into(),
        "CREATE OR REPLACE FUNCTION ironforge_sync_issues_fts() RETURNS TRIGGER AS $$
         BEGIN
           IF TG_OP = 'DELETE' THEN
             DELETE FROM issues_fts WHERE rowid = OLD.id;
           ELSE
             INSERT INTO issues_fts(rowid, title, body) VALUES (NEW.id, NEW.title, COALESCE(NEW.body,''))
               ON CONFLICT (rowid) DO UPDATE SET title = EXCLUDED.title, body = EXCLUDED.body;
           END IF;
           RETURN NULL;
         END;
         $$ LANGUAGE plpgsql".into(),
        "CREATE TRIGGER issues_fts_ai AFTER INSERT OR UPDATE ON issues FOR EACH ROW EXECUTE FUNCTION ironforge_sync_issues_fts()".into(),
        "CREATE TRIGGER issues_fts_ad AFTER DELETE ON issues FOR EACH ROW EXECUTE FUNCTION ironforge_sync_issues_fts()".into(),
        "CREATE OR REPLACE FUNCTION ironforge_sync_wiki_fts() RETURNS TRIGGER AS $$
         BEGIN
           IF TG_OP = 'DELETE' THEN
             DELETE FROM wiki_pages_fts WHERE rowid = OLD.id;
           ELSE
             INSERT INTO wiki_pages_fts(rowid, title, content) VALUES (NEW.id, NEW.title, NEW.content)
               ON CONFLICT (rowid) DO UPDATE SET title = EXCLUDED.title, content = EXCLUDED.content;
           END IF;
           RETURN NULL;
         END;
         $$ LANGUAGE plpgsql".into(),
        "CREATE TRIGGER wiki_pages_fts_ai AFTER INSERT OR UPDATE ON wiki_pages FOR EACH ROW EXECUTE FUNCTION ironforge_sync_wiki_fts()".into(),
        "CREATE TRIGGER wiki_pages_fts_ad AFTER DELETE ON wiki_pages FOR EACH ROW EXECUTE FUNCTION ironforge_sync_wiki_fts()".into(),
        "INSERT INTO repos_fts(rowid, name, description) SELECT id, name, COALESCE(description,'') FROM repositories ON CONFLICT (rowid) DO NOTHING".into(),
        "INSERT INTO issues_fts(rowid, title, body) SELECT id, title, COALESCE(body,'') FROM issues ON CONFLICT (rowid) DO NOTHING".into(),
        "INSERT INTO wiki_pages_fts(rowid, title, content) SELECT id, title, content FROM wiki_pages ON CONFLICT (rowid) DO NOTHING".into(),
    ]
}

/// MySQL: regular tables with a `FULLTEXT` index, kept in sync by triggers.
fn mysql_stmts() -> Vec<String> {
    vec![
        "DROP TABLE IF EXISTS repos_fts".into(),
        "DROP TABLE IF EXISTS issues_fts".into(),
        "DROP TABLE IF EXISTS wiki_pages_fts".into(),
        "CREATE TABLE repos_fts (rowid BIGINT PRIMARY KEY, name TEXT, description TEXT, FULLTEXT(name, description)) ENGINE=InnoDB".into(),
        "CREATE TABLE issues_fts (rowid BIGINT PRIMARY KEY, title TEXT, body TEXT, FULLTEXT(title, body)) ENGINE=InnoDB".into(),
        "CREATE TABLE wiki_pages_fts (rowid BIGINT PRIMARY KEY, title TEXT, content TEXT, FULLTEXT(title, content)) ENGINE=InnoDB".into(),
        "CREATE TRIGGER repos_fts_ai AFTER INSERT ON repositories FOR EACH ROW INSERT INTO repos_fts(rowid, name, description) VALUES (NEW.id, NEW.name, COALESCE(NEW.description,'')) ON DUPLICATE KEY UPDATE name = NEW.name, description = COALESCE(NEW.description,'')".into(),
        "CREATE TRIGGER repos_fts_au AFTER UPDATE ON repositories FOR EACH ROW INSERT INTO repos_fts(rowid, name, description) VALUES (NEW.id, NEW.name, COALESCE(NEW.description,'')) ON DUPLICATE KEY UPDATE name = NEW.name, description = COALESCE(NEW.description,'')".into(),
        "CREATE TRIGGER repos_fts_ad AFTER DELETE ON repositories FOR EACH ROW DELETE FROM repos_fts WHERE rowid = OLD.id".into(),
        "CREATE TRIGGER issues_fts_ai AFTER INSERT ON issues FOR EACH ROW INSERT INTO issues_fts(rowid, title, body) VALUES (NEW.id, NEW.title, COALESCE(NEW.body,'')) ON DUPLICATE KEY UPDATE title = NEW.title, body = COALESCE(NEW.body,'')".into(),
        "CREATE TRIGGER issues_fts_au AFTER UPDATE ON issues FOR EACH ROW INSERT INTO issues_fts(rowid, title, body) VALUES (NEW.id, NEW.title, COALESCE(NEW.body,'')) ON DUPLICATE KEY UPDATE title = NEW.title, body = COALESCE(NEW.body,'')".into(),
        "CREATE TRIGGER issues_fts_ad AFTER DELETE ON issues FOR EACH ROW DELETE FROM issues_fts WHERE rowid = OLD.id".into(),
        "CREATE TRIGGER wiki_pages_fts_ai AFTER INSERT ON wiki_pages FOR EACH ROW INSERT INTO wiki_pages_fts(rowid, title, content) VALUES (NEW.id, NEW.title, NEW.content) ON DUPLICATE KEY UPDATE title = NEW.title, content = NEW.content".into(),
        "CREATE TRIGGER wiki_pages_fts_au AFTER UPDATE ON wiki_pages FOR EACH ROW INSERT INTO wiki_pages_fts(rowid, title, content) VALUES (NEW.id, NEW.title, NEW.content) ON DUPLICATE KEY UPDATE title = NEW.title, content = NEW.content".into(),
        "CREATE TRIGGER wiki_pages_fts_ad AFTER DELETE ON wiki_pages FOR EACH ROW DELETE FROM wiki_pages_fts WHERE rowid = OLD.id".into(),
        "INSERT INTO repos_fts(rowid, name, description) SELECT id, name, COALESCE(description,'') FROM repositories ON DUPLICATE KEY UPDATE name = VALUES(name), description = VALUES(description)".into(),
        "INSERT INTO issues_fts(rowid, title, body) SELECT id, title, COALESCE(body,'') FROM issues ON DUPLICATE KEY UPDATE title = VALUES(title), body = VALUES(body)".into(),
        "INSERT INTO wiki_pages_fts(rowid, title, content) SELECT id, title, content FROM wiki_pages ON DUPLICATE KEY UPDATE title = VALUES(title), content = VALUES(content)".into(),
    ]
}
