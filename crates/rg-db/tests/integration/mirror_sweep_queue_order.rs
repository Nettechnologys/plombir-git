//! Regression coverage for the second half of `card_3d4c7b8b27c8`.
//!
//! `list_due_sync` takes a `LIMIT` — it is a queue — and took it with no
//! `ORDER BY`, so which mirrors a pass actually refreshed was whatever the
//! backend's plan handed back. That is not merely untidy: a mirror that is due
//! on *every* tick (the permanently-due row the interval floor now prevents,
//! but also any mirror whose interval is shorter than the sweep's period) can
//! sit in the batch forever, and the correctly configured mirrors behind it
//! never reach the queue at all.
//!
//! What is pinned here is the queue discipline: longest-waiting first, and a
//! mirror that has never synced ahead of one that has.
//!
//! Honest limitation, so nobody later reads more into a green run than is
//! there: on SQLite this test passes with the `ORDER BY` removed. The table
//! carries `idx_mirrors_next_sync`, SQLite's planner walks that index ascending
//! with `NULL` first, and the order comes out right by accident of the plan.
//! That accident is precisely why the defect went unnoticed — and why it is not
//! portable: PostgreSQL sorts `NULL` *last* in an ascending scan and is free to
//! pick an entirely different plan, which is what the explicit null-first key in
//! `list_due_sync` is for. This test therefore pins the contract on every
//! backend and discriminates on the ones where the planner does not happen to
//! agree with it.

use rg_db::entities::{mirror, repository};
use rg_db::sea_orm::{DatabaseConnection, NotSet, Set};

struct TempDb(std::path::PathBuf);

impl TempDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "plombir-git-mirror-queue-{}.db",
            uuid::Uuid::new_v4().simple()
        )))
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.0.display())
    }
}

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "test cleanup must not hide the assertion that failed"
    )]
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

async fn setup() -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new();
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

/// One mirrored repository whose next pass is due at `next_sync_at`.
async fn due_mirror(
    db: &DatabaseConnection,
    owner_id: i64,
    name: &str,
    next_sync_at: Option<chrono::DateTime<chrono::Utc>>,
) -> i64 {
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(owner_id),
            name: Set(name.to_string()),
            description: Set(None),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            origin_repo_id: Set(None),
        },
    )
    .await
    .expect("create the mirrored repository");

    let created = rg_db::ops::mirror_ops::create(
        db,
        mirror::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            url: Set(format!("https://example.invalid/{name}.git")),
            username: Set(None),
            password_encrypted: Set(None),
            sync_interval_seconds: Set(3600),
            next_sync_at: Set(next_sync_at),
            last_sync_at: Set(None),
            last_sync_error: Set(None),
            status: Set(mirror::STATUS_ACTIVE.to_string()),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .expect("create the mirror");
    created.id
}

#[tokio::test]
async fn the_sweep_takes_the_longest_waiting_mirrors_and_never_the_same_few() {
    let (db, _temp) = setup().await;
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        "mirror-queue",
        "mirror-queue@example.invalid",
        "",
        "Mirror Queue",
    )
    .await
    .expect("create the owner")
    .id;

    let now = chrono::Utc::now();
    // Inserted newest-due first, so an implementation that just returns rows in
    // insertion (or primary-key) order fails this.
    let recent = due_mirror(
        &db,
        owner,
        "recent",
        Some(now - chrono::Duration::seconds(5)),
    )
    .await;
    let older = due_mirror(&db, owner, "older", Some(now - chrono::Duration::hours(2))).await;
    let oldest = due_mirror(&db, owner, "oldest", Some(now - chrono::Duration::days(3))).await;
    let never = due_mirror(&db, owner, "never-synced", None).await;
    // Not due at all — it must not appear whatever the ordering does.
    let future = due_mirror(&db, owner, "future", Some(now + chrono::Duration::hours(1))).await;

    let ids: Vec<i64> = rg_db::ops::mirror_ops::list_due_sync(&db, 10)
        .await
        .expect("list due mirrors")
        .into_iter()
        .map(|row| row.id)
        .collect();

    assert_eq!(
        ids,
        vec![never, oldest, older, recent],
        "the sweep must drain its queue longest-waiting first, with a never-synced mirror ahead \
         of every mirror that has run at least once"
    );
    assert!(
        !ids.contains(&future),
        "a mirror that is not due yet was handed to the sweep"
    );

    // The half the missing ORDER BY actually cost: with a batch smaller than
    // the queue, the mirrors that have waited longest are the ones that run.
    let batch: Vec<i64> = rg_db::ops::mirror_ops::list_due_sync(&db, 2)
        .await
        .expect("list one batch")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(
        batch,
        vec![never, oldest],
        "a batch smaller than the queue took something other than the front of it"
    );
}
