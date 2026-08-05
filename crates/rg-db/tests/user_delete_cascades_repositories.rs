//! card_fd0ebc0adfcd: the premise the account-deletion guard rests on.
//!
//! `repositories.owner_id` is declared `REFERENCES users(id) ON DELETE CASCADE`
//! (`m20260424_000002_create_repositories`), foreign keys are enabled on every
//! pooled SQLite connection, and the SQLite table rebuild in
//! `m20260730_000001_repositories_namespace_unique` re-emits the clause. So
//! `DELETE FROM users` is not a row removal — it destroys repository rows,
//! including the ones in an organization's namespace that merely carry this
//! account as `owner_id`, and leaves every byte they named live in storage.
//!
//! This test is here so that fact is asserted rather than remembered. If a
//! future migration drops the cascade, `rg_core::user::service::delete_user`'s
//! refusals become over-strict rather than load-bearing, and whoever changes it
//! should find out here.

use rg_db::sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-user-cascade-{label}-{}.db",
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
    let user = rg_db::entities::user::ActiveModel {
        username: Set(username.to_string()),
        email: Set(format!("{username}@example.com")),
        password_hash: Set("x".to_string()),
        is_active: Set(true),
        is_admin: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    rg_db::entities::user::Entity::insert(user)
        .exec(db)
        .await
        .expect("seed user")
        .last_insert_id
}

async fn seed_repo(db: &DatabaseConnection, owner_id: i64, org_id: Option<i64>, name: &str) -> i64 {
    let now = chrono::Utc::now();
    let repo = rg_db::entities::repository::ActiveModel {
        owner_id: Set(owner_id),
        org_id: Set(org_id),
        name: Set(name.to_string()),
        is_private: Set(false),
        default_branch: Set("main".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    rg_db::ops::repo_ops::create(db, repo)
        .await
        .expect("seed repository")
        .id
}

/// Both namespaces, one statement. The personal repository and the
/// organization's repository disappear together, which is why deleting an
/// account has to retire storage first and refuse the organization outright.
#[tokio::test]
async fn deleting_a_user_row_destroys_every_repository_it_owns() {
    let (db, _temp) = setup("both-namespaces").await;

    let owner = seed_user(&db, "cascade-owner").await;
    let bystander = seed_user(&db, "cascade-bystander").await;
    let now = chrono::Utc::now();
    let org =
        rg_db::entities::organization::Entity::insert(rg_db::entities::organization::ActiveModel {
            name: Set("cascade-org".to_string()),
            owner_id: Set(owner),
            visibility: Set("public".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        })
        .exec(&db)
        .await
        .expect("seed organization")
        .last_insert_id;

    let personal = seed_repo(&db, owner, None, "personal").await;
    let organization_scoped = seed_repo(&db, owner, Some(org), "org-scoped").await;
    let untouched = seed_repo(&db, bystander, None, "untouched").await;

    assert!(
        rg_db::ops::user_ops::delete_by_id(&db, owner)
            .await
            .expect("delete the owner row"),
        "the owner row was not there to delete"
    );

    for (repo_id, what) in [
        (personal, "personal repository"),
        (organization_scoped, "organization repository"),
    ] {
        assert!(
            rg_db::ops::repo_ops::find_by_id(&db, repo_id)
                .await
                .expect("read repository row after the owner was deleted")
                .is_none(),
            "the {what} survived its owner's deletion — the cascade this guard exists for is gone, \
             and `delete_user`'s refusals should be revisited"
        );
    }
    assert!(
        rg_db::ops::repo_ops::find_by_id(&db, untouched)
            .await
            .expect("read the bystander's repository")
            .is_some(),
        "the cascade reached a repository owned by somebody else"
    );

    // No foreign key backs `organizations.owner_id`, so the organization is the
    // half that stays — pointing at a user id nothing resolves.
    assert!(
        rg_db::ops::org_ops::get_org(&db, org)
            .await
            .expect("read organization after the owner was deleted")
            .is_some_and(|organization| organization.owner_id == owner),
        "the organization no longer outlives its owner — the refusal in `delete_user` can be \
         relaxed once that is true by construction"
    );
}
