//! Acceptance coverage for the shared user-grant writer. The API-facing JSON
//! columns remain mirrors; authorization is backed by FK relations and every
//! replacement validates the principal while the transaction owns the needed
//! database locks.

use rg_db::sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, TransactionTrait,
};
use rg_db::user_grants::{self, Target};

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-user-grant-writer-{}.db",
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

async fn setup() -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new();
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

async fn seed_user(db: &DatabaseConnection, username: &str, is_active: bool) -> i64 {
    seed_user_with_id(db, None, username, is_active).await
}

async fn seed_user_with_id(
    db: &DatabaseConnection,
    id: Option<i64>,
    username: &str,
    is_active: bool,
) -> i64 {
    let now = chrono::Utc::now();
    let mut user = rg_db::entities::user::ActiveModel {
        username: Set(username.to_string()),
        email: Set(format!("{username}@example.com")),
        password_hash: Set("x".to_string()),
        is_active: Set(is_active),
        is_admin: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    if let Some(id) = id {
        user.id = Set(id);
    }
    rg_db::entities::user::Entity::insert(user)
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
            name: Set("grant-integrity".to_string()),
            is_private: Set(true),
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

async fn seed_targets(db: &DatabaseConnection, repo_id: i64, user_id: i64) -> [Target; 3] {
    let now = chrono::Utc::now();
    let branch = rg_db::ops::protected_branch_ops::create_with_push_grants(
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
        Some(vec![user_id]),
    )
    .await
    .expect("seed protected branch grants");
    let tag = rg_db::ops::protected_tag_ops::create_with_push_grants(
        db,
        rg_db::entities::protected_tag::ActiveModel {
            repo_id: Set(repo_id),
            pattern: Set("v*".to_string()),
            allowed_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![user_id]),
    )
    .await
    .expect("seed protected tag grants");
    let environment = rg_db::ops::ci_environment_ops::create_with_approvers(
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
        vec![user_id],
    )
    .await
    .expect("seed CI environment grants");
    [
        Target::ProtectedBranch(branch.id),
        Target::ProtectedTag(tag.id),
        Target::CiEnvironment(environment.id),
    ]
}

async fn assert_rejected(db: &DatabaseConnection, targets: &[Target], ids: &[i64], expected: &str) {
    for &target in targets {
        let transaction = db.begin().await.expect("begin rejected write");
        let error = user_grants::replace(&transaction, target, Some(ids))
            .await
            .expect_err("invalid principal must reject the write");
        assert_eq!(
            user_grants::invalid_principal_message(&error).as_deref(),
            Some(expected),
            "different validation semantic for {target:?}: {error:#}"
        );
        transaction
            .rollback()
            .await
            .expect("roll back rejected write");
    }
}

async fn replace_all(db: &DatabaseConnection, targets: &[Target], ids: &[i64]) {
    for &target in targets {
        let transaction = db.begin().await.expect("begin grant replacement");
        user_grants::replace(&transaction, target, Some(ids))
            .await
            .expect("replace grants");
        transaction
            .commit()
            .await
            .expect("commit grant replacement");
    }
}

async fn assert_all_equal(db: &DatabaseConnection, targets: &[Target], expected: &[i64]) {
    for &target in targets {
        let mirror = match target {
            Target::ProtectedBranch(id) => {
                rg_db::entities::protected_branch::Entity::find_by_id(id)
                    .one(db)
                    .await
                    .unwrap()
                    .unwrap()
                    .allowed_push_user_ids
            }
            Target::ProtectedTag(id) => {
                rg_db::entities::protected_tag::Entity::find_by_id(id)
                    .one(db)
                    .await
                    .unwrap()
                    .unwrap()
                    .allowed_user_ids
            }
            Target::CiEnvironment(id) => {
                rg_db::entities::ci_environment::Entity::find_by_id(id)
                    .one(db)
                    .await
                    .unwrap()
                    .unwrap()
                    .allowed_approver_ids
            }
        };
        assert_eq!(
            user_grants::load_verified(db, target, mirror.as_deref())
                .await
                .expect("load mirror-verified grants"),
            expected,
            "different persisted grants for {target:?}"
        );
    }
}

#[tokio::test]
async fn all_writers_reject_invalid_principals_and_deleted_ids_cannot_revive() {
    let (db, _temp) = setup().await;
    let owner = seed_user(&db, "grant-owner", true).await;
    let live = seed_user(&db, "grant-live", true).await;
    let inactive = seed_user(&db, "grant-inactive", false).await;
    let retiring = seed_user(&db, "grant-retiring", true).await;
    let repo_id = seed_repo(&db, owner).await;
    let targets = seed_targets(&db, repo_id, live).await;

    assert_all_equal(&db, &targets, &[live]).await;
    assert_rejected(
        &db,
        &targets,
        &[9_999_999],
        "grant user 9999999 does not exist",
    )
    .await;
    assert_rejected(
        &db,
        &targets,
        &[inactive],
        &format!("grant user {inactive} is inactive"),
    )
    .await;
    assert_rejected(
        &db,
        &targets,
        &[live, live],
        &format!("user grant list contains duplicate user {live}"),
    )
    .await;

    assert!(
        rg_db::ops::user_ops::find_by_id(&db, retiring)
            .await
            .expect("preflight user lookup")
            .is_some(),
        "race fixture did not pass its preflight"
    );
    assert!(rg_db::ops::user_ops::begin_user_retirement(&db, retiring)
        .await
        .expect("begin deterministic retirement"));
    assert_rejected(
        &db,
        &targets,
        &[retiring],
        &format!("grant user {retiring} is being retired"),
    )
    .await;
    assert_all_equal(&db, &targets, &[live]).await;

    let doomed = seed_user(&db, "grant-doomed", true).await;
    replace_all(&db, &targets, &[doomed, live]).await;
    assert_all_equal(&db, &targets, &[live, doomed]).await;
    assert!(rg_db::ops::user_ops::delete_by_id(&db, doomed)
        .await
        .expect("delete granted user"));

    assert_eq!(
        rg_db::entities::protected_branch_push_grant::Entity::find()
            .filter(rg_db::entities::protected_branch_push_grant::Column::UserId.eq(doomed))
            .count(&db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        rg_db::entities::protected_tag_push_grant::Entity::find()
            .filter(rg_db::entities::protected_tag_push_grant::Column::UserId.eq(doomed))
            .count(&db)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        rg_db::entities::ci_environment_approver_grant::Entity::find()
            .filter(rg_db::entities::ci_environment_approver_grant::Column::UserId.eq(doomed))
            .count(&db)
            .await
            .unwrap(),
        0
    );
    assert_all_equal(&db, &targets, &[live]).await;

    assert_eq!(
        seed_user_with_id(&db, Some(doomed), "grant-replacement", true).await,
        doomed
    );
    assert_all_equal(&db, &targets, &[live]).await;

    let Target::ProtectedBranch(branch_id) = targets[0] else {
        unreachable!()
    };
    let branch = rg_db::entities::protected_branch::Entity::find_by_id(branch_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    let mut branch: rg_db::entities::protected_branch::ActiveModel = branch.into();
    branch.allowed_push_user_ids = Set(Some(format!("[{live},{doomed}]")));
    branch.update(&db).await.expect("inject mirror drift");
    let error = rg_db::ops::protected_branch_ops::list_rules_by_repo(&db, repo_id)
        .await
        .expect_err("authorization load must fail closed on mirror drift");
    assert!(
        format!("{error:#}").contains("JSON mirror disagrees"),
        "{error:#}"
    );
    replace_all(&db, &[targets[0]], &[live]).await;

    let Target::ProtectedTag(tag_id) = targets[1] else {
        unreachable!()
    };
    let tag = rg_db::entities::protected_tag::Entity::find_by_id(tag_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    let mut tag: rg_db::entities::protected_tag::ActiveModel = tag.into();
    tag.allowed_user_ids = Set(Some(format!("[{live},{doomed}]")));
    tag.update(&db).await.expect("inject tag mirror drift");
    let error = rg_db::ops::protected_tag_ops::list_rules_by_repo(&db, repo_id)
        .await
        .expect_err("tag authorization load must fail closed on mirror drift");
    assert!(
        format!("{error:#}").contains("JSON mirror disagrees"),
        "{error:#}"
    );
    replace_all(&db, &[targets[1]], &[live]).await;

    let Target::CiEnvironment(environment_id) = targets[2] else {
        unreachable!()
    };
    let environment = rg_db::entities::ci_environment::Entity::find_by_id(environment_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    let mut environment: rg_db::entities::ci_environment::ActiveModel = environment.into();
    environment.allowed_approver_ids = Set(Some(format!("[{live},{doomed}]")));
    let environment = environment
        .update(&db)
        .await
        .expect("inject environment mirror drift");
    let error = rg_db::ops::ci_environment_ops::allowed_approver_ids(&db, &environment)
        .await
        .expect_err("environment authorization load must fail closed on mirror drift");
    assert!(
        format!("{error:#}").contains("JSON mirror disagrees"),
        "{error:#}"
    );
}
