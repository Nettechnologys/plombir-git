//! Scope repository CI secrets to a deployment environment.
//!
//! Before this migration `ci_secrets` had one scope — the repository — so a
//! secret meant for a protected `production` environment was handed to every
//! job of every branch. `environment_id` adds the missing scope: `NULL` is the
//! old, repository-wide scope; a value names one `ci_environments` row and the
//! secret is only injected into that environment's jobs (security audit
//! finding #11).
//!
//! # Why `scope_key`
//!
//! The new uniqueness is "one name per scope", `(repo_id, environment_id,
//! name)`, but every backend SQL treats `NULL` as distinct, so a unique index
//! on those columns would let two concurrent writes of the same repository-wide
//! secret both land. `scope_key` is a generated column projecting the scope
//! onto a non-NULL value — `0` for repository-wide, the environment id
//! otherwise (environment ids are positive) — and the unique index is over
//! `(repo_id, scope_key, name)`. That keeps the concurrency guarantee the old
//! `uq_ci_secrets_repo_name` index provided (see `ci_secret_ops::upsert`)
//! while allowing the same name in two different environments.
//!
//! MySQL stores the generated column `VIRTUAL`: a stored generated column with
//! an `ON DELETE CASCADE` foreign key on its base column is rejected.
//!
//! # SQLite
//!
//! The pre-migration shape declares `uq_ci_secrets_repo_name` as `UNIQUE`
//! inside `CREATE TABLE` (the ghost-author rebuild in
//! `m20260805_000004_repo_config_outlives_its_author`), and SQLite creates an
//! implicit index for it that `DROP INDEX` cannot name or remove. The table is
//! rebuilt instead — the same answer `m20260730_000001` had to give. No other
//! table references `ci_secrets`, so the rename does not have to cascade
//! through children.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr};
use sea_orm_migration::prelude::*;

pub const OLD_UNIQUE_INDEX: &str = "uq_ci_secrets_repo_name";
pub const NEW_UNIQUE_INDEX: &str = "uq_ci_secrets_repo_environment_name";
pub const CREATED_BY_INDEX: &str = "idx_ci_secrets_created_by";
pub const ENVIRONMENT_INDEX: &str = "idx_ci_secrets_environment";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("ci_secrets").await? {
            return Ok(());
        }
        if manager.has_column("ci_secrets", "environment_id").await? {
            return ensure_scope_indexes(manager).await;
        }
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => up_sqlite(manager).await,
            DatabaseBackend::Postgres => up_postgres(manager).await,
            DatabaseBackend::MySql => up_mysql(manager).await,
        }
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("ci_secrets").await? {
            return Ok(());
        }
        if !manager.has_column("ci_secrets", "environment_id").await? {
            return Ok(());
        }
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => down_sqlite(manager).await,
            DatabaseBackend::Postgres => down_postgres(manager).await,
            DatabaseBackend::MySql => down_mysql(manager).await,
        }
    }
}

async fn execute(manager: &SchemaManager<'_>, sql: &str) -> Result<(), DbErr> {
    manager.get_connection().execute_unprepared(sql).await?;
    Ok(())
}

/// Bring an already-rebuilt schema up to the full set of indexes.
///
/// A half-applied run must still converge: the column check above is the only
/// gate for the rebuild, so every index this migration owns is re-checked here
/// rather than assumed to have come along with the column.
async fn ensure_scope_indexes(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    if !manager.has_index("ci_secrets", NEW_UNIQUE_INDEX).await? {
        execute(
            manager,
            &format!(
                "CREATE UNIQUE INDEX \"{NEW_UNIQUE_INDEX}\" ON \"ci_secrets\" \
                 (\"repo_id\", \"scope_key\", \"name\")"
            ),
        )
        .await?;
    }
    if !manager.has_index("ci_secrets", ENVIRONMENT_INDEX).await? {
        execute(
            manager,
            &format!("CREATE INDEX \"{ENVIRONMENT_INDEX}\" ON \"ci_secrets\" (\"environment_id\")"),
        )
        .await?;
    }
    Ok(())
}

async fn up_postgres(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    for statement in [
        "ALTER TABLE \"ci_secrets\" ADD COLUMN \"environment_id\" BIGINT NULL \
         REFERENCES \"ci_environments\" (\"id\") ON DELETE CASCADE",
        "ALTER TABLE \"ci_secrets\" ADD COLUMN \"scope_key\" BIGINT \
         GENERATED ALWAYS AS (COALESCE(\"environment_id\", 0)) STORED",
        "ALTER TABLE \"ci_secrets\" DROP CONSTRAINT IF EXISTS \"uq_ci_secrets_repo_name\"",
        "DROP INDEX IF EXISTS \"uq_ci_secrets_repo_name\"",
        &format!(
            "CREATE INDEX IF NOT EXISTS \"{ENVIRONMENT_INDEX}\" ON \"ci_secrets\" (\"environment_id\")"
        ),
        &format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS \"{NEW_UNIQUE_INDEX}\" ON \"ci_secrets\" \
             (\"repo_id\", \"scope_key\", \"name\")"
        ),
    ] {
        execute(manager, statement).await?;
    }
    Ok(())
}

async fn up_mysql(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    for statement in [
        "ALTER TABLE `ci_secrets` ADD COLUMN `environment_id` BIGINT NULL",
        "ALTER TABLE `ci_secrets` ADD CONSTRAINT `fk_ci_secrets_environment` \
         FOREIGN KEY (`environment_id`) REFERENCES `ci_environments` (`id`) ON DELETE CASCADE",
        "ALTER TABLE `ci_secrets` ADD COLUMN `scope_key` BIGINT \
         GENERATED ALWAYS AS (COALESCE(`environment_id`, 0)) VIRTUAL",
        &format!("CREATE INDEX `{ENVIRONMENT_INDEX}` ON `ci_secrets` (`environment_id`)"),
        &format!(
            "CREATE UNIQUE INDEX `{NEW_UNIQUE_INDEX}` ON `ci_secrets` \
             (`repo_id`, `scope_key`, `name`)"
        ),
    ] {
        execute(manager, statement).await?;
    }
    if manager.has_index("ci_secrets", OLD_UNIQUE_INDEX).await? {
        execute(
            manager,
            &format!("ALTER TABLE `ci_secrets` DROP INDEX `{OLD_UNIQUE_INDEX}`"),
        )
        .await?;
    }
    Ok(())
}

/// The SQLite rebuild, in one multi-statement call on one connection.
///
/// Ordering is forced by the implicit unique index: the table is renamed with
/// its constraint intact, the replacement is created and filled from it, and
/// only then is the old table dropped. `scope_key` is computed by the new
/// table's `INSERT ... SELECT`, so the copied rows keep their exact identity
/// while every repository-wide row lands in scope `0`.
async fn up_sqlite(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    execute(
        manager,
        r#"
        ALTER TABLE "ci_secrets" RENAME TO "ci_secrets_pre_environment_scope";
        CREATE TABLE "ci_secrets" (
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "repo_id" bigint NOT NULL,
            "name" varchar NOT NULL,
            "encrypted_value" text NOT NULL,
            "created_by_id" bigint NULL,
            "created_at" timestamp_with_timezone_text NOT NULL,
            "updated_at" timestamp_with_timezone_text NOT NULL,
            "environment_id" bigint NULL REFERENCES "ci_environments" ("id") ON DELETE CASCADE,
            "scope_key" bigint GENERATED ALWAYS AS (COALESCE("environment_id", 0)) STORED,
            FOREIGN KEY ("repo_id") REFERENCES "repositories" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("created_by_id") REFERENCES "users" ("id") ON DELETE SET NULL
        );
        INSERT INTO "ci_secrets"
            ("id", "repo_id", "name", "encrypted_value", "created_by_id", "created_at", "updated_at")
        SELECT "id", "repo_id", "name", "encrypted_value", "created_by_id", "created_at", "updated_at"
        FROM "ci_secrets_pre_environment_scope";
        DROP INDEX IF EXISTS "uq_ci_secrets_repo_name";
        DROP INDEX IF EXISTS "idx_ci_secrets_created_by";
        INSERT INTO "sqlite_sequence" ("name", "seq")
            SELECT 'ci_secrets', "seq" FROM "sqlite_sequence"
            WHERE "name" = 'ci_secrets_pre_environment_scope'
              AND NOT EXISTS (SELECT 1 FROM "sqlite_sequence" WHERE "name" = 'ci_secrets');
        UPDATE "sqlite_sequence"
            SET "seq" = (SELECT "seq" FROM "sqlite_sequence" WHERE "name" = 'ci_secrets_pre_environment_scope')
            WHERE "name" = 'ci_secrets'
              AND "seq" < (SELECT "seq" FROM "sqlite_sequence" WHERE "name" = 'ci_secrets_pre_environment_scope');
        DROP TABLE "ci_secrets_pre_environment_scope";
        CREATE INDEX "idx_ci_secrets_created_by" ON "ci_secrets" ("created_by_id");
        CREATE INDEX "idx_ci_secrets_environment" ON "ci_secrets" ("environment_id");
        CREATE UNIQUE INDEX "uq_ci_secrets_repo_environment_name"
            ON "ci_secrets" ("repo_id", "scope_key", "name");
        "#,
    )
    .await
}

async fn down_postgres(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    for statement in [
        &format!("DROP INDEX IF EXISTS \"{NEW_UNIQUE_INDEX}\""),
        &format!("DROP INDEX IF EXISTS \"{ENVIRONMENT_INDEX}\""),
        "ALTER TABLE \"ci_secrets\" DROP COLUMN \"scope_key\"",
        "ALTER TABLE \"ci_secrets\" DROP COLUMN \"environment_id\"",
        "CREATE UNIQUE INDEX \"uq_ci_secrets_repo_name\" ON \"ci_secrets\" (\"repo_id\", \"name\")",
    ] {
        execute(manager, statement).await?;
    }
    Ok(())
}

async fn down_mysql(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    for statement in [
        &format!("ALTER TABLE `ci_secrets` DROP INDEX `{NEW_UNIQUE_INDEX}`"),
        &format!("ALTER TABLE `ci_secrets` DROP INDEX `{ENVIRONMENT_INDEX}`"),
        "ALTER TABLE `ci_secrets` DROP FOREIGN KEY `fk_ci_secrets_environment`",
        "ALTER TABLE `ci_secrets` DROP COLUMN `scope_key`",
        "ALTER TABLE `ci_secrets` DROP COLUMN `environment_id`",
        "CREATE UNIQUE INDEX `uq_ci_secrets_repo_name` ON `ci_secrets` (`repo_id`, `name`)",
    ] {
        execute(manager, statement).await?;
    }
    Ok(())
}

async fn down_sqlite(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    execute(
        manager,
        r#"
        ALTER TABLE "ci_secrets" RENAME TO "ci_secrets_with_environment_scope";
        CREATE TABLE "ci_secrets" (
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "repo_id" bigint NOT NULL,
            "name" varchar NOT NULL,
            "encrypted_value" text NOT NULL,
            "created_by_id" bigint NULL,
            "created_at" timestamp_with_timezone_text NOT NULL,
            "updated_at" timestamp_with_timezone_text NOT NULL,
            CONSTRAINT "uq_ci_secrets_repo_name" UNIQUE ("repo_id", "name"),
            FOREIGN KEY ("repo_id") REFERENCES "repositories" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("created_by_id") REFERENCES "users" ("id") ON DELETE SET NULL
        );
        INSERT INTO "ci_secrets"
            ("id", "repo_id", "name", "encrypted_value", "created_by_id", "created_at", "updated_at")
        SELECT "id", "repo_id", "name", "encrypted_value", "created_by_id", "created_at", "updated_at"
        FROM "ci_secrets_with_environment_scope";
        DROP INDEX IF EXISTS "uq_ci_secrets_repo_environment_name";
        DROP INDEX IF EXISTS "idx_ci_secrets_environment";
        DROP INDEX IF EXISTS "idx_ci_secrets_created_by";
        INSERT INTO "sqlite_sequence" ("name", "seq")
            SELECT 'ci_secrets', "seq" FROM "sqlite_sequence"
            WHERE "name" = 'ci_secrets_with_environment_scope'
              AND NOT EXISTS (SELECT 1 FROM "sqlite_sequence" WHERE "name" = 'ci_secrets');
        UPDATE "sqlite_sequence"
            SET "seq" = (SELECT "seq" FROM "sqlite_sequence" WHERE "name" = 'ci_secrets_with_environment_scope')
            WHERE "name" = 'ci_secrets'
              AND "seq" < (SELECT "seq" FROM "sqlite_sequence" WHERE "name" = 'ci_secrets_with_environment_scope');
        DROP TABLE "ci_secrets_with_environment_scope";
        CREATE INDEX "idx_ci_secrets_created_by" ON "ci_secrets" ("created_by_id");
        "#,
    )
    .await
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};

    use super::*;

    /// The pre-migration `ci_secrets` shape, plus the parents its foreign keys
    /// name. Deliberately includes `uq_ci_secrets_repo_name` as the inline
    /// constraint the ghost-author rebuild leaves behind: that is the shape
    /// this migration actually meets on SQLite.
    async fn fixture() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared("PRAGMA foreign_keys = ON")
            .await
            .unwrap();
        for statement in [
            "CREATE TABLE repositories (id INTEGER PRIMARY KEY)",
            "CREATE TABLE users (id INTEGER PRIMARY KEY)",
            "CREATE TABLE ci_environments (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "CREATE TABLE ci_secrets (
                id integer NOT NULL PRIMARY KEY AUTOINCREMENT,
                repo_id bigint NOT NULL,
                name varchar NOT NULL,
                encrypted_value text NOT NULL,
                created_by_id bigint NULL,
                created_at timestamp_with_timezone_text NOT NULL,
                updated_at timestamp_with_timezone_text NOT NULL,
                CONSTRAINT \"uq_ci_secrets_repo_name\" UNIQUE (\"repo_id\", \"name\"),
                FOREIGN KEY (\"repo_id\") REFERENCES \"repositories\" (\"id\") ON DELETE CASCADE,
                FOREIGN KEY (\"created_by_id\") REFERENCES \"users\" (\"id\") ON DELETE SET NULL
            )",
            "CREATE INDEX \"idx_ci_secrets_created_by\" ON \"ci_secrets\" (\"created_by_id\")",
            "INSERT INTO repositories(id) VALUES(1), (2)",
            "INSERT INTO users(id) VALUES(7)",
            "INSERT INTO ci_environments(id, name) VALUES(10, 'staging'), (11, 'production')",
            "INSERT INTO ci_secrets(id, repo_id, name, encrypted_value, created_by_id, created_at, updated_at)
             VALUES(3, 1, 'REPO_WIDE', 'v1', 7, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        ] {
            db.execute_unprepared(statement).await.unwrap();
        }
        db
    }

    async fn scalar_i64(db: &DatabaseConnection, sql: &str) -> i64 {
        db.query_one(Statement::from_string(DatabaseBackend::Sqlite, sql))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "n")
            .unwrap()
    }

    #[tokio::test]
    async fn existing_secrets_keep_their_identity_in_the_repository_scope() {
        let db = fixture().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) AS n FROM ci_secrets s \
                 WHERE s.id = 3 AND s.name = 'REPO_WIDE' AND s.environment_id IS NULL"
            )
            .await,
            1
        );

        // Repository-wide rows still collide on name...
        assert!(db
            .execute_unprepared(
                "INSERT INTO ci_secrets(repo_id, name, encrypted_value, created_at, updated_at)
                 VALUES(1, 'REPO_WIDE', 'v2', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)"
            )
            .await
            .is_err());
        // ... while the same name is a different secret in each environment.
        db.execute_unprepared(
            "INSERT INTO ci_secrets(repo_id, environment_id, name, encrypted_value, created_at, updated_at)
             VALUES(1, 10, 'REPO_WIDE', 'v2', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP),
                   (1, 11, 'REPO_WIDE', 'v3', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .await
        .unwrap();
        assert!(db
            .execute_unprepared(
                "INSERT INTO ci_secrets(repo_id, environment_id, name, encrypted_value, created_at, updated_at)
                 VALUES(1, 10, 'REPO_WIDE', 'v4', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)"
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn an_environment_secret_cascades_with_its_environment() {
        let db = fixture().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        db.execute_unprepared(
            "INSERT INTO ci_secrets(repo_id, environment_id, name, encrypted_value, created_at, updated_at)
             VALUES(1, 11, 'PROD_KEY', 'v', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .await
        .unwrap();
        db.execute_unprepared("DELETE FROM ci_environments WHERE id = 11")
            .await
            .unwrap();
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) AS n FROM ci_secrets WHERE environment_id = 11"
            )
            .await,
            0
        );
        // A secret cannot name an environment that does not exist.
        assert!(db
            .execute_unprepared(
                "INSERT INTO ci_secrets(repo_id, environment_id, name, encrypted_value, created_at, updated_at)
                 VALUES(1, 12, 'ORPHAN', 'v', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)"
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn the_sequence_high_water_survives_the_rebuild() {
        let db = fixture().await;
        // The highest id was deleted before the migration; the rebuild must not
        // let the next secret reuse it.
        db.execute_unprepared(
            "INSERT INTO ci_secrets(repo_id, name, encrypted_value, created_at, updated_at)
             VALUES(2, 'MOMENTARY', 'v', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .await
        .unwrap();
        db.execute_unprepared("DELETE FROM ci_secrets WHERE repo_id = 2")
            .await
            .unwrap();
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        db.execute_unprepared(
            "INSERT INTO ci_secrets(repo_id, name, encrypted_value, created_at, updated_at)
             VALUES(1, 'NEXT', 'v', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .await
        .unwrap();
        assert!(
            scalar_i64(&db, "SELECT id AS n FROM ci_secrets WHERE name = 'NEXT'").await > 4,
            "the next id must be above the deleted high-water mark"
        );
    }

    #[tokio::test]
    async fn down_restores_the_repository_wide_constraint() {
        let db = fixture().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        Migration.down(&SchemaManager::new(&db)).await.unwrap();
        assert!(db
            .execute_unprepared(
                "INSERT INTO ci_secrets(repo_id, name, encrypted_value, created_at, updated_at)
                 VALUES(1, 'REPO_WIDE', 'v2', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)"
            )
            .await
            .is_err());
        assert_eq!(
            scalar_i64(
                &db,
                "SELECT COUNT(*) AS n FROM ci_secrets WHERE name = 'REPO_WIDE'"
            )
            .await,
            1
        );
    }
}
