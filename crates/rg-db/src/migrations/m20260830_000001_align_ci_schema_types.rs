//! Align the historical CI schema with its SeaORM entities on server databases.
//!
//! The first CI migration used `integer()` for ids represented as `i64` and a
//! later migration used `timestamp_with_time_zone()` for an entity field
//! represented as `DateTime` (`NaiveDateTime`). SQLite masks both mismatches;
//! PostgreSQL rejects the ids as INT4 vs INT8, while MySQL rejects TIMESTAMP vs
//! DATETIME. Fresh schemas now use the right definitions at their source, and
//! this migration repairs databases which already recorded the old migrations.

use sea_orm::{ConnectionTrait, DatabaseBackend};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => Ok(()),
            DatabaseBackend::Postgres => repair_postgres(manager).await,
            DatabaseBackend::MySql => repair_mysql(manager).await,
        }
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: after this migration accepts an id above i32::MAX,
        // narrowing back to INTEGER would either lose data or make rollback
        // fail at the worst possible time.
        Ok(())
    }
}

async fn repair_postgres(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    for statement in [
        "ALTER TABLE ci_environment_approvals \
         DROP CONSTRAINT IF EXISTS ci_environment_approvals_job_id_fkey",
        "ALTER TABLE ci_environment_approvals \
         DROP CONSTRAINT IF EXISTS fk_ci_environment_approvals_job_id",
        "ALTER TABLE pipelines \
           ALTER COLUMN id TYPE BIGINT, \
           ALTER COLUMN repo_id TYPE BIGINT, \
           ALTER COLUMN triggered_by TYPE BIGINT",
        "ALTER TABLE pipeline_stages \
           ALTER COLUMN id TYPE BIGINT, \
           ALTER COLUMN pipeline_id TYPE BIGINT",
        "ALTER TABLE pipeline_jobs \
           ALTER COLUMN id TYPE BIGINT, \
           ALTER COLUMN stage_id TYPE BIGINT, \
           ALTER COLUMN updated_at TYPE TIMESTAMP \
             USING updated_at AT TIME ZONE 'UTC'",
        "ALTER TABLE ci_environment_approvals \
           ALTER COLUMN job_id TYPE BIGINT",
        "ALTER SEQUENCE IF EXISTS pipelines_id_seq AS BIGINT",
        "ALTER SEQUENCE IF EXISTS pipeline_stages_id_seq AS BIGINT",
        "ALTER SEQUENCE IF EXISTS pipeline_jobs_id_seq AS BIGINT",
        "ALTER TABLE ci_environment_approvals \
           ADD CONSTRAINT fk_ci_environment_approvals_job_id \
           FOREIGN KEY (job_id) REFERENCES pipeline_jobs(id) ON DELETE CASCADE",
    ] {
        db.execute_unprepared(statement).await?;
    }
    Ok(())
}

async fn repair_mysql(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let pool = match manager.get_connection() {
        SchemaManagerConnection::Connection(db) => db.get_mysql_connection_pool(),
        SchemaManagerConnection::Transaction(_) => {
            return Err(DbErr::Custom(
                "CI schema repair requires a dedicated non-transactional MySQL connection"
                    .to_string(),
            ));
        }
    };
    let mut connection = pool.acquire().await.map_err(|error| {
        DbErr::Custom(format!(
            "CI schema repair: acquire a dedicated MySQL connection: {error}"
        ))
    })?;
    let original_time_zone: String = sea_orm::sqlx::query_scalar("SELECT @@session.time_zone")
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| {
            DbErr::Custom(format!("CI schema repair: read MySQL time zone: {error}"))
        })?;
    let foreign_key: Option<String> = sea_orm::sqlx::query_scalar(
        "SELECT CONSTRAINT_NAME FROM information_schema.KEY_COLUMN_USAGE \
         WHERE CONSTRAINT_SCHEMA = DATABASE() \
           AND TABLE_NAME = 'ci_environment_approvals' \
           AND COLUMN_NAME = 'job_id' \
           AND REFERENCED_TABLE_NAME = 'pipeline_jobs' \
           AND REFERENCED_COLUMN_NAME = 'id' \
         LIMIT 1",
    )
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| DbErr::Custom(format!("CI schema repair: find MySQL job FK: {error}")))?;

    let repair = async {
        if let Some(foreign_key) = foreign_key {
            if !foreign_key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err(DbErr::Custom(format!(
                    "unsafe MySQL foreign-key identifier for CI schema repair: {foreign_key:?}"
                )));
            }
            mysql_execute(
                &mut connection,
                &format!("ALTER TABLE `ci_environment_approvals` DROP FOREIGN KEY `{foreign_key}`"),
            )
            .await?;
        }

        // TIMESTAMP -> DATETIME conversion uses the session time zone. Every
        // statement therefore runs on this one pinned UTC session.
        for statement in [
            "SET time_zone = '+00:00'",
            "ALTER TABLE `pipelines` \
               MODIFY COLUMN `id` BIGINT NOT NULL AUTO_INCREMENT, \
               MODIFY COLUMN `repo_id` BIGINT NOT NULL, \
               MODIFY COLUMN `triggered_by` BIGINT NULL",
            "ALTER TABLE `pipeline_stages` \
               MODIFY COLUMN `id` BIGINT NOT NULL AUTO_INCREMENT, \
               MODIFY COLUMN `pipeline_id` BIGINT NOT NULL",
            "ALTER TABLE `pipeline_jobs` \
               MODIFY COLUMN `id` BIGINT NOT NULL AUTO_INCREMENT, \
               MODIFY COLUMN `stage_id` BIGINT NOT NULL, \
               MODIFY COLUMN `updated_at` DATETIME NULL",
            "ALTER TABLE `ci_environment_approvals` \
               MODIFY COLUMN `job_id` BIGINT NOT NULL",
            "ALTER TABLE `ci_environment_approvals` \
               ADD CONSTRAINT `fk_ci_environment_approvals_job_id` \
               FOREIGN KEY (`job_id`) REFERENCES `pipeline_jobs` (`id`) ON DELETE CASCADE",
        ] {
            mysql_execute(&mut connection, statement).await?;
        }
        Ok(())
    }
    .await;

    let restore = sea_orm::sqlx::query("SET time_zone = ?")
        .bind(&original_time_zone)
        .execute(&mut *connection)
        .await
        .map(|_| ())
        .map_err(|error| {
            DbErr::Custom(format!(
                "CI schema repair: restore MySQL time zone: {error}"
            ))
        });

    match (repair, restore) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => {
            connection.close_on_drop();
            Err(error)
        }
        (Err(error), Err(restore_error)) => {
            connection.close_on_drop();
            Err(DbErr::Custom(format!(
                "{error}; restoring the MySQL time zone also failed: {restore_error}"
            )))
        }
    }
}

async fn mysql_execute(
    connection: &mut sea_orm::sqlx::pool::PoolConnection<sea_orm::sqlx::MySql>,
    statement: &str,
) -> Result<(), DbErr> {
    use sea_orm::sqlx::Executor as _;

    (&mut **connection)
        .execute(statement)
        .await
        .map(|_| ())
        .map_err(|error| DbErr::Custom(format!("CI schema repair: execute MySQL DDL: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{Database, Statement};

    #[tokio::test]
    async fn sqlite_keeps_its_native_wide_integer_and_naive_datetime_contract() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE pipelines (id INTEGER PRIMARY KEY); \
             CREATE TABLE pipeline_stages (id INTEGER PRIMARY KEY); \
             CREATE TABLE pipeline_jobs (id INTEGER PRIMARY KEY, updated_at TEXT); \
             INSERT INTO pipeline_jobs (id, updated_at) \
               VALUES (9223372036854775806, '2026-08-30 04:00:00');",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let row = db
            .query_one(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT id, updated_at FROM pipeline_jobs".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<i64>("", "id").unwrap(),
            9_223_372_036_854_775_806
        );
        assert_eq!(
            row.try_get::<String>("", "updated_at").unwrap(),
            "2026-08-30 04:00:00"
        );
    }
}
