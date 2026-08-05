//! Remove historical user ids that no longer resolve from JSON authorization
//! lists. The runtime deletion path performs the same cleanup transactionally;
//! this migration closes the pre-upgrade gap so an explicitly reused numeric
//! id cannot inherit a grant left by an account deleted on an older version.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        crate::serialized_user_grants::prune_missing_users(manager.get_connection())
            .await
            .map_err(|error| {
                DbErr::Custom(format!(
                    "clean historical serialized user grants: {error:#}"
                ))
            })
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Removed grants belonged to users that no longer existed and cannot be
        // reconstructed safely.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, Statement};

    use super::*;

    #[tokio::test]
    async fn historical_orphans_are_removed_without_touching_live_grants() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE users (id INTEGER PRIMARY KEY);\
             CREATE TABLE protected_branches (\
                 id INTEGER PRIMARY KEY, allowed_push_user_ids TEXT, updated_at TEXT NOT NULL\
             );\
             CREATE TABLE protected_tags (\
                 id INTEGER PRIMARY KEY, allowed_user_ids TEXT, updated_at TEXT NOT NULL\
             );\
             CREATE TABLE ci_environments (\
                 id INTEGER PRIMARY KEY, allowed_approver_ids TEXT, updated_at TEXT NOT NULL\
             );\
             INSERT INTO users (id) VALUES (7);\
             INSERT INTO protected_branches VALUES (1, '[7,9]', CURRENT_TIMESTAMP);\
             INSERT INTO protected_tags VALUES (1, '[9,7,9]', CURRENT_TIMESTAMP);\
             INSERT INTO ci_environments VALUES (1, '[9]', CURRENT_TIMESTAMP);",
        )
        .await
        .unwrap();

        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("clean historical serialized grants");

        for (table, column, expected) in [
            ("protected_branches", "allowed_push_user_ids", "[7]"),
            ("protected_tags", "allowed_user_ids", "[7]"),
            ("ci_environments", "allowed_approver_ids", "[]"),
        ] {
            let row = db
                .query_one(Statement::from_string(
                    db.get_database_backend(),
                    format!("SELECT {column} FROM {table} WHERE id = 1"),
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.try_get::<String>("", column).unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn invalid_json_is_rejected_before_any_historical_grant_is_rewritten() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE users (id INTEGER PRIMARY KEY);\
             CREATE TABLE protected_branches (\
                 id INTEGER PRIMARY KEY, allowed_push_user_ids TEXT, updated_at TEXT NOT NULL\
             );\
             CREATE TABLE protected_tags (\
                 id INTEGER PRIMARY KEY, allowed_user_ids TEXT, updated_at TEXT NOT NULL\
             );\
             CREATE TABLE ci_environments (\
                 id INTEGER PRIMARY KEY, allowed_approver_ids TEXT, updated_at TEXT NOT NULL\
             );\
             INSERT INTO users (id) VALUES (7);\
             INSERT INTO protected_branches VALUES (1, '[7,9]', CURRENT_TIMESTAMP);\
             INSERT INTO protected_tags VALUES (1, 'not-json', CURRENT_TIMESTAMP);",
        )
        .await
        .unwrap();

        let error = Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect_err("invalid stored grants must block the migration");
        assert!(format!("{error:#}").contains("allowed_user_ids"));
        let row = db
            .query_one(Statement::from_string(
                db.get_database_backend(),
                "SELECT allowed_push_user_ids FROM protected_branches WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<String>("", "allowed_push_user_ids").unwrap(),
            "[7,9]",
            "the branch grant was rewritten before the tag JSON was validated"
        );
    }
}
