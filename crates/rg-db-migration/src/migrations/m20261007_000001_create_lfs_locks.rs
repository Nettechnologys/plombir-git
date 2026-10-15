//! card_e8afcaf3edf6: the Git LFS File Locking API.
//!
//! `git lfs lock <path>` asks the server to record that one person is editing
//! a file nobody can merge — a texture, a scene, a CAD drawing — so that
//! everyone else's client refuses to push a change to it until it is unlocked.
//! A path is locked at most once per repository, which is the unique index
//! below and the arbiter between two people locking the same file at once:
//! the second insert fails, and the API answers `409` with the lock that won.
//!
//! Both foreign keys cascade. A lock is the repository's while it exists and
//! its owner's claim, so purging either takes the lock with it rather than
//! leaving a row that points at nothing and blocks everyone's push.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UNIQUE_PATH: &str = "uq_lfs_locks_repo_path";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(LfsLocks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(LfsLocks::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(LfsLocks::RepoId).big_integer().not_null())
                    // A path inside the repository, as the client spells it.
                    // 512 characters is what keeps the composite unique index
                    // inside InnoDB's 3072-byte key limit under `utf8mb4`
                    // (4 bytes each, plus the repository key); the API refuses
                    // a longer path rather than letting the insert fail.
                    .col(ColumnDef::new(LfsLocks::Path).string_len(512).not_null())
                    .col(ColumnDef::new(LfsLocks::RefName).string_len(1024).null())
                    .col(ColumnDef::new(LfsLocks::OwnerId).big_integer().not_null())
                    .col(
                        ColumnDef::new(LfsLocks::LockedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(LfsLocks::Table, LfsLocks::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(LfsLocks::Table, LfsLocks::OwnerId)
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
                    .unique()
                    .name(UNIQUE_PATH)
                    .table(LfsLocks::Table)
                    .col(LfsLocks::RepoId)
                    .col(LfsLocks::Path)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(LfsLocks::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum LfsLocks {
    Table,
    Id,
    RepoId,
    Path,
    RefName,
    OwnerId,
    LockedAt,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
