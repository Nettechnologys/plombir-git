//! Durable channel delivery, personal encrypted Telegram configuration and
//! account-scoped browser evidence. Legacy email choices remain untouched.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

fn alias(value: &str) -> Alias {
    Alias::new(value)
}
fn account_table(name: &str) -> TableCreateStatement {
    Table::create()
        .table(alias(name))
        .if_not_exists()
        .col(
            ColumnDef::new(alias("id"))
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(ColumnDef::new(alias("user_id")).big_integer().not_null())
        .foreign_key(
            ForeignKey::create()
                .from(alias(name), alias("user_id"))
                .to(alias("users"), alias("id"))
                .on_delete(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mut profile = account_table("notification_profiles");
        profile
            .col(
                ColumnDef::new(alias("locale"))
                    .string_len(8)
                    .not_null()
                    .default("en"),
            )
            .col(ColumnDef::new(alias("choices")).text().not_null())
            .col(
                ColumnDef::new(alias("telegram_token_encrypted"))
                    .text()
                    .null(),
            )
            .col(ColumnDef::new(alias("chat_id")).big_integer().null())
            .col(ColumnDef::new(alias("message_thread_id")).integer().null())
            .col(
                ColumnDef::new(alias("telegram_confirmed"))
                    .boolean()
                    .not_null()
                    .default(false),
            )
            .col(
                ColumnDef::new(alias("generation"))
                    .big_integer()
                    .not_null()
                    .default(0),
            )
            .col(
                ColumnDef::new(alias("revision"))
                    .big_integer()
                    .not_null()
                    .default(0),
            )
            .col(
                ColumnDef::new(alias("updated_at"))
                    .timestamp_with_time_zone()
                    .not_null(),
            );
        manager.create_table(profile).await?;

        let mut delivery = account_table("notification_deliveries");
        for (name, size) in [
            ("event_id", 80),
            ("event_key", 64),
            ("channel", 16),
            ("destination_hash", 64),
            ("status", 16),
        ] {
            delivery.col(ColumnDef::new(alias(name)).string_len(size).not_null());
        }
        delivery
            .col(ColumnDef::new(alias("metadata")).text().not_null())
            .col(ColumnDef::new(alias("repo_id")).big_integer().null())
            .col(
                ColumnDef::new(alias("admin_only"))
                    .boolean()
                    .not_null()
                    .default(false),
            )
            .col(ColumnDef::new(alias("generation")).big_integer().not_null())
            .col(
                ColumnDef::new(alias("previous_destination_encrypted"))
                    .text()
                    .null(),
            )
            .col(
                ColumnDef::new(alias("previous_token_encrypted"))
                    .text()
                    .null(),
            )
            .col(ColumnDef::new(alias("prior_locale")).string_len(8).null())
            .col(
                ColumnDef::new(alias("is_test"))
                    .boolean()
                    .not_null()
                    .default(false),
            )
            .col(
                ColumnDef::new(alias("attempts"))
                    .integer()
                    .not_null()
                    .default(0),
            )
            .col(ColumnDef::new(alias("lease_token")).string_len(36).null())
            .col(ColumnDef::new(alias("last_error")).string_len(80).null());
        for name in ["created_at", "expires_at", "next_attempt_at"] {
            delivery.col(
                ColumnDef::new(alias(name))
                    .timestamp_with_time_zone()
                    .not_null(),
            );
        }
        for name in ["lease_until", "completed_at"] {
            delivery.col(
                ColumnDef::new(alias(name))
                    .timestamp_with_time_zone()
                    .null(),
            );
        }
        manager.create_table(delivery).await?;

        let mut browser = account_table("known_browsers");
        browser
            .col(
                ColumnDef::new(alias("witness_hash"))
                    .string_len(64)
                    .not_null(),
            )
            .col(
                ColumnDef::new(alias("created_at"))
                    .timestamp_with_time_zone()
                    .not_null(),
            )
            .col(
                ColumnDef::new(alias("expires_at"))
                    .timestamp_with_time_zone()
                    .not_null(),
            );
        manager.create_table(browser).await?;

        for (table, name, columns, unique) in [
            (
                "notification_profiles",
                "uq_notification_profile_user",
                vec!["user_id"],
                true,
            ),
            (
                "notification_deliveries",
                "uq_notification_delivery_event",
                vec!["event_id", "user_id", "channel"],
                true,
            ),
            (
                "notification_deliveries",
                "idx_notification_delivery_due",
                vec!["status", "next_attempt_at"],
                false,
            ),
            (
                "notification_deliveries",
                "idx_notification_delivery_tests",
                vec!["user_id", "is_test", "created_at"],
                false,
            ),
            (
                "known_browsers",
                "uq_known_browser_user_witness",
                vec!["user_id", "witness_hash"],
                true,
            ),
            (
                "known_browsers",
                "idx_known_browser_expiry",
                vec!["expires_at"],
                false,
            ),
        ] {
            // MySQL has no CREATE INDEX IF NOT EXISTS.
            if !manager.has_index(table, name).await? {
                let mut index = Index::create();
                index.name(name).table(alias(table));
                for column in columns {
                    index.col(alias(column));
                }
                if unique {
                    index.unique();
                }
                manager.create_index(index.to_owned()).await?;
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            "known_browsers",
            "notification_deliveries",
            "notification_profiles",
        ] {
            manager
                .drop_table(Table::drop().table(alias(table)).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }
}
