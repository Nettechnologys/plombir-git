//! card_e49592b4fd03 (L18): no write may leave the instance without an open
//! administrator account — not a demotion, not a deactivation, not the start of
//! an account deletion, and not two of them racing.
//!
//! The guard reads a set that spans the whole `users` table, so a database
//! shared with other tests cannot answer "is this the last one". The tests run
//! on a throwaway SQLite file by default. The race also has a server variant,
//! ignored by default: point `PLOMBIR_GIT_TEST_DATABASE_URL` at a *disposable*
//! PostgreSQL or MySQL database that holds no administrators yet and run it
//! with `--ignored`.

use rg_db::entities::user;
use rg_db::ops::user_ops::{self, LastAdministrator};
use rg_db::sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    Set,
};

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-last-admin-{}.db",
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

/// A migrated database with several pooled connections, so racing tasks run
/// on separate connections.
async fn connect(url: &str) -> DatabaseConnection {
    let db = rg_db::connect_with_pool(url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to the test database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    db
}

/// An open administrator, written as rows: promoting through `update_by_id`
/// would also queue an authority notice, which these tests do not look at.
async fn administrator(db: &DatabaseConnection, name: &str) -> i64 {
    let name = format!("{name}{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let account = user_ops::create_user(db, &name, &format!("{name}@example.invalid"), "", &name)
        .await
        .expect("create an account");
    user::ActiveModel {
        id: Set(account.id),
        is_admin: Set(true),
        ..Default::default()
    }
    .update(db)
    .await
    .expect("make it an administrator");
    account.id
}

async fn open_administrators(db: &DatabaseConnection) -> u64 {
    user::Entity::find()
        .filter(user::Column::IsAdmin.eq(true))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::DeletedAt.is_null())
        .filter(user::Column::BotOwnerId.is_null())
        .count(db)
        .await
        .expect("count open administrators")
}

fn is_last_administrator(error: &anyhow::Error) -> bool {
    error.downcast_ref::<LastAdministrator>().is_some()
}

#[tokio::test]
async fn the_last_open_administrator_is_neither_demoted_deactivated_nor_retired() {
    let temp = TempDb::new();
    let db = connect(&temp.url()).await;
    let only = administrator(&db, "only").await;

    let demoted = user_ops::update_by_id(&db, only, None, None, Some(false), None).await;
    assert!(
        demoted.as_ref().is_err_and(is_last_administrator),
        "demoting the last administrator: {demoted:?}"
    );
    let deactivated = user_ops::update_by_id(&db, only, None, None, None, Some(false)).await;
    assert!(
        deactivated.as_ref().is_err_and(is_last_administrator),
        "deactivating the last administrator: {deactivated:?}"
    );
    let retired = user_ops::begin_user_retirement(&db, only).await;
    assert!(
        retired.as_ref().is_err_and(is_last_administrator),
        "retiring the last administrator: {retired:?}"
    );
    let row = user_ops::find_by_id(&db, only).await.unwrap().unwrap();
    assert!(row.is_admin && row.is_active && row.deleted_at.is_none());

    // Edits that keep the account an open administrator are not refused.
    let renamed = user_ops::update_by_id(
        &db,
        only,
        Some(Some("Still here".into())),
        None,
        Some(true),
        Some(true),
    )
    .await
    .expect("an edit that keeps the administrator")
    .expect("the account exists");
    assert_eq!(renamed.display_name.as_deref(), Some("Still here"));

    // With a second administrator either may step down — once.
    let second = administrator(&db, "second").await;
    user_ops::update_by_id(&db, only, None, None, Some(false), None)
        .await
        .expect("demotion with another administrator left")
        .expect("the account exists");
    let last = user_ops::update_by_id(&db, second, None, None, None, Some(false)).await;
    assert!(
        last.as_ref().is_err_and(is_last_administrator),
        "the remaining administrator is now the last: {last:?}"
    );
    assert_eq!(open_administrators(&db).await, 1);

    // A bot never counts as the administrator that keeps the instance open.
    let bot = administrator(&db, "bot").await;
    user::ActiveModel {
        id: Set(bot),
        bot_owner_id: Set(Some(second)),
        ..Default::default()
    }
    .update(&db)
    .await
    .unwrap();
    let still_last = user_ops::begin_user_retirement(&db, second).await;
    assert!(
        still_last.as_ref().is_err_and(is_last_administrator),
        "a bot administrator does not stand in: {still_last:?}"
    );
}

/// Two writes that each remove one of the last two administrators, started
/// together, again and again. Exactly one wins each round; the other is refused
/// as the last administrator, and one open administrator is left after every
/// round. Without the shared lock both read "two administrators" and both
/// commit.
async fn racing_removals_leave_one_administrator(url: &str) {
    let db = connect(url).await;
    assert_eq!(
        open_administrators(&db).await,
        0,
        "this test needs a database without administrators"
    );
    let mut survivor = administrator(&db, "first").await;
    for round in 0..40 {
        let newcomer = administrator(&db, "next").await;
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        // Alternate the kinds of removal, so demotion, deactivation and the
        // start of a deletion each race each other.
        let remove = |id: i64, kind: usize| {
            let db = db.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                match kind % 3 {
                    0 => user_ops::update_by_id(&db, id, None, None, Some(false), None)
                        .await
                        .map(|_| ()),
                    1 => user_ops::update_by_id(&db, id, None, None, None, Some(false))
                        .await
                        .map(|_| ()),
                    _ => user_ops::begin_user_retirement(&db, id).await.map(|_| ()),
                }
            })
        };
        let first = remove(survivor, round);
        let second = remove(newcomer, round + 1);
        let outcomes = [first.await.unwrap(), second.await.unwrap()];
        let won = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
        let refused = outcomes
            .iter()
            .filter(|outcome| outcome.as_ref().is_err_and(is_last_administrator))
            .count();
        assert_eq!(
            (won, refused),
            (1, 1),
            "round {round}: exactly one removal goes through: {outcomes:?}"
        );
        assert_eq!(
            open_administrators(&db).await,
            1,
            "round {round}: the instance keeps one open administrator"
        );
        survivor = if outcomes[0].is_ok() {
            newcomer
        } else {
            survivor
        };
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_removals_never_leave_the_instance_without_an_administrator() {
    let temp = TempDb::new();
    racing_removals_leave_one_administrator(&temp.url()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at a disposable PostgreSQL or MySQL database without administrators"]
async fn racing_removals_never_leave_a_server_instance_without_an_administrator() {
    let url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
    assert!(
        !url.starts_with("sqlite"),
        "the SQLite case runs by default"
    );
    racing_removals_leave_one_administrator(&url).await;
}
