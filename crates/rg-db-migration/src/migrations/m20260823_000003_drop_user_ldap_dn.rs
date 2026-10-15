//! Drop `users.ldap_dn` — directory data Plombir Git copies in and never asks a
//! question of.
//!
//! An LDAP first login stores the entry's distinguished name, and every
//! subsequent bind rewrites it (`user_ops::create_ldap_user`,
//! `user_ops::sync_ldap_identity`). Nothing reads it back but a test assertion
//! (card_b70de2169bd6).
//!
//! It is not the key either. A directory account is found by
//! `(ldap_provider_id, ldap_uid)` — `user_ops::find_by_ldap_provider_and_uid` —
//! and a bind resolves through `ldap_uid` falling back to the login name
//! (`user::service::login_ldap`). A DN moves whenever the entry is moved
//! between organisational units, which is exactly why it was never the
//! identity; what it is instead is a copy of somebody's place in an
//! organisation chart, kept indefinitely, for nothing.
//!
//! ## Why there is no preflight here
//!
//! Its two neighbours in this batch have one, because dropping them could
//! destroy the last record of something. This one cannot: no code path reads
//! the value, and none can be made to depend on it, since the pair that
//! identifies a directory account is stored separately and the login falls back
//! to the username when even `ldap_uid` is absent. A gate that can only ever
//! refuse for a reason that does not exist is a boot this instance can fail for
//! nothing, so there isn't one.
//!
//! `down` restores the column empty; the next successful bind fills it again
//! for any account that still logs in through a directory.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260823_000003_drop_user_ldap_dn"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("users", "ldap_dn").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .drop_column(Users::LdapDn)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("users", "ldap_dn").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .add_column(ColumnDef::new(Users::LdapDn).string_len(255).null())
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
    LdapDn,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{
        ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    };

    const SCHEMA: &str = "\
        CREATE TABLE users (\
            id INTEGER PRIMARY KEY, username TEXT NOT NULL, auth_provider TEXT NOT NULL,\
            ldap_dn TEXT, ldap_uid TEXT, ldap_provider_id BIGINT\
        );\
        INSERT INTO users (id, username, auth_provider, ldap_dn, ldap_uid, ldap_provider_id) \
        VALUES (1, 'alice', 'ldap', 'uid=alice,ou=people,dc=example,dc=invalid', 'alice', 4),\
               (2, 'bob', 'local', NULL, NULL, NULL);";

    async fn fixture() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(SCHEMA).await.unwrap();
        db
    }

    /// The identity that actually resolves a bind survives; the copy of the
    /// directory tree does not.
    #[tokio::test]
    async fn the_lookup_pair_stays_and_the_dn_goes() {
        let db = fixture().await;
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        assert!(!manager.has_column("users", "ldap_dn").await.unwrap());
        let identity = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT ldap_uid, ldap_provider_id FROM users WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .expect("the directory account must survive");
        assert_eq!(
            identity.try_get::<String>("", "ldap_uid").unwrap(),
            "alice",
            "the half of the lookup key that names the entry is gone"
        );
        assert_eq!(
            identity.try_get::<i64>("", "ldap_provider_id").unwrap(),
            4,
            "the half of the lookup key that names the directory is gone"
        );

        let accounts = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM users".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(accounts.try_get::<i64>("", "n").unwrap(), 2);
    }

    #[tokio::test]
    async fn down_restores_the_column_empty() {
        let db = fixture().await;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();

        Migration.down(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();

        assert!(manager.has_column("users", "ldap_dn").await.unwrap());
        let filled = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM users WHERE ldap_dn IS NOT NULL".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            filled.try_get::<i64>("", "n").unwrap(),
            0,
            "the next successful bind refills it; a migration inventing DNs would not"
        );
    }
}
