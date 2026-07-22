//! Create the append-only pull-request event stream used by the review timeline.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000001_create_pr_events"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("pr_events").await? {
            return Ok(());
        }
        manager
            .create_table(
                Table::create()
                    .table(PrEvents::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PrEvents::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(PrEvents::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(PrEvents::PrId).big_integer().not_null())
                    .col(ColumnDef::new(PrEvents::ActorId).big_integer().null())
                    .col(ColumnDef::new(PrEvents::EventType).string().not_null())
                    .col(ColumnDef::new(PrEvents::Body).text().null())
                    // Writers always persist an explicit JSON object. Avoid a
                    // TEXT default because older MySQL/MariaDB variants reject
                    // defaults on BLOB/TEXT columns.
                    .col(ColumnDef::new(PrEvents::Metadata).text().not_null())
                    .col(
                        ColumnDef::new(PrEvents::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(PrEvents::Table, PrEvents::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(PrEvents::Table, PrEvents::PrId)
                            .to(PullRequests::Table, PullRequests::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(PrEvents::Table, PrEvents::ActorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_pr_events_pr_created")
                    .table(PrEvents::Table)
                    .col(PrEvents::PrId)
                    .col(PrEvents::CreatedAt)
                    .col(PrEvents::Id)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PrEvents::Table).if_exists().to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum PrEvents {
    Table,
    Id,
    RepoId,
    PrId,
    ActorId,
    EventType,
    Body,
    Metadata,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum PullRequests {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
