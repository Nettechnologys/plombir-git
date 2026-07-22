//! Add structured, applicable review suggestions.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260711_000004_review_suggestions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let columns = [
            (
                ReviewComments::Suggestion,
                ColumnDef::new(ReviewComments::Suggestion)
                    .text()
                    .null()
                    .to_owned(),
            ),
            (
                ReviewComments::SuggestionAppliedAt,
                ColumnDef::new(ReviewComments::SuggestionAppliedAt)
                    .timestamp_with_time_zone()
                    .null()
                    .to_owned(),
            ),
            (
                ReviewComments::SuggestionAppliedById,
                ColumnDef::new(ReviewComments::SuggestionAppliedById)
                    .big_integer()
                    .null()
                    .to_owned(),
            ),
            (
                ReviewComments::SuggestionCommitSha,
                ColumnDef::new(ReviewComments::SuggestionCommitSha)
                    .string()
                    .null()
                    .to_owned(),
            ),
        ];
        for (name, definition) in columns {
            if !manager
                .has_column("review_comments", name.to_string().as_str())
                .await?
            {
                manager
                    .alter_table(
                        Table::alter()
                            .table(ReviewComments::Table)
                            .add_column(&mut definition.clone())
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            ReviewComments::SuggestionCommitSha,
            ReviewComments::SuggestionAppliedById,
            ReviewComments::SuggestionAppliedAt,
            ReviewComments::Suggestion,
        ] {
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
    Suggestion,
    SuggestionAppliedAt,
    SuggestionAppliedById,
    SuggestionCommitSha,
}
