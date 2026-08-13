//! card_24f475c09a17: give the `workflow_dispatch` inputs of a manual run
//! somewhere to live.
//!
//! A pipeline row records the event, the ref and the commit — everything a
//! retry needs to run the *same* build again, except the named values the
//! caller actually typed into it. Retry therefore re-read the workflow with no
//! inputs at all: a declared `default:` quietly replaced the value the original
//! run was started with, and a `required: true` input turned the retry of a
//! perfectly good pipeline into a refusal about a missing value the caller had
//! already supplied once.
//!
//! The caller's own map is stored, not the resolved one. A retry re-runs the
//! same commit, so the same declarations apply and re-resolving reproduces the
//! original run exactly — while storing the resolved values would freeze
//! defaults into rows that never asked for them, and lose the distinction
//! between "the caller chose `staging`" and "the workflow defaults to it".
//!
//! `NULL` is "this pipeline carries no dispatch inputs": every row that exists
//! when this migration runs, every automatic producer, and every manual run
//! started with an empty map. Backfilling is impossible and would be a guess:
//! the values were never written down anywhere else — the job environment holds
//! normalized `INPUT_*` names a reusable child job may have overwritten with
//! its own `workflow_call` input of the same name.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260813_000001_add_pipeline_dispatch_inputs"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipelines", "dispatch_inputs").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Pipelines::Table)
                        .add_column(ColumnDef::new(Pipelines::DispatchInputs).text().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("pipelines", "dispatch_inputs").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Pipelines::Table)
                        .drop_column(Pipelines::DispatchInputs)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Pipelines {
    Table,
    DispatchInputs,
}
