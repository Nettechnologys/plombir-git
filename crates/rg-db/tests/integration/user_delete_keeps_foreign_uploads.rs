//! card_1cfc81035e92: deleting an account must not delete other people's files.
//!
//! Three columns name the account that *produced* a piece of content —
//! `attachments.uploader_id`, `release_assets.uploader_id` and
//! `releases.author_id` — and none of those rows belong to it: they live in
//! whichever repository the file was uploaded into, which is routinely somebody
//! else's. All three used to be `ON DELETE CASCADE`, so `DELETE FROM users`
//! destroyed attachments inside other people's issues and whole releases of
//! other people's repositories, while
//! `attachments/<repo_id>/<uuid>/<file>` and
//! `releases/<owner>/<repo>/<release_id>/<asset_id>/<file>` stayed live with
//! nothing left able to name them.
//!
//! `m20260805_000002_uploads_outlive_their_uploader` made all three
//! `ON DELETE SET NULL`. This test asserts that rule where it is actually
//! enforced — in the database — rather than trusting one careful call site: the
//! content survives its author's departure, carrying a ghost uploader.
//!
//! The complement is `user_delete_cascades_repositories.rs`: what the account
//! *owns* is still destroyed by the same statement, which is why
//! `rg_core::user::service::delete_user` retires that storage first.

use rg_db::sea_orm::{ActiveValue::Set, DatabaseConnection, EntityTrait};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "plombir-git-ghost-uploads-{label}-{}.db",
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

/// The guest uploads into the host's repository: an attachment on the host's
/// issue, and a release with an asset. Then the guest's account is deleted.
#[tokio::test]
async fn deleting_an_account_keeps_the_files_it_left_in_another_repository() {
    let (db, _temp) = setup("foreign-uploads").await;
    let now = chrono::Utc::now();

    let host = seed_user(&db, "upload-host").await;
    let guest = seed_user(&db, "upload-guest").await;
    let repo = seed_repo(&db, host, "shared").await;
    let issue = seed_issue(&db, repo, host).await;

    let attachment = rg_db::ops::attachment_ops::create(
        &db,
        rg_db::entities::attachment::ActiveModel {
            uuid: Set("11111111-2222-3333-4444-555555555555".to_string()),
            repo_id: Set(repo),
            uploader_id: Set(Some(guest)),
            issue_id: Set(Some(issue)),
            filename: Set("trace.log".to_string()),
            blob_key: Set(format!(
                "attachments/{repo}/11111111-2222-3333-4444-555555555555/trace.log"
            )),
            content_type: Set("text/plain".to_string()),
            size: Set(5),
            download_count: Set(0),
            created_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed attachment uploaded by the guest");

    let release = rg_db::ops::release_ops::create(
        &db,
        rg_db::entities::release::ActiveModel {
            repo_id: Set(repo),
            tag_name: Set("v1.0.0".to_string()),
            target_commitish: Set("main".to_string()),
            title: Set("First".to_string()),
            is_draft: Set(false),
            is_prerelease: Set(false),
            author_id: Set(Some(guest)),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed release published by the guest");

    let asset = rg_db::ops::release_ops::create_asset(
        &db,
        rg_db::entities::release_asset::ActiveModel {
            release_id: Set(release.id),
            filename: Set("binary.tar.gz".to_string()),
            size: Set(3),
            content_type: Set("application/gzip".to_string()),
            download_count: Set(0),
            uploader_id: Set(Some(guest)),
            created_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed release asset uploaded by the guest");

    assert!(
        rg_db::ops::user_ops::delete_by_id(&db, guest)
            .await
            .expect("delete the guest account"),
        "the guest row was not there to delete"
    );

    let attachment = rg_db::ops::attachment_ops::find_by_id(&db, attachment.id)
        .await
        .expect("read the attachment after its uploader was deleted")
        .expect(
            "the attachment was destroyed with its uploader — its bytes are now unreachable in a \
             repository that is still alive",
        );
    assert_eq!(
        attachment.uploader_id, None,
        "the departed uploader was not ghosted"
    );

    let asset = rg_db::ops::release_ops::find_asset_by_id(&db, asset.id)
        .await
        .expect("read the release asset after its uploader was deleted")
        .expect("the release asset was destroyed with the account that uploaded it");
    assert_eq!(
        asset.uploader_id, None,
        "the departed uploader was not ghosted"
    );

    let release = rg_db::ops::release_ops::find_by_id(&db, release.id)
        .await
        .expect("read the release after its author was deleted")
        .expect(
            "the release was destroyed with the account that published it, taking every asset of \
             a repository that is still alive with it",
        );
    assert_eq!(
        release.author_id, None,
        "the departed author was not ghosted"
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
