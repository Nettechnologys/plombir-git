//! One repository name per namespace — whatever the case it is spelled in.
//!
//! `m20260730_000001_repositories_namespace_unique` made the database refuse a
//! second repository of one name in a namespace, but compared names the way
//! each backend's own unique key does: case-sensitively on SQLite and
//! PostgreSQL, case-insensitively under MySQL's default collation. So on two of
//! the three backends `payments` was created next to `Payments` — and the two
//! are one name to everybody who reads a link, and to every stock client that
//! lowercases what it is given: Docker refuses an upper-case letter anywhere in
//! an image path. `validate_repo_name` already refuses non-ASCII names for the
//! same reason, and any member allowed to create repositories in an
//! organization could make the twin (card_552648b36416).
//!
//! `repositories.name_key` is the name folded to lower case — a generated
//! column, so no write path can forget it and the entity does not carry it —
//! and a unique index on `(namespace_key, name_key)` is the rule. Repository
//! names are ASCII by construction (`validate_repo_name`), so `LOWER()` folds
//! them identically on all three backends and in Rust's `to_ascii_lowercase`.
//!
//! A generated column rather than a trigger-written one because of SQLite: it
//! cannot assign `new` in a BEFORE trigger, and an AFTER INSERT trigger that
//! writes the key back is an UPDATE, which fires `repos_fts_update` ahead of
//! `repos_fts_insert` and makes the second insert of the same FTS row fail the
//! repository's creation.
//!
//! ## Names two repositories of one namespace already share
//!
//! As with owner names (`m20261014_000001_owner_names_fold_case`), the upgrade
//! does not refuse to run over pairs that predate the rule: which one to rename
//! is the owner's call. The older repository keeps the key; the younger is
//! marked `name_case_exempt`, which makes its key `NULL`, and startup names it
//! (`report_repository_names_that_differ_only_in_case`). The mark is cleared by
//! the database the moment the repository is renamed or moved to another
//! namespace — the two changes that make it a new claim on a name — and never
//! by an unrelated update, which must not trip the index.

use std::collections::HashSet;

use sea_orm::{ConnectionTrait, DatabaseBackend, TransactionTrait};
use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261014_000004_repositories_name_fold_case"
    }
}

const INDEX: &str = "uq_repositories_namespace_name_key";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        if !manager
            .has_column("repositories", "name_case_exempt")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("repositories"))
                        .add_column(
                            ColumnDef::new(Alias::new("name_case_exempt"))
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        exempt_younger_twins(manager).await?;

        if !has_name_key(manager).await? {
            manager
                .get_connection()
                .execute_unprepared(&name_key_column(backend))
                .await?;
        }

        // MySQL has no `CREATE INDEX IF NOT EXISTS`: ask first, so a re-run
        // after a partial apply is a no-op instead of a duplicate-key error.
        if !manager.has_index("repositories", INDEX).await? {
            manager
                .create_index(
                    Index::create()
                        .name(INDEX)
                        .table(Alias::new("repositories"))
                        .col(Alias::new("namespace_key"))
                        .col(Alias::new("name_key"))
                        .unique()
                        .to_owned(),
                )
                .await?;
        }

        run_unprepared(manager, exemption_triggers(backend)).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_unprepared(manager, dropped_triggers(manager.get_database_backend())).await?;
        if manager.has_index("repositories", INDEX).await? {
            manager
                .drop_index(
                    Index::drop()
                        .name(INDEX)
                        .table(Alias::new("repositories"))
                        .to_owned(),
                )
                .await?;
        }
        // The generated column first: it reads the mark.
        for column in ["name_key", "name_case_exempt"] {
            let present = if column == "name_key" {
                has_name_key(manager).await?
            } else {
                manager.has_column("repositories", column).await?
            };
            if present {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new("repositories"))
                            .drop_column(Alias::new(column))
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

/// Whether `repositories.name_key` exists. Not `has_column` on SQLite: its
/// `pragma_table_info` leaves a virtual generated column out, so a re-run
/// would add the column twice and `down` would try to drop the mark from under
/// it. `pragma_table_xinfo` lists it.
async fn has_name_key(manager: &SchemaManager<'_>) -> Result<bool, DbErr> {
    if manager.get_database_backend() != DatabaseBackend::Sqlite {
        return manager.has_column("repositories", "name_key").await;
    }
    let found = manager
        .get_connection()
        .query_one(sea_orm::Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT 1 AS present FROM pragma_table_xinfo('repositories') WHERE name = 'name_key'"
                .to_string(),
        ))
        .await?;
    Ok(found.is_some())
}

/// `name_key`, spelled for each backend: `VIRTUAL` where `ALTER TABLE` cannot
/// add a stored generated column (SQLite) or where the namespace key beside it
/// is virtual too (MySQL), `STORED` on PostgreSQL, which has nothing else.
fn name_key_column(backend: DatabaseBackend) -> String {
    let fold = "CASE WHEN name_case_exempt THEN NULL ELSE LOWER(name) END";
    match backend {
        DatabaseBackend::Sqlite => format!(
            "ALTER TABLE repositories ADD COLUMN name_key TEXT GENERATED ALWAYS AS ({fold}) VIRTUAL"
        ),
        DatabaseBackend::Postgres => format!(
            "ALTER TABLE repositories ADD COLUMN name_key VARCHAR(255) \
             GENERATED ALWAYS AS ({fold}) STORED"
        ),
        DatabaseBackend::MySql => format!(
            "ALTER TABLE repositories ADD COLUMN name_key VARCHAR(255) \
             GENERATED ALWAYS AS ({fold}) VIRTUAL"
        ),
    }
}

/// Run trigger DDL — unprepared, because MySQL refuses `CREATE TRIGGER` over the
/// prepared statement protocol (1295). On SQLite through one transaction, so
/// through one connection: see `m20261014_000001_owner_names_fold_case`.
async fn run_unprepared(manager: &SchemaManager<'_>, statements: Vec<String>) -> Result<(), DbErr> {
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

/// Mark every live repository whose folded name an older live repository of
/// the same namespace already has.
///
/// A soft-deleted row occupies no namespace (`namespace_key` is `NULL`), so it
/// is never marked; restoring it next to a twin is then refused by the index
/// like any other collision. Rows read oldest first, and rows already marked
/// are left out of the names taken, so a re-run marks exactly what the first
/// run did.
async fn exempt_younger_twins(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = manager.get_database_backend();

    let rows = Query::select()
        .columns([
            Alias::new("id"),
            Alias::new("name"),
            Alias::new("owner_id"),
            Alias::new("org_id"),
            Alias::new("name_case_exempt"),
        ])
        .from(Alias::new("repositories"))
        .and_where(Expr::col(Alias::new("deleted_at")).is_null())
        .order_by(Alias::new("id"), Order::Asc)
        .to_owned();
    let mut taken: HashSet<(String, String)> = HashSet::new();
    for row in db.query_all(backend.build(&rows)).await? {
        if row.try_get::<bool>("", "name_case_exempt")? {
            continue;
        }
        let namespace = match row.try_get::<Option<i64>>("", "org_id")? {
            Some(org_id) => format!("o{org_id}"),
            None => format!("u{}", row.try_get::<i64>("", "owner_id")?),
        };
        let key = row.try_get::<String>("", "name")?.to_ascii_lowercase();
        if taken.insert((namespace, key)) {
            continue;
        }
        let mark = Query::update()
            .table(Alias::new("repositories"))
            .value(Alias::new("name_case_exempt"), true)
            .and_where(Expr::col(Alias::new("id")).eq(row.try_get::<i64>("", "id")?))
            .to_owned();
        db.execute(backend.build(&mark)).await?;
    }
    Ok(())
}

/// The triggers that clear the mark when a marked repository is renamed or
/// moved to another namespace — see the module header.
fn exemption_triggers(backend: DatabaseBackend) -> Vec<String> {
    let mut statements = dropped_triggers(backend);
    statements.extend(match backend {
        // An AFTER UPDATE writing the row it fired for is safe here, unlike on
        // insert: `repos_fts_update` deletes the row's FTS entry before it
        // inserts it again.
        DatabaseBackend::Sqlite => vec!["CREATE TRIGGER repositories_name_case_exempt_cleared \
             AFTER UPDATE OF name, owner_id, org_id ON repositories \
             WHEN new.name_case_exempt AND (new.name <> old.name \
               OR new.owner_id <> old.owner_id OR new.org_id IS NOT old.org_id) \
             BEGIN UPDATE repositories SET name_case_exempt = 0 WHERE id = new.id; END"
            .to_string()],
        DatabaseBackend::Postgres => vec![
            "CREATE OR REPLACE FUNCTION repositories_name_case_exempt_clear() \
             RETURNS trigger AS $$ \
             BEGIN \
               IF NEW.name_case_exempt AND (NEW.name IS DISTINCT FROM OLD.name \
                   OR NEW.owner_id IS DISTINCT FROM OLD.owner_id \
                   OR NEW.org_id IS DISTINCT FROM OLD.org_id) THEN \
                 NEW.name_case_exempt := FALSE; \
               END IF; \
               RETURN NEW; \
             END $$ LANGUAGE plpgsql"
                .to_string(),
            "CREATE TRIGGER repositories_name_case_exempt_cleared \
             BEFORE UPDATE OF name, owner_id, org_id ON repositories \
             FOR EACH ROW EXECUTE FUNCTION repositories_name_case_exempt_clear()"
                .to_string(),
        ],
        // MySQL's default collation calls `Payments` and `payments` equal, so
        // "did the name change" is asked of the bytes.
        DatabaseBackend::MySql => vec![
            "CREATE TRIGGER repositories_name_case_exempt_cleared BEFORE UPDATE ON repositories \
             FOR EACH ROW SET NEW.name_case_exempt = IF( \
               NEW.name_case_exempt AND (CAST(NEW.name AS BINARY) <> CAST(OLD.name AS BINARY) \
                 OR NEW.owner_id <> OLD.owner_id OR NOT (NEW.org_id <=> OLD.org_id)), \
               FALSE, NEW.name_case_exempt)"
                .to_string(),
        ],
    });
    statements
}

fn dropped_triggers(backend: DatabaseBackend) -> Vec<String> {
    match backend {
        DatabaseBackend::Sqlite | DatabaseBackend::MySql => {
            vec!["DROP TRIGGER IF EXISTS repositories_name_case_exempt_cleared".to_string()]
        }
        DatabaseBackend::Postgres => vec![
            "DROP TRIGGER IF EXISTS repositories_name_case_exempt_cleared ON repositories"
                .to_string(),
            "DROP FUNCTION IF EXISTS repositories_name_case_exempt_clear()".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{Database, DbBackend, DbConn, Statement};

    /// The state an upgraded SQLite instance is in: `repositories` with its
    /// generated namespace key and `UNIQUE (namespace_key, name)`, holding
    /// `Payments` and a younger `payments` in one account, the same spelling
    /// in another account, and a deleted `PAYMENTS`.
    async fn upgraded_database_with_twins() -> DbConn {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE repositories (id INTEGER PRIMARY KEY, owner_id BIGINT NOT NULL, \
               org_id BIGINT, name TEXT NOT NULL, description TEXT, deleted_at TEXT, \
               namespace_key TEXT GENERATED ALWAYS AS ( \
                 CASE WHEN deleted_at IS NOT NULL THEN NULL \
                      WHEN org_id IS NULL THEN 'u' || owner_id \
                      ELSE 'o' || org_id END) STORED, \
               UNIQUE (namespace_key, name)); \
             INSERT INTO repositories (id, owner_id, org_id, name) VALUES \
               (1, 1, NULL, 'Payments'), (2, 1, NULL, 'payments'), \
               (3, 2, NULL, 'payments'), (4, 1, 7, 'payments'); \
             INSERT INTO repositories (id, owner_id, org_id, name, deleted_at) VALUES \
               (5, 1, NULL, 'PAYMENTS', '2026-10-01');",
        )
        .await
        .unwrap();
        db
    }

    async fn keys(db: &DbConn) -> Vec<(i64, Option<String>)> {
        db.query_all(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT id, name_key FROM repositories ORDER BY id".to_string(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.try_get::<i64>("", "id").unwrap(),
                row.try_get::<Option<String>>("", "name_key").unwrap(),
            )
        })
        .collect()
    }

    fn key(value: &str) -> Option<String> {
        Some(value.to_string())
    }

    /// The older twin keeps the key; other namespaces and the deleted row are
    /// not twins of anything; a re-run settles every name the same way.
    #[tokio::test]
    async fn a_shared_name_goes_to_the_older_repository_and_the_twin_keeps_no_key() {
        let db = upgraded_database_with_twins().await;

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        let expected = vec![
            (1, key("payments")),
            (2, None),
            (3, key("payments")),
            (4, key("payments")),
            (5, key("payments")),
        ];
        assert_eq!(keys(&db).await, expected);
        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        assert_eq!(keys(&db).await, expected);
    }

    /// The point of the migration: a new repository, or a rename, into
    /// another case of a name its namespace holds is refused by the database
    /// itself — and only in that namespace.
    #[tokio::test]
    async fn a_new_name_that_differs_only_in_case_is_refused_in_its_namespace() {
        let db = upgraded_database_with_twins().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        for statement in [
            "INSERT INTO repositories (id, owner_id, name) VALUES (10, 2, 'PayMents')",
            "INSERT INTO repositories (id, owner_id, org_id, name) VALUES (11, 9, 7, 'PAYMENTS')",
        ] {
            let refused = db.execute_unprepared(statement).await.expect_err(statement);
            assert!(
                refused.to_string().contains("UNIQUE"),
                "{statement}: {refused}"
            );
        }
        db.execute_unprepared(
            "INSERT INTO repositories (id, owner_id, name) VALUES (12, 2, 'invoices')",
        )
        .await
        .unwrap();
        db.execute_unprepared("UPDATE repositories SET name = 'Invoices2' WHERE id = 12")
            .await
            .expect("a rename to a free name");
        let refused = db
            .execute_unprepared("UPDATE repositories SET name = 'PAYMENTS' WHERE id = 12")
            .await
            .expect_err("a rename into another case of a held name is refused");
        assert!(refused.to_string().contains("UNIQUE"), "{refused}");

        db.execute_unprepared(
            "INSERT INTO repositories (id, owner_id, name) VALUES (13, 3, 'PAYMENTS')",
        )
        .await
        .expect("a namespace that holds no spelling of the name");
        assert!(keys(&db).await.contains(&(13, key("payments"))));
    }

    /// A keyless twin survives every update that is not a rename, may change
    /// its own case, and gets its key the moment it is renamed to a free name.
    #[tokio::test]
    async fn a_keyless_twin_is_left_alone_until_it_is_renamed() {
        let db = upgraded_database_with_twins().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        db.execute_unprepared("UPDATE repositories SET description = 'still here' WHERE id = 2")
            .await
            .expect("an unrelated update of a keyless twin must not trip the index");
        db.execute_unprepared("UPDATE repositories SET deleted_at = '2026-10-14' WHERE id = 2")
            .await
            .expect("a soft delete of a keyless twin");
        db.execute_unprepared("UPDATE repositories SET deleted_at = NULL WHERE id = 2")
            .await
            .expect("restoring it as it was");
        db.execute_unprepared("UPDATE repositories SET name = 'payments-old' WHERE id = 2")
            .await
            .expect("a rename to a free name");
        assert!(keys(&db).await.contains(&(2, key("payments-old"))));

        db.execute_unprepared("UPDATE repositories SET name = 'PAYMENTS' WHERE id = 1")
            .await
            .expect("a case-only rename of a repository's own name");
        assert!(keys(&db).await.contains(&(1, key("payments"))));
    }

    /// `down` gives back the exact comparison.
    #[tokio::test]
    async fn down_restores_the_exact_comparison() {
        let db = upgraded_database_with_twins().await;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();

        assert!(!manager
            .has_column("repositories", "name_key")
            .await
            .unwrap());
        db.execute_unprepared(
            "INSERT INTO repositories (id, owner_id, name) VALUES (10, 2, 'PayMents')",
        )
        .await
        .expect("case-sensitive again after down");
    }
}
