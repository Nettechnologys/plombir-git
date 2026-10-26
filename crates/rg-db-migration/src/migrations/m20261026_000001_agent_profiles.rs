//! card_d4d689acfa70 (ADR-0001): how a bot works when a person assigns it an
//! issue.
//!
//! - **`agent_profiles`** — one row per bot (`bot_id` is unique) that may run
//!   as an agent: which
//!   preset or image and command it runs, on which runner labels, with whose
//!   model key, and under which limits. The model key is sealed by
//!   `rg_core::auth::encryption` and registered in `encrypted_columns`, so a
//!   key rotation re-seals it and the startup preflight samples it. The row
//!   belongs to the bot (`ON DELETE CASCADE`): deleting the bot takes its
//!   profile and its key with it.
//! - **`agent_profile_repositories`** — the repositories the bot's *owner* has
//!   allowed it to work in. A repository admin can add anybody's bot as a
//!   collaborator; only this list says the owner agreed to spend their model
//!   key there (ADR-0001 D6). Both references cascade, so a deleted repository
//!   drops out of the list and a deleted profile takes its list.
//!
//! Every string is `TEXT`: on MySQL `.string()` is `VARCHAR(255)`, and none of
//! these is a server-filled value short enough to be pinned there.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261026_000001_agent_profiles"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AgentProfiles::Table)
                    .if_not_exists()
                    // A surrogate `id` like every other table: the key-rotation
                    // pass pages each sealed column's table by `id`.
                    .col(
                        ColumnDef::new(AgentProfiles::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AgentProfiles::BotId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentProfiles::Enabled)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    // `claude_code`, `codex` or `custom`.
                    .col(ColumnDef::new(AgentProfiles::Preset).text().not_null())
                    .col(ColumnDef::new(AgentProfiles::Image).text().null())
                    .col(ColumnDef::new(AgentProfiles::Command).text().null())
                    // Comma-joined runner labels; `NULL` means the preset's.
                    .col(ColumnDef::new(AgentProfiles::RunnerTags).text().null())
                    .col(
                        ColumnDef::new(AgentProfiles::ModelKeyEncrypted)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AgentProfiles::TimeoutSecs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentProfiles::MaxRunsPerDay)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentProfiles::MaxIterations)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentProfiles::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentProfiles::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_agent_profiles_bot_id")
                            .from(AgentProfiles::Table, AgentProfiles::BotId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_agent_profiles_bot_id")
                    .table(AgentProfiles::Table)
                    .col(AgentProfiles::BotId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(AgentProfileRepositories::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AgentProfileRepositories::ProfileId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AgentProfileRepositories::RepositoryId)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(AgentProfileRepositories::ProfileId)
                            .col(AgentProfileRepositories::RepositoryId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_agent_profile_repositories_profile_id")
                            .from(
                                AgentProfileRepositories::Table,
                                AgentProfileRepositories::ProfileId,
                            )
                            .to(AgentProfiles::Table, AgentProfiles::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_agent_profile_repositories_repository_id")
                            .from(
                                AgentProfileRepositories::Table,
                                AgentProfileRepositories::RepositoryId,
                            )
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
                    .name("idx_agent_profile_repositories_repository_id")
                    .table(AgentProfileRepositories::Table)
                    .col(AgentProfileRepositories::RepositoryId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(AgentProfileRepositories::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(AgentProfiles::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum AgentProfiles {
    Table,
    Id,
    BotId,
    Enabled,
    Preset,
    Image,
    Command,
    RunnerTags,
    ModelKeyEncrypted,
    TimeoutSecs,
    MaxRunsPerDay,
    MaxIterations,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum AgentProfileRepositories {
    Table,
    ProfileId,
    RepositoryId,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}
