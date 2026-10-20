//! `on: schedule`: the tick ledger, and the provenance a scheduled run's retry
//! replays.
//!
//! `ci_schedule_ticks` holds one row per `- cron:` entry of one workflow file of
//! one repository: the last tick that was claimed for it. A tick is run by
//! whoever advances that row past it, in one conditional `UPDATE`, so a restart
//! cannot run a tick a second time and two replicas cannot both run it. The
//! tick is stored as whole minutes since the Unix epoch rather than as a
//! timestamp: the claim is a `<` comparison, and an integer compares the same
//! way on every backend, where SQLite's textual timestamps compare by spelling.
//!
//! `pipelines.schedule_workflow` / `schedule_cron` say which entry of which file
//! a `schedule` pipeline ran for. A retry replays the event a run was created
//! under, and for this event the name alone selects nothing.
//!
//! Both text columns are `VARCHAR(255)`: they are the ledger's unique key, and
//! MySQL cannot index a `TEXT` column whole. The engine refuses a cron entry
//! longer than that when the workflow is read.

use sea_orm_migration::prelude::*;

pub struct Migration;

pub const NAME: &str = "m20261020_000001_ci_schedules";
pub const KEY_INDEX: &str = "idx_ci_schedule_ticks_key";

impl MigrationName for Migration {
    fn name(&self) -> &str {
        NAME
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CiScheduleTicks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CiScheduleTicks::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CiScheduleTicks::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiScheduleTicks::Workflow)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiScheduleTicks::Cron)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiScheduleTicks::LastTickMinute)
                            .big_integer()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ci_schedule_ticks_repo")
                            .from(CiScheduleTicks::Table, CiScheduleTicks::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(KEY_INDEX)
                    .table(CiScheduleTicks::Table)
                    .col(CiScheduleTicks::RepoId)
                    .col(CiScheduleTicks::Workflow)
                    .col(CiScheduleTicks::Cron)
                    .unique()
                    .to_owned(),
            )
            .await?;

        for (column, name) in PIPELINE_COLUMNS {
            if !manager.has_column("pipelines", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Pipelines::Table)
                            .add_column(ColumnDef::new(column).string_len(255).null())
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (column, name) in PIPELINE_COLUMNS {
            if manager.has_column("pipelines", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Pipelines::Table)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        manager
            .drop_table(
                Table::drop()
                    .table(CiScheduleTicks::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden)]
enum CiScheduleTicks {
    Table,
    Id,
    RepoId,
    Workflow,
    Cron,
    LastTickMinute,
}

#[derive(Iden, Clone, Copy)]
enum Pipelines {
    Table,
    ScheduleWorkflow,
    ScheduleCron,
}

const PIPELINE_COLUMNS: [(Pipelines, &str); 2] = [
    (Pipelines::ScheduleWorkflow, "schedule_workflow"),
    (Pipelines::ScheduleCron, "schedule_cron"),
];

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}
