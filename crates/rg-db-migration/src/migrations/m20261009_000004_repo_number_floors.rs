//! A repository's issue and pull-request numbers are never handed out twice.
//!
//! Both sequences were `max(number) + 1` over the rows that exist, so deleting
//! the newest issue (card_60961272e1ba) — and now the newest pull request
//! (card_ee4f318c50f1) — gave its number to the next one filed. Every `#N` in a
//! commit message, a mention, a notification e-mail or an external link then
//! named a different item, and a pull request's CI ref (`refs/pull/N/head`)
//! inherited the pipelines of the one deleted before it.
//!
//! This table keeps, per repository and number space, the highest number ever
//! retired by a deletion. The allocators take the larger of it and the highest
//! live number, so a deleted number stays spent. One row per space rather than
//! one per deleted number: only the high-water mark decides the next number,
//! and a gap below it is never refilled anyway.
//!
//! The row is the repository's, so it cascades with it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RepoNumberFloors::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RepoNumberFloors::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    // `issue` or `pull_request`.
                    .col(
                        ColumnDef::new(RepoNumberFloors::Space)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RepoNumberFloors::RetiredUpTo)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(RepoNumberFloors::RepoId)
                            .col(RepoNumberFloors::Space),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(RepoNumberFloors::Table, RepoNumberFloors::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RepoNumberFloors::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum RepoNumberFloors {
    Table,
    RepoId,
    Space,
    RetiredUpTo,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}
