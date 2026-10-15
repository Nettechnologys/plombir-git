use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260511_000001_add_pr_head_repo_id"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PullRequests::Table)
                    .add_column(
                        ColumnDef::new(PullRequests::HeadRepoId)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create index on head_repo_id for fork PR queries
        manager
            .create_index(
                Index::create()
                    .name("idx_pull_requests_head_repo_id")
                    .table(PullRequests::Table)
                    .col(PullRequests::HeadRepoId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_pull_requests_head_repo_id")
                    .table(PullRequests::Table)
                    .to_owned(),
            )
            .await?;

        // SQLite doesn't support DROP COLUMN, so we use a table rebuild approach
        // For simplicity, we leave the column in place on down migration
        Ok(())
    }
}

#[derive(Iden)]
enum PullRequests {
    Table,
    HeadRepoId,
}
