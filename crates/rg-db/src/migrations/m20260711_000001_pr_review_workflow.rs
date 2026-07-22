//! Add draft PRs, requested reviewers, and resolvable review threads.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260711_000001_pr_review_workflow"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pull_requests", "is_draft").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PullRequests::Table)
                        .add_column(
                            ColumnDef::new(PullRequests::IsDraft)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !manager.has_column("review_comments", "resolved_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(ReviewComments::Table)
                        .add_column(
                            ColumnDef::new(ReviewComments::ResolvedAt)
                                .timestamp_with_time_zone()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("review_comments", "resolved_by_id")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(ReviewComments::Table)
                        .add_column(
                            ColumnDef::new(ReviewComments::ResolvedById)
                                .big_integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !manager.has_table("pr_reviewer_requests").await? {
            manager
                .create_table(
                    Table::create()
                        .table(PrReviewerRequests::Table)
                        .col(
                            ColumnDef::new(PrReviewerRequests::Id)
                                .big_integer()
                                .not_null()
                                .auto_increment()
                                .primary_key(),
                        )
                        .col(
                            ColumnDef::new(PrReviewerRequests::PrId)
                                .big_integer()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(PrReviewerRequests::ReviewerId)
                                .big_integer()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(PrReviewerRequests::RequestedById)
                                .big_integer()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(PrReviewerRequests::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .foreign_key(
                            ForeignKey::create()
                                .from(PrReviewerRequests::Table, PrReviewerRequests::PrId)
                                .to(PullRequests::Table, PullRequests::Id)
                                .on_delete(ForeignKeyAction::Cascade),
                        )
                        .foreign_key(
                            ForeignKey::create()
                                .from(PrReviewerRequests::Table, PrReviewerRequests::ReviewerId)
                                .to(Users::Table, Users::Id)
                                .on_delete(ForeignKeyAction::Cascade),
                        )
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_pr_reviewer_request_unique")
                        .table(PrReviewerRequests::Table)
                        .col(PrReviewerRequests::PrId)
                        .col(PrReviewerRequests::ReviewerId)
                        .unique()
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(PrReviewerRequests::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        for column in [ReviewComments::ResolvedById, ReviewComments::ResolvedAt] {
            if manager
                .has_column("review_comments", column.to_string().as_str())
                .await?
            {
                manager
                    .alter_table(
                        Table::alter()
                            .table(ReviewComments::Table)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        if manager.has_column("pull_requests", "is_draft").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PullRequests::Table)
                        .drop_column(PullRequests::IsDraft)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Iden)]
enum PullRequests {
    Table,
    Id,
    IsDraft,
}

#[derive(Iden)]
enum ReviewComments {
    Table,
    ResolvedAt,
    ResolvedById,
}

#[derive(Iden)]
enum PrReviewerRequests {
    Table,
    Id,
    PrId,
    ReviewerId,
    RequestedById,
    CreatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
