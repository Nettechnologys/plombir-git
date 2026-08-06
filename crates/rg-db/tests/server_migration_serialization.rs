//! card_7c55dca00a61: concurrent migrators on a PostgreSQL/MySQL database.
//!
//! The file-backed SQLite gate is a lock on the database *file*, so it excluded
//! nothing at all on a server backend — and `Migrator::up` is not idempotent
//! with respect to a concurrent copy of itself there. Two ForgeKeep processes
//! migrating one PostgreSQL database used to race inside `CREATE TABLE`, and the
//! loser died with `duplicate key value violates unique constraint
//! "pg_type_typname_nsp_index"`.
//!
//! Run against a **disposable** server database, same switch as
//! `multi_backend_smoke`:
//! `FORGEKEEP_TEST_DATABASE_URL=... cargo test -p rg-db --test server_migration_serialization -- --ignored`
//!
//! What these tests guard:
//!
//! * **The lock excludes.** Four holders of `migration_lock` on one database
//!   never overlap, which is the property `run_migrations` is built on.
//! * **Concurrent migrators all survive.** Four `run_migrations` calls against
//!   one database, each on its own pool, all return `Ok` — this is the exact
//!   shape that used to fail. It reproduces the original failure when the
//!   database is still empty, which is why CI runs it as its first step against
//!   a fresh service container; against an already-migrated database it degrades
//!   to a guard that the lock path does not break a no-op run.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rg_db::sea_orm::{ConnectionTrait, Statement};

/// The database under test, refusing a URL that cannot answer the question.
///
/// SQLite is excluded rather than skipped: its exclusion lives in
/// `sqlite_process_guard`, so pointing this file at a SQLite URL would measure a
/// lock that deliberately holds nothing and report it as a defect.
fn server_database_url() -> String {
    let url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    assert!(
        !url.starts_with("sqlite"),
        "this file measures the server-side migration lock; a SQLite database is guarded by \
         sqlite_process_guard instead"
    );
    url
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable PostgreSQL or MySQL database"]
async fn concurrent_holders_of_the_migration_lock_never_overlap() {
    let db = rg_db::connect_with_pool(
        &server_database_url(),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        2,
    )
    .await
    .expect("connect to the test database");

    let db = Arc::new(db);
    let inside = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));

    let holders: Vec<_> = (0..4)
        .map(|_| {
            let db = Arc::clone(&db);
            let inside = Arc::clone(&inside);
            let peak = Arc::clone(&peak);
            let completed = Arc::clone(&completed);
            tokio::spawn(async move {
                let lock = rg_db::migration_lock::acquire(&db, Duration::from_secs(60))
                    .await
                    .expect("take the migration lock");

                let now_inside = inside.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now_inside, Ordering::SeqCst);
                // Long enough that an unserialised sibling would be observed
                // inside the section rather than merely scheduled after it.
                tokio::time::sleep(Duration::from_millis(200)).await;
                inside.fetch_sub(1, Ordering::SeqCst);

                lock.release().await;
                completed.fetch_add(1, Ordering::SeqCst);
            })
        })
        .collect();

    for holder in holders {
        holder.await.expect("a lock holder panicked");
    }

    assert_eq!(
        completed.load(Ordering::SeqCst),
        4,
        "every holder must have taken and released the lock"
    );
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "two ForgeKeep processes were inside the migration section at once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable PostgreSQL or MySQL database"]
async fn a_migrator_that_runs_out_of_patience_says_what_to_do_about_it() {
    let db = rg_db::connect_with_pool(
        &server_database_url(),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        2,
    )
    .await
    .expect("connect to the test database");

    let held = rg_db::migration_lock::acquire(&db, Duration::from_secs(30))
        .await
        .expect("take the migration lock");
    let refusal = rg_db::migration_lock::acquire(&db, Duration::from_secs(1))
        .await
        .expect_err("the lock is held by this very test");
    held.release().await;

    // The whole point of the gate: the operator is told who is holding this up
    // and what to do, instead of reading a PostgreSQL system-index name.
    let message = format!("{refusal:#}");
    assert!(
        message.contains("ForgeKeep") && message.contains("migration lock"),
        "the refusal must name ForgeKeep and what is held: {message}"
    );
    assert!(
        message.contains("stop that process"),
        "the refusal must name the action that clears it: {message}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable PostgreSQL or MySQL database"]
async fn concurrent_run_migrations_on_one_server_database_all_succeed() {
    let url = server_database_url();

    // A pool each, so the racing migrators talk to the database over separate
    // connections the way separate processes do.
    let migrators: Vec<_> = (0..4)
        .map(|_| {
            let url = url.clone();
            tokio::spawn(async move {
                let db =
                    rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 120, 2).await?;
                rg_db::run_migrations(&db).await
            })
        })
        .collect();

    for (index, migrator) in migrators.into_iter().enumerate() {
        migrator
            .await
            .expect("a migrator panicked")
            .unwrap_or_else(|error| {
                panic!("concurrent migrator {index} failed: {error:#}");
            });
    }

    // Not vacuous: the schema the migrators were racing over has to be there
    // afterwards, whichever of them applied it.
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to the migrated database");
    let backend = db.get_database_backend();
    db.query_one(Statement::from_string(
        backend,
        "SELECT COUNT(*) FROM users",
    ))
    .await
    .expect("query a table the racing migrations created");
}
