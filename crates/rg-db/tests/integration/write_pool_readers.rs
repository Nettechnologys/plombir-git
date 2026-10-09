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

/// The writers stage 2 moved onto the write pool (card_a84b25c9efbe) — a PAT,
/// an SSH key and a deploy key stamping their use, and the embedded runner's
/// job heartbeat — each sent once while somebody holds the write lock. The
/// operations are the real ones, not a stand-in `INSERT`.
fn moved_writers(
    pool: &DatabaseConnection,
    ids: MovedWriterIds,
) -> Vec<tokio::task::JoinHandle<()>> {
    let token = {
        let db = pool.clone();
        tokio::spawn(async move {
            rg_db::ops::token_ops::touch_last_used(&db, ids.token, None)
                .await
                .unwrap();
        })
    };
    let ssh_key = {
        let db = pool.clone();
        tokio::spawn(async move {
            rg_db::ops::ssh_key_ops::touch_last_used(&db, ids.ssh_key, None)
                .await
                .unwrap();
        })
    };
    let deploy_key = {
        let db = pool.clone();
        tokio::spawn(async move {
            rg_db::ops::deploy_key_ops::touch_last_used(&db, ids.deploy_key, None)
                .await
                .unwrap();
        })
    };
    let heartbeat = {
        let db = pool.clone();
        tokio::spawn(async move {
            rg_db::ops::pipeline_ops::touch_running_job(&db, ids.job)
                .await
                .unwrap();
        })
    };
    vec![token, ssh_key, deploy_key, heartbeat]
}

#[derive(Clone, Copy)]
struct MovedWriterIds {
    token: i64,
    ssh_key: i64,
    deploy_key: i64,
    job: i64,
}

async fn read_latency_while_moved_writers_wait(
    shared: &DatabaseConnection,
    writers_use: &DatabaseConnection,
    holder: &DatabaseConnection,
    ids: MovedWriterIds,
) -> Duration {
    let lock = {
        let holder = holder.clone();
        tokio::spawn(async move {
            let txn = holder.begin().await.unwrap();
            txn.execute_unprepared("UPDATE users SET updated_at = updated_at")
                .await
                .unwrap();
            tokio::time::sleep(LOCK_HELD).await;
            txn.commit().await.unwrap();
        })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    let writers = moved_writers(writers_use, ids);
    tokio::time::sleep(Duration::from_millis(150)).await;

    let started = Instant::now();
    shared
        .execute_unprepared("SELECT count(*) FROM repositories")
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
async fn the_writers_moved_to_the_write_pool_do_not_queue_a_read() {
    use sea_orm::{ActiveValue::Set, NotSet};

    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("moved.db").display()
    );
    let shared = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .unwrap();
    rg_db::run_migrations(&shared).await.unwrap();
    let write = rg_db::open_write_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, &shared)
        .await
        .unwrap();
    let holder = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .unwrap();

    let now = chrono::Utc::now();
    let user = rg_db::ops::user_ops::create_user(&shared, "moved", "moved@example.test", "", "")
        .await
        .unwrap();
    let token = rg_db::ops::token_ops::create(
        &shared,
        rg_db::entities::access_token::ActiveModel {
            id: NotSet,
            user_id: Set(user.id),
            name: Set("stamped".to_string()),
            token_hash: Set("0".repeat(64)),
            scopes: Set("repo".to_string()),
            expires_at: Set(None),
            last_used_at: Set(None),
            created_at: Set(now),
            ..Default::default()
        },
        &[],
    )
    .await
    .unwrap();
    let ssh_key = rg_db::ops::ssh_key_ops::create(
        &shared,
        rg_db::entities::ssh_key::ActiveModel {
            id: NotSet,
            user_id: Set(user.id),
            title: Set("stamped".to_string()),
            public_key: Set("ssh-ed25519 AAAA moved".to_string()),
            fingerprint: Set("SHA256:moved-user".to_string()),
            created_at: Set(now),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();
    let repo = rg_db::ops::repo_ops::create(
        &shared,
        rg_db::entities::repository::ActiveModel {
            owner_id: Set(user.id),
            name: Set("moved".to_string()),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            stars_count: Set(0),
            forks_count: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let deploy_key = rg_db::ops::deploy_key_ops::create(
        &shared,
        rg_db::entities::deploy_key::ActiveModel {
            repo_id: Set(repo.id),
            created_by_id: Set(Some(user.id)),
            title: Set("stamped".to_string()),
            public_key: Set("ssh-ed25519 AAAA moved-deploy".to_string()),
            fingerprint: Set("SHA256:moved-deploy".to_string()),
            read_only: Set(true),
            created_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &shared,
        repo.id,
        &"a".repeat(40),
        "refs/heads/main",
        "push",
        Some(user.id),
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(&shared, pipeline.id, "build", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        &shared, stage.id, "build", "true", None, None, None, None, None, None, false, None, None,
        None,
    )
    .await
    .unwrap();
    assert!(
        rg_db::ops::pipeline_ops::start_job_if_active(&shared, job.id, None)
            .await
            .unwrap()
    );
    let ids = MovedWriterIds {
        token: token.id,
        ssh_key: ssh_key.id,
        deploy_key: deploy_key.id,
        job: job.id,
    };

    let starved = read_latency_while_moved_writers_wait(&shared, &shared, &holder, ids).await;
    assert!(
        starved >= LOCK_HELD / 2,
        "the control did not reproduce the starvation ({starved:?}), so the measurement below \
         would prove nothing"
    );
    let answered = read_latency_while_moved_writers_wait(&shared, &write, &holder, ids).await;
    eprintln!("read latency with the lock held: moved writers on the shared pool {starved:?}, on the write pool {answered:?}");
    assert!(
        answered < LOCK_HELD / 4,
        "a read waited {answered:?} behind the moved writers queued on the write pool"
    );
}
