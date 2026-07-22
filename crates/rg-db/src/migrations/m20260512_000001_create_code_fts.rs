use sea_orm::DatabaseBackend;
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260512_000001_create_code_fts"
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
        let stmt = match backend {
            DatabaseBackend::Sqlite => "DROP TABLE IF EXISTS code_fts".to_string(),
            DatabaseBackend::Postgres => "DROP TABLE IF EXISTS code_fts".to_string(),
            DatabaseBackend::MySql => "DROP TABLE IF EXISTS code_fts".to_string(),
        };
        manager.get_connection().execute_unprepared(&stmt).await?;
        Ok(())
    }
}

/// SQLite: FTS5 virtual table for code search.
///
/// Note: No triggers needed because Git blob content is not stored in database
/// tables — indexing is performed by the `CodeIndexer` service, which scans Git
/// objects and writes FTS5 rows directly.
fn sqlite_stmts() -> Vec<String> {
    vec![r#"
        -- Clean up any previous partial state
        DROP TABLE IF EXISTS code_fts;

        -- Create FTS5 virtual table for code search
        -- Columns: repo_id, file_path, file_name, content, language
        CREATE VIRTUAL TABLE code_fts USING fts5(
            repo_id,
            file_path,
            file_name,
            content,
            language
        );
    "#
    .into()]
}

/// Postgres: a regular table with a `tsvector` generated column + GIN index.
/// The `tsv` column is kept in sync automatically by the generated expression;
/// the `CodeIndexer` service INSERTs/DELETEs rows directly, so no triggers are
/// required here.
fn postgres_stmts() -> Vec<String> {
    vec![
        "DROP TABLE IF EXISTS code_fts".into(),
        "CREATE TABLE code_fts (\
            id BIGSERIAL PRIMARY KEY, \
            repo_id BIGINT NOT NULL, \
            file_path TEXT, \
            file_name TEXT, \
            content TEXT, \
            language TEXT, \
            tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', \
                coalesce(content,'') || ' ' || coalesce(file_path,'') || ' ' || \
                coalesce(file_name,'') || ' ' || coalesce(language,''))) STORED\
        )"
        .into(),
        "CREATE INDEX code_fts_tsv_idx ON code_fts USING GIN(tsv)".into(),
        "CREATE INDEX code_fts_repo_id_idx ON code_fts(repo_id)".into(),
    ]
}

/// MySQL: a regular InnoDB table with a `FULLTEXT` index over the searchable
/// columns. The `CodeIndexer` service manages rows directly.
fn mysql_stmts() -> Vec<String> {
    vec![
        "DROP TABLE IF EXISTS code_fts".into(),
        "CREATE TABLE code_fts (\
            id BIGINT AUTO_INCREMENT PRIMARY KEY, \
            repo_id BIGINT NOT NULL, \
            file_path TEXT, \
            file_name TEXT, \
            content LONGTEXT, \
            language TEXT, \
            FULLTEXT(content, file_path, file_name, language)\
        ) ENGINE=InnoDB"
            .into(),
        "CREATE INDEX code_fts_repo_id_idx ON code_fts(repo_id)".into(),
    ]
}
