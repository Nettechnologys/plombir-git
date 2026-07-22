//! Add multi-line review-comment range coordinates.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260711_000005_review_comment_ranges"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("review_comments", "start_line").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(ReviewComments::Table)
                        .add_column(
                            ColumnDef::new(ReviewComments::StartLine)
                                .big_integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column("review_comments", "start_side").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(ReviewComments::Table)
                        .add_column(ColumnDef::new(ReviewComments::StartSide).string().null())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [ReviewComments::StartSide, ReviewComments::StartLine] {
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
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ReviewComments {
    Table,
    StartLine,
    StartSide,
}
