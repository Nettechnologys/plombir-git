//! card_a2123e31ee6e: deleting an account must not reconfigure other people's
//! repositories.
//!
//! Five columns named the account that *configured* something —
//! `ci_secrets.created_by_id`, `deploy_keys.created_by_id`,
//! `commit_statuses.creator_id`, `boards.created_by` and
//! `ci_environment_approvals.approved_by` — and none of those rows belong to it:
//! they live in the namespace of whichever repository or organization they
//! configure, which is routinely somebody else's. All five used to be
//! `ON DELETE CASCADE`, so `DELETE FROM users` silently took a live repository's
//! CI secrets, its deploy key, the check results on its commits and whole boards
//! with every column and card on them — from an owner who never learns that a
//! collaborator's account was removed.
//!
//! `m20260805_000004_repo_config_outlives_its_author` made all five
//! `ON DELETE SET NULL`. This test asserts that rule where it is actually
//! enforced — in the database, through the ops the application really calls —
//! rather than trusting one careful call site.
//!
//! The sibling is `user_delete_keeps_foreign_uploads.rs` (the same class for the
//! three columns that own bytes); the complement is
//! `user_delete_cascades_repositories.rs`: what the account *owns* is still
//! destroyed by the same statement.

use rg_db::sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "plombir-git-ghost-config-{label}-{}.db",
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

async fn seed_issue(db: &DatabaseConnection, repo_id: i64, author_id: i64) -> i64 {
    let now = chrono::Utc::now();
    rg_db::entities::issue::Entity::insert(rg_db::entities::issue::ActiveModel {
        repo_id: Set(repo_id),
        number: Set(1),
        title: Set("A bug".to_string()),
        state: Set("open".to_string()),
        author_id: Set(author_id),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .exec(db)
    .await
    .expect("seed issue")
    .last_insert_id
}

/// The guest configures the host's repository: a CI secret, a deploy key, a
/// commit status, a board with a column and a card, an approval of a protected
/// deployment and time logged on the host's issue. Then the guest's account is
/// deleted.
#[tokio::test]
async fn deleting_an_account_keeps_the_configuration_it_left_in_another_repository() {
    let (db, _temp) = setup("foreign-config").await;
    let now = chrono::Utc::now();

    let host = seed_user(&db, "config-host").await;
    let guest = seed_user(&db, "config-guest").await;
    let repo = seed_repo(&db, host, "shared").await;

    rg_db::ops::ci_secret_ops::upsert(&db, repo, "DEPLOY_TOKEN", "ciphertext", guest)
        .await
        .expect("seed the CI secret the guest set on the host's repository");

    let key = rg_db::ops::deploy_key_ops::create(
        &db,
        rg_db::entities::deploy_key::ActiveModel {
            repo_id: Set(repo),
            created_by_id: Set(Some(guest)),
            title: Set("ci".to_string()),
            public_key: Set("ssh-ed25519 AAAA".to_string()),
            fingerprint: Set("SHA256:deadbeef".to_string()),
            read_only: Set(true),
            created_at: Set(now),
            last_used_at: Set(None),
            ..Default::default()
        },
    )
    .await
    .expect("seed the deploy key the guest added to the host's repository");

    let status = rg_db::ops::commit_status_ops::create_or_update(
        &db,
        repo,
        "c0ffee",
        "ci/build",
        rg_db::entities::commit_status::ActiveModel {
            repo_id: Set(repo),
            sha: Set("c0ffee".to_string()),
            state: Set("success".to_string()),
            context: Set("ci/build".to_string()),
            description: Set(None),
            target_url: Set(None),
            creator_id: Set(Some(guest)),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed the commit status the guest reported on the host's repository")
    .expect("the host repository exists while its status is seeded");

    let board = rg_db::ops::board_ops::create_board(
        &db,
        rg_db::entities::board::ActiveModel {
            repo_id: Set(Some(repo)),
            org_id: Set(None),
            name: Set("Roadmap".to_string()),
            description: Set(None),
            created_by: Set(Some(guest)),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed the board the guest created in the host's repository");
    let column = rg_db::ops::board_ops::create_column(
        &db,
        rg_db::entities::board_column::ActiveModel {
            board_id: Set(board.id),
            name: Set("To do".to_string()),
            color: Set(None),
            position: Set(0),
            created_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed the board's column");
    let card = rg_db::ops::board_ops::create_card(
        &db,
        rg_db::entities::board_card::ActiveModel {
            column_id: Set(column.id),
            issue_id: Set(None),
            note: Set(Some("ship it".to_string())),
            position: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed the board's card");

    let environment = rg_db::ops::ci_environment_ops::create_with_approvers(
        &db,
        rg_db::entities::ci_environment::ActiveModel {
            repo_id: Set(repo),
            name: Set("production".to_string()),
            protected: Set(true),
            required_approvals: Set(1),
            allowed_approver_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Vec::new(),
    )
    .await
    .expect("seed the protected environment");
    // `pipeline_jobs` predates the ops layer and declares no foreign keys, so
    // the job the approval hangs off is inserted directly.
    db.execute_unprepared(
        "INSERT INTO pipeline_jobs (id, stage_id, name, script, status)
         VALUES (1, 1, 'deploy', 'make deploy', 'success')",
    )
    .await
    .expect("seed the job that was deployed");
    assert!(
        rg_db::ops::ci_environment_ops::add_approval(&db, 1, environment.id, guest)
            .await
            .expect("seed the guest's approval of the protected deployment"),
        "the approval was not recorded"
    );

    let issue = seed_issue(&db, repo, host).await;
    let entry = rg_db::ops::time_entry_ops::create(
        &db,
        rg_db::entities::time_entry::ActiveModel {
            issue_id: Set(issue),
            user_id: Set(Some(guest)),
            duration_minutes: Set(180),
            description: Set(Some("debugging".to_string())),
            created_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed the time the guest logged on the host's issue");

    assert!(
        rg_db::ops::user_ops::delete_by_id(&db, guest)
            .await
            .expect("delete the guest account"),
        "the guest row was not there to delete"
    );

    let secret = rg_db::ops::ci_secret_ops::find_by_repo_and_name(&db, repo, "DEPLOY_TOKEN")
        .await
        .expect("read the CI secret after its author was deleted")
        .expect(
            "the CI secret died with the account that set it — the host's pipelines now fail on \
             an empty variable",
        );
    assert_eq!(secret.created_by_id, None, "the author was not ghosted");
    assert_eq!(
        secret.encrypted_value, "ciphertext",
        "the secret's value did not survive its author"
    );

    let key = rg_db::ops::deploy_key_ops::find_by_id(&db, key.id)
        .await
        .expect("read the deploy key after its author was deleted")
        .expect(
            "the deploy key died with the account that added it — the host's deployments now \
             silently have no access",
        );
    assert_eq!(key.created_by_id, None, "the author was not ghosted");

    let statuses = rg_db::ops::commit_status_ops::list_by_sha(&db, repo, "c0ffee")
        .await
        .expect("read the commit statuses after their author was deleted");
    assert_eq!(
        statuses.len(),
        1,
        "the commit's check result died with the account that reported it"
    );
    assert_eq!(statuses[0].id, status.id);
    assert_eq!(statuses[0].creator_id, None, "the author was not ghosted");

    let board = rg_db::ops::board_ops::find_board_by_id(&db, board.id)
        .await
        .expect("read the board after its author was deleted")
        .expect("the board died with the account that created it");
    assert_eq!(board.created_by, None, "the author was not ghosted");
    assert!(
        rg_db::ops::board_ops::find_column_by_id(&db, column.id)
            .await
            .expect("read the board column")
            .is_some(),
        "the board's column died with the account that created the board"
    );
    assert!(
        rg_db::ops::board_ops::find_card_by_id(&db, card.id)
            .await
            .expect("read the board card")
            .is_some(),
        "the board's card died with the account that created the board"
    );

    let approvals = rg_db::entities::ci_environment_approval::Entity::find()
        .filter(rg_db::entities::ci_environment_approval::Column::JobId.eq(1))
        .all(&db)
        .await
        .expect("read the environment approvals after the approver was deleted");
    assert_eq!(
        approvals.len(),
        1,
        "the record of who approved the protected deployment died with the approver, so nothing \
         says it was ever approved"
    );
    assert_eq!(
        approvals[0].approved_by, None,
        "the approver was not ghosted"
    );

    let entry = rg_db::ops::time_entry_ops::find_by_id(&db, entry.id)
        .await
        .expect("read the time entry after its author was deleted")
        .expect("the hours logged on the host's issue died with the account that logged them");
    assert_eq!(entry.user_id, None, "the author was not ghosted");
    assert_eq!(
        rg_db::ops::time_entry_ops::total_minutes_by_issue(&db, issue)
            .await
            .expect("read the issue's total tracked time"),
        180,
        "the issue's total tracked time fell when a contributor's account was deleted"
    );

    // The complement, in the same statement: what the account owned is still
    // reached by the cascade, which is why `delete_user` retires that storage
    // before the row goes.
    assert!(
        rg_db::ops::repo_ops::find_by_id(&db, repo)
            .await
            .expect("read the host's repository")
            .is_some(),
        "deleting the guest reached the host's repository"
    );
}
