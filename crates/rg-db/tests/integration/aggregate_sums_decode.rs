//! A `SUM` over an integer column decodes on every backend.
//!
//! PostgreSQL widens `sum(bigint)` to `numeric` and MySQL answers `DECIMAL`
//! for any exact-integer `SUM`; sqlx refuses either one as `i64` — but only
//! when the value is not `NULL`, so an empty table passes and the first stored
//! row turns `GET /lfs/usage` and the LFS sweep into a database error. SQLite
//! answers `INTEGER`, which is why nothing that runs only on SQLite could see
//! it. These tests honour `PLOMBIR_GIT_TEST_DATABASE_URL` and run in the
//! PostgreSQL and MySQL smoke jobs for exactly that reason.

use rg_db::entities::{issue, lfs_object, repository, time_entry};
use rg_db::sea_orm::{ActiveModelTrait, DatabaseConnection, NotSet, Set};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-aggregate-sums-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
}

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "cleanup must not mask the assertion that failed the test"
    )]
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

/// A migrated database with an account, a repository and an issue of its own.
/// Names carry a uuid so repeated runs against one server database never meet.
async fn setup() -> (DatabaseConnection, Option<TempDb>, i64, i64) {
    let (url, temp) = match std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => (url, None),
        _ => {
            let temp = TempDb::new();
            (temp.url(), Some(temp))
        }
    };
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let user = rg_db::ops::user_ops::create_user(
        &db,
        &format!("sums{suffix}"),
        &format!("sums{suffix}@example.com"),
        "",
        "Sums",
    )
    .await
    .expect("create the account the rows hang off");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set(format!("sums{suffix}")),
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
    .expect("create the repository the rows hang off");
    let issue = issue::ActiveModel {
        id: NotSet,
        repo_id: Set(repo.id),
        number: Set(1),
        title: Set("time is tracked here".to_string()),
        body: Set(None),
        state: Set("open".to_string()),
        author_id: Set(user.id),
        assignee_id: Set(None),
        milestone_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        closed_at: Set(None),
        deleted_at: Set(None),
    }
    .insert(&db)
    .await
    .expect("create the issue the time entries hang off");
    (db, temp, repo.id, issue.id)
}

#[tokio::test]
async fn lfs_usage_sums_stored_objects_on_every_backend() {
    let (db, _temp, repo_id, _issue_id) = setup().await;

    // The empty table is the case that always worked: `SUM` of nothing is NULL.
    assert_eq!(
        rg_db::ops::lfs_object_ops::usage(&db, repo_id)
            .await
            .expect("usage of an empty repository"),
        (0, 0)
    );

    // Past `i32::MAX`, so a backend that narrowed the total would show it.
    let sizes = [3_000_000_000_i64, 7];
    for (index, size) in sizes.into_iter().enumerate() {
        rg_db::ops::lfs_object_ops::create(
            &db,
            lfs_object::ActiveModel {
                id: NotSet,
                repo_id: Set(repo_id),
                oid: Set(format!("{index:064x}")),
                size: Set(size),
                uploaded: Set(true),
                created_at: Set(chrono::Utc::now()),
                publisher_token: Set(None),
                publisher_since: Set(None),
                last_claimed_at: Set(None),
            },
        )
        .await
        .expect("store an LFS object");
    }

    assert_eq!(
        rg_db::ops::lfs_object_ops::usage(&db, repo_id)
            .await
            .expect("a SUM over stored objects must decode, not fail on its numeric type"),
        (2, 3_000_000_007)
    );
}

#[tokio::test]
async fn tracked_minutes_sum_on_every_backend() {
    let (db, _temp, _repo_id, issue_id) = setup().await;

    for minutes in [45_i64, 90] {
        rg_db::ops::time_entry_ops::create(
            &db,
            time_entry::ActiveModel {
                id: NotSet,
                issue_id: Set(issue_id),
                user_id: Set(None),
                duration_minutes: Set(minutes),
                description: Set(None),
                created_at: Set(chrono::Utc::now()),
            },
        )
        .await
        .expect("log time");
    }

    assert_eq!(
        rg_db::ops::time_entry_ops::total_minutes_by_issue(&db, issue_id)
            .await
            .expect("a SUM over logged minutes must decode, not fail on its numeric type"),
        135
    );
}
