use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("pull_requests").await? {
            return Ok(());
        }

        if !manager.has_column("pull_requests", "milestone_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PullRequests::Table)
                        .add_column(
                            ColumnDef::new(PullRequests::MilestoneId)
                                .big_integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !manager.has_column("pull_requests", "labels").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PullRequests::Table)
                        .add_column(ColumnDef::new(PullRequests::Labels).string().null())
                        .to_owned(),
                )
                .await?;
        }

        // Index is used when listing PRs by milestone in the future.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_pull_requests_milestone_id")
                    .table(PullRequests::Table)
                    .col(PullRequests::MilestoneId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite does not support DROP COLUMN in a non-destructive migration path we
        // can safely use here, so keep columns in place on rollback.
        Ok(())
    }
}

#[derive(DeriveIden)]
enum PullRequests {
    Table,
    MilestoneId,
    Labels,
}
