use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Pull requests
        manager
            .create_table(
                Table::create()
                    .table(PullRequests::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PullRequests::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PullRequests::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PullRequests::Number)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PullRequests::Title).string().not_null())
                    .col(ColumnDef::new(PullRequests::Body).string().null())
                    .col(
                        ColumnDef::new(PullRequests::State)
                            .string()
                            .not_null()
                            .default("open"),
                    )
                    .col(
                        ColumnDef::new(PullRequests::AuthorId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PullRequests::ReviewerId)
                            .big_integer()
                            .null(),
                    )
                    .col(ColumnDef::new(PullRequests::HeadBranch).string().not_null())
                    .col(ColumnDef::new(PullRequests::BaseBranch).string().not_null())
                    .col(ColumnDef::new(PullRequests::HeadSha).string().null())
                    .col(ColumnDef::new(PullRequests::MergeStrategy).string().null())
                    .col(ColumnDef::new(PullRequests::MergeCommitSha).string().null())
                    .col(
                        ColumnDef::new(PullRequests::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(PullRequests::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(PullRequests::ClosedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(PullRequests::MergedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique index: (repo_id, number)
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_pr_repo_number")
                    .table(PullRequests::Table)
                    .col(PullRequests::RepoId)
                    .col(PullRequests::Number)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Index for listing open PRs by repo
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_pr_repo_state")
                    .table(PullRequests::Table)
                    .col(PullRequests::RepoId)
                    .col(PullRequests::State)
                    .to_owned(),
            )
            .await?;

        // Milestones
        manager
            .create_table(
                Table::create()
                    .table(Milestones::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Milestones::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Milestones::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(Milestones::Title).string().not_null())
                    .col(ColumnDef::new(Milestones::Description).string().null())
                    .col(
                        ColumnDef::new(Milestones::State)
                            .string()
                            .not_null()
                            .default("open"),
                    )
                    .col(
                        ColumnDef::new(Milestones::DueDate)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Milestones::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Milestones::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Milestones::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(PullRequests::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum PullRequests {
    Table,
    Id,
    RepoId,
    Number,
    Title,
    Body,
    State,
    AuthorId,
    ReviewerId,
    HeadBranch,
    BaseBranch,
    HeadSha,
    MergeStrategy,
    MergeCommitSha,
    CreatedAt,
    UpdatedAt,
    ClosedAt,
    MergedAt,
}

#[derive(DeriveIden)]
enum Milestones {
    Table,
    Id,
    RepoId,
    Title,
    Description,
    State,
    DueDate,
    CreatedAt,
    UpdatedAt,
}
