//! card_bb685235de6f: a SQLite writer that meets another writer's lock waits in
//! the busy handler *holding its pool connection*. Enough of them and every
//! connection a reader could use is parked there. `rg_db::open_write_pool`
//! gives the steady writers a pool of their own, and this measures the point
//! of it: while somebody holds the write lock, a read on the shared pool is
//! answered at once — and, as the control, it is not when the same writers
//! queue on the shared pool.

use std::time::{Duration, Instant};

use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};

/// How long the outside writer keeps SQLite's write lock.
const LOCK_HELD: Duration = Duration::from_millis(2000);

async fn read_latency_while_writers_wait(
    shared: &DatabaseConnection,
    writers_use: &DatabaseConnection,
    holder: &DatabaseConnection,
) -> Duration {
    let lock = {
        let holder = holder.clone();
        tokio::spawn(async move {
            let txn = holder.begin().await.unwrap();
            txn.execute_unprepared("INSERT INTO pool_probe VALUES (0)")
                .await
                .unwrap();
            tokio::time::sleep(LOCK_HELD).await;
            txn.commit().await.unwrap();
        })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    let writers: Vec<_> = (0..4)
        .map(|n| {
            let db = writers_use.clone();
            tokio::spawn(async move {
                db.execute_unprepared(&format!("INSERT INTO pool_probe VALUES ({n})"))
                    .await
                    .unwrap();
            })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let started = Instant::now();
    shared
        .execute_unprepared("SELECT count(*) FROM pool_probe")
        .await
        .unwrap();
    let latency = started.elapsed();

    lock.await.unwrap();
    for writer in writers {
        writer.await.unwrap();
    }
    latency
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_read_is_not_queued_behind_writers_waiting_for_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("pool.db").display());
    let shared = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .unwrap();
    shared
        .execute_unprepared("CREATE TABLE pool_probe (x INTEGER)")
        .await
        .unwrap();
    let write = rg_db::open_write_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, &shared)
        .await
        .unwrap();
    let holder = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .unwrap();

    // Control: writers on the shared pool take both of its connections into
    // the busy handler, and the read waits for the lock it does not need.
    let starved = read_latency_while_writers_wait(&shared, &shared, &holder).await;
    assert!(
        starved >= LOCK_HELD / 2,
        "the control did not reproduce the starvation ({starved:?}), so the measurement below \
         would prove nothing"
    );

    // The same writers on the write pool: the read has a connection.
    let answered = read_latency_while_writers_wait(&shared, &write, &holder).await;
    eprintln!("read latency with the lock held: writers on the shared pool {starved:?}, on the write pool {answered:?}");
    assert!(
        answered < LOCK_HELD / 4,
        "a read waited {answered:?} behind writers queued on the write pool"
    );
}

#[tokio::test]
async fn an_in_memory_database_writes_through_the_pool_it_has() {
    let shared =
        rg_db::connect_with_pool("sqlite::memory:", rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
            .await
            .unwrap();
    shared
        .execute_unprepared("CREATE TABLE pool_probe (x INTEGER)")
        .await
        .unwrap();
    let write = rg_db::open_write_pool(
        "sqlite::memory:",
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        &shared,
    )
    .await
    .unwrap();
    // A second pool would be a second, empty database.
    write
        .execute_unprepared("INSERT INTO pool_probe VALUES (1)")
        .await
        .unwrap();
}
