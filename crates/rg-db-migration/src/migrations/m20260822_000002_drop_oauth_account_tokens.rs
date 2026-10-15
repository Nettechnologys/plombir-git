//! Drop the external identity's OAuth tokens — three columns with writers and
//! no readers.
//!
//! `oauth_accounts` stored the provider's `access_token` and `refresh_token`
//! encrypted, plus the expiry that only describes them. The one path that ever
//! read them back was `POST /auth/sso/{slug}/refresh`, and that endpoint was
//! removed once it turned out nothing ever opened it (card_76820bc5325e), which
//! left the instance holding somebody else's long-lived credentials to GitHub /
//! GitLab / an OIDC provider with nothing on the instance depending on them. A
//! database dump plus `[auth].encryption_key` handed over live access to those
//! external accounts, and no feature paid for the risk (card_51dd82b6dc82).
//!
//! Signing in is unaffected: a link is identified by
//! `(provider, provider_user_id)`, and that pair is what resolves a callback to
//! an account.
//!
//! `down` restores the three columns, but not their values — the ciphertext is
//! the thing this migration exists to destroy, and a token that was valid when
//! it was written is not one an operator wants back weeks later anyway. The
//! restored columns are nullable, the shape the entity had.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260822_000002_drop_oauth_account_tokens"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("oauth_accounts", "access_token").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(OauthAccounts::Table)
                        .drop_column(OauthAccounts::AccessToken)
                        .to_owned(),
                )
                .await?;
        }
        if manager
            .has_column("oauth_accounts", "refresh_token")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(OauthAccounts::Table)
                        .drop_column(OauthAccounts::RefreshToken)
                        .to_owned(),
                )
                .await?;
        }
        if manager
            .has_column("oauth_accounts", "token_expires_at")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(OauthAccounts::Table)
                        .drop_column(OauthAccounts::TokenExpiresAt)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("oauth_accounts", "access_token").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(OauthAccounts::Table)
                        .add_column(ColumnDef::new(OauthAccounts::AccessToken).text().null())
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("oauth_accounts", "refresh_token")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(OauthAccounts::Table)
                        .add_column(ColumnDef::new(OauthAccounts::RefreshToken).text().null())
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("oauth_accounts", "token_expires_at")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(OauthAccounts::Table)
                        .add_column(
                            ColumnDef::new(OauthAccounts::TokenExpiresAt)
                                .timestamp_with_time_zone()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum OauthAccounts {
    Table,
    AccessToken,
    RefreshToken,
    TokenExpiresAt,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    const SCHEMA: &str = "\
        CREATE TABLE oauth_accounts (\
            id INTEGER PRIMARY KEY, provider TEXT NOT NULL, provider_user_id TEXT NOT NULL,\
            provider_username TEXT NOT NULL, email TEXT NOT NULL,\
            access_token TEXT, refresh_token TEXT, token_expires_at TEXT,\
            user_id BIGINT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL\
        );\
        INSERT INTO oauth_accounts\
            (id, provider, provider_user_id, provider_username, email,\
             access_token, refresh_token, token_expires_at, user_id, created_at, updated_at)\
            VALUES (1, 'github', 'gh-1', 'alice', 'alice@example.invalid',\
                    'ciphertext-access', 'ciphertext-refresh', '2026-08-22T00:00:00Z',\
                    7, '2026-08-01T00:00:00Z', '2026-08-01T00:00:00Z');";

    async fn fixture() -> sea_orm_migration::sea_orm::DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(SCHEMA).await.unwrap();
        db
    }

    /// The link survives; only the credentials leave. Applied twice, because a
    /// migration that already ran must not fail the next boot.
    #[tokio::test]
    async fn the_tokens_go_and_the_identity_stays() {
        let db = fixture().await;
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        for column in ["access_token", "refresh_token", "token_expires_at"] {
            assert!(
                !manager.has_column("oauth_accounts", column).await.unwrap(),
                "`oauth_accounts.{column}` still holds the provider's credential"
            );
        }

        let link = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT provider_user_id, user_id FROM oauth_accounts WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .expect("the link that identifies this person must survive");
        assert_eq!(
            link.try_get::<String>("", "provider_user_id").unwrap(),
            "gh-1"
        );
        assert_eq!(link.try_get::<i64>("", "user_id").unwrap(), 7);
    }

    #[tokio::test]
    async fn down_restores_the_columns_empty() {
        let db = fixture().await;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();

        Migration.down(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();

        for column in ["access_token", "refresh_token", "token_expires_at"] {
            assert!(manager.has_column("oauth_accounts", column).await.unwrap());
        }

        let restored = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM oauth_accounts WHERE access_token IS NULL".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            restored.try_get::<i64>("", "n").unwrap(),
            1,
            "the ciphertext this migration destroyed must not come back"
        );
    }
}
