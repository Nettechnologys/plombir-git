use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260508_000001_create_repo_stars_watches"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // --- repo_stars ---
        manager
            .create_table(
                Table::create()
                    .table(RepoStars::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RepoStars::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(RepoStars::UserId).big_integer().not_null())
                    .col(ColumnDef::new(RepoStars::RepoId).big_integer().not_null())
                    .col(
                        ColumnDef::new(RepoStars::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(RepoStars::Table, RepoStars::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(RepoStars::Table, RepoStars::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create unique index for repo_stars (user_id, repo_id)
        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_repo_stars_user_repo_unique")
                    .table(RepoStars::Table)
                    .col(RepoStars::UserId)
                    .col(RepoStars::RepoId)
                    .to_owned(),
            )
            .await?;

        // Create index for repo_stars (repo_id)
        manager
            .create_index(
                Index::create()
                    .name("idx_repo_stars_repo_id")
                    .table(RepoStars::Table)
                    .col(RepoStars::RepoId)
                    .to_owned(),
            )
            .await?;

        // --- repo_watches ---
        manager
            .create_table(
                Table::create()
                    .table(RepoWatches::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RepoWatches::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(RepoWatches::UserId).big_integer().not_null())
                    .col(ColumnDef::new(RepoWatches::RepoId).big_integer().not_null())
                    .col(
                        ColumnDef::new(RepoWatches::WatchState)
                            .string()
                            .not_null()
                            .default("not_watching".to_owned()),
                    )
                    .col(
                        ColumnDef::new(RepoWatches::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RepoWatches::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(RepoWatches::Table, RepoWatches::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(RepoWatches::Table, RepoWatches::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create unique index for repo_watches (user_id, repo_id)
        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_repo_watches_user_repo_unique")
                    .table(RepoWatches::Table)
                    .col(RepoWatches::UserId)
                    .col(RepoWatches::RepoId)
                    .to_owned(),
            )
            .await?;

        // Create index for repo_watches (repo_id)
        manager
            .create_index(
                Index::create()
                    .name("idx_repo_watches_repo_id")
                    .table(RepoWatches::Table)
                    .col(RepoWatches::RepoId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RepoWatches::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(RepoStars::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum RepoStars {
    Table,
    Id,
    UserId,
    RepoId,
    CreatedAt,
}

#[derive(Iden)]
enum RepoWatches {
    Table,
    Id,
    UserId,
    RepoId,
    WatchState,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}
