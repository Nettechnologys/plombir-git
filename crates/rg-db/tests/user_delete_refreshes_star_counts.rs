//! card_957cc2683f70: an account's departure must not leave somebody else's
//! star count inflated.
//!
//! `repo_stars.user_id` is `ON DELETE CASCADE`, which is the right decision —
//! a star is the account's own and goes with it. But the stars an account gave
//! sit on *other people's* repositories, and `repositories.stars_count` is
//! declared to be `COUNT(*)` over `repo_stars`. Nothing recomputed it on this
//! path: the only other writer is `toggle_star`, which refreshes exactly the
//! one repository somebody just starred, so a repository nobody stars again
//! would advertise the departed account's star forever.
//!
//! `user_ops::delete_by_id` now inventories the starred repositories before the
//! delete — afterwards the cascade has already taken the rows that name them —
//! and refreshes their counters in the same transaction.

use rg_db::sea_orm::{ActiveValue::Set, ConnectionTrait, DatabaseConnection, EntityTrait};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-star-counts-{label}-{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        Self { path }
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

async fn setup(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

async fn seed_user(db: &DatabaseConnection, username: &str) -> i64 {
    let now = chrono::Utc::now();
    rg_db::entities::user::Entity::insert(rg_db::entities::user::ActiveModel {
        username: Set(username.to_string()),
        email: Set(format!("{username}@example.com")),
        password_hash: Set("x".to_string()),
        is_active: Set(true),
        is_admin: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .exec(db)
    .await
    .expect("seed user")
    .last_insert_id
}

async fn seed_repo(db: &DatabaseConnection, owner_id: i64, name: &str) -> i64 {
    let now = chrono::Utc::now();
    rg_db::ops::repo_ops::create(
        db,
        rg_db::entities::repository::ActiveModel {
            owner_id: Set(owner_id),
            name: Set(name.to_string()),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed repository")
    .id
}

/// Star `repo_id` as `user_id` through the same pair of calls the HTTP path
/// makes, so the counter starts out agreeing with the rows.
async fn star(db: &DatabaseConnection, user_id: i64, repo_id: i64) {
    assert!(
        rg_db::ops::repo_star_ops::toggle_star(db, user_id, repo_id)
            .await
            .expect("star the repository"),
        "the star was not new"
    );
    rg_db::ops::repo_ops::update_stars_count(db, repo_id)
        .await
        .expect("refresh the star counter the way `toggle_star` does");
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(rg_db::sea_orm::Statement::from_string(
        db.get_database_backend(),
        sql.to_string(),
    ))
    .await
    .expect("run the counting query")
    .expect("the counting query returned no row")
    .try_get_by_index::<i64>(0)
    .expect("decode the count")
}

async fn cached_stars(db: &DatabaseConnection, repo_id: i64) -> i64 {
    scalar(
        db,
        &format!("SELECT stars_count AS n FROM repositories WHERE id = {repo_id}"),
    )
    .await
}

async fn live_stars(db: &DatabaseConnection, repo_id: i64) -> i64 {
    scalar(
        db,
        &format!("SELECT COUNT(*) AS n FROM repo_stars WHERE repo_id = {repo_id}"),
    )
    .await
}

/// The guest stars the host's repository and then closes their account. The
/// host's shopfront must fall back to the rows that are left — and it must not
/// fall past them, taking a bystander's star with it.
#[tokio::test]
async fn deleting_an_account_refreshes_the_star_counts_of_other_peoples_repositories() {
    let (db, _temp) = setup("foreign-stars").await;

    let host = seed_user(&db, "star_host").await;
    let guest = seed_user(&db, "star_guest").await;
    let bystander = seed_user(&db, "star_bystander").await;

    // Two repositories of the host, so the refresh has to cover the whole
    // inventory rather than whichever one it happened to touch last.
    let shared = seed_repo(&db, host, "shared").await;
    let lonely = seed_repo(&db, host, "lonely").await;

    star(&db, guest, shared).await;
    star(&db, bystander, shared).await;
    star(&db, guest, lonely).await;

    assert_eq!(cached_stars(&db, shared).await, 2);
    assert_eq!(cached_stars(&db, lonely).await, 1);

    assert!(
        rg_db::ops::user_ops::delete_by_id(&db, guest)
            .await
            .expect("delete the guest account"),
        "the guest row was not there to delete"
    );

    assert_eq!(
        live_stars(&db, shared).await,
        1,
        "the cascade took a star that was not the guest's"
    );
    assert_eq!(
        cached_stars(&db, shared).await,
        1,
        "the host's repository still advertises the departed account's star"
    );
    assert_eq!(
        live_stars(&db, lonely).await,
        0,
        "the guest's star outlived the guest"
    );
    assert_eq!(
        cached_stars(&db, lonely).await,
        0,
        "a repository nobody stars again keeps the departed account's star forever"
    );

    // The bystander's star is the one that survived, and it is the one being
    // counted: the refresh recomputed the counter rather than decrementing it.
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) AS n FROM repo_stars \
                 WHERE repo_id = {shared} AND user_id = {bystander}"
            ),
        )
        .await,
        1,
        "the bystander's star was collected with the guest's account"
    );
}

/// The complement: an account with no stars at all is deleted by the same
/// statement, and nothing else moves. Guards the empty-inventory branch, which
/// must issue no `UPDATE` rather than one with an empty `IN ()` list — SQLite
/// and PostgreSQL both reject that syntax.
#[tokio::test]
async fn deleting_an_account_that_starred_nothing_touches_no_counter() {
    let (db, _temp) = setup("no-stars").await;

    let host = seed_user(&db, "quiet_host").await;
    let guest = seed_user(&db, "quiet_guest").await;
    let bystander = seed_user(&db, "quiet_bystander").await;
    let repo = seed_repo(&db, host, "quiet").await;

    star(&db, bystander, repo).await;
    assert_eq!(cached_stars(&db, repo).await, 1);

    assert!(rg_db::ops::user_ops::delete_by_id(&db, guest)
        .await
        .expect("delete an account that starred nothing"));

    assert_eq!(
        cached_stars(&db, repo).await,
        1,
        "deleting an unrelated account disturbed a counter it had no stars in"
    );
    assert_eq!(live_stars(&db, repo).await, 1);
}
