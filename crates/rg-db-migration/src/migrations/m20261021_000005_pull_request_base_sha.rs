//! `base_sha` on a pull request: the base branch's tip it is measured from once
//! it is no longer open (card_95b947a371fc).
//!
//! An open pull request's diff reads its branches, and the base's tip *now* is
//! the right base for it. After the merge it is not: the tip holds the head, so
//! `base...head` is empty — and once the branch is deleted there is no head to
//! read either. The merge knows the tip it merged onto, and an import is told
//! the base by its source, so they write it here and the diff reads that
//! commit. A pull request closed unmerged needs none: its head is not in the
//! base, and the base branch's tip is still the right base for it.
//!
//! The first parent of `merge_commit_sha` gives the same answer for most merged
//! rows and is what a row from before this column falls back to. It cannot for
//! a rebase that rewrote nothing: the merge is then a fast-forward, the merge
//! commit *is* the head, and the tip it moved from is recorded nowhere else.
//!
//! `NULL` is "not recorded": every open pull request, and every row that exists
//! when this migration runs.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261021_000005_pull_request_base_sha"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("pull_requests", "base_sha").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(PullRequests::Table)
                    .add_column(ColumnDef::new(PullRequests::BaseSha).string().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pull_requests", "base_sha").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(PullRequests::Table)
                    .drop_column(PullRequests::BaseSha)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum PullRequests {
    Table,
    BaseSha,
}
