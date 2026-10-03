//! card_74d58ec3ac1e: give the branch a run was filtered on, and the revision it
//! was diffed against, somewhere to live.
//!
//! The pipeline row records the event, the ref and the commit — three of the
//! five values a producer parameterises a trigger with. `dispatch_inputs` is
//! the fourth; these two are the rest. Without them a retry re-read the
//! workflow with `base_branch: None` and `previous_sha: None`, and both
//! defaults are answers about *today's* repository rather than about the run
//! being repeated:
//!
//! * `base_branch` falls back to the repository's default branch, so a retried
//!   `pull_request` run for a PR into `develop` was matched against `main`. The
//!   workflow filtered on `branches: [develop]` then selects nothing, and the
//!   fallthrough to `.plombir-git-ci.yml` means the retry can publish a
//!   *different graph* under the same `201` — or, with no native config, a
//!   `400` about a pipeline that ran an hour ago.
//! * `previous_sha` falls back to the commit's first parent, so a `paths:`
//!   filter on the retry of a multi-commit push is computed over a narrower
//!   diff than the push itself had, and a job that ran the first time is
//!   skipped the second.
//!
//! `NULL` is "this run had none", which is the honest reading for every row
//! that exists when this migration runs: the values were never written down, so
//! a retry of an old row keeps exactly today's behaviour instead of inventing a
//! branch or a revision. Backfilling is impossible — the base branch of the PR
//! a pipeline belonged to is not recoverable from the pipeline, and the ref's
//! position before a push months ago is gone with the reflog.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260813_000002_add_pipeline_replay_context"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipelines", "base_branch").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Pipelines::Table)
                        .add_column(ColumnDef::new(Pipelines::BaseBranch).text().null())
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column("pipelines", "previous_sha").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Pipelines::Table)
                        .add_column(ColumnDef::new(Pipelines::PreviousSha).text().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("pipelines", "previous_sha").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Pipelines::Table)
                        .drop_column(Pipelines::PreviousSha)
                        .to_owned(),
                )
                .await?;
        }
        if manager.has_column("pipelines", "base_branch").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Pipelines::Table)
                        .drop_column(Pipelines::BaseBranch)
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
    BaseBranch,
    PreviousSha,
}
