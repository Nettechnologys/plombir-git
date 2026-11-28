//! `write_ref_prefix` on an agent run: the refs its tokens may write
//! (ADR-0001 D3, D4; card_49c845088d31).
//!
//! The CI gate holds a pull request whose head an agent wrote until a person
//! approves that head. Knowing the pull requests a run opened or iterates on
//! (`result_pull_request_id`, `pull_request_id`) is not enough: a person can
//! open one from a branch the agent pushed to, and that pull request belongs
//! to no run. What the agent could have written is decided by the prefix its
//! token was confined to, and the token is deleted when the attempt ends — so
//! the run keeps the prefix, for as long as the run row exists.
//!
//! `NULL` is a run recorded before this column, and a run that never got a
//! token. Neither held a branch of its own, so neither holds one now.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261128_000001_agent_run_write_ref_prefix"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("agent_runs", "write_ref_prefix").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRuns::Table)
                    .add_column(ColumnDef::new(AgentRuns::WriteRefPrefix).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("agent_runs", "write_ref_prefix").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(AgentRuns::Table)
                    .drop_column(AgentRuns::WriteRefPrefix)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum AgentRuns {
    Table,
    WriteRefPrefix,
}
