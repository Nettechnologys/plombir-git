//! Which generation of `code_fts` rows a repository's code search reads
//! (card_cfaccf4c5241).
//!
//! Until now a refresh replaced a repository's whole snapshot inside ONE
//! transaction: delete every row, insert every file, commit. That kept readers
//! from seeing a half-built index, but on SQLite it also held the
//! database-wide write lock for as long as the insert took, so every push to a
//! default branch stalled every other writer of the instance behind it.
//!
//! `code_index_snapshots` is what lets the work leave that transaction:
//!
//! * `published_key` is the `code_fts.repo_id` value readers resolve the
//!   repository to. A rebuild writes its rows under a fresh negative key in
//!   small transactions and becomes visible by moving this one column, so a
//!   reader still sees exactly one complete generation. A repository without a
//!   row reads the rows keyed by its own id — the layout every snapshot taken
//!   before this table existed has.
//! * `building_key` names the rebuild currently allowed to write. A newer
//!   rebuild takes it over; the older one notices on its next chunk and stops.
//! * `indexed_tree` is the Git tree the published generation describes. It is
//!   what makes a push incremental: the refresh diffs that tree against the new
//!   one and touches only the paths that changed.
//! * `revision` moves on every change to the published generation, so an
//!   incremental apply can compare-and-swap against exactly the rows it read —
//!   a revert back to an earlier tree must not look like "nothing happened".
//! * `indexed_files` / `indexed_bytes` keep the snapshot's size, so an
//!   incremental apply can enforce the same ceilings a full traversal does
//!   without reading the whole tree.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261009_000901_code_index_snapshots"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CodeIndexSnapshots::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CodeIndexSnapshots::RepoId)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CodeIndexSnapshots::PublishedKey)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodeIndexSnapshots::BuildingKey)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CodeIndexSnapshots::IndexedTree)
                            .string_len(64)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CodeIndexSnapshots::Revision)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(CodeIndexSnapshots::NextGeneration)
                            .big_integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(CodeIndexSnapshots::IndexedFiles)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(CodeIndexSnapshots::IndexedBytes)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(CodeIndexSnapshots::Table, CodeIndexSnapshots::RepoId)
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
                    .table(CodeIndexSnapshots::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum CodeIndexSnapshots {
    Table,
    RepoId,
    PublishedKey,
    BuildingKey,
    IndexedTree,
    Revision,
    NextGeneration,
    IndexedFiles,
    IndexedBytes,
}

#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}
