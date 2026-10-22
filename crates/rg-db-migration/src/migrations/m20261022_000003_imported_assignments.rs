//! Who an imported issue or pull request was assigned to, and whose review it
//! asked for, on the platform it came from (card_f6c67a05c6b0).
//!
//! `issues.assignee_id` names a local account, and the source's `alice` is not
//! this instance's `alice` — the rule the imported bylines follow
//! (`m20261022_000001_imported_original_author`). So these are bylines too:
//! the source's login, its numeric id and the platform instance that issued
//! it, one row per person, never in place of a local assignment. Until a
//! person can take them over, they say who was on the work when it moved.
//!
//! A row belongs to exactly one issue or one pull request, and both foreign
//! keys cascade: deleting either takes its bylines with it. Text columns,
//! because a login is the source's text and nothing indexes on it.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261022_000003_imported_assignments"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ImportedAssignments::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ImportedAssignments::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ImportedAssignments::IssueId)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ImportedAssignments::PullRequestId)
                            .big_integer()
                            .null(),
                    )
                    // `assignee` or `requested_reviewer`.
                    .col(ColumnDef::new(ImportedAssignments::Role).text().not_null())
                    .col(ColumnDef::new(ImportedAssignments::Login).text().not_null())
                    .col(
                        ColumnDef::new(ImportedAssignments::SourceUserId)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ImportedAssignments::Service)
                            .text()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(ImportedAssignments::Table, ImportedAssignments::IssueId)
                            .to(Issues::Table, Issues::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                ImportedAssignments::Table,
                                ImportedAssignments::PullRequestId,
                            )
                            .to(PullRequests::Table, PullRequests::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_imported_assignments_issue")
                    .table(ImportedAssignments::Table)
                    .col(ImportedAssignments::IssueId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_imported_assignments_pull_request")
                    .table(ImportedAssignments::Table)
                    .col(ImportedAssignments::PullRequestId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(ImportedAssignments::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ImportedAssignments {
    Table,
    Id,
    IssueId,
    PullRequestId,
    Role,
    Login,
    SourceUserId,
    Service,
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
