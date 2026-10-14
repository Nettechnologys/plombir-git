//! Give every organization a member holding the `owner` role.
//!
//! Organization rights used to come from two places: the membership roles, and
//! `organizations.owner_id` on its own — `is_org_admin` answered "yes" for the
//! account in that column whether or not it was still a member. From this
//! release on only the roles grant anything (security audit PR #11), so an
//! organization whose `owner`-role members are all gone would become one nobody
//! can delete, hand over or give the role to. On earlier releases any admin
//! could remove the creator's membership row, so such organizations can exist.
//!
//! For each live organization with no `owner`-role member, the account its
//! `owner_id` names gets the role: its membership row is upgraded, or inserted
//! when there is none. That is not a new right — it is the one the column
//! carried until now, moved to where it is read from. Every organization it
//! touches gets an `audit_log` row naming the account and whether its
//! membership was inserted or upgraded.
//!
//! An `owner_id` that names no account (the column has no foreign key) is left
//! alone: there is nobody to give the role to, and an instance administrator
//! can still delete the organization.
//!
//! Timestamps are bound as values, not written as `CURRENT_TIMESTAMP`: on
//! SQLite that is a `YYYY-MM-DD HH:MM:SS` string with no offset, a different
//! text from the one every other row carries.
//!
//! Idempotent by construction — an organization it has given an owner is no
//! longer selected — and `down` takes nothing back: the rows it wrote are
//! indistinguishable from roles granted by hand since.

use sea_orm::{ConnectionTrait, DbErr, Statement};
use sea_orm_migration::prelude::*;

pub const NAME: &str = "m20261014_000003_org_owner_role_backfill";

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        NAME
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let granted = backfill_owner_roles(manager.get_connection()).await?;
        if granted > 0 {
            tracing::info!(
                organizations = granted,
                "gave the account in `organizations.owner_id` the `owner` role of every \
                 organization that had no owner-role member left"
            );
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

/// The live organizations without an `owner`-role member whose `owner_id`
/// names an account, with that account's membership row if it has one.
const ORPHANED: &str = "\
    SELECT o.id AS org_id, o.name AS org_name, o.owner_id AS user_id, m.id AS member_id \
    FROM organizations o \
    JOIN users u ON u.id = o.owner_id \
    LEFT JOIN organization_members m ON m.org_id = o.id AND m.user_id = o.owner_id \
    WHERE o.deleted_at IS NULL \
      AND NOT EXISTS (SELECT 1 FROM organization_members om \
                      WHERE om.org_id = o.id AND om.role = 'owner') \
    ORDER BY o.id";

/// Give each organization [`ORPHANED`] selects its `owner_id` as an
/// `owner`-role member, returning how many it changed.
///
/// Apart from the migration so it can be run against a server database that
/// has already been migrated: it changes nothing an earlier run, or a person,
/// has already settled.
pub(crate) async fn backfill_owner_roles<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
    let backend = db.get_database_backend();
    let orphaned = db
        .query_all(Statement::from_string(backend, ORPHANED.to_string()))
        .await?;

    let mut granted = 0;
    for row in orphaned {
        let org_id: i64 = row.try_get("", "org_id")?;
        let org_name: String = row.try_get("", "org_name")?;
        let user_id: i64 = row.try_get("", "user_id")?;
        let member_id: Option<i64> = row.try_get("", "member_id")?;
        let now = chrono::Utc::now();

        let membership = match member_id {
            Some(member_id) => {
                db.execute(
                    backend.build(
                        Query::update()
                            .table(Alias::new("organization_members"))
                            .value(Alias::new("role"), "owner")
                            .and_where(Expr::col(Alias::new("id")).eq(member_id)),
                    ),
                )
                .await?;
                "upgraded"
            }
            None => {
                db.execute(
                    backend.build(
                        Query::insert()
                            .into_table(Alias::new("organization_members"))
                            .columns([
                                Alias::new("org_id"),
                                Alias::new("user_id"),
                                Alias::new("role"),
                                Alias::new("created_at"),
                            ])
                            .values_panic([
                                org_id.into(),
                                user_id.into(),
                                "owner".into(),
                                now.into(),
                            ]),
                    ),
                )
                .await?;
                "inserted"
            }
        };

        let details = serde_json::json!({
            "source": format!("migration:{NAME}"),
            "owner_user_id": user_id,
            "membership": membership,
        })
        .to_string();
        db.execute(
            backend.build(
                Query::insert()
                    .into_table(Alias::new("audit_log"))
                    .columns([
                        Alias::new("action"),
                        Alias::new("resource_type"),
                        Alias::new("resource_id"),
                        Alias::new("resource_name"),
                        Alias::new("details"),
                        Alias::new("created_at"),
                    ])
                    .values_panic([
                        "org.owner_role_backfill".into(),
                        "org".into(),
                        org_id.into(),
                        org_name.into(),
                        details.into(),
                        now.into(),
                    ]),
            ),
        )
        .await?;
        granted += 1;
    }
    Ok(granted)
}

#[cfg(test)]
#[path = "m20261014_000003_org_owner_role_backfill_tests.rs"]
mod tests;
