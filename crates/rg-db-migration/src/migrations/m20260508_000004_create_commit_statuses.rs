use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260508_000004_create_commit_statuses"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CommitStatuses::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CommitStatuses::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CommitStatuses::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CommitStatuses::Sha)
                            .string_len(40)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CommitStatuses::State)
                            .string_len(20)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CommitStatuses::Context)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CommitStatuses::Description)
                            .string_len(500)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CommitStatuses::TargetUrl)
                            .string_len(500)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CommitStatuses::CreatorId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CommitStatuses::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CommitStatuses::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    // Unique constraint: keep inline (valid SQLite syntax)
                    .index(
                        Index::create()
                            .unique()
                            .col(CommitStatuses::RepoId)
                            .col(CommitStatuses::Sha)
                            .col(CommitStatuses::Context)
                            .name("idx_commit_statuses_repo_sha_context_unique"),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(CommitStatuses::Table, CommitStatuses::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(CommitStatuses::Table, CommitStatuses::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Non-unique index: create separately for SQLite compatibility
        manager
            .create_index(
                Index::create()
                    .table(CommitStatuses::Table)
                    .col(CommitStatuses::RepoId)
                    .col(CommitStatuses::Sha)
                    .name("idx_commit_statuses_repo_sha")
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(CommitStatuses::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum CommitStatuses {
    Table,
    Id,
    RepoId,
    Sha,
    State,
    Context,
    Description,
    TargetUrl,
    CreatorId,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
