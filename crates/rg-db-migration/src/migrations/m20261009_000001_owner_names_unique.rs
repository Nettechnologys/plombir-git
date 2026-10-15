//! One owner name, one holder — enforced by the database, across two tables.
//!
//! Accounts (`users.username`) and organizations (`organizations.name`) answer
//! to one URL segment, `/{owner}`. Each table is unique on its own column, but
//! nothing spanned the two: every door asked
//! `rg_core::namespace::owner_name_is_taken` first (card_4b0594a02218), and two
//! creates of one name in the two tables could both read "free" and both
//! insert — registration is anonymous and repeatable, so the window was a race
//! anybody could run (card_8f3f821705f2).
//!
//! `owner_names` is the constraint that spans them: `name` is its primary key,
//! and a row is written by the database itself, in the same statement as the
//! account or organization it names — `AFTER INSERT` / `UPDATE` / `DELETE`
//! triggers on both tables. A second holder of a name fails *its own insert*
//! with an ordinary unique violation, whichever door it came through: the
//! registration form, an organization, a bot, an SSO or LDAP account the
//! server names itself, or a row written by a future path nobody has written
//! yet. The pre-read stays as the friendly first answer; this is what makes it
//! true.
//!
//! The comparison is the backend's own for a unique key, exactly like the two
//! tables' own unique keys: case-sensitive on SQLite and PostgreSQL,
//! case-insensitive under MySQL's default collation.
//!
//! ## Names two holders already share
//!
//! Collisions from before the rule exist on upgraded instances, and startup
//! already names them (`report_names_held_by_an_account_and_an_organization`).
//! The backfill does not refuse to run over them — the operator decides which
//! of the two to rename, not the upgrade — so the account keeps the name (it is
//! the one `/{name}` resolves to) and the organization is simply not entered.

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261009_000001_owner_names_unique"
    }
}

/// The two tables that hold owner names, the column each keeps it in, and the
/// `kind` its rows carry in `owner_names`.
const HOLDERS: [(&str, &str, &str); 2] = [
    ("users", "username", "user"),
    ("organizations", "name", "org"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("owner_names"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("name"))
                            .string_len(255)
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("kind")).string_len(8).not_null())
                    .col(
                        ColumnDef::new(Alias::new("owner_id"))
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        // MySQL has no `CREATE INDEX IF NOT EXISTS`: ask first, so a re-run
        // after a partial apply is a no-op instead of a duplicate-key error.
        if !manager
            .has_index("owner_names", "idx_owner_names_kind_owner")
            .await?
        {
            manager
                .create_index(
                    Index::create()
                        .name("idx_owner_names_kind_owner")
                        .table(Alias::new("owner_names"))
                        .col(Alias::new("kind"))
                        .col(Alias::new("owner_id"))
                        .to_owned(),
                )
                .await?;
        }

        let db = manager.get_connection();
        let backend = manager.get_database_backend();
        // Accounts first: where a name is held twice, the account is the
        // holder `/{name}` reaches.
        for (table, column, kind) in HOLDERS {
            db.execute(Statement::from_string(
                backend,
                format!(
                    "INSERT INTO owner_names (name, kind, owner_id) \
                     SELECT h.{column}, '{kind}', h.id FROM {table} h \
                     WHERE NOT EXISTS (SELECT 1 FROM owner_names n WHERE n.name = h.{column})"
                ),
            ))
            .await?;
        }

        for (table, column, kind) in HOLDERS {
            // Unprepared: MySQL refuses `CREATE TRIGGER` over the prepared
            // statement protocol (1295).
            for statement in create_triggers(backend, table, column, kind) {
                db.execute_unprepared(&statement).await?;
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = manager.get_database_backend();
        for (table, _, _) in HOLDERS {
            for statement in drop_triggers(backend, table) {
                db.execute_unprepared(&statement).await?;
            }
        }
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("owner_names"))
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

/// The triggers that keep `owner_names` in step with `table`.
fn create_triggers(backend: DatabaseBackend, table: &str, column: &str, kind: &str) -> Vec<String> {
    match backend {
        DatabaseBackend::Sqlite => vec![
            format!(
                "CREATE TRIGGER IF NOT EXISTS owner_names_{table}_insert AFTER INSERT ON {table} \
                 BEGIN INSERT INTO owner_names (name, kind, owner_id) \
                 VALUES (new.{column}, '{kind}', new.id); END"
            ),
            format!(
                "CREATE TRIGGER IF NOT EXISTS owner_names_{table}_update \
                 AFTER UPDATE OF {column} ON {table} WHEN new.{column} <> old.{column} \
                 BEGIN UPDATE owner_names SET name = new.{column} \
                 WHERE kind = '{kind}' AND owner_id = new.id; END"
            ),
            format!(
                "CREATE TRIGGER IF NOT EXISTS owner_names_{table}_delete AFTER DELETE ON {table} \
                 BEGIN DELETE FROM owner_names WHERE kind = '{kind}' AND owner_id = old.id; END"
            ),
        ],
        DatabaseBackend::Postgres => vec![
            format!(
                "CREATE OR REPLACE FUNCTION owner_names_{table}_sync() RETURNS trigger AS $$ \
                 BEGIN \
                   IF TG_OP = 'INSERT' THEN \
                     INSERT INTO owner_names (name, kind, owner_id) \
                     VALUES (NEW.{column}, '{kind}', NEW.id); \
                   ELSIF TG_OP = 'UPDATE' THEN \
                     IF NEW.{column} <> OLD.{column} THEN \
                       UPDATE owner_names SET name = NEW.{column} \
                       WHERE kind = '{kind}' AND owner_id = NEW.id; \
                     END IF; \
                   ELSE \
                     DELETE FROM owner_names WHERE kind = '{kind}' AND owner_id = OLD.id; \
                   END IF; \
                   RETURN NULL; \
                 END $$ LANGUAGE plpgsql"
            ),
            format!("DROP TRIGGER IF EXISTS owner_names_{table} ON {table}"),
            format!(
                "CREATE TRIGGER owner_names_{table} AFTER INSERT OR UPDATE OR DELETE ON {table} \
                 FOR EACH ROW EXECUTE FUNCTION owner_names_{table}_sync()"
            ),
        ],
        DatabaseBackend::MySql => vec![
            format!("DROP TRIGGER IF EXISTS owner_names_{table}_insert"),
            format!(
                "CREATE TRIGGER owner_names_{table}_insert AFTER INSERT ON {table} FOR EACH ROW \
                 INSERT INTO owner_names (name, kind, owner_id) \
                 VALUES (NEW.{column}, '{kind}', NEW.id)"
            ),
            format!("DROP TRIGGER IF EXISTS owner_names_{table}_update"),
            format!(
                "CREATE TRIGGER owner_names_{table}_update AFTER UPDATE ON {table} FOR EACH ROW \
                 UPDATE owner_names SET name = NEW.{column} \
                 WHERE kind = '{kind}' AND owner_id = NEW.id AND name <> NEW.{column}"
            ),
            format!("DROP TRIGGER IF EXISTS owner_names_{table}_delete"),
            format!(
                "CREATE TRIGGER owner_names_{table}_delete AFTER DELETE ON {table} FOR EACH ROW \
                 DELETE FROM owner_names WHERE kind = '{kind}' AND owner_id = OLD.id"
            ),
        ],
    }
}

fn drop_triggers(backend: DatabaseBackend, table: &str) -> Vec<String> {
    match backend {
        DatabaseBackend::Sqlite | DatabaseBackend::MySql => ["insert", "update", "delete"]
            .iter()
            .map(|event| format!("DROP TRIGGER IF EXISTS owner_names_{table}_{event}"))
            .collect(),
        DatabaseBackend::Postgres => vec![
            format!("DROP TRIGGER IF EXISTS owner_names_{table} ON {table}"),
            format!("DROP FUNCTION IF EXISTS owner_names_{table}_sync()"),
        ],
    }
}
