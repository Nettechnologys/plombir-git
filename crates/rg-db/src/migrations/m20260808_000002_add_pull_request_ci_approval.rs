//! card_94834ecee708: let a maintainer let a fork PR's CI run.
//!
//! `trigger_pull_request_ci` refuses a fork PR outright, and for a good reason:
//! the pipeline is created under the *base* repository's id, and
//! `ci_secret_ops::list_by_repo` injects every one of that repository's secrets
//! into every job. Running it automatically would hand them to anyone who can
//! open a pull request. The consequence was that a fork PR got no CI at all —
//! the one contribution shape that most needs it.
//!
//! The missing piece was a maintainer's consent, recorded against a *commit*
//! rather than against the PR. `ci_approved_sha` is what makes an approval
//! unforgeable by a later push: the run is permitted for exactly the head the
//! maintainer looked at, so `approve → push something else` leaves the approval
//! no longer matching and CI closed again until it is renewed.
//!
//! `NULL` is "no approval on record", which is every pull request at the moment
//! this migration runs.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260808_000002_add_pull_request_ci_approval"
    }
}

const COLUMNS: [(&str, PullRequests); 3] = [
    ("ci_approved_sha", PullRequests::CiApprovedSha),
    ("ci_approved_by", PullRequests::CiApprovedBy),
    ("ci_approved_at", PullRequests::CiApprovedAt),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, column) in COLUMNS {
            if manager.has_column("pull_requests", name).await? {
                continue;
            }
            let mut definition = ColumnDef::new(column);
            match column {
                PullRequests::CiApprovedSha => definition.string_len(64).null(),
                PullRequests::CiApprovedBy => definition.big_integer().null(),
                PullRequests::CiApprovedAt => definition.timestamp_with_time_zone().null(),
                PullRequests::Table => unreachable!("the table is not a column"),
            };
            manager
                .alter_table(
                    Table::alter()
                        .table(PullRequests::Table)
                        .add_column(definition.to_owned())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, column) in COLUMNS {
            if !manager.has_column("pull_requests", name).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(PullRequests::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden, Clone, Copy)]
enum PullRequests {
    Table,
    CiApprovedSha,
    CiApprovedBy,
    CiApprovedAt,
}
