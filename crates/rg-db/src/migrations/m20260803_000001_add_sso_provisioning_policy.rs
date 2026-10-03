//! Who an SSO / LDAP provider may create an account *for*.
//!
//! `sso_providers` could say whether a provider is usable (`enabled`) but never
//! whom it may provision: the callback created a Plombir Git account for anyone
//! who showed a valid identity at a configured provider. That is the right
//! reading for a private directory and the wrong one for `github.com`, where
//! everyone has an identity — and there was nowhere to write the difference
//! down.
//!
//! Two columns, and the defaults are the whole point:
//!
//! * `auto_provision` — `NOT NULL DEFAULT TRUE`, so every provider that exists
//!   at upgrade time keeps provisioning exactly as it did. A migration that
//!   defaulted to `false` would log an entire company out of its own instance
//!   on a patch release. The *new*-provider default is a separate decision and
//!   lives in the admin API, which starts one at `false`.
//! * `allowed_email_domains` — nullable, `NULL` meaning "no domain
//!   restriction". Absent is the historical behaviour; an empty list is not a
//!   thing an upgrade may invent.
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260803_000001_add_sso_provisioning_policy"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("sso_providers", "auto_provision")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(SsoProviders::Table)
                        .add_column(
                            ColumnDef::new(SsoProviders::AutoProvision)
                                .boolean()
                                .not_null()
                                .default(true),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("sso_providers", "allowed_email_domains")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(SsoProviders::Table)
                        .add_column(
                            ColumnDef::new(SsoProviders::AllowedEmailDomains)
                                .string_len(1024)
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (column, name) in [
            (SsoProviders::AllowedEmailDomains, "allowed_email_domains"),
            (SsoProviders::AutoProvision, "auto_provision"),
        ] {
            if manager.has_column("sso_providers", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(SsoProviders::Table)
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
enum SsoProviders {
    Table,
    AutoProvision,
    AllowedEmailDomains,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    /// The upgrade must not switch off a directory that already works: a
    /// provider row that predates the column comes out of the migration
    /// provisioning exactly as before, and a row inserted later without
    /// naming the column gets the same default from the schema itself.
    #[tokio::test]
    async fn existing_providers_keep_provisioning_after_the_upgrade() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE sso_providers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             slug TEXT NOT NULL, provider_type TEXT NOT NULL, enabled BOOLEAN NOT NULL);\
             INSERT INTO sso_providers (id, name, slug, provider_type, enabled) \
             VALUES (1, 'Corp Directory', 'corp', 'ldap', 1);",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        db.execute_unprepared(
            "INSERT INTO sso_providers (id, name, slug, provider_type, enabled) \
             VALUES (2, 'Later', 'later', 'oidc', 1);",
        )
        .await
        .unwrap();

        for id in [1, 2] {
            let row = db
                .query_one(Statement::from_string(
                    DbBackend::Sqlite,
                    format!(
                        "SELECT auto_provision, allowed_email_domains FROM sso_providers WHERE id = {id}"
                    ),
                ))
                .await
                .unwrap()
                .unwrap();
            assert!(
                row.try_get::<bool>("", "auto_provision").unwrap(),
                "provider {id} lost auto-provisioning across the upgrade"
            );
            assert_eq!(
                row.try_get::<Option<String>>("", "allowed_email_domains")
                    .unwrap(),
                None,
                "the upgrade invented an email allowlist for provider {id}"
            );
        }
    }

    /// Re-running the migration over a database that already has the columns
    /// must be a no-op rather than a duplicate-column failure.
    #[tokio::test]
    async fn the_upgrade_is_idempotent() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE sso_providers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             slug TEXT NOT NULL, provider_type TEXT NOT NULL, enabled BOOLEAN NOT NULL);",
        )
        .await
        .unwrap();

        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();
    }
}
