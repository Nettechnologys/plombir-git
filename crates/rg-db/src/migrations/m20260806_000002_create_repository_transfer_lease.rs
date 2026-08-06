//! Migration: create `repository_transfer_lease` — who owns a repository's
//! storage while it is being moved between namespaces.
//!
//! `transfer_repo` moves Git, blob prefixes, legacy LFS/release directories and
//! OCI objects *before* the transaction that rewrites `repositories.owner_id`.
//! It has to: the ownership update and arbitrary blob I/O cannot share a
//! transaction, and holding one across the move would stall every other writer
//! on SQLite for as long as the repository takes to move.
//!
//! That leaves a window in which the bytes are already at the destination and
//! the row still names the source — and the deletion of the *source* account or
//! organization walks exactly that row. It reads `<source>/<repo>.git`, finds
//! nothing there, and `delete_repo` is documented to treat a missing directory
//! as the end state it was asked for, so it soft-deletes the row. If the
//! transfer then refuses (its destination closed, its own commit failed) it
//! returns every namespace to the source, and the result is a repository's bytes
//! sitting live under an account with no row naming them: two independent
//! compensation protocols each convinced they succeeded (card_507ff03ec043).
//!
//! Nothing the deleter can read tells it the difference between "already gone"
//! and "in flight", so the transfer has to say so. One row per repository being
//! moved, claimed by a single conditional write, is that statement — the same
//! protocol `m20260805_000003_create_oci_publication_lease` gave a
//! content-addressed key that has no row of its own to carry it. The deleter's
//! existing quiescence gate reads it and backs off retryably; the destination
//! admission inside `transfer_owner` stays as it is.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RepositoryTransferLease::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RepositoryTransferLease::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // The repository being moved. Unique because it *is* the
                    // thing being arbitrated: two rows would be two transfers
                    // moving one repository's storage at once, which is the
                    // state the lease exists to make impossible.
                    //
                    // `ON DELETE CASCADE` so a repository that really is gone
                    // never leaves a lease behind to block its namespace.
                    .col(
                        ColumnDef::new(RepositoryTransferLease::RepoId)
                            .big_integer()
                            .not_null()
                            .unique_key(),
                    )
                    // Identifies the holder, not the request: a release checks
                    // that the lease it is dropping is still the one it took, so
                    // a transfer that was timed out cannot free its successor's.
                    .col(
                        ColumnDef::new(RepositoryTransferLease::Token)
                            .string()
                            .not_null(),
                    )
                    // The namespaces this move is between. Neither is read by
                    // the protocol — they are here so an operator looking at a
                    // stuck lease can see what it was doing without joining
                    // three tables.
                    .col(
                        ColumnDef::new(RepositoryTransferLease::SourceNamespace)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RepositoryTransferLease::DestinationNamespace)
                            .string()
                            .not_null(),
                    )
                    // When the current holder took it. A process that dies
                    // mid-transfer cannot release, so this is what lets a later
                    // transfer take the repository over — and what stops one
                    // crash from making a namespace permanently undeletable.
                    .col(
                        ColumnDef::new(RepositoryTransferLease::Since)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                RepositoryTransferLease::Table,
                                RepositoryTransferLease::RepoId,
                            )
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(RepositoryTransferLease::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden)]
enum RepositoryTransferLease {
    Table,
    Id,
    RepoId,
    Token,
    SourceNamespace,
    DestinationNamespace,
    Since,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}
