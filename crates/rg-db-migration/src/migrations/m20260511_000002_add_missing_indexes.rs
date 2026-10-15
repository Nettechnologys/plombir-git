use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── Ensure notifications table exists (may have been missed in phase8) ──
        manager
            .create_table(
                Table::create()
                    .table(Notifications::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Notifications::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Notifications::UserId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Notifications::EventType).string().not_null())
                    .col(ColumnDef::new(Notifications::Title).string().not_null())
                    .col(ColumnDef::new(Notifications::Body).string().null())
                    .col(ColumnDef::new(Notifications::RepoId).big_integer().null())
                    .col(
                        ColumnDef::new(Notifications::IsRead)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Notifications::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // repositories(org_id) — used by list_by_org, find_by_org_and_name
        manager
            .create_index(
                Index::create()
                    .name("idx_repositories_org_id")
                    .table(Repositories::Table)
                    .col(Repositories::OrgId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        // repositories(origin_repo_id) — used by list_forks, count forks
        manager
            .create_index(
                Index::create()
                    .name("idx_repositories_origin_repo_id")
                    .table(Repositories::Table)
                    .col(Repositories::OriginRepoId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        // notifications(user_id, is_read) — used by list_notifications
        manager
            .create_index(
                Index::create()
                    .name("idx_notifications_user_id_is_read")
                    .table(Notifications::Table)
                    .col(Notifications::UserId)
                    .col(Notifications::IsRead)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        // notifications(repo_id) — used by watcher notification lookups
        manager
            .create_index(
                Index::create()
                    .name("idx_notifications_repo_id")
                    .table(Notifications::Table)
                    .col(Notifications::RepoId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(Index::drop().name("idx_repositories_org_id").to_owned())
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_repositories_origin_repo_id")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_notifications_user_id_is_read")
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(Index::drop().name("idx_notifications_repo_id").to_owned())
            .await?;
        Ok(())
    }
}

// Learn these table/column names from existing migration files.
#[derive(DeriveIden)]
enum Repositories {
    Table,
    OrgId,
    OriginRepoId,
}

#[derive(DeriveIden)]
enum Notifications {
    Table,
    Id,
    UserId,
    EventType,
    Title,
    Body,
    RepoId,
    IsRead,
    CreatedAt,
}
