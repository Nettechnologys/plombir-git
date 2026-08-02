//! An empty string is a value, and `UNIQUE` lets exactly one row hold it.
//!
//! `card_0a08de4d6707` closed the SSO path in code: a provider response missing
//! `email` / `sub` / `id` no longer collapses to `""` and no longer reaches
//! `find_by_email` / `find_by_provider_and_uid` as an ordinary lookup key. That
//! gate is a property of one entry point, though, and the schema underneath it
//! still treats `''` as an ordinary value of a unique key:
//!
//! * `users.email` is `UNIQUE` — and `''` satisfies it.
//! * `oauth_accounts (provider, provider_user_id)` is `UNIQUE` — same.
//!
//! One such row restores the original defect whole: the next caller that
//! searches for `''` finds it and is handed somebody else's account. The gate in
//! code cannot see what is already in the database, and it does not cover a row
//! written by an older build, a restored backup, or a future call site that
//! reaches `create_user` without passing through it. So the rule is moved to
//! where the data lives.
//!
//! Two more columns are guarded here, because they are the same defect on the
//! same tables rather than a separate idea:
//!
//! * `users.username` — `UNIQUE`, and `find_by_username` is precisely how the
//!   LDAP and SSO paths decide whether an identity already belongs to someone.
//! * `users.ldap_uid` — half of `UNIQUE (ldap_provider_id, ldap_uid)`, the
//!   directory's stable identity key, read by `find_by_ldap_provider_and_uid`.
//!   It is nullable, and `NULL` stays allowed: "this account has no directory
//!   identity" is a real state, distinct from "the directory returned nothing".
//!
//! Blank means blank *after trimming*: a key of `" "` identifies a person no
//! better than `""` does, and the code-side rules (`SsoIdentity`,
//! `validate_username`, `valid_email`) all trim before judging.
//!
//! ## Per-backend notes
//!
//! **PostgreSQL / MySQL** — a real `CHECK` constraint, added by name so a re-run
//! is a no-op. Neither backend has `ADD CONSTRAINT IF NOT EXISTS`, so the
//! catalogue is consulted first. On MySQL the constraint is only *enforced* from
//! 8.0.16 (and MariaDB 10.2); older servers parse and ignore it, which leaves
//! them exactly where they are today rather than making anything worse.
//!
//! **SQLite** — cannot add a `CHECK` to an existing table at all, and the
//! documented alternative, a full table rebuild, is the wrong trade here:
//! `users` is the parent of a large number of `ON DELETE CASCADE` children (see
//! `m20260730_000001_repositories_namespace_unique` for how careful that
//! procedure has to be), and none of that risk is worth taking for a predicate.
//! `BEFORE INSERT` / `BEFORE UPDATE OF` triggers that `RAISE(ABORT, ...)` enforce
//! the same rule without touching a row. Anything that ever *does* rebuild
//! `users` or `oauth_accounts` on SQLite must recreate these triggers, the way
//! the repositories rebuild recreates the FTS ones.
//!
//! A rejection here is a plain error on every backend, never a UNIQUE violation,
//! so the race-recovery paths in `oauth_account_ops::upsert` and
//! `create_or_resolve_ldap_identity` (which resolve *only*
//! `is_unique_violation`) keep reporting it instead of retrying into a wrong
//! account.

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260802_000003_identity_keys_not_blank"
    }
}

/// Rows named in the refusal message before it gives up listing them.
const SAMPLE_LIMIT: usize = 20;

/// One identity key that must never hold a present-but-blank value.
struct Guard {
    table: &'static str,
    column: &'static str,
    /// `true` when `NULL` is a legitimate value for this column, meaning "no
    /// such identity" — which the constraint must keep allowing.
    nullable: bool,
    /// The `CHECK` constraint's name on PostgreSQL / MySQL, and the stem of the
    /// two SQLite triggers standing in for it.
    constraint: &'static str,
}

impl Guard {
    /// True exactly for a row whose key is present but blank.
    ///
    /// `NULL` is not selected by this on any backend, which is what the nullable
    /// column wants and is harmless for the `NOT NULL` ones.
    fn blank_rows(&self, prefix: &str) -> String {
        format!("trim({prefix}{}) = ''", self.column)
    }

    /// The `CHECK` body — the negation, written so `NULL` passes.
    fn check_body(&self) -> String {
        let column = self.column;
        if self.nullable {
            format!("{column} IS NULL OR trim({column}) <> ''")
        } else {
            format!("trim({column}) <> ''")
        }
    }

    /// The message an abort carries. Kept free of quotes: it is embedded in a
    /// single-quoted SQL literal inside the SQLite triggers.
    fn message(&self) -> String {
        format!(
            "{}.{} is an identity key and must not be blank",
            self.table, self.column
        )
    }
}

const GUARDS: [Guard; 4] = [
    Guard {
        table: "users",
        column: "email",
        nullable: false,
        constraint: "ck_users_email_not_blank",
    },
    Guard {
        table: "users",
        column: "username",
        nullable: false,
        constraint: "ck_users_username_not_blank",
    },
    Guard {
        table: "users",
        column: "ldap_uid",
        nullable: true,
        constraint: "ck_users_ldap_uid_not_blank",
    },
    Guard {
        table: "oauth_accounts",
        column: "provider_user_id",
        nullable: false,
        constraint: "ck_oauth_accounts_provider_user_id_not_blank",
    },
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        assert_no_blank_keys(manager).await?;

        for guard in &GUARDS {
            if !present(manager, guard).await? {
                continue;
            }
            match manager.get_database_backend() {
                DatabaseBackend::Sqlite => sqlite_add(manager, guard).await?,
                DatabaseBackend::Postgres => postgres_add(manager, guard).await?,
                DatabaseBackend::MySql => mysql_add(manager, guard).await?,
            }
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for guard in &GUARDS {
            if !present(manager, guard).await? {
                continue;
            }
            match manager.get_database_backend() {
                DatabaseBackend::Sqlite => sqlite_drop(manager, guard).await?,
                DatabaseBackend::Postgres => postgres_drop(manager, guard).await?,
                DatabaseBackend::MySql => mysql_drop(manager, guard).await?,
            }
        }

        Ok(())
    }
}

/// Whether this guard's table and column exist on the live schema.
async fn present(manager: &SchemaManager<'_>, guard: &Guard) -> Result<bool, DbErr> {
    Ok(manager.has_table(guard.table).await?
        && manager.has_column(guard.table, guard.column).await?)
}

// ── Preflight ───────────────────────────────────────────────────────────────

/// Refuse the upgrade — before writing anything — if a row already holds a blank
/// identity key.
///
/// Every backend would otherwise answer with its own constraint-violation text,
/// which names the constraint and not one offending row: an operator would be
/// left to work out both what the rule is and which of their users breaks it,
/// mid-upgrade. All four guards are checked before failing, so one run reports
/// the whole repair job rather than one column of it.
async fn assert_no_blank_keys(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = manager.get_database_backend();
    let mut defects: Vec<String> = Vec::new();

    for guard in &GUARDS {
        if !present(manager, guard).await? {
            continue;
        }

        let rows = db
            .query_all(Statement::from_string(
                backend,
                format!(
                    "SELECT id FROM {table} WHERE {blank} ORDER BY id LIMIT {limit}",
                    table = guard.table,
                    blank = guard.blank_rows(""),
                    limit = SAMPLE_LIMIT + 1,
                ),
            ))
            .await?;

        if rows.is_empty() {
            continue;
        }

        let mut ids: Vec<String> = Vec::with_capacity(rows.len());
        for row in rows.iter().take(SAMPLE_LIMIT) {
            ids.push(row.try_get::<i64>("", "id")?.to_string());
        }
        let more = if rows.len() > SAMPLE_LIMIT {
            ", ..."
        } else {
            ""
        };

        defects.push(format!(
            "{table}.{column} is blank on row id(s) {ids}{more} \
             (`SELECT id FROM {table} WHERE {blank}`)",
            table = guard.table,
            column = guard.column,
            ids = ids.join(", "),
            blank = guard.blank_rows(""),
        ));
    }

    if defects.is_empty() {
        return Ok(());
    }

    Err(DbErr::Migration(format!(
        "m20260802_000003: refusing to make the identity keys non-blank — {count} of them are \
         already blank in this database: {defects}. A blank identity key is found by a lookup \
         for the empty string, so the next login whose provider or directory returned no value \
         is handed that account (card_0a08de4d6707). Repair or remove those rows — give the \
         account its real address / username / directory uid, or delete it — and run the \
         upgrade again.",
        count = defects.len(),
        defects = defects.join("; "),
    )))
}

// ── SQLite ──────────────────────────────────────────────────────────────────

/// SQLite has no `ALTER TABLE ... ADD CONSTRAINT`, so the rule lives in a pair
/// of triggers instead. `BEFORE UPDATE OF <column>` keeps the update-side check
/// off every write that does not touch the key.
async fn sqlite_add(manager: &SchemaManager<'_>, guard: &Guard) -> Result<(), DbErr> {
    let Guard {
        table,
        column,
        constraint,
        ..
    } = guard;
    let blank = guard.blank_rows("NEW.");
    let message = guard.message();

    manager
        .get_connection()
        .execute_unprepared(&format!(
            r#"
            CREATE TRIGGER IF NOT EXISTS "{constraint}_insert"
            BEFORE INSERT ON "{table}"
            FOR EACH ROW WHEN {blank}
            BEGIN
                SELECT RAISE(ABORT, '{message}');
            END;

            CREATE TRIGGER IF NOT EXISTS "{constraint}_update"
            BEFORE UPDATE OF "{column}" ON "{table}"
            FOR EACH ROW WHEN {blank}
            BEGIN
                SELECT RAISE(ABORT, '{message}');
            END;
            "#
        ))
        .await?;

    Ok(())
}

async fn sqlite_drop(manager: &SchemaManager<'_>, guard: &Guard) -> Result<(), DbErr> {
    let constraint = guard.constraint;

    manager
        .get_connection()
        .execute_unprepared(&format!(
            r#"
            DROP TRIGGER IF EXISTS "{constraint}_insert";
            DROP TRIGGER IF EXISTS "{constraint}_update";
            "#
        ))
        .await?;

    Ok(())
}

// ── PostgreSQL ──────────────────────────────────────────────────────────────

async fn postgres_add(manager: &SchemaManager<'_>, guard: &Guard) -> Result<(), DbErr> {
    if postgres_has_constraint(manager, guard).await? {
        return Ok(());
    }

    manager
        .get_connection()
        .execute_unprepared(&format!(
            "ALTER TABLE {table} ADD CONSTRAINT {constraint} CHECK ({body})",
            table = guard.table,
            constraint = guard.constraint,
            body = guard.check_body(),
        ))
        .await?;

    Ok(())
}

async fn postgres_drop(manager: &SchemaManager<'_>, guard: &Guard) -> Result<(), DbErr> {
    manager
        .get_connection()
        .execute_unprepared(&format!(
            "ALTER TABLE {table} DROP CONSTRAINT IF EXISTS {constraint}",
            table = guard.table,
            constraint = guard.constraint,
        ))
        .await?;

    Ok(())
}

/// PostgreSQL has no `ADD CONSTRAINT IF NOT EXISTS`; ask the catalogue instead.
async fn postgres_has_constraint(
    manager: &SchemaManager<'_>,
    guard: &Guard,
) -> Result<bool, DbErr> {
    let row = manager
        .get_connection()
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "SELECT count(*) AS n FROM pg_constraint c \
                 JOIN pg_class t ON t.oid = c.conrelid \
                 WHERE c.conname = '{constraint}' AND t.relname = '{table}'",
                constraint = guard.constraint,
                table = guard.table,
            ),
        ))
        .await?;

    match row {
        Some(row) => Ok(row.try_get::<i64>("", "n")? > 0),
        None => Ok(false),
    }
}

// ── MySQL ───────────────────────────────────────────────────────────────────

async fn mysql_add(manager: &SchemaManager<'_>, guard: &Guard) -> Result<(), DbErr> {
    if mysql_has_constraint(manager, guard).await? {
        return Ok(());
    }

    manager
        .get_connection()
        .execute_unprepared(&format!(
            "ALTER TABLE {table} ADD CONSTRAINT {constraint} CHECK ({body})",
            table = guard.table,
            constraint = guard.constraint,
            body = guard.check_body(),
        ))
        .await?;

    Ok(())
}

/// `DROP CONSTRAINT` rather than MySQL's older `DROP CHECK`: it is the spelling
/// both MariaDB 10.2+ and MySQL 8.0.19+ accept, and a server too old to have it
/// is also too old to have enforced the constraint in the first place.
async fn mysql_drop(manager: &SchemaManager<'_>, guard: &Guard) -> Result<(), DbErr> {
    if !mysql_has_constraint(manager, guard).await? {
        return Ok(());
    }

    manager
        .get_connection()
        .execute_unprepared(&format!(
            "ALTER TABLE {table} DROP CONSTRAINT {constraint}",
            table = guard.table,
            constraint = guard.constraint,
        ))
        .await?;

    Ok(())
}

async fn mysql_has_constraint(manager: &SchemaManager<'_>, guard: &Guard) -> Result<bool, DbErr> {
    let row = manager
        .get_connection()
        .query_one(Statement::from_string(
            DatabaseBackend::MySql,
            format!(
                "SELECT count(*) AS n FROM information_schema.table_constraints \
                 WHERE constraint_schema = DATABASE() \
                 AND table_name = '{table}' AND constraint_name = '{constraint}'",
                table = guard.table,
                constraint = guard.constraint,
            ),
        ))
        .await?;

    match row {
        Some(row) => Ok(row.try_get::<i64>("", "n")? > 0),
        None => Ok(false),
    }
}
