//! Add pull-request auto-merge state.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260711_000002_pr_auto_merge"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let columns = [
            (
                PullRequests::AutoMergeEnabled,
                ColumnDef::new(PullRequests::AutoMergeEnabled)
                    .boolean()
                    .not_null()
                    .default(false)
                    .to_owned(),
            ),
            (
                PullRequests::AutoMergeStrategy,
                ColumnDef::new(PullRequests::AutoMergeStrategy)
                    .string()
                    .null()
                    .to_owned(),
            ),
            (
                PullRequests::AutoMergeEnabledById,
                ColumnDef::new(PullRequests::AutoMergeEnabledById)
                    .big_integer()
                    .null()
                    .to_owned(),
            ),
            (
                PullRequests::AutoMergeEnabledAt,
                ColumnDef::new(PullRequests::AutoMergeEnabledAt)
                    .timestamp_with_time_zone()
                    .null()
                    .to_owned(),
            ),
        ];
        for (name, definition) in columns {
            if !manager
                .has_column("pull_requests", name.to_string().as_str())
                .await?
            {
                manager
                    .alter_table(
                        Table::alter()
                            .table(PullRequests::Table)
                            .add_column(&mut definition.clone())
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            PullRequests::AutoMergeEnabledAt,
            PullRequests::AutoMergeEnabledById,
            PullRequests::AutoMergeStrategy,
            PullRequests::AutoMergeEnabled,
        ] {
            if manager
                .has_column("pull_requests", column.to_string().as_str())
                .await?
            {
                manager
                    .alter_table(
                        Table::alter()
                            .table(PullRequests::Table)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum PullRequests {
    Table,
    AutoMergeEnabled,
    AutoMergeStrategy,
    AutoMergeEnabledById,
    AutoMergeEnabledAt,
}
