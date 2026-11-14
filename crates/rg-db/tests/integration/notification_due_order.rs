//! `notification_delivery_ops::due` on every backend.
//!
//! The page is built with a window function over a subquery and a bound rank —
//! the first such query in this code base. If a dialect rejected it, `due()`
//! would fail on every pass and no notification of any class would leave that
//! database, so it runs here against SQLite, and against PostgreSQL and MySQL
//! when `PLOMBIR_GIT_TEST_DATABASE_URL` points at one (as the server jobs do).
//!
//! The server database is shared with the rest of the job, so this test's rows
//! are dated far in the past — they lead every class — and only they are
//! asserted on. They are removed at the end.

use rg_db::notification::Event;
use rg_db::ops::{notification_delivery_ops as queue, user_ops};
use rg_db::sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set,
};

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-due-order-{}.db",
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

async fn open_database() -> (DatabaseConnection, Option<TempDb>) {
    let (url, temp) = match std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => (url, None),
        _ => {
            let temp = TempDb::new();
            (temp.url(), Some(temp))
        }
    };
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to the test database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

/// Queue `count` rows of `event` for `user_id`, due at `at`, `at + 1s`, ….
async fn queue_rows(
    db: &DatabaseConnection,
    user_id: i64,
    event: Event,
    count: i64,
    at: chrono::DateTime<chrono::Utc>,
) -> Vec<i64> {
    let user = rg_db::entities::user::Entity::find_by_id(user_id)
        .one(db)
        .await
        .unwrap()
        .unwrap();
    let profile = queue::profile(db, user_id).await.unwrap();
    let mut ids = Vec::new();
    for i in 0..count {
        let event = queue::QueueEvent::security(user_id, event, None);
        let mut row = queue::row(&event, &user, &profile, "email").unwrap();
        row.next_attempt_at = Set(at + chrono::Duration::seconds(i));
        ids.push(row.insert(db).await.unwrap().id);
    }
    ids
}

#[tokio::test]
async fn due_serves_classes_in_order_caps_recipients_and_skips_retiring_accounts() {
    let (db, _temp) = open_database().await;
    let suffix = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let mut users = Vec::new();
    for role in ["flood", "other", "activity", "leaving"] {
        let name = format!("due{role}{suffix}");
        let user = user_ops::create_user(&db, &name, &format!("{name}@example.com"), "", "Due")
            .await
            .unwrap();
        users.push(user.id);
    }
    let [flood, other, activity, leaving] = users[..] else {
        unreachable!()
    };
    // Earlier than anything another test of the job can queue.
    let past = chrono::Utc::now() - chrono::Duration::days(3650);

    let flood_account = queue_rows(&db, flood, Event::NewDevice, 8, past).await;
    let other_account = queue_rows(
        &db,
        other,
        Event::NewDevice,
        1,
        past + chrono::Duration::seconds(100),
    )
    .await;
    let flood_shared = queue_rows(&db, flood, Event::RepositoryAccessChanged, 3, past).await;
    let activity_rows = queue_rows(&db, activity, Event::Participating, 6, past).await;
    // The oldest rows of all, for an account whose deletion is in progress.
    let leaving_rows = queue_rows(
        &db,
        leaving,
        Event::NewDevice,
        2,
        past - chrono::Duration::seconds(10),
    )
    .await;
    assert!(user_ops::begin_user_retirement(&db, leaving).await.unwrap());

    let page = queue::due(&db, 50)
        .await
        .expect("due() runs on this backend");
    let position = |id: &i64| page.iter().position(|p| p == id);
    let placed = |ids: &[i64]| -> Vec<usize> { ids.iter().filter_map(position).collect() };

    let account = [placed(&flood_account), placed(&other_account)].concat();
    let shared = placed(&flood_shared);
    let activity_placed = placed(&activity_rows);
    assert_eq!(
        placed(&flood_account).len(),
        queue::PER_RECIPIENT as usize,
        "one recipient takes at most PER_RECIPIENT places of a class"
    );
    assert_eq!(
        placed(&other_account).len(),
        1,
        "a later recipient is served"
    );
    assert_eq!(shared.len(), 3);
    assert!(
        activity_placed.len() >= queue::CLASS_FLOOR,
        "activity keeps its floor"
    );
    assert!(
        placed(&leaving_rows).is_empty(),
        "a retiring account is skipped"
    );
    let last = |p: &[usize]| p.iter().copied().max().unwrap();
    let first = |p: &[usize]| p.iter().copied().min().unwrap();
    assert!(last(&account) < first(&shared), "class 0 before class 1");
    assert!(
        last(&shared) < first(&activity_placed),
        "class 1 before activity"
    );

    let mine: Vec<i64> = [
        flood_account,
        other_account,
        flood_shared,
        activity_rows,
        leaving_rows,
    ]
    .concat();
    rg_db::entities::notification_delivery::Entity::delete_many()
        .filter(rg_db::entities::notification_delivery::Column::Id.is_in(mine))
        .exec(&db)
        .await
        .unwrap();
}
