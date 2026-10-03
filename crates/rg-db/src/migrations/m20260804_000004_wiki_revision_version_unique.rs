//! Migration: make a wiki revision number unique within its page.
//!
//! Older schemas indexed only `wiki_page_id`, so two concurrent edits could
//! both store the same `MAX(version) + 1`. Repair those rows before adding the
//! constraint: within each page the original order is preserved, and a
//! duplicate shifts itself and every later collision to the next free number.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DbBackend, Statement};

const UNIQUE_INDEX: &str = "uq_wiki_revisions_page_version";

#[derive(DeriveMigrationName)]
pub struct Migration;

async fn repair_duplicate_versions(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = db.get_database_backend();
    let rows = db
        .query_all(Statement::from_string(
            backend,
            "SELECT id, wiki_page_id, version FROM wiki_revisions \
             ORDER BY wiki_page_id, version, created_at, id"
                .to_string(),
        ))
        .await?;

    let mut current_page = None;
    let mut previous_version: Option<i32> = None;

    for row in rows {
        let id: i64 = row.try_get("", "id")?;
        let wiki_page_id: i64 = row.try_get("", "wiki_page_id")?;
        let version: i32 = row.try_get("", "version")?;

        let repaired_version = if current_page == Some(wiki_page_id) {
            let next_free = previous_version
                .and_then(|previous| previous.checked_add(1))
                .ok_or_else(|| {
                    DbErr::Custom(format!(
                        "wiki page {wiki_page_id} has no free revision number after i32::MAX"
                    ))
                })?;
            version.max(next_free)
        } else {
            current_page = Some(wiki_page_id);
            version
        };

        if repaired_version != version {
            db.execute(Statement::from_sql_and_values(
                backend,
                match backend {
                    DbBackend::Postgres => "UPDATE wiki_revisions SET version = $1 WHERE id = $2",
                    _ => "UPDATE wiki_revisions SET version = ? WHERE id = ?",
                },
                [repaired_version.into(), id.into()],
            ))
            .await?;
        }

        previous_version = Some(repaired_version);
    }

    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("wiki_revisions").await? {
            return Ok(());
        }

        repair_duplicate_versions(manager).await?;
        if !manager.has_index("wiki_revisions", UNIQUE_INDEX).await? {
            manager
                .create_index(
                    Index::create()
                        .name(UNIQUE_INDEX)
                        .table(WikiRevisions::Table)
                        .col(WikiRevisions::WikiPageId)
                        .col(WikiRevisions::Version)
                        .unique()
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_index("wiki_revisions", UNIQUE_INDEX).await? {
            manager
                .drop_index(
                    Index::drop()
                        .name(UNIQUE_INDEX)
                        .table(WikiRevisions::Table)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Iden)]
enum WikiRevisions {
    Table,
    WikiPageId,
    Version,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::Database;

    /// Optional disposable server database for the cross-backend acceptance
    /// run. Ordinary `cargo test` keeps using an in-memory SQLite database.
    const TEST_DATABASE_URL_ENV: &str = "PLOMBIR_GIT_WIKI_MIGRATION_TEST_DATABASE_URL";

    const OLD_SCHEMA: &str = "CREATE TABLE wiki_revisions (\
        id BIGINT PRIMARY KEY, \
        wiki_page_id BIGINT NOT NULL, \
        content TEXT NOT NULL, \
        version INTEGER NOT NULL, \
        created_at TIMESTAMP NOT NULL\
    );";

    #[tokio::test]
    async fn duplicate_versions_are_repaired_before_uniqueness_is_added() {
        let database_url =
            std::env::var(TEST_DATABASE_URL_ENV).unwrap_or_else(|_| "sqlite::memory:".to_string());
        let db = Database::connect(&database_url).await.unwrap();
        let backend = db.get_database_backend();
        db.execute_unprepared(&format!(
            "{OLD_SCHEMA}\
             INSERT INTO wiki_revisions \
                 (id, wiki_page_id, content, version, created_at) VALUES \
                 (1, 10, 'first', 1, '2026-08-01 00:00:00'), \
                 (2, 10, 'raced', 1, '2026-08-01 00:00:01'), \
                 (3, 10, 'later', 2, '2026-08-01 00:00:02'), \
                 (4, 20, 'gap', 3, '2026-08-01 00:00:00'), \
                 (5, 20, 'gap-raced', 3, '2026-08-01 00:00:01');"
        ))
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let rows = db
            .query_all(Statement::from_string(
                backend,
                "SELECT id, version FROM wiki_revisions ORDER BY id".to_string(),
            ))
            .await
            .unwrap();
        let repaired = rows
            .into_iter()
            .map(|row| {
                (
                    row.try_get::<i64>("", "id").unwrap(),
                    row.try_get::<i32>("", "version").unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(repaired, vec![(1, 1), (2, 2), (3, 3), (4, 3), (5, 4)]);

        let duplicate = db
            .execute_unprepared(
                "INSERT INTO wiki_revisions \
                 (id, wiki_page_id, content, version, created_at) \
                 VALUES (6, 10, 'must fail', 2, '2026-08-01 00:00:03')",
            )
            .await;
        assert!(
            duplicate.is_err(),
            "the migration repaired rows but did not enforce future uniqueness"
        );

        Migration.up(&SchemaManager::new(&db)).await.unwrap();
    }

    #[tokio::test]
    async fn it_is_a_no_op_before_the_wiki_revision_table_exists() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
    }
}
