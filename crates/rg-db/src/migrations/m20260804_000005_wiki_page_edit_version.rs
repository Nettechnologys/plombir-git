//! Migration: attach the current wiki state to its latest revision number.
//!
//! The token is initialized from existing history and then advanced in the
//! same transaction that snapshots and overwrites the page. It gives all three
//! database backends the same compare-and-swap boundary without depending on
//! timestamp precision or process-local locks.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

async fn backfill_edit_versions(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    if !manager.has_table("wiki_revisions").await? {
        return Ok(());
    }

    let db = manager.get_connection();
    let backend = db.get_database_backend();
    db.execute(Statement::from_string(
        backend,
        "UPDATE wiki_pages \
         SET edit_version = COALESCE((\
             SELECT MAX(wiki_revisions.version) \
             FROM wiki_revisions \
             WHERE wiki_revisions.wiki_page_id = wiki_pages.id\
         ), 0)"
            .to_string(),
    ))
    .await?;
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("wiki_pages").await? {
            return Ok(());
        }
        if !manager.has_column("wiki_pages", "edit_version").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WikiPages::Table)
                        .add_column(
                            ColumnDef::new(WikiPages::EditVersion)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }
        backfill_edit_versions(manager).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("wiki_pages", "edit_version").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WikiPages::Table)
                        .drop_column(WikiPages::EditVersion)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum WikiPages {
    Table,
    EditVersion,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::Database;

    const TEST_DATABASE_URL_ENV: &str = "FORGEKEEP_WIKI_EDIT_VERSION_TEST_DATABASE_URL";

    #[tokio::test]
    async fn existing_pages_start_at_their_latest_revision() {
        let database_url =
            std::env::var(TEST_DATABASE_URL_ENV).unwrap_or_else(|_| "sqlite::memory:".to_string());
        let db = Database::connect(&database_url).await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE wiki_pages (id BIGINT PRIMARY KEY); \
             CREATE TABLE wiki_revisions (\
                 id BIGINT PRIMARY KEY, wiki_page_id BIGINT NOT NULL, version INTEGER NOT NULL\
             ); \
             INSERT INTO wiki_pages (id) VALUES (10), (20); \
             INSERT INTO wiki_revisions (id, wiki_page_id, version) VALUES \
                 (1, 10, 1), (2, 10, 4);",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        let rows = db
            .query_all(Statement::from_string(
                db.get_database_backend(),
                "SELECT id, edit_version FROM wiki_pages ORDER BY id".to_string(),
            ))
            .await
            .unwrap();
        let versions = rows
            .into_iter()
            .map(|row| {
                (
                    row.try_get::<i64>("", "id").unwrap(),
                    row.try_get::<i32>("", "edit_version").unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(versions, vec![(10, 4), (20, 0)]);

        Migration.up(&SchemaManager::new(&db)).await.unwrap();
    }
}
