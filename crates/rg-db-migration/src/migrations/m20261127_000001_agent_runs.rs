//! card_621be18a01e5 (ADR-0001 D12): the rows a run of an agent stands on,
//! written before the code that starts, narrows and finishes one.
//!
//! - **`agent_runs`** — one row per run a person asked a bot for: which bot,
//!   in which repository, for which issue (and, on an iteration, which pull
//!   request), who asked, how it ended, and the pull request it opened.
//!   - `trigger_key` is unique. The event that asked for the run (an
//!     assignment, a mention) names it, so a delivery that arrives twice
//!     inserts once — idempotency lives in the database, not in a check that
//!     two requests can both pass.
//!   - `active` is `TRUE` while the run is not finished and `NULL` after. The
//!     unique `(repository_id, bot_id, active)` index is "one active run per
//!     bot in a repository" (ADR-0001 D9) on all three backends: PostgreSQL,
//!     MySQL and SQLite all treat `NULL`s as distinct in a unique index, so any
//!     number of finished runs sit beside the one live one. A partial index
//!     would say the same more directly, but MySQL has none.
//!   - `bot_id` and `triggered_by` carry no foreign key on purpose. The run is
//!     the durable record that a pull request was written by an agent: the CI
//!     gate (`card_f573f8f39585`) asks it whether to hold a head commit for a
//!     person, long after the run's token is gone — and after the bot itself
//!     is, since its pull requests stay. A cascade from `users` would erase
//!     exactly that answer. Readers treat a missing account as a ghost.
//!   - The repository owns its runs (`ON DELETE CASCADE`). The issue and both
//!     pull-request references are `SET NULL`: deleting one must not take the
//!     record of what an agent did with it.
//! - **`agent_run_attempts`** — a run is executed by one attempt at a time,
//!   each with the pipeline that carried it. A runner lost mid-run is a new
//!   attempt — a new row, never an overwrite of the old one, so the history of
//!   which pipeline held which token survives. The run owns its attempts.
//! - **`access_tokens`**:
//!   - `write_ref_prefix` confines every ref write of the token (ADR-0001 D3).
//!     `NULL` is every token that exists today.
//!   - `allow_git` lets a token confined to MCP tools use the git transport
//!     too. `FALSE` is what every existing token can do now.
//!   - `agent_run_attempt_id` ties a token to the attempt it was issued for.
//!     Unique, so an attempt holds at most one token; `ON DELETE CASCADE`, so
//!     deleting a run takes its attempts and their tokens with it.
//!
//! Every free string is `TEXT` (on MySQL `.string()` is `VARCHAR(255)`). The
//! one exception is `trigger_key`: a server-built key that a unique index has
//! to cover, which MySQL cannot do for `TEXT` without a prefix length.

use sea_orm::{ConnectionTrait, DatabaseBackend};
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261127_000001_agent_runs"
    }
}

/// Longest trigger key the server writes. Room for an event kind and a few
/// ids; MySQL indexes a `VARCHAR` of this width without a prefix length.
pub const TRIGGER_KEY_MAX_CHARS: u32 = 255;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AgentRuns::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AgentRuns::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(AgentRuns::BotId).big_integer().not_null())
                    .col(
                        ColumnDef::new(AgentRuns::RepositoryId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AgentRuns::IssueId).big_integer().null())
                    // The pull request an iteration works on; `NULL` for the
                    // first run of an issue.
                    .col(
                        ColumnDef::new(AgentRuns::PullRequestId)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AgentRuns::Iteration)
                            .big_integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(AgentRuns::TriggeredBy)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentRuns::TriggerKey)
                            .string_len(TRIGGER_KEY_MAX_CHARS)
                            .not_null(),
                    )
                    // `queued`, `running`, `succeeded`, `failed` or `cancelled`.
                    .col(ColumnDef::new(AgentRuns::Status).text().not_null())
                    .col(ColumnDef::new(AgentRuns::Active).boolean().null())
                    .col(
                        ColumnDef::new(AgentRuns::ResultPullRequestId)
                            .big_integer()
                            .null(),
                    )
                    .col(ColumnDef::new(AgentRuns::FailureReason).text().null())
                    .col(
                        ColumnDef::new(AgentRuns::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentRuns::FinishedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_agent_runs_repository_id")
                            .from(AgentRuns::Table, AgentRuns::RepositoryId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_agent_runs_issue_id")
                            .from(AgentRuns::Table, AgentRuns::IssueId)
                            .to(Issues::Table, Issues::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_agent_runs_pull_request_id")
                            .from(AgentRuns::Table, AgentRuns::PullRequestId)
                            .to(PullRequests::Table, PullRequests::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_agent_runs_result_pull_request_id")
                            .from(AgentRuns::Table, AgentRuns::ResultPullRequestId)
                            .to(PullRequests::Table, PullRequests::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        for (name, columns, unique) in [
            (
                "idx_agent_runs_trigger_key",
                vec![AgentRuns::TriggerKey],
                true,
            ),
            (
                "idx_agent_runs_one_active",
                vec![AgentRuns::RepositoryId, AgentRuns::BotId, AgentRuns::Active],
                true,
            ),
            (
                "idx_agent_runs_bot_id",
                vec![AgentRuns::BotId, AgentRuns::CreatedAt],
                false,
            ),
            ("idx_agent_runs_issue_id", vec![AgentRuns::IssueId], false),
            (
                "idx_agent_runs_pull_request_id",
                vec![AgentRuns::PullRequestId],
                false,
            ),
            (
                "idx_agent_runs_result_pull_request_id",
                vec![AgentRuns::ResultPullRequestId],
                false,
            ),
        ] {
            let mut index = Index::create();
            index.if_not_exists().name(name).table(AgentRuns::Table);
            for column in columns {
                index.col(column);
            }
            if unique {
                index.unique();
            }
            manager.create_index(index.to_owned()).await?;
        }

        manager
            .create_table(
                Table::create()
                    .table(AgentRunAttempts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AgentRunAttempts::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AgentRunAttempts::RunId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentRunAttempts::Attempt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentRunAttempts::PipelineId)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AgentRunAttempts::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentRunAttempts::FinishedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_agent_run_attempts_run_id")
                            .from(AgentRunAttempts::Table, AgentRunAttempts::RunId)
                            .to(AgentRuns::Table, AgentRuns::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_agent_run_attempts_pipeline_id")
                            .from(AgentRunAttempts::Table, AgentRunAttempts::PipelineId)
                            .to(Pipelines::Table, Pipelines::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        for (name, columns, unique) in [
            (
                "idx_agent_run_attempts_run_attempt",
                vec![AgentRunAttempts::RunId, AgentRunAttempts::Attempt],
                true,
            ),
            (
                "idx_agent_run_attempts_pipeline_id",
                vec![AgentRunAttempts::PipelineId],
                false,
            ),
        ] {
            let mut index = Index::create();
            index
                .if_not_exists()
                .name(name)
                .table(AgentRunAttempts::Table);
            for column in columns {
                index.col(column);
            }
            if unique {
                index.unique();
            }
            manager.create_index(index.to_owned()).await?;
        }

        for (column, definition) in [
            (
                AccessTokens::WriteRefPrefix,
                ColumnDef::new(AccessTokens::WriteRefPrefix)
                    .text()
                    .null()
                    .to_owned(),
            ),
            (
                AccessTokens::AllowGit,
                ColumnDef::new(AccessTokens::AllowGit)
                    .boolean()
                    .not_null()
                    .default(false)
                    .to_owned(),
            ),
        ] {
            if !manager
                .has_column("access_tokens", &column.to_string())
                .await?
            {
                manager
                    .alter_table(
                        Table::alter()
                            .table(AccessTokens::Table)
                            .add_column(definition)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        if !manager
            .has_column("access_tokens", "agent_run_attempt_id")
            .await?
        {
            // SQLite cannot add a constraint to an existing table, only a
            // column that carries one — the same split as `users.bot_owner_id`.
            let statement = match manager.get_database_backend() {
                DatabaseBackend::Sqlite => {
                    "ALTER TABLE access_tokens ADD COLUMN agent_run_attempt_id INTEGER NULL \
                     REFERENCES agent_run_attempts(id) ON DELETE CASCADE"
                }
                DatabaseBackend::Postgres => {
                    "ALTER TABLE access_tokens ADD COLUMN agent_run_attempt_id BIGINT NULL \
                     REFERENCES agent_run_attempts(id) ON DELETE CASCADE"
                }
                DatabaseBackend::MySql => {
                    "ALTER TABLE access_tokens ADD COLUMN agent_run_attempt_id BIGINT NULL, \
                     ADD CONSTRAINT fk_access_tokens_agent_run_attempt_id \
                     FOREIGN KEY (agent_run_attempt_id) REFERENCES agent_run_attempts(id) \
                     ON DELETE CASCADE"
                }
            };
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_access_tokens_agent_run_attempt_id")
                    .table(AccessTokens::Table)
                    .col(AccessTokens::AgentRunAttemptId)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("access_tokens", "agent_run_attempt_id")
            .await?
        {
            if manager.get_database_backend() == DatabaseBackend::MySql {
                manager
                    .get_connection()
                    .execute_unprepared(
                        "ALTER TABLE access_tokens \
                         DROP FOREIGN KEY fk_access_tokens_agent_run_attempt_id",
                    )
                    .await?;
            }
            manager
                .drop_index(
                    Index::drop()
                        .name("idx_access_tokens_agent_run_attempt_id")
                        .table(AccessTokens::Table)
                        .to_owned(),
                )
                .await?;
        }
        for column in [
            AccessTokens::AgentRunAttemptId,
            AccessTokens::AllowGit,
            AccessTokens::WriteRefPrefix,
        ] {
            if manager
                .has_column("access_tokens", &column.to_string())
                .await?
            {
                manager
                    .alter_table(
                        Table::alter()
                            .table(AccessTokens::Table)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        for table in [
            AgentRunAttempts::Table.into_iden(),
            AgentRuns::Table.into_iden(),
        ] {
            manager
                .drop_table(Table::drop().table(table).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AgentRuns {
    Table,
    Id,
    BotId,
    RepositoryId,
    IssueId,
    PullRequestId,
    Iteration,
    TriggeredBy,
    TriggerKey,
    Status,
    Active,
    ResultPullRequestId,
    FailureReason,
    CreatedAt,
    FinishedAt,
}

#[derive(DeriveIden)]
enum AgentRunAttempts {
    Table,
    Id,
    RunId,
    Attempt,
    PipelineId,
    CreatedAt,
    FinishedAt,
}

#[derive(DeriveIden)]
enum AccessTokens {
    Table,
    WriteRefPrefix,
    AllowGit,
    AgentRunAttemptId,
}

#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Issues {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum PullRequests {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Pipelines {
    Table,
    Id,
}

#[cfg(test)]
#[path = "m20261127_000001_agent_runs_tests.rs"]
mod tests;
