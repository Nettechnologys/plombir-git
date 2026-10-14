//! `org_ops::remove_org_member` keeps the last `owner` and keeps the columns
//! that name the organization's owner on someone who still belongs.
//!
//! Its first statement is a no-op write to the organization row — the row
//! lock on PostgreSQL and MySQL, the writer slot on SQLite — so the statements
//! that follow have to be valid on every backend. These tests honour
//! `PLOMBIR_GIT_TEST_DATABASE_URL` and run in the PostgreSQL and MySQL smoke
//! jobs for that reason.

use rg_db::entities::{organization, repository};
use rg_db::ops::org_ops::{self, RemoveOrgMemberOutcome};
use rg_db::sea_orm::{DatabaseConnection, EntityTrait, NotSet, Set};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
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

async fn setup() -> (DatabaseConnection, Option<TempDb>, String) {
    let (url, temp) = match std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => (url, None),
        _ => {
            let temp = TempDb {
                path: std::env::temp_dir().join(format!(
                    "plombir-git-org-member-removal-{}.db",
                    uuid::Uuid::new_v4().simple()
                )),
            };
            (
                format!("sqlite://{}?mode=rwc", temp.path.display()),
                Some(temp),
            )
        }
    };
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    let suffix = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    (db, temp, suffix)
}

async fn user(db: &DatabaseConnection, name: &str) -> i64 {
    rg_db::ops::user_ops::create_user(db, name, &format!("{name}@example.invalid"), "", name)
        .await
        .expect("create account")
        .id
}

#[tokio::test]
async fn the_last_owner_stays_and_the_recorded_owner_moves_to_one_who_does() {
    let (db, _temp, suffix) = setup().await;
    let creator = user(&db, &format!("rmc{suffix}")).await;
    let second = user(&db, &format!("rms{suffix}")).await;
    let member = user(&db, &format!("rmm{suffix}")).await;
    let outsider = user(&db, &format!("rmo{suffix}")).await;
    let org = org_ops::create_org(
        &db,
        &format!("rmorg{suffix}"),
        None,
        None,
        creator,
        "public",
    )
    .await
    .expect("create organization");
    org_ops::add_org_member(&db, org.id, second, "owner")
        .await
        .unwrap();
    org_ops::add_org_member(&db, org.id, member, "member")
        .await
        .unwrap();
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(creator),
            name: Set(format!("rmrepo{suffix}")),
            description: Set(None),
            is_private: Set(true),
            default_branch: Set("main".to_string()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(Some(org.id)),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            origin_repo_id: Set(None),
        },
    )
    .await
    .expect("create the organization's repository");

    assert_eq!(
        org_ops::remove_org_member(&db, org.id, outsider)
            .await
            .unwrap(),
        RemoveOrgMemberOutcome::NotMember
    );
    assert_eq!(
        org_ops::remove_org_member(&db, org.id, member)
            .await
            .unwrap(),
        RemoveOrgMemberOutcome::Removed
    );
    assert_eq!(
        org_ops::remove_org_member(&db, org.id, creator)
            .await
            .unwrap(),
        RemoveOrgMemberOutcome::Removed
    );
    let reloaded = organization::Entity::find_by_id(org.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.owner_id, second);
    let reloaded = repository::Entity::find_by_id(repo.id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.owner_id, second);

    assert_eq!(
        org_ops::remove_org_member(&db, org.id, second)
            .await
            .unwrap(),
        RemoveOrgMemberOutcome::LastOwner
    );
    assert!(org_ops::find_org_member(&db, org.id, second)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        org_ops::list_orgs_solely_owned_by(&db, second)
            .await
            .unwrap()
            .into_iter()
            .map(|org| org.id)
            .collect::<Vec<_>>(),
        vec![org.id]
    );
}
