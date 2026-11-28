//! What starting an agent run has to write down so that a start is never lost
//! and never doubled (ADR-0001 D7, D9; card_b391e92474a0).
//!
//! - **`agent_runs.commit_sha`** — the commit the run was started from. The
//!   pipeline records it too, but a run whose pipeline was never created (the
//!   server stopped between the assignment and the queue) has only its own row
//!   to say where it starts, and the job's checkout and task file read it from
//!   there rather than from wherever the default branch is by then. `NULL` is
//!   a run recorded before this column.
//! - **`agent_runs.task`** — the task file the server wrote for the run when
//!   it was assigned (ADR-0001 D7, card_dcf93bf54722). Kept, not rebuilt: an
//!   edit of the issue after the assignment does not reach a run already under
//!   way, and every attempt reads the same task. `NULL` is a run recorded
//!   before this column.
//! - **`agent_run_quotas`** — how many runs each bot started on one UTC day.
//!   The daily limit is reserved by a conditional `UPDATE ... WHERE used <
//!   limit` in the transaction that records the run, so two assignments racing
//!   for the last run of the day cannot both pass a count read a moment
//!   earlier. One row per bot and day, created by the first run of that day.
//!   - `bot_id` carries no foreign key, as on `agent_runs`: the count sits
//!     beside the runs it counts, which outlive the bot. Nothing reads a row
//!     once its day is over.
//!   - `day` is `YYYY-MM-DD`, a server-built key a unique index covers, so it
//!     is a short `VARCHAR` rather than `TEXT` (MySQL indexes no `TEXT` without
//!     a prefix length).

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261128_000002_agent_run_launch_state"
    }
}

/// Width of `agent_run_quotas.day`: `YYYY-MM-DD`.
pub const QUOTA_DAY_CHARS: u32 = 10;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, column) in [
            ("commit_sha", AgentRuns::CommitSha),
            ("task", AgentRuns::Task),
        ] {
            if !manager.has_column("agent_runs", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(AgentRuns::Table)
                            .add_column(ColumnDef::new(column).text().null())
                            .to_owned(),
                    )
                    .await?;
            }
        }
        manager
            .create_table(
                Table::create()
                    .table(AgentRunQuotas::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AgentRunQuotas::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AgentRunQuotas::BotId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentRunQuotas::Day)
                            .string_len(QUOTA_DAY_CHARS)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentRunQuotas::Used)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_agent_run_quotas_bot_day")
                    .table(AgentRunQuotas::Table)
                    .col(AgentRunQuotas::BotId)
                    .col(AgentRunQuotas::Day)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(AgentRunQuotas::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        for (name, column) in [
            ("task", AgentRuns::Task),
            ("commit_sha", AgentRuns::CommitSha),
        ] {
            if manager.has_column("agent_runs", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(AgentRuns::Table)
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
enum AgentRuns {
    Table,
    CommitSha,
    Task,
}

#[derive(DeriveIden)]
enum AgentRunQuotas {
    Table,
    Id,
    BotId,
    Day,
    Used,
}
