//! Persist the WebAuthn relying-party id alongside each passkey credential.
//!
//! Rows created before this migration must remain NULL: the database has no
//! evidence of the hostname under which they were enrolled, and assigning the
//! current `external_url` would turn an unknown fact into a silent mismatch.
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260803_000002_add_passkey_credential_rp_id"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("passkey_credentials", "rp_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PasskeyCredentials::Table)
                        .add_column(
                            ColumnDef::new(PasskeyCredentials::RpId)
                                .string_len(253)
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("passkey_credentials", "rp_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PasskeyCredentials::Table)
                        .drop_column(PasskeyCredentials::RpId)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum PasskeyCredentials {
    Table,
    RpId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    /// A hostname guessed at upgrade time is worse than no hostname: it would
    /// make an old authenticator look intentionally bound to a host it may
    /// never have seen. The nullable column preserves that uncertainty.
    #[tokio::test]
    async fn existing_credentials_keep_an_unknown_rp_id() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE passkey_credentials (id INTEGER PRIMARY KEY); \
             INSERT INTO passkey_credentials (id) VALUES (1);",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT rp_id FROM passkey_credentials WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .expect("the historical credential remains");
        let rp_id: Option<String> = row.try_get("", "rp_id").unwrap();
        assert_eq!(rp_id, None);
    }
}
