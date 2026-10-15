//! Migration: create `oci_publication_lease` — who owns an OCI
//! content-addressed key while it is being published.
//!
//! Publishing a layer is two writes that cannot share a transaction: the bytes
//! go to blob storage, the `oci_blob` row goes to the database. Between them
//! the key holds bytes nothing points at, so a failed row write has to take
//! them back — and the only thing it had to decide that on was whether its own
//! `exists` probe came back empty.
//!
//! Under two concurrent first pushes of one digest that is not a decision at
//! all. `exists → put` is not atomic, and `oci_ops::insert_blob` is
//! deliberately idempotent, so the request that lost the race for the bytes
//! still records a perfectly live row for them. When the winner's own row write
//! then fails, its rollback deletes the layer the loser's row now points at:
//! the image stops pulling with its metadata intact.
//!
//! The lease is the arbiter the storage layer cannot be. One row per key being
//! published, claimed by a single conditional write, so the answer to "are
//! these bytes still mine" survives across processes — the same protocol
//! `m20260804_000001_add_lfs_publication_lease` gave LFS, over a key that has
//! no row of its own to carry it.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(OciPublicationLease::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OciPublicationLease::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // The backend key being published, e.g.
                    // `oci/{owner}/{repo}/blobs/sha256/ab/abc…`. Unique because
                    // it *is* the thing being arbitrated: two rows for one key
                    // would be two holders, which is the state the lease
                    // exists to make impossible. Sized well clear of the
                    // longest key a namespace can produce while staying inside
                    // the index-length ceiling InnoDB puts on a unique column.
                    .col(
                        ColumnDef::new(OciPublicationLease::StorageKey)
                            .string_len(512)
                            .not_null()
                            .unique_key(),
                    )
                    // Identifies the holder, not the request: a rollback checks
                    // that the lease it is about to act under is still the one
                    // it took, and a token it no longer holds is exactly the
                    // case where the bytes may already be somebody else's.
                    .col(
                        ColumnDef::new(OciPublicationLease::Token)
                            .string()
                            .not_null(),
                    )
                    // When the current holder took it. A process that dies
                    // mid-publish cannot release, so this is what lets a later
                    // request take the key over instead of waiting forever.
                    .col(
                        ColumnDef::new(OciPublicationLease::Since)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OciPublicationLease::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum OciPublicationLease {
    Table,
    Id,
    StorageKey,
    Token,
    Since,
}
