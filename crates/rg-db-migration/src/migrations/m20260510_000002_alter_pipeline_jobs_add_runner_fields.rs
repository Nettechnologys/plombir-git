use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260510_000002_alter_pipeline_jobs_add_runner_fields"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipeline_jobs", "runner_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("pipeline_jobs"))
                        .add_column(ColumnDef::new(Alias::new("runner_id")).big_integer().null())
                        .to_owned(),
                )
                .await?;
        }
        for column in ["started_at", "finished_at"] {
            if !manager.has_column("pipeline_jobs", column).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new("pipeline_jobs"))
                            .add_column(
                                ColumnDef::new(Alias::new(column))
                                    .timestamp_with_time_zone()
                                    .null(),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
