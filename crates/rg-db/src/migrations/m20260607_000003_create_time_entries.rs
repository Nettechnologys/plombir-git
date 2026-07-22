//! Migration: create time_entries table for issue time tracking.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260607_000003_create_time_entries"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TimeEntry::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TimeEntry::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TimeEntry::IssueId).big_integer().not_null())
                    .col(ColumnDef::new(TimeEntry::UserId).big_integer().not_null())
                    .col(
                        ColumnDef::new(TimeEntry::DurationMinutes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TimeEntry::Description).text().null())
                    .col(
                        ColumnDef::new(TimeEntry::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(TimeEntry::Table, TimeEntry::IssueId)
                            .to(Issues::Table, Issues::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(TimeEntry::Table, TimeEntry::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_time_entries_issue")
                    .table(TimeEntry::Table)
                    .col(TimeEntry::IssueId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_time_entries_user")
                    .table(TimeEntry::Table)
                    .col(TimeEntry::UserId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TimeEntry::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum TimeEntry {
    Table,
    Id,
    IssueId,
    UserId,
    DurationMinutes,
    Description,
    CreatedAt,
}

#[derive(Iden)]
enum Issues {
    Table,
    Id,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
