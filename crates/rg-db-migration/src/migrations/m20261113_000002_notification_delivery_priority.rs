//! Migration: a delivery class on `notification_deliveries`, so the dispatcher
//! can serve security notifications before thread activity.
//!
//! The outbox was one FIFO: an account producing activity faster than the
//! dispatcher drains it delayed every user's security notifications, and kept
//! up long enough, let them expire undelivered.
//!
//! The class comes from `Event::priority` at enqueue time. Rows already queued
//! are classified here from a frozen copy of that mapping (a migration must not
//! change meaning when the code moves on); a test keeps the copy equal to the
//! code as of this migration. Terminal rows keep the default: they are never
//! selected again.
//!
//! Every step checks before it acts, so an interrupted MySQL run, which commits
//! DDL implicitly, completes on the next `up`.
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const TABLE: &str = "notification_deliveries";
const COLUMN: &str = "priority";
const INDEX: &str = "idx_notification_delivery_class";

/// Class 0: the account's own credentials and factors.
pub(crate) const ACCOUNT_EVENTS: &[&str] = &[
    "new_device",
    "token_created",
    "token_revoked",
    "ssh_key_added",
    "ssh_key_removed",
    "signing_key_added",
    "signing_key_removed",
    "passkey_added",
    "passkey_removed",
    "mfa_enabled",
    "mfa_replaced",
    "mfa_disabled",
    "backup_codes_regenerated",
    "password_changed",
    "password_reset",
    "email_changed",
    "external_login_linked",
    "external_login_unlinked",
    "admin_password_reset",
    "admin_factors_reset",
    "notification_settings_changed",
];

/// Class 1: security of shared resources, which other members can cause.
pub(crate) const SHARED_EVENTS: &[&str] = &[
    "agent_token_created",
    "agent_token_revoked",
    "deploy_key_added",
    "deploy_key_removed",
    "runner_token_created",
    "runner_token_revoked",
    "repository_access_changed",
    "organization_access_changed",
    "repository_security_changed",
    "organization_security_changed",
    "webhook_changed",
    "mirror_changed",
    "ci_security_changed",
    "admin_security_changed",
    "agent_profile_changed",
    "bot_created",
    "bot_deleted",
];

fn alias(value: &str) -> Alias {
    Alias::new(value)
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column(TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(alias(TABLE))
                        .add_column(
                            ColumnDef::new(alias(COLUMN))
                                .small_integer()
                                .not_null()
                                .default(2),
                        )
                        .to_owned(),
                )
                .await?;
        }
        let db = manager.get_connection();
        let backend = db.get_database_backend();
        for (class, keys) in [(0i16, ACCOUNT_EVENTS), (1i16, SHARED_EVENTS)] {
            db.execute(
                backend.build(
                    Query::update()
                        .table(alias(TABLE))
                        .value(alias(COLUMN), class)
                        .and_where(Expr::col(alias("status")).is_in(["queued", "inflight"]))
                        .and_where(Expr::col(alias("event_key")).is_in(keys.iter().copied())),
                ),
            )
            .await?;
        }
        if !manager.has_index(TABLE, INDEX).await? {
            manager
                .create_index(
                    Index::create()
                        .name(INDEX)
                        .table(alias(TABLE))
                        .col(alias(COLUMN))
                        .col(alias("status"))
                        .col(alias("next_attempt_at"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_index(TABLE, INDEX).await? {
            manager
                .drop_index(Index::drop().name(INDEX).table(alias(TABLE)).to_owned())
                .await?;
        }
        if manager.has_column(TABLE, COLUMN).await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(alias(TABLE))
                        .drop_column(alias(COLUMN))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "m20261113_000002_notification_delivery_priority_tests.rs"]
mod tests;
