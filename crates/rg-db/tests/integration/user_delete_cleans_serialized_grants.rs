//! A user deletion must revoke authorization grants stored outside relational
//! foreign keys. The three allow-lists below are JSON text, so this test proves
//! the application-level transaction removes the id and rolls every rewrite
//! back when the user row itself cannot be deleted.

use rg_db::sea_orm::{ActiveValue::Set, ConnectionTrait, DatabaseConnection, EntityTrait};

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-serialized-user-grants-{}.db",
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

async fn setup() -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new();
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

async fn seed_repo(db: &DatabaseConnection, owner_id: i64) -> i64 {
    let now = chrono::Utc::now();
    rg_db::ops::repo_ops::create(
        db,
        rg_db::entities::repository::ActiveModel {
            owner_id: Set(owner_id),
            name: Set("shared".to_string()),
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

async fn seed_grants(db: &DatabaseConnection, repo_id: i64, victim: i64, host: i64) {
    let now = chrono::Utc::now();
    rg_db::ops::protected_branch_ops::create_with_push_grants(
        db,
        rg_db::entities::protected_branch::ActiveModel {
            repo_id: Set(repo_id),
            branch_name: Set("main".to_string()),
            require_pr: Set(true),
            require_status_check: Set(false),
            required_status_checks: Set(None),
            require_approval: Set(false),
            required_approvals: Set(None),
            allow_force_push: Set(false),
            require_signed_commits: Set(false),
            allowed_push_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![victim, host]),
    )
    .await
    .expect("seed protected branch grant");
    rg_db::ops::protected_tag_ops::create_with_push_grants(
        db,
        rg_db::entities::protected_tag::ActiveModel {
            repo_id: Set(repo_id),
            pattern: Set("v*".to_string()),
            allowed_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![host, victim]),
    )
    .await
    .expect("seed protected tag grant");
    rg_db::ops::ci_environment_ops::create_with_approvers(
        db,
        rg_db::entities::ci_environment::ActiveModel {
            repo_id: Set(repo_id),
            name: Set("production".to_string()),
            protected: Set(true),
            required_approvals: Set(1),
            allowed_approver_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        vec![victim, host],
    )
    .await
    .expect("seed environment grant");
}

async fn stored_grants(db: &DatabaseConnection) -> [String; 3] {
    let branch = rg_db::ops::protected_branch_ops::find_by_repo_and_branch(db, 1, "main")
        .await
        .expect("read protected branch")
        .expect("protected branch exists");
    let tag = rg_db::ops::protected_tag_ops::list_by_repo(db, 1)
        .await
        .expect("read protected tag")
        .pop()
        .expect("protected tag exists");
    let environment = rg_db::ops::ci_environment_ops::list(db, 1)
        .await
        .expect("read environment")
        .pop()
        .expect("environment exists");
    [
        branch.allowed_push_user_ids.expect("branch grant is set"),
        tag.allowed_user_ids.expect("tag grant is set"),
        environment
            .allowed_approver_ids
            .expect("environment grant is set"),
    ]
}

#[tokio::test]
async fn deleting_a_user_atomically_removes_every_serialized_grant() {
    let (db, _temp) = setup().await;
    let host = seed_user(&db, "grant-host").await;
    let victim = seed_user(&db, "grant-victim").await;
    let repo_id = seed_repo(&db, host).await;
    assert_eq!(repo_id, 1, "fixture assumes the first repository id");
    seed_grants(&db, repo_id, victim, host).await;

    let original = stored_grants(&db).await;
    db.execute_unprepared(&format!(
        "CREATE TRIGGER abort_user_delete BEFORE DELETE ON users \
         WHEN OLD.id = {victim} BEGIN SELECT RAISE(ABORT, 'faulted user delete'); END;"
    ))
    .await
    .expect("install delete fault");

    let error = rg_db::ops::user_ops::delete_by_id(&db, victim)
        .await
        .expect_err("faulted user deletion must fail");
    assert!(format!("{error:#}").contains("delete user"), "{error:#}");
    assert_eq!(
        stored_grants(&db).await,
        original,
        "grant cleanup committed before the user deletion did"
    );
    assert!(
        rg_db::ops::user_ops::find_by_id(&db, victim)
            .await
            .expect("read victim after rollback")
            .is_some(),
        "the failed transaction removed the user"
    );

    db.execute_unprepared("DROP TRIGGER abort_user_delete")
        .await
        .expect("remove delete fault");
    assert!(rg_db::ops::user_ops::delete_by_id(&db, victim)
        .await
        .expect("delete user after removing fault"));
    assert_eq!(
        stored_grants(&db).await,
        [
            format!("[{host}]"),
            format!("[{host}]"),
            format!("[{host}]")
        ]
    );
}
