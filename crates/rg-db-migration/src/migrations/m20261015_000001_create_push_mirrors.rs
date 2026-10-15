//! Create `push_mirrors`: the remotes a repository sends its branches and tags
//! to after every push.
//!
//! The pull mirror's `mirrors` table points the other way and allows one row
//! per repository; a repository may keep a copy on several forges, so this is
//! a table of its own rather than a direction column on that one.
//!
//! Every textual column a user fills is `TEXT`: MySQL's `VARCHAR(255)` is
//! narrower than the addresses and logins the API accepts, which is the
//! mistake `m20261012_000005_remote_addresses_mysql_text` had to correct for
//! the pull mirror after the fact.
//!
//! The switch is a boolean (`enabled`) and the outcome of the last pass lives
//! in `last_sync_error` / `last_success_at`. Keeping both in one `status`
//! column is what took a failing pull mirror out of its own retry sweep
//! (card_770723efaa96).
//!
//! `pending_since` / `pending_revision` say whether there is something the
//! remote has not received yet: a ref move stamps the first and bumps the
//! second, and a successful pass clears the stamp only if the revision it
//! started from is still current — a push that lands while the pass runs keeps
//! its claim.
//!
//! `lease_token` / `lease_acquired_at` keep two passes from pushing to one
//! remote at once; a lease older than the staleness horizon is taken over.

use sea_orm_migration::prelude::*;

pub struct Migration;

pub const NAME: &str = "m20261015_000001_create_push_mirrors";
pub const REPO_INDEX: &str = "idx_push_mirrors_repo";
pub const DUE_INDEX: &str = "idx_push_mirrors_due";

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
                    .table(PushMirrors::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PushMirrors::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(PushMirrors::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(PushMirrors::Url).text().not_null())
                    .col(ColumnDef::new(PushMirrors::Username).text().null())
                    .col(ColumnDef::new(PushMirrors::PasswordEncrypted).text().null())
                    .col(
                        ColumnDef::new(PushMirrors::SyncOnCommit)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::SyncIntervalSeconds)
                            .big_integer()
                            .not_null()
                            .default(28_800),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::Force)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::Enabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::NextSyncAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::PendingSince)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::PendingRevision)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::LastSyncAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::LastSuccessAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(PushMirrors::LastSyncError).text().null())
                    .col(
                        ColumnDef::new(PushMirrors::LeaseToken)
                            .string_len(64)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::LeaseAcquiredAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PushMirrors::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_push_mirrors_repo")
                            .from(PushMirrors::Table, PushMirrors::RepoId)
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
                    .name(REPO_INDEX)
                    .table(PushMirrors::Table)
                    .col(PushMirrors::RepoId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(DUE_INDEX)
                    .table(PushMirrors::Table)
                    .col(PushMirrors::Enabled)
                    .col(PushMirrors::NextSyncAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(PushMirrors::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden)]
enum PushMirrors {
    Table,
    Id,
    RepoId,
    Url,
    Username,
    PasswordEncrypted,
    SyncOnCommit,
    SyncIntervalSeconds,
    Force,
    Enabled,
    NextSyncAt,
    PendingSince,
    PendingRevision,
    LastSyncAt,
    LastSuccessAt,
    LastSyncError,
    LeaseToken,
    LeaseAcquiredAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}
