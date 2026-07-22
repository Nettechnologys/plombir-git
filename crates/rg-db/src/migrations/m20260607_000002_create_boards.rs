//! Migration: create boards, board_columns, and board_cards tables
//! for Kanban-style project boards.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260607_000002_create_boards"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── boards table ──
        manager
            .create_table(
                Table::create()
                    .table(Board::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Board::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Board::RepoId).big_integer().null())
                    .col(ColumnDef::new(Board::OrgId).big_integer().null())
                    .col(ColumnDef::new(Board::Name).string().not_null())
                    .col(ColumnDef::new(Board::Description).text().null())
                    .col(ColumnDef::new(Board::CreatedBy).big_integer().not_null())
                    .col(
                        ColumnDef::new(Board::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Board::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Board::Table, Board::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Board::Table, Board::OrgId)
                            .to(Organizations::Table, Organizations::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Board::Table, Board::CreatedBy)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // ── board_columns table ──
        manager
            .create_table(
                Table::create()
                    .table(BoardColumn::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BoardColumn::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(BoardColumn::BoardId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(BoardColumn::Name).string().not_null())
                    .col(ColumnDef::new(BoardColumn::Color).string().null())
                    .col(
                        ColumnDef::new(BoardColumn::Position)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(BoardColumn::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(BoardColumn::Table, BoardColumn::BoardId)
                            .to(Board::Table, Board::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // ── board_cards table ──
        manager
            .create_table(
                Table::create()
                    .table(BoardCard::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BoardCard::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(BoardCard::ColumnId).big_integer().not_null())
                    .col(ColumnDef::new(BoardCard::IssueId).big_integer().null())
                    .col(ColumnDef::new(BoardCard::Note).text().null())
                    .col(
                        ColumnDef::new(BoardCard::Position)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(BoardCard::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(BoardCard::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(BoardCard::Table, BoardCard::ColumnId)
                            .to(BoardColumn::Table, BoardColumn::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(BoardCard::Table, BoardCard::IssueId)
                            .to(Issues::Table, Issues::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BoardCard::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(BoardColumn::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Board::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum Board {
    Table,
    Id,
    RepoId,
    OrgId,
    Name,
    Description,
    CreatedBy,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum BoardColumn {
    Table,
    Id,
    BoardId,
    Name,
    Color,
    Position,
    CreatedAt,
}

#[derive(Iden)]
enum BoardCard {
    Table,
    Id,
    ColumnId,
    IssueId,
    Note,
    Position,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}

#[derive(Iden)]
enum Organizations {
    Table,
    Id,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}

#[derive(Iden)]
enum Issues {
    Table,
    Id,
}
