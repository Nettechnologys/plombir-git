use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000015_fix_oauth_accounts_table_name"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("o_auth_accounts").await?
            && !manager.has_table("oauth_accounts").await?
        {
            manager
                .rename_table(
                    Table::rename()
                        .table(Alias::new("o_auth_accounts"), Alias::new("oauth_accounts"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("oauth_accounts").await?
            && !manager.has_table("o_auth_accounts").await?
        {
            manager
                .rename_table(
                    Table::rename()
                        .table(Alias::new("oauth_accounts"), Alias::new("o_auth_accounts"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn renames_legacy_table_without_losing_oauth_accounts() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE o_auth_accounts (id INTEGER PRIMARY KEY, provider TEXT NOT NULL, provider_user_id TEXT NOT NULL);\
             INSERT INTO o_auth_accounts (id, provider, provider_user_id) VALUES (7, 'gitea', 'user-7');",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let manager = SchemaManager::new(&db);
        assert!(!manager.has_table("o_auth_accounts").await.unwrap());
        assert!(manager.has_table("oauth_accounts").await.unwrap());
        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT id, provider_user_id FROM oauth_accounts".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<i64>("", "id").unwrap(), 7);
        assert_eq!(
            row.try_get::<String>("", "provider_user_id").unwrap(),
            "user-7"
        );
    }
}
