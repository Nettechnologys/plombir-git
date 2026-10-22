//! `run_name` on a pipeline: the `run-name:` of the Actions workflow it runs,
//! rendered for the run (`Deploy staging`), what the pipeline list shows in
//! place of the commit and ref (card_9f345eccf1e3).
//!
//! `NULL` is "no workflow this run executes has a `run-name:`": every native
//! pipeline, and every row that exists when this migration runs, is shown as
//! before. A text column, because the name is the author's text rendered with
//! the caller's inputs, not an identifier of bounded length.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261022_000002_pipeline_run_name"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("pipelines", "run_name").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Pipelines::Table)
                    .add_column(ColumnDef::new(Pipelines::RunName).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipelines", "run_name").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Pipelines::Table)
                    .drop_column(Pipelines::RunName)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Pipelines {
    Table,
    RunName,
}
