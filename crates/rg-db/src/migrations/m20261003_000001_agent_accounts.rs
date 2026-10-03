//! card_60a80311d512: agents as first-class participants.
//!
//! Three things an AI agent needs that a person's Personal Access Token did not
//! give it:
//!
//! - **An identity of its own.** `users.bot_owner_id` marks an account as a
//!   bot and names the person answerable for it. `NULL` is every account that
//!   exists today, so no live account changes kind. The reference carries no
//!   `ON DELETE` action on purpose: deleting the owner must not take the bot
//!   row — and through `repositories.owner_id ON DELETE CASCADE`, the bot's
//!   repository rows — with it while their bytes stay in storage. The account
//!   deletion service refuses an owner who still has bots, and the database
//!   refuses it too.
//! - **A narrower token.** `access_tokens.repo_restricted` +
//!   `access_token_repositories` confine a token to named repositories,
//!   `mcp_tools` confines it to named MCP tools served by this instance, and
//!   `deny_protected_merge` keeps it off protected branches. The flag is
//!   separate from the rows so a token whose last repository was deleted stays
//!   restricted to nothing instead of becoming unrestricted: the cascade
//!   removes a row, never the restriction. Every existing token gets the
//!   defaults, which are exactly what it could do before.

use sea_orm::{ConnectionTrait, DatabaseBackend};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let connection = manager.get_connection();

        if !manager.has_column("users", "bot_owner_id").await? {
            let statement = match backend {
                DatabaseBackend::Sqlite => {
                    "ALTER TABLE users ADD COLUMN bot_owner_id INTEGER NULL REFERENCES users(id)"
                }
                DatabaseBackend::Postgres => {
                    "ALTER TABLE users ADD COLUMN bot_owner_id BIGINT NULL REFERENCES users(id)"
                }
                DatabaseBackend::MySql => {
                    "ALTER TABLE users ADD COLUMN bot_owner_id BIGINT NULL, \
                     ADD CONSTRAINT fk_users_bot_owner_id FOREIGN KEY (bot_owner_id) \
                     REFERENCES users(id)"
                }
            };
            connection.execute_unprepared(statement).await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_users_bot_owner_id")
                        .table(Users::Table)
                        .col(Users::BotOwnerId)
                        .to_owned(),
                )
                .await?;
        }

        for (column, definition) in [
            (
                AccessTokens::RepoRestricted,
                ColumnDef::new(AccessTokens::RepoRestricted)
                    .boolean()
                    .not_null()
                    .default(false)
                    .to_owned(),
            ),
            (
                AccessTokens::McpTools,
                ColumnDef::new(AccessTokens::McpTools)
                    .text()
                    .null()
                    .to_owned(),
            ),
            (
                AccessTokens::DenyProtectedMerge,
                ColumnDef::new(AccessTokens::DenyProtectedMerge)
                    .boolean()
                    .not_null()
                    .default(false)
                    .to_owned(),
            ),
        ] {
            if !manager
                .has_column("access_tokens", &column.to_string())
                .await?
            {
                manager
                    .alter_table(
                        Table::alter()
                            .table(AccessTokens::Table)
                            .add_column(definition)
                            .to_owned(),
                    )
                    .await?;
            }
        }

        manager
            .create_table(
                Table::create()
                    .table(AccessTokenRepositories::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AccessTokenRepositories::TokenId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AccessTokenRepositories::RepositoryId)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(AccessTokenRepositories::TokenId)
                            .col(AccessTokenRepositories::RepositoryId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_access_token_repositories_token")
                            .from(
                                AccessTokenRepositories::Table,
                                AccessTokenRepositories::TokenId,
                            )
                            .to(AccessTokens::Table, AccessTokens::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_access_token_repositories_repository")
                            .from(
                                AccessTokenRepositories::Table,
                                AccessTokenRepositories::RepositoryId,
                            )
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_access_token_repositories_repository")
                    .table(AccessTokenRepositories::Table)
                    .col(AccessTokenRepositories::RepositoryId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(AccessTokenRepositories::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        for column in [
            AccessTokens::DenyProtectedMerge,
            AccessTokens::McpTools,
            AccessTokens::RepoRestricted,
        ] {
            if manager
                .has_column("access_tokens", &column.to_string())
                .await?
            {
                manager
                    .alter_table(
                        Table::alter()
                            .table(AccessTokens::Table)
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        if manager.has_column("users", "bot_owner_id").await? {
            let connection = manager.get_connection();
            if manager.get_database_backend() == DatabaseBackend::MySql {
                connection
                    .execute_unprepared("ALTER TABLE users DROP FOREIGN KEY fk_users_bot_owner_id")
                    .await?;
            }
            manager
                .drop_index(
                    Index::drop()
                        .name("idx_users_bot_owner_id")
                        .table(Users::Table)
                        .to_owned(),
                )
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .drop_column(Users::BotOwnerId)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    BotOwnerId,
}

#[derive(DeriveIden)]
enum AccessTokens {
    Table,
    Id,
    RepoRestricted,
    McpTools,
    DenyProtectedMerge,
}

#[derive(DeriveIden)]
enum AccessTokenRepositories {
    Table,
    TokenId,
    RepositoryId,
}

#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseBackend, Statement};

    async fn migrated() -> sea_orm::DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options).await.unwrap();
        db.execute_unprepared("PRAGMA foreign_keys = ON")
            .await
            .unwrap();
        crate::run_migrations(&db).await.unwrap();
        db
    }

    async fn count(db: &sea_orm::DatabaseConnection, sql: &str) -> i64 {
        db.query_one(Statement::from_string(DatabaseBackend::Sqlite, sql))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "n")
            .unwrap()
    }

    /// The owner of a bot cannot be removed from under it: the cascade that
    /// would follow takes the bot's repository rows and leaves their bytes.
    #[tokio::test]
    async fn a_bot_owner_cannot_be_deleted_while_the_bot_exists() {
        let db = migrated().await;
        for statement in [
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(1, 'alice', 'alice@example.test', 'x', 0, 1, '2026-01-01', '2026-01-01')",
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at, auth_provider, bot_owner_id) \
             VALUES(2, 'alice-agent', 'alice-agent@bots.invalid', '', 0, 1, '2026-01-01', '2026-01-01', 'bot', 1)",
        ] {
            db.execute_unprepared(statement).await.unwrap();
        }
        assert!(db
            .execute_unprepared("DELETE FROM users WHERE id = 1")
            .await
            .is_err());
        db.execute_unprepared("DELETE FROM users WHERE id = 2")
            .await
            .unwrap();
        db.execute_unprepared("DELETE FROM users WHERE id = 1")
            .await
            .unwrap();
    }

    /// Deleting a repository removes it from every allow-list without turning
    /// a restricted token into an unrestricted one.
    #[tokio::test]
    async fn deleting_the_last_allowed_repository_keeps_the_token_restricted() {
        let db = migrated().await;
        for statement in [
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(1, 'alice', 'alice@example.test', 'x', 0, 1, '2026-01-01', '2026-01-01')",
            "INSERT INTO repositories(id, owner_id, name, is_private, default_branch, created_at, updated_at) \
             VALUES(10, 1, 'app', 0, 'main', '2026-01-01', '2026-01-01')",
            "INSERT INTO access_tokens(id, user_id, name, token_hash, scopes, created_at, repo_restricted) \
             VALUES(5, 1, 'agent', 'h', 'repo', '2026-01-01', 1)",
            "INSERT INTO access_token_repositories(token_id, repository_id) VALUES(5, 10)",
        ] {
            db.execute_unprepared(statement).await.unwrap();
        }
        db.execute_unprepared("DELETE FROM repositories WHERE id = 10")
            .await
            .unwrap();
        assert_eq!(
            count(&db, "SELECT COUNT(*) AS n FROM access_token_repositories").await,
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) AS n FROM access_tokens WHERE id = 5 AND repo_restricted = 1"
            )
            .await,
            1
        );
    }
}
