//! card_dc0f5d58e5f4: make a dismissal a property of the review it dismisses.
//!
//! `dismiss_review` used to insert a *second* `pr_reviews` row with
//! `action = "dismiss"` under the dismissor's `reviewer_id`. Nothing read it:
//! `current_approvers` folds only `approve` / `request_changes` into its
//! per-reviewer verdict map, so the dismissal displaced nobody's approval — and
//! had it been folded in, it carried the wrong `reviewer_id` and would have
//! displaced the wrong person's verdict. A maintainer dismissed a stale
//! approval, got a 200 and a timeline entry, and the pull request stayed
//! mergeable on exactly the approval that had just been withdrawn.
//!
//! An approval that has been withdrawn is not a separate opinion by whoever
//! withdrew it — it is the same opinion, no longer current. So the fact lives
//! on the original row: `dismissed_at` is what `current_approvers`
//! filters on, and `dismissed_by` records who withdrew it so the review itself
//! can be rendered without joining the event log.
//!
//! `NULL` is "still standing", which is every review at the moment this
//! migration runs.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260822_000001_add_pr_review_dismissal"
    }
}

const COLUMNS: [(&str, PrReviews); 2] = [
    ("dismissed_at", PrReviews::DismissedAt),
    ("dismissed_by", PrReviews::DismissedBy),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, column) in COLUMNS {
            if manager.has_column("pr_reviews", name).await? {
                continue;
            }
            let mut definition = ColumnDef::new(column);
            match column {
                PrReviews::DismissedAt => definition.timestamp_with_time_zone().null(),
                PrReviews::DismissedBy => definition.big_integer().null(),
                PrReviews::Table => unreachable!("the table is not a column"),
            };
            manager
                .alter_table(
                    Table::alter()
                        .table(PrReviews::Table)
                        .add_column(definition.to_owned())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, column) in COLUMNS {
            if !manager.has_column("pr_reviews", name).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(PrReviews::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden, Clone, Copy)]
enum PrReviews {
    Table,
    DismissedAt,
    DismissedBy,
}
