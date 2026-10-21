//! Who wrote an imported issue, comment, pull request, review or release on the
//! platform it came from (card_04088376105b).
//!
//! An import writes all of these as the account that ran it: `author_id` is a
//! local account, and the source's `alice` is not this instance's `alice`, so a
//! name match would let anyone who can import a repository publish under a
//! colleague's name. That rule stays. What went with it was the name itself —
//! after a move, the whole history of a project read as one person talking to
//! themselves. These columns keep the source's own record of who wrote it, as a
//! byline next to `author_id`, never in place of it: permissions, notifications
//! and audit keep reading `author_id`.
//!
//! * `original_author` — the login on the source.
//! * `original_author_id` — the source's numeric account id. A login can be
//!   renamed and taken by someone else; this cannot, and it is what a later
//!   re-attribution to whoever signs in with that source account is keyed on.
//! * `original_service` — the platform instance that issued the id: `github`
//!   (github.com), `gitlab` (gitlab.com), or `github:<host>` / `gitlab:<host>`
//!   for any other server. The id means nothing without it.
//!
//! `NULL` in all three is "written here": every row that exists when this
//! migration runs and everything not brought by an import.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261022_000001_imported_original_author"
    }
}

const TABLES: [&str; 5] = [
    "issues",
    "issue_comments",
    "pull_requests",
    "pr_reviews",
    "releases",
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in TABLES {
            if !manager.has_column(table, "original_author").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .add_column(
                                ColumnDef::new(Alias::new("original_author"))
                                    .string()
                                    .null(),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
            if !manager.has_column(table, "original_author_id").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .add_column(
                                ColumnDef::new(Alias::new("original_author_id"))
                                    .big_integer()
                                    .null(),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
            if !manager.has_column(table, "original_service").await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new(table))
                            .add_column(
                                ColumnDef::new(Alias::new("original_service"))
                                    .string()
                                    .null(),
                            )
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in TABLES {
            for name in ["original_service", "original_author_id", "original_author"] {
                if manager.has_column(table, name).await? {
                    manager
                        .alter_table(
                            Table::alter()
                                .table(Alias::new(table))
                                .drop_column(Alias::new(name))
                                .to_owned(),
                        )
                        .await?;
                }
            }
        }
        Ok(())
    }
}
