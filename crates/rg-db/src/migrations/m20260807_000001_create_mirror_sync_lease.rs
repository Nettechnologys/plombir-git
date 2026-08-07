//! Migration: create `mirror_sync_lease` — who is cloning a mirror's upstream
//! right now.
//!
//! A mirror keeps a full clone of its upstream at `<repo_root>/<repo_id>.mirror`,
//! and `delete_repo` retires that directory with the rest of the repository's
//! storage. `sync_mirror` re-reads the repository's lifecycle immediately before
//! it spawns `git`, which closes the window between the sweep's *selection* and
//! the pass — but not the window inside the pass itself. `git clone --mirror` of
//! a real upstream takes minutes, it holds the absolute path it was given, and
//! it keeps writing to that path after the deletion has renamed it away and
//! removed the tombstone. What lands on disk is a full copy of a third party's
//! repository under a name nothing owns, after the client was told `200`
//! (`card_a1f2a20281af`, the residual of `card_374998ffebc1`).
//!
//! No column of `mirrors` can be read as "a pass is running": `last_sync_at` is
//! written *after* the pass, so it cannot distinguish a sync in flight from one
//! that finished an hour ago. That is the same missing *information* the
//! repository transfer had (`m20260806_000002_create_repository_transfer_lease`)
//! and it is fixed the same way — the writer says so itself. One row per
//! repository being synced, taken before `git` and dropped after it, read by
//! `delete_repo`'s quiescence gate and by the transaction that soft-deletes the
//! row.
//!
//! Keyed on `repo_id` and not on `mirrors.id` deliberately: the deleter knows
//! the repository, and a lease that outlived a `DELETE /mirror` issued mid-sync
//! must still block the deletion — the `git` subprocess does not stop because
//! its row went away.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(MirrorSyncLease::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MirrorSyncLease::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // The repository whose mirror clone is being written. Unique
                    // because it *is* the thing being arbitrated: two rows would
                    // be two `git` processes writing one directory, which is the
                    // state this table exists to make impossible.
                    //
                    // `ON DELETE CASCADE` so a repository row that is really
                    // gone — a hard delete, a purge — never leaves a lease
                    // behind to block anything.
                    .col(
                        ColumnDef::new(MirrorSyncLease::RepoId)
                            .big_integer()
                            .not_null()
                            .unique_key(),
                    )
                    // Identifies the holder, not the request: a release checks
                    // that the lease it is dropping is still the one it took, so
                    // a pass that was timed out cannot free its successor's.
                    .col(ColumnDef::new(MirrorSyncLease::Token).string().not_null())
                    // When the current holder took it. A process that dies
                    // mid-clone cannot release, so this is what lets a later
                    // pass take the mirror over — and what stops one crash from
                    // making a repository permanently undeletable.
                    .col(
                        ColumnDef::new(MirrorSyncLease::Since)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MirrorSyncLease::Table, MirrorSyncLease::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(MirrorSyncLease::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum MirrorSyncLease {
    Table,
    Id,
    RepoId,
    Token,
    Since,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}
