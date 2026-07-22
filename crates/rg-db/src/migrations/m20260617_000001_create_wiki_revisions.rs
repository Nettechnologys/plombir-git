//! Migration: create wiki_revisions table.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260617_000001_create_wiki_revisions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("wiki_revisions").await? {
            return Ok(());
        }
        manager
            .create_table(
                Table::create()
                    .table(WikiRevisions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WikiRevisions::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(WikiRevisions::WikiPageId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(WikiRevisions::Content).text().not_null())
                    .col(ColumnDef::new(WikiRevisions::Message).text().null())
                    .col(ColumnDef::new(WikiRevisions::AuthorId).big_integer().null())
                    .col(
                        ColumnDef::new(WikiRevisions::Version)
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(WikiRevisions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(WikiRevisions::Table, WikiRevisions::WikiPageId)
                            .to(WikiPages::Table, WikiPages::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_wiki_revisions_page")
                    .table(WikiRevisions::Table)
                    .col(WikiRevisions::WikiPageId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(WikiRevisions::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden)]
enum WikiRevisions {
    Table,
    Id,
    WikiPageId,
    Content,
    Message,
    AuthorId,
    Version,
    CreatedAt,
}

#[derive(Iden)]
enum WikiPages {
    Table,
    Id,
}
