//! One owner name, one holder — whatever the case it is spelled in.
//!
//! `m20261009_000001_owner_names_unique` made the database refuse a second
//! holder of a name, but compared names the way each backend's own unique key
//! does: case-sensitively on SQLite and PostgreSQL, case-insensitively under
//! MySQL's default collation. So on two of the three backends `zubrenok`
//! registered next to `Zubrenok` — and the two are one name to everybody who
//! reads a link, and to every stock client that lowercases what it is given.
//! Docker refuses an upper-case letter anywhere in an image path, so
//! `docker pull host/zubrenok/app` is the only spelling that reaches the
//! registry at all, and it reached whoever had registered the lower-case twin
//! (card_2095e88c98f1).
//!
//! `owner_names.name_key` is the name folded to lower case, and a unique index
//! on it is the rule. Owner names are ASCII by construction
//! (`validate_username_shape`: letters, digits, `-` and `_`), so `LOWER()` folds
//! them identically on all three backends and in Rust's `to_ascii_lowercase`.
//! The triggers that already keep `owner_names` in step with `users` and
//! `organizations` write the key in the same statement as the name.
//!
//! ## Names two holders already share
//!
//! Pairs that differ only in case exist on upgraded SQLite and PostgreSQL
//! instances, and the upgrade does not refuse to run over them — which of the
//! two to rename is the operator's call, as it was for the account/organization
//! collisions before. The first holder keeps the key (accounts before
//! organizations, then the older row), every later one keeps its name with no
//! key, and startup names them (`report_owner_names_that_differ_only_in_case`).
//! A keyless row is only ever given its key by renaming it: an unrelated update
//! of the account must not trip the unique index and take the account down
//! with it, which is why every trigger below writes the key only when the name
//! itself changed.

use std::collections::HashSet;

use sea_orm::{ConnectionTrait, DatabaseBackend, TransactionTrait};
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261014_000001_owner_names_fold_case"
    }
}

const INDEX: &str = "uq_owner_names_name_key";

/// The two tables that hold owner names, the column each keeps it in, and the
/// `kind` its rows carry in `owner_names` — as in `m20261009_000001`.
const HOLDERS: [(&str, &str, &str); 2] = [
    ("users", "username", "user"),
    ("organizations", "name", "org"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("owner_names", "name_key").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("owner_names"))
                        .add_column(
                            ColumnDef::new(Alias::new("name_key"))
                                .string_len(255)
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        backfill(manager).await?;

        // MySQL has no `CREATE INDEX IF NOT EXISTS`: ask first, so a re-run
        // after a partial apply is a no-op instead of a duplicate-key error.
        if !manager.has_index("owner_names", INDEX).await? {
            manager
                .create_index(
                    Index::create()
                        .name(INDEX)
                        .table(Alias::new("owner_names"))
                        .col(Alias::new("name_key"))
                        .unique()
                        .to_owned(),
                )
                .await?;
        }

        let backend = manager.get_database_backend();
        replace_triggers(
            manager,
            HOLDERS
                .iter()
                .flat_map(|&(table, column, kind)| folding_triggers(backend, table, column, kind))
                .collect(),
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        replace_triggers(
            manager,
            HOLDERS
                .iter()
                .flat_map(|&(table, column, kind)| exact_triggers(backend, table, column, kind))
                .collect(),
        )
        .await?;
        if manager.has_index("owner_names", INDEX).await? {
            manager
                .drop_index(
                    Index::drop()
                        .name(INDEX)
                        .table(Alias::new("owner_names"))
                        .to_owned(),
                )
                .await?;
        }
        if manager.has_column("owner_names", "name_key").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("owner_names"))
                        .drop_column(Alias::new("name_key"))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

/// Run trigger DDL — unprepared, because MySQL refuses `CREATE TRIGGER` over the
/// prepared statement protocol (1295).
///
/// On SQLite every statement goes through one transaction, and so through one
/// connection. Migrations run over the whole pool, and a connection whose
/// cached schema predates another connection's `DROP TRIGGER` still believes
/// the trigger is there: `CREATE TRIGGER` on it answers "already exists" for a
/// trigger that was just dropped. One connection sees its own drop, and the
/// replacement lands whole or not at all.
async fn replace_triggers(
    manager: &SchemaManager<'_>,
    statements: Vec<String>,
) -> Result<(), DbErr> {
    let db = manager.get_connection();
    if manager.get_database_backend() != DatabaseBackend::Sqlite {
        for statement in &statements {
            db.execute_unprepared(statement).await?;
        }
        return Ok(());
    }
    let transaction = db.begin().await?;
    for statement in &statements {
        transaction.execute_unprepared(statement).await?;
    }
    transaction.commit().await
}

/// Give every row without a key its folded name, unless an earlier holder
/// already has that key.
///
/// Rows are read in the order the rule settles a shared name in — accounts
/// before organizations, then the older row — and the keys already written are
/// taken first, so a re-run after a partial apply settles every name exactly as
/// the first run would have.
async fn backfill(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = manager.get_database_backend();

    let keyed = Query::select()
        .column(Alias::new("name_key"))
        .from(Alias::new("owner_names"))
        .and_where(Expr::col(Alias::new("name_key")).is_not_null())
        .to_owned();
    let mut taken: HashSet<String> = HashSet::new();
    for row in db.query_all(backend.build(&keyed)).await? {
        taken.insert(row.try_get::<String>("", "name_key")?);
    }

    let unkeyed = Query::select()
        .columns([
            Alias::new("name"),
            Alias::new("kind"),
            Alias::new("owner_id"),
        ])
        .from(Alias::new("owner_names"))
        .and_where(Expr::col(Alias::new("name_key")).is_null())
        .order_by_expr(
            Expr::case(Expr::col(Alias::new("kind")).eq("user"), 0)
                .finally(1)
                .into(),
            Order::Asc,
        )
        .order_by(Alias::new("owner_id"), Order::Asc)
        .to_owned();
    for row in db.query_all(backend.build(&unkeyed)).await? {
        let name: String = row.try_get("", "name")?;
        let kind: String = row.try_get("", "kind")?;
        let owner_id: i64 = row.try_get("", "owner_id")?;
        let key = name.to_ascii_lowercase();
        if !taken.insert(key.clone()) {
            continue;
        }
        let update = Query::update()
            .table(Alias::new("owner_names"))
            .value(Alias::new("name_key"), key)
            .and_where(Expr::col(Alias::new("kind")).eq(kind))
            .and_where(Expr::col(Alias::new("owner_id")).eq(owner_id))
            .to_owned();
        db.execute(backend.build(&update)).await?;
    }
    Ok(())
}

/// The triggers that keep `owner_names` — key included — in step with `table`.
///
/// Each writes the key only when the name itself changed: see the module
/// header for why an unrelated update must not touch a keyless row.
fn folding_triggers(
    backend: DatabaseBackend,
    table: &str,
    column: &str,
    kind: &str,
) -> Vec<String> {
    match backend {
        DatabaseBackend::Sqlite => vec![
            format!("DROP TRIGGER IF EXISTS owner_names_{table}_insert"),
            format!(
                "CREATE TRIGGER owner_names_{table}_insert AFTER INSERT ON {table} \
                 BEGIN INSERT INTO owner_names (name, name_key, kind, owner_id) \
                 VALUES (new.{column}, lower(new.{column}), '{kind}', new.id); END"
            ),
            format!("DROP TRIGGER IF EXISTS owner_names_{table}_update"),
            format!(
                "CREATE TRIGGER owner_names_{table}_update \
                 AFTER UPDATE OF {column} ON {table} WHEN new.{column} <> old.{column} \
                 BEGIN UPDATE owner_names SET name = new.{column}, name_key = lower(new.{column}) \
                 WHERE kind = '{kind}' AND owner_id = new.id; END"
            ),
        ],
        DatabaseBackend::Postgres => vec![format!(
            "CREATE OR REPLACE FUNCTION owner_names_{table}_sync() RETURNS trigger AS $$ \
             BEGIN \
               IF TG_OP = 'INSERT' THEN \
                 INSERT INTO owner_names (name, name_key, kind, owner_id) \
                 VALUES (NEW.{column}, lower(NEW.{column}), '{kind}', NEW.id); \
               ELSIF TG_OP = 'UPDATE' THEN \
                 IF NEW.{column} <> OLD.{column} THEN \
                   UPDATE owner_names SET name = NEW.{column}, name_key = lower(NEW.{column}) \
                   WHERE kind = '{kind}' AND owner_id = NEW.id; \
                 END IF; \
               ELSE \
                 DELETE FROM owner_names WHERE kind = '{kind}' AND owner_id = OLD.id; \
               END IF; \
               RETURN NULL; \
             END $$ LANGUAGE plpgsql"
        )],
        // MySQL's default collation calls `Zubrenok` and `zubrenok` equal, so
        // "did the name change" is asked of the bytes: a rename that changes
        // only the case must still reach `owner_names`.
        DatabaseBackend::MySql => vec![
            format!("DROP TRIGGER IF EXISTS owner_names_{table}_insert"),
            format!(
                "CREATE TRIGGER owner_names_{table}_insert AFTER INSERT ON {table} FOR EACH ROW \
                 INSERT INTO owner_names (name, name_key, kind, owner_id) \
                 VALUES (NEW.{column}, LOWER(NEW.{column}), '{kind}', NEW.id)"
            ),
            format!("DROP TRIGGER IF EXISTS owner_names_{table}_update"),
            format!(
                "CREATE TRIGGER owner_names_{table}_update AFTER UPDATE ON {table} FOR EACH ROW \
                 UPDATE owner_names SET name = NEW.{column}, name_key = LOWER(NEW.{column}) \
                 WHERE kind = '{kind}' AND owner_id = NEW.id \
                 AND CAST(NEW.{column} AS BINARY) <> CAST(OLD.{column} AS BINARY)"
            ),
        ],
    }
}

/// The triggers `m20261009_000001` installed, restored by `down`.
fn exact_triggers(backend: DatabaseBackend, table: &str, column: &str, kind: &str) -> Vec<String> {
    match backend {
        DatabaseBackend::Sqlite => vec![
            format!("DROP TRIGGER IF EXISTS owner_names_{table}_insert"),
            format!(
                "CREATE TRIGGER owner_names_{table}_insert AFTER INSERT ON {table} \
                 BEGIN INSERT INTO owner_names (name, kind, owner_id) \
                 VALUES (new.{column}, '{kind}', new.id); END"
            ),
            format!("DROP TRIGGER IF EXISTS owner_names_{table}_update"),
            format!(
                "CREATE TRIGGER owner_names_{table}_update \
                 AFTER UPDATE OF {column} ON {table} WHEN new.{column} <> old.{column} \
                 BEGIN UPDATE owner_names SET name = new.{column} \
                 WHERE kind = '{kind}' AND owner_id = new.id; END"
            ),
        ],
        DatabaseBackend::Postgres => vec![format!(
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
        )],
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
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{Database, DbBackend, DbConn, Statement};

    /// The state an upgraded SQLite instance is in: `owner_names` as
    /// `m20261009_000001` left it, with its triggers, holding `Acme` and `acme`
    /// as two names — and an organization `ACME` that took a third spelling.
    async fn upgraded_database_with_a_shared_name() -> DbConn {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL UNIQUE, \
               display_name TEXT); \
             CREATE TABLE organizations (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE); \
             CREATE TABLE owner_names (name VARCHAR(255) NOT NULL PRIMARY KEY, \
               kind VARCHAR(8) NOT NULL, owner_id BIGINT NOT NULL);",
        )
        .await
        .unwrap();
        for (table, column, kind) in HOLDERS {
            for statement in exact_triggers(DbBackend::Sqlite, table, column, kind) {
                db.execute_unprepared(&statement).await.unwrap();
            }
        }
        db.execute_unprepared(
            "INSERT INTO organizations (id, name) VALUES (1, 'ACME'); \
             INSERT INTO users (id, username) VALUES (2, 'acme'), (1, 'Acme'), (3, 'solo');",
        )
        .await
        .unwrap();
        db
    }

    async fn keys(db: &DbConn) -> Vec<(String, Option<String>)> {
        db.query_all(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name, name_key FROM owner_names ORDER BY name".to_string(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.try_get::<String>("", "name").unwrap(),
                row.try_get::<Option<String>>("", "name_key").unwrap(),
            )
        })
        .collect()
    }

    /// The account keeps the shared name over the organization, the older
    /// account over the younger, and nobody's row is lost.
    #[tokio::test]
    async fn a_shared_name_goes_to_the_older_account_and_the_rest_keep_no_key() {
        let db = upgraded_database_with_a_shared_name().await;

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        assert_eq!(
            keys(&db).await,
            vec![
                ("ACME".to_string(), None),
                ("Acme".to_string(), Some("acme".to_string())),
                ("acme".to_string(), None),
                ("solo".to_string(), Some("solo".to_string())),
            ]
        );
        // A re-run after a partial apply settles every name the same way.
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        assert_eq!(keys(&db).await.len(), 4);
    }

    /// The point of the migration: a new holder in another case is refused by
    /// the database itself, whichever table it lands in.
    #[tokio::test]
    async fn a_new_name_that_differs_only_in_case_is_refused() {
        let db = upgraded_database_with_a_shared_name().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        for statement in [
            "INSERT INTO users (id, username) VALUES (10, 'SOLO')",
            "INSERT INTO organizations (id, name) VALUES (10, 'Solo')",
            "INSERT INTO users (id, username) VALUES (11, 'aCmE')",
        ] {
            let refused = db.execute_unprepared(statement).await.expect_err(statement);
            assert!(
                refused.to_string().contains("UNIQUE"),
                "{statement}: {refused}"
            );
        }
        db.execute_unprepared("INSERT INTO users (id, username) VALUES (12, 'fresh')")
            .await
            .expect("a name nobody holds in any case is free");
        assert!(keys(&db)
            .await
            .contains(&("fresh".to_string(), Some("fresh".to_string()))));
    }

    /// A keyless row must survive every update that is not a rename — the
    /// account keeps working — and gets its key the moment it is renamed to a
    /// name nobody holds.
    #[tokio::test]
    async fn a_keyless_name_is_left_alone_until_it_is_renamed() {
        let db = upgraded_database_with_a_shared_name().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        db.execute_unprepared("UPDATE users SET display_name = 'Second' WHERE id = 2")
            .await
            .expect("an unrelated update of a keyless account must not trip the index");

        let refused = db
            .execute_unprepared("UPDATE users SET username = 'ACME' WHERE id = 2")
            .await
            .expect_err("a rename into a spelling of a held name is refused");
        assert!(refused.to_string().contains("UNIQUE"), "{refused}");

        db.execute_unprepared("UPDATE users SET username = 'acme2' WHERE id = 2")
            .await
            .expect("a rename to a free name");
        assert!(keys(&db)
            .await
            .contains(&("acme2".to_string(), Some("acme2".to_string()))));
    }

    /// An owner may change the case of its own name: the key it already holds
    /// is its own.
    #[tokio::test]
    async fn an_owner_may_change_only_the_case_of_its_own_name() {
        let db = upgraded_database_with_a_shared_name().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        db.execute_unprepared("UPDATE users SET username = 'Solo' WHERE id = 3")
            .await
            .expect("a case-only rename of one's own name");
        assert!(keys(&db)
            .await
            .contains(&("Solo".to_string(), Some("solo".to_string()))));
    }

    /// `down` gives back exactly what `m20261009_000001` left.
    #[tokio::test]
    async fn down_restores_the_exact_comparison() {
        let db = upgraded_database_with_a_shared_name().await;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();

        assert!(!manager.has_column("owner_names", "name_key").await.unwrap());
        db.execute_unprepared("INSERT INTO users (id, username) VALUES (10, 'SOLO')")
            .await
            .expect("case-sensitive again after down");
    }
}
