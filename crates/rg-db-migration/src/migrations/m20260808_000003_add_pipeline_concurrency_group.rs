//! card_4c5214698ae9: give `concurrency.group` somewhere to live.
//!
//! The group name was parsed from both config dialects, had its `${{ ref }}` /
//! `${{ branch }}` placeholders expanded, and was then used for nothing but a
//! log line: the search for "what does this pipeline have to wait for" ran on
//! `ref_name`. That is wrong in both directions — a fixed group shared by two
//! branches did not serialize, and two workflows with *different* groups on one
//! branch cancelled each other.
//!
//! `NULL` is "this pipeline declared no concurrency group", which is every row
//! that exists when this migration runs and every row a workflow without a
//! `concurrency:` block will write afterwards. Such a pipeline neither waits for
//! anything nor is cancelled by anyone, so backfilling is not just unnecessary —
//! guessing a group for history would invent serialization that never applied.
//!
//! The index is `(repo_id, concurrency_group)`: the lookup is always scoped to
//! one repository, and a group name is only meaningful inside one.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260808_000003_add_pipeline_concurrency_group"
    }
}

const INDEX_NAME: &str = "idx_pipelines_repo_concurrency_group";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipelines", "concurrency_group").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Pipelines::Table)
                        .add_column(ColumnDef::new(Pipelines::ConcurrencyGroup).string().null())
                        .to_owned(),
                )
                .await?;
        }

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(INDEX_NAME)
                    .table(Pipelines::Table)
                    .col(Pipelines::RepoId)
                    .col(Pipelines::ConcurrencyGroup)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(INDEX_NAME)
                    .table(Pipelines::Table)
                    .to_owned(),
            )
            .await?;

        if manager.has_column("pipelines", "concurrency_group").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Pipelines::Table)
                        .drop_column(Pipelines::ConcurrencyGroup)
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
    RepoId,
    ConcurrencyGroup,
}
