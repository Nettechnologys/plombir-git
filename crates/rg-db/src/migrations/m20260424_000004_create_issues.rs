use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Issues::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Issues::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Issues::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(Issues::Number).big_integer().not_null())
                    .col(ColumnDef::new(Issues::Title).string().not_null())
                    .col(ColumnDef::new(Issues::Body).string().null())
                    .col(
                        ColumnDef::new(Issues::State)
                            .string()
                            .not_null()
                            .default("open"),
                    )
                    .col(ColumnDef::new(Issues::AuthorId).big_integer().not_null())
                    .col(ColumnDef::new(Issues::AssigneeId).big_integer().null())
                    .col(ColumnDef::new(Issues::MilestoneId).big_integer().null())
                    .col(ColumnDef::new(Issues::Labels).string().null())
                    .col(
                        ColumnDef::new(Issues::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Issues::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Issues::ClosedAt)
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
                    .name("idx_issues_repo_number")
                    .table(Issues::Table)
                    .col(Issues::RepoId)
                    .col(Issues::Number)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Index for listing open issues by repo
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_issues_repo_state")
                    .table(Issues::Table)
                    .col(Issues::RepoId)
                    .col(Issues::State)
                    .to_owned(),
            )
            .await?;

        // Issue comments
        manager
            .create_table(
                Table::create()
                    .table(IssueComments::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(IssueComments::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(IssueComments::IssueId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(IssueComments::AuthorId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(IssueComments::Body).string().not_null())
                    .col(
                        ColumnDef::new(IssueComments::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(IssueComments::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_issue_comments_issue_id")
                    .table(IssueComments::Table)
                    .col(IssueComments::IssueId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(IssueComments::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Issues::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Issues {
    Table,
    Id,
    RepoId,
    Number,
    Title,
    Body,
    State,
    AuthorId,
    AssigneeId,
    MilestoneId,
    Labels,
    CreatedAt,
    UpdatedAt,
    ClosedAt,
}

#[derive(DeriveIden)]
enum IssueComments {
    Table,
    Id,
    IssueId,
    AuthorId,
    Body,
    CreatedAt,
    UpdatedAt,
}
