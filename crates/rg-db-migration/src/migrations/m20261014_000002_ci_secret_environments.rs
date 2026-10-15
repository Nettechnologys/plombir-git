//! Scope repository CI secrets to a deployment environment.
//!
//! Before this migration `ci_secrets` had one scope, the repository, so a
//! secret meant for a protected `production` environment was handed to every
//! job of every branch. `environment_id` adds the missing scope: `NULL` is the
//! old, repository-wide scope; a value names one `ci_environments` row, and the
//! secret only reaches jobs of that environment, after its gate.
//!
//! # Why `scope_key`
//!
//! The new uniqueness is "one name per scope", `(repo_id, environment_id,
//! name)`, but every backend treats `NULL` as distinct in a unique index, so
//! two concurrent writes of one repository-wide secret would both land.
//! `scope_key` is a generated column projecting the scope onto a non-`NULL`
//! value (`0` for repository-wide, the environment id otherwise; ids are
//! positive) and the unique index is over `(repo_id, scope_key, name)`. That
//! keeps the guarantee `ci_secret_ops::upsert` relies on while allowing the
//! same name in two environments.
//!
//! MySQL stores the generated column `VIRTUAL`: it refuses a stored generated
//! column whose base column carries an `ON DELETE CASCADE` foreign key.
//!
//! # SQLite
//!
//! `uq_ci_secrets_repo_name` is declared inside `CREATE TABLE`, and SQLite can
//! neither drop that implicit index nor `ALTER` in a generated column, so the
//! table is rebuilt through [`super::ghost_author::SqliteReshape`]: one pinned
//! connection, both pragmas proven on it, one `BEGIN IMMEDIATE`, a foreign-key
//! check before `COMMIT`. A failure anywhere rolls the whole rebuild back, so a
//! secret is never left behind in the staging table while the replacement sits
//! empty and the migration is recorded as applied.
//!
//! # PostgreSQL and MySQL
//!
//! PostgreSQL runs a migration inside a transaction. MySQL commits every DDL
//! statement on its own, so each step checks the live schema first and a
//! re-run after a crash converges instead of failing on the step that already
//! happened.
//!
//! # `down`
//!
//! Refuses while any environment-scoped secret exists. Folding them into the
//! repository-wide scope would hand a production secret to every branch, and
//! the same name in two scopes cannot fit the old constraint anyway. Deleting
//! them is the operator's decision, not a rollback's.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement};
use sea_orm_migration::prelude::*;

use super::ghost_author::{GhostAuthorRebuild, SqliteReshape, SqliteTable};

pub const NAME: &str = "m20261014_000002_ci_secret_environments";
pub const OLD_UNIQUE_INDEX: &str = "uq_ci_secrets_repo_name";
pub const NEW_UNIQUE_INDEX: &str = "uq_ci_secrets_repo_environment_name";
pub const CREATED_BY_INDEX: &str = "idx_ci_secrets_created_by";
pub const ENVIRONMENT_INDEX: &str = "idx_ci_secrets_environment";
pub const ENVIRONMENT_FOREIGN_KEY: &str = "fk_ci_secrets_environment";
/// The table a SQLite rebuild renames `ci_secrets` to while it copies the rows.
pub const STAGING_TABLE: &str = "ci_secrets_pre_environment_scope";

const COLUMNS: &[&str] = &[
    "id",
    "repo_id",
    "name",
    "encrypted_value",
    "created_by_id",
    "created_at",
    "updated_at",
];

/// `pragma_table_info` leaves generated columns out, so the scoped table shows
/// `environment_id` but not `scope_key`.
const SCOPED_COLUMNS: &[&str] = &[
    "id",
    "repo_id",
    "name",
    "encrypted_value",
    "created_by_id",
    "created_at",
    "updated_at",
    "environment_id",
];

const CASCADE_WARNING: &str = "No table references `ci_secrets`, but a rebuild with foreign \
                               keys enforced is not one this migration will run.";

/// Carries the `AUTOINCREMENT` high-water mark across a rebuild: a secret id
/// is never handed out twice, even when the newest secret was deleted.
const SEQUENCE_ROW: &str = r#"
    INSERT INTO ci_secrets
        (id, repo_id, name, encrypted_value, created_by_id, created_at, updated_at)
    SELECT {seq}, 0, '_forgekeep_sequence_high_water', '', NULL,
           CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
    WHERE {seq} > COALESCE((SELECT MAX(id) FROM ci_secrets), 0);
    DELETE FROM ci_secrets
    WHERE id = {seq} AND repo_id = 0 AND name = '_forgekeep_sequence_high_water';"#;

const FOREIGN_KEYS: &str = r#"
            FOREIGN KEY ("repo_id") REFERENCES "repositories" ("id") ON DELETE CASCADE,
            FOREIGN KEY ("created_by_id") REFERENCES "users" ("id") ON DELETE SET NULL"#;

const SCOPED_TABLE: SqliteTable = SqliteTable {
    name: "ci_secrets",
    columns: COLUMNS,
    body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "repo_id" bigint NOT NULL,
            "name" varchar NOT NULL,
            "encrypted_value" text NOT NULL,
            "created_by_id" bigint NULL,
            "created_at" timestamp_with_timezone_text NOT NULL,
            "updated_at" timestamp_with_timezone_text NOT NULL,
            "environment_id" bigint NULL
                REFERENCES "ci_environments" ("id") ON DELETE CASCADE,
            "scope_key" bigint GENERATED ALWAYS AS (COALESCE("environment_id", 0)) STORED"#,
    foreign_keys: FOREIGN_KEYS,
    indexes: &[
        (
            CREATED_BY_INDEX,
            r#"CREATE INDEX "idx_ci_secrets_created_by" ON "ci_secrets" ("created_by_id");"#,
        ),
        (
            ENVIRONMENT_INDEX,
            r#"CREATE INDEX "idx_ci_secrets_environment" ON "ci_secrets" ("environment_id");"#,
        ),
        (
            NEW_UNIQUE_INDEX,
            r#"CREATE UNIQUE INDEX "uq_ci_secrets_repo_environment_name"
                ON "ci_secrets" ("repo_id", "scope_key", "name");"#,
        ),
    ],
    sequence_row: SEQUENCE_ROW,
};

const REPOSITORY_WIDE_TABLE: SqliteTable = SqliteTable {
    name: "ci_secrets",
    columns: COLUMNS,
    body: r#"
            "id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
            "repo_id" bigint NOT NULL,
            "name" varchar NOT NULL,
            "encrypted_value" text NOT NULL,
            "created_by_id" bigint NULL,
            "created_at" timestamp_with_timezone_text NOT NULL,
            "updated_at" timestamp_with_timezone_text NOT NULL,
            CONSTRAINT "uq_ci_secrets_repo_name" UNIQUE ("repo_id", "name")"#,
    foreign_keys: FOREIGN_KEYS,
    indexes: &[(
        CREATED_BY_INDEX,
        r#"CREATE INDEX "idx_ci_secrets_created_by" ON "ci_secrets" ("created_by_id");"#,
    )],
    sequence_row: SEQUENCE_ROW,
};

pub(crate) const SQLITE_UP: SqliteReshape = SqliteReshape {
    migration: NAME,
    staging_suffix: "_pre_environment_scope",
    table: &SCOPED_TABLE,
    live_columns: COLUMNS,
    cascade_warning: CASCADE_WARNING,
};

pub(crate) const SQLITE_DOWN: SqliteReshape = SqliteReshape {
    migration: NAME,
    staging_suffix: "_with_environment_scope",
    table: &REPOSITORY_WIDE_TABLE,
    live_columns: SCOPED_COLUMNS,
    cascade_warning: CASCADE_WARNING,
};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("ci_secrets").await? {
            return Ok(());
        }
        // The rebuild is atomic, so this table only exists if something other
        // than this migration interrupted one. Its rows may be the only copy
        // of the secrets; converging past it would record the migration as
        // applied and leave them there.
        if manager.has_table(STAGING_TABLE).await? {
            return Err(DbErr::Migration(format!(
                "{NAME}: refusing to run while `{STAGING_TABLE}` exists. It holds CI secrets \
                 from an interrupted rebuild; move its rows back into `ci_secrets` (or drop it \
                 once they are accounted for) and start again."
            )));
        }
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => {
                if !manager.has_column("ci_secrets", "environment_id").await? {
                    SQLITE_UP.sqlite(manager).await?;
                }
                Ok(())
            }
            DatabaseBackend::Postgres => up_postgres(manager).await,
            DatabaseBackend::MySql => up_mysql(manager).await,
        }
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("ci_secrets").await?
            || !manager.has_column("ci_secrets", "environment_id").await?
        {
            return Ok(());
        }
        refuse_environment_secrets(manager).await?;
        match manager.get_database_backend() {
            DatabaseBackend::Sqlite => SQLITE_DOWN.sqlite(manager).await,
            DatabaseBackend::Postgres => down_postgres(manager).await,
            DatabaseBackend::MySql => down_mysql(manager).await,
        }
    }
}

async fn execute(manager: &SchemaManager<'_>, sql: &str) -> Result<(), DbErr> {
    manager.get_connection().execute_unprepared(sql).await?;
    Ok(())
}

async fn refuse_environment_secrets(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let backend = manager.get_database_backend();
    let quote = if backend == DatabaseBackend::MySql {
        '`'
    } else {
        '"'
    };
    let scoped = manager
        .get_connection()
        .query_one(Statement::from_string(
            backend,
            format!(
                "SELECT COUNT(*) AS n FROM {quote}ci_secrets{quote} \
                 WHERE {quote}environment_id{quote} IS NOT NULL"
            ),
        ))
        .await?
        .map(|row| row.try_get::<i64>("", "n"))
        .transpose()?
        .unwrap_or(0);
    if scoped > 0 {
        return Err(DbErr::Migration(format!(
            "{NAME}: refusing to roll back while {scoped} environment-scoped CI secret(s) \
             exist. Moving them into the repository-wide scope would hand them to every \
             branch; delete them first if that is what you want."
        )));
    }
    Ok(())
}

/// A foreign-key lookup that only needs the migration's name for its errors.
fn lookup() -> GhostAuthorRebuild {
    GhostAuthorRebuild {
        migration: NAME,
        staging_suffix: "",
        columns: &[],
        sqlite_tables: &[],
        cascade_warning: CASCADE_WARNING,
    }
}

async fn up_postgres(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    for statement in [
        "ALTER TABLE \"ci_secrets\" ADD COLUMN IF NOT EXISTS \"environment_id\" BIGINT NULL",
        "ALTER TABLE \"ci_secrets\" ADD COLUMN IF NOT EXISTS \"scope_key\" BIGINT \
         GENERATED ALWAYS AS (COALESCE(\"environment_id\", 0)) STORED",
    ] {
        execute(manager, statement).await?;
    }
    if lookup()
        .postgres_foreign_key(manager, "ci_secrets", "environment_id", "ci_environments")
        .await?
        .is_none()
    {
        execute(
            manager,
            &format!(
                "ALTER TABLE \"ci_secrets\" ADD CONSTRAINT \"{ENVIRONMENT_FOREIGN_KEY}\" \
                 FOREIGN KEY (\"environment_id\") REFERENCES \"ci_environments\" (\"id\") \
                 ON DELETE CASCADE"
            ),
        )
        .await?;
    }
    for statement in [
        format!("CREATE INDEX IF NOT EXISTS \"{ENVIRONMENT_INDEX}\" ON \"ci_secrets\" (\"environment_id\")"),
        format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS \"{NEW_UNIQUE_INDEX}\" ON \"ci_secrets\" \
             (\"repo_id\", \"scope_key\", \"name\")"
        ),
        // Declared inline by `m20260712_000005_create_ci_secrets`, so it is a
        // constraint; the index form covers a database that rebuilt it as one.
        format!("ALTER TABLE \"ci_secrets\" DROP CONSTRAINT IF EXISTS \"{OLD_UNIQUE_INDEX}\""),
        format!("DROP INDEX IF EXISTS \"{OLD_UNIQUE_INDEX}\""),
    ] {
        execute(manager, &statement).await?;
    }
    Ok(())
}

/// Every step checks the live schema first: MySQL commits each DDL statement
/// on its own, and a re-run after a crash must pick up where it stopped.
///
/// The new unique index is created before the old one is dropped. InnoDB needs
/// an index leading with `repo_id` for the repository foreign key, and between
/// the two steps the old one is the only such index.
async fn up_mysql(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    if !manager.has_column("ci_secrets", "environment_id").await? {
        execute(
            manager,
            "ALTER TABLE `ci_secrets` ADD COLUMN `environment_id` BIGINT NULL",
        )
        .await?;
    }
    if lookup()
        .mysql_foreign_key(manager, "ci_secrets", "environment_id", "ci_environments")
        .await?
        .is_none()
    {
        execute(
            manager,
            &format!(
                "ALTER TABLE `ci_secrets` ADD CONSTRAINT `{ENVIRONMENT_FOREIGN_KEY}` \
                 FOREIGN KEY (`environment_id`) REFERENCES `ci_environments` (`id`) \
                 ON DELETE CASCADE"
            ),
        )
        .await?;
    }
    if !manager.has_column("ci_secrets", "scope_key").await? {
        execute(
            manager,
            "ALTER TABLE `ci_secrets` ADD COLUMN `scope_key` BIGINT \
             GENERATED ALWAYS AS (COALESCE(`environment_id`, 0)) VIRTUAL",
        )
        .await?;
    }
    if !manager.has_index("ci_secrets", ENVIRONMENT_INDEX).await? {
        execute(
            manager,
            &format!("CREATE INDEX `{ENVIRONMENT_INDEX}` ON `ci_secrets` (`environment_id`)"),
        )
        .await?;
    }
    if !manager.has_index("ci_secrets", NEW_UNIQUE_INDEX).await? {
        execute(
            manager,
            &format!(
                "CREATE UNIQUE INDEX `{NEW_UNIQUE_INDEX}` ON `ci_secrets` \
                 (`repo_id`, `scope_key`, `name`)"
            ),
        )
        .await?;
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

async fn down_postgres(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    for statement in [
        format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS \"{OLD_UNIQUE_INDEX}\" ON \"ci_secrets\" \
             (\"repo_id\", \"name\")"
        ),
        format!("DROP INDEX IF EXISTS \"{NEW_UNIQUE_INDEX}\""),
        format!("DROP INDEX IF EXISTS \"{ENVIRONMENT_INDEX}\""),
        "ALTER TABLE \"ci_secrets\" DROP COLUMN IF EXISTS \"scope_key\"".to_string(),
        "ALTER TABLE \"ci_secrets\" DROP COLUMN IF EXISTS \"environment_id\"".to_string(),
    ] {
        execute(manager, &statement).await?;
    }
    Ok(())
}

/// The mirror of [`up_mysql`], in the order InnoDB allows: the foreign key
/// before the index it lives on, and the old unique index back before the new
/// one goes.
async fn down_mysql(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    if let Some(existing) = lookup()
        .mysql_foreign_key(manager, "ci_secrets", "environment_id", "ci_environments")
        .await?
    {
        execute(
            manager,
            &format!("ALTER TABLE `ci_secrets` DROP FOREIGN KEY `{existing}`"),
        )
        .await?;
    }
    if !manager.has_index("ci_secrets", OLD_UNIQUE_INDEX).await? {
        execute(
            manager,
            &format!(
                "CREATE UNIQUE INDEX `{OLD_UNIQUE_INDEX}` ON `ci_secrets` (`repo_id`, `name`)"
            ),
        )
        .await?;
    }
    for index in [NEW_UNIQUE_INDEX, ENVIRONMENT_INDEX] {
        if manager.has_index("ci_secrets", index).await? {
            execute(
                manager,
                &format!("ALTER TABLE `ci_secrets` DROP INDEX `{index}`"),
            )
            .await?;
        }
    }
    for column in ["scope_key", "environment_id"] {
        if manager.has_column("ci_secrets", column).await? {
            execute(
                manager,
                &format!("ALTER TABLE `ci_secrets` DROP COLUMN `{column}`"),
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "m20261014_000002_ci_secret_environments_tests.rs"]
mod tests;
