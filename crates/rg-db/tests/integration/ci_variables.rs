//! card_a6f59b3d45f5: `vars.*` — a repository's variables override its
//! organization's, and both live exactly as long as their owner.
//!
//! The overlay is what a workflow reads (`ci_variable_ops::for_repo`); the
//! lifetimes are enforced by the database itself — `ON DELETE CASCADE` to the
//! owner, `ON DELETE SET NULL` to the author — and asserted here through the
//! statements the application really runs.

use rg_db::ops::ci_variable_ops::{self, Owner, Written};
use rg_db::sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait};

struct TempDb {
    path: std::path::PathBuf,
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
    let temp = TempDb {
        path: std::env::temp_dir().join(format!(
            "plombir-git-ci-variables-{}.db",
            uuid::Uuid::new_v4().simple()
        )),
    };
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", temp.path.display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        4,
    )
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

async fn seed_repo(db: &DatabaseConnection, owner_id: i64, org_id: Option<i64>, name: &str) -> i64 {
    let now = chrono::Utc::now();
    rg_db::ops::repo_ops::create(
        db,
        rg_db::entities::repository::ActiveModel {
            owner_id: Set(owner_id),
            org_id: Set(org_id),
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

fn pairs(items: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    items
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

#[tokio::test]
async fn a_repository_overrides_its_organization_and_both_die_with_their_owner() {
    let (db, _temp) = setup().await;
    let owner = seed_user(&db, "vars-owner").await;
    let author = seed_user(&db, "vars-author").await;
    let org = rg_db::ops::org_ops::create_org(&db, "vars-org", None, None, owner, "public")
        .await
        .unwrap();
    let repo = seed_repo(&db, owner, Some(org.id), "app").await;
    let other = seed_repo(&db, owner, None, "solo").await;

    for (scope, name, value) in [
        (Owner::Organization(org.id), "REGION", "eu"),
        (Owner::Organization(org.id), "TIER", "org-tier"),
        (Owner::Repository(repo), "TIER", "repo-tier"),
        (Owner::Repository(other), "ONLY", "solo"),
    ] {
        assert_eq!(
            ci_variable_ops::upsert(&db, scope, name, value, author)
                .await
                .unwrap(),
            Written::Created
        );
    }
    assert_eq!(
        ci_variable_ops::upsert(&db, Owner::Repository(repo), "TIER", "repo-tier", author)
            .await
            .unwrap(),
        Written::Updated
    );
    assert_eq!(
        ci_variable_ops::for_repo(&db, repo).await.unwrap(),
        pairs(&[("REGION", "eu"), ("TIER", "repo-tier")])
    );
    assert_eq!(
        ci_variable_ops::for_repo(&db, other).await.unwrap(),
        pairs(&[("ONLY", "solo")])
    );

    // The author's account goes; what they configured stays.
    rg_db::entities::user::Entity::delete_by_id(author)
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(ci_variable_ops::for_repo(&db, repo).await.unwrap().len(), 2);

    // An organization being retired no longer configures its repositories.
    assert!(rg_db::ops::org_ops::begin_org_retirement(&db, org.id)
        .await
        .unwrap());
    assert_eq!(
        ci_variable_ops::for_repo(&db, repo).await.unwrap(),
        pairs(&[("TIER", "repo-tier")])
    );

    // The owners go, and their variables with them.
    rg_db::entities::repository::Entity::delete_by_id(repo)
        .exec(&db)
        .await
        .unwrap();
    assert!(ci_variable_ops::list(&db, Owner::Repository(repo))
        .await
        .unwrap()
        .is_empty());
    assert!(rg_db::ops::org_ops::delete_org(&db, org.id).await.unwrap());
    assert!(ci_variable_ops::list(&db, Owner::Organization(org.id))
        .await
        .unwrap()
        .is_empty());
    assert!(
        ci_variable_ops::delete(&db, Owner::Repository(other), "ONLY")
            .await
            .unwrap()
    );
    assert!(
        !ci_variable_ops::delete(&db, Owner::Repository(other), "ONLY")
            .await
            .unwrap()
    );
}
