//! `head_branch` on a pipeline: the branch a pull request comes from, what a
//! job reads as `github.head_ref` (card_af3833480552).
//!
//! The run's other event context already lives on the row — `base_branch` is
//! `github.base_ref` — and this is the half no other column carries: the ref of
//! a pull-request run is `refs/pull/<n>/head`, which names the request, not the
//! branch. Both runners read it from here, and so does a retry and a matrix
//! decided at run time, so it is written by the producer that holds the pull
//! request rather than looked up again.
//!
//! `NULL` is "this run had none": every event other than `pull_request`, and
//! every row that exists when this migration runs. Such a row reads
//! `github.head_ref` as `''` — what it would have read before, since the name
//! used to be refused outright.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261021_000003_pipeline_head_branch"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("pipelines", "head_branch").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Pipelines::Table)
                    .add_column(ColumnDef::new(Pipelines::HeadBranch).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipelines", "head_branch").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Pipelines::Table)
                    .drop_column(Pipelines::HeadBranch)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Pipelines {
    Table,
    HeadBranch,
}
