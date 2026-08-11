//! A credential typed into a URL (card_e71e1ba04ae7).
//!
//! `mirrors.password_encrypted` exists so a database dump does not hand out the
//! credential to somebody else's remote. A URL written as
//! `https://user:token@host/repo.git` puts that same secret in the row next to
//! it — `mirrors.url`, `import_tasks.source_url`, `webhooks.url` — all three of
//! them plaintext columns, and two of them served back out through the API.
//!
//! What these tests hold down, per table:
//!
//! * **mirrors** — the credential is moved into `username` +
//!   `password_encrypted` on the way in, so the row (and therefore
//!   `GET …/mirror`) never holds the token, and a sync still authenticates.
//! * **import_tasks** — the token is taken out of the stored URL and lives in
//!   memory for the run, the way an explicitly-supplied one already did.
//! * **webhooks** — refused outright: there is nothing to move it into, and a
//!   webhook authenticates by signing its payload.
//! * **legacy rows** — the startup passes convert what was stored before any of
//!   the above existed, and repeat harmlessly.

use std::path::Path;

use rg_core::auth::encryption;
use rg_core::webhook::service::{CreateWebhookRequest, UpdateWebhookRequest};
use sea_orm::{ActiveValue::NotSet, DatabaseConnection, EntityTrait, Set};

const ENCRYPTION_KEY: &str = "the-at-rest-key-this-instance-was-built-with";
const TOKEN: &str = "ghp-SECRET-TOKEN";

async fn fresh_db(dir: &Path) -> DatabaseConnection {
    let db = crate::common::migrated_sqlite(&dir.join("test.db"), 2).await;
    db
}

/// An account and a repository for everything else to hang off.
async fn owner_and_repo(db: &DatabaseConnection) -> (i64, i64) {
    let user = rg_db::ops::user_ops::create_user(db, "alice", "alice@example.invalid", "", "Alice")
        .await
        .expect("create user");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        db,
        rg_db::entities::repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set("deploy".to_string()),
            description: Set(None),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            origin_repo_id: Set(None),
        },
    )
    .await
    .expect("create repository");
    (user.id, repo.id)
}

/// The mirror row as the database holds it, not as a service returns it.
async fn stored_mirror(db: &DatabaseConnection, id: i64) -> rg_db::entities::mirror::Model {
    rg_db::entities::mirror::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("read mirror")
        .expect("the mirror is there")
}

// ── mirrors ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_mirror_url_with_a_credential_stores_the_secret_encrypted_and_the_url_bare() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fresh_db(dir.path()).await;
    let (_user, repo_id) = owner_and_repo(&db).await;

    let mirror = rg_core::mirror::service::create_mirror(
        &db,
        repo_id,
        format!("https://sync-bot:{TOKEN}@example.com/o/upstream.git"),
        None,
        None,
        3600,
        ENCRYPTION_KEY,
    )
    .await
    .expect("a URL with a credential is accepted, not refused");

    let stored = stored_mirror(&db, mirror.id).await;
    assert_eq!(
        stored.url, "https://example.com/o/upstream.git",
        "the stored URL still carries the userinfo section"
    );
    assert!(
        !stored.url.contains(TOKEN),
        "the token survived in `mirrors.url`: {}",
        stored.url
    );
    // Not merely absent — moved, and moved into the column that encrypts it.
    assert_eq!(stored.username.as_deref(), Some("sync-bot"));
    let ciphertext = stored
        .password_encrypted
        .as_deref()
        .expect("the credential was kept, not dropped");
    assert_ne!(ciphertext, TOKEN, "the column holds the token verbatim");
    assert_eq!(
        encryption::decrypt(ciphertext, &encryption::derive_key(ENCRYPTION_KEY))
            .expect("the stored credential opens with the instance key"),
        TOKEN,
        "the credential no longer round-trips, so the mirror can no longer sync"
    );

    // What the API returns is built from this row, so the endpoint cannot leak
    // what the row does not hold.
    assert!(!format!("{stored:?}").contains(TOKEN));
}

#[tokio::test]
async fn an_explicit_credential_outranks_the_one_in_the_url() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fresh_db(dir.path()).await;
    let (_user, repo_id) = owner_and_repo(&db).await;

    let mirror = rg_core::mirror::service::create_mirror(
        &db,
        repo_id,
        format!("https://from-url:{TOKEN}@example.com/o/upstream.git"),
        Some("from-form".to_string()),
        Some("form-password".to_string()),
        3600,
        ENCRYPTION_KEY,
    )
    .await
    .expect("create the mirror");

    let stored = stored_mirror(&db, mirror.id).await;
    assert_eq!(stored.username.as_deref(), Some("from-form"));
    assert_eq!(
        encryption::decrypt(
            stored.password_encrypted.as_deref().expect("a credential"),
            &encryption::derive_key(ENCRYPTION_KEY)
        )
        .unwrap(),
        "form-password"
    );
    // The URL's credential loses — and is still not stored anywhere.
    assert!(!stored.url.contains(TOKEN));
    assert_eq!(stored.url, "https://example.com/o/upstream.git");
}

#[tokio::test]
async fn changing_the_url_to_one_with_a_credential_splits_it_too() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fresh_db(dir.path()).await;
    let (_user, repo_id) = owner_and_repo(&db).await;

    let mirror = rg_core::mirror::service::create_mirror(
        &db,
        repo_id,
        "https://example.com/o/upstream.git".to_string(),
        None,
        None,
        3600,
        ENCRYPTION_KEY,
    )
    .await
    .expect("create the mirror");

    rg_core::mirror::service::update_mirror(
        &db,
        repo_id,
        Some(format!("https://sync-bot:{TOKEN}@example.com/o/moved.git")),
        None,
        None,
        None,
        None,
        ENCRYPTION_KEY,
    )
    .await
    .expect("update the mirror");

    let stored = stored_mirror(&db, mirror.id).await;
    assert_eq!(stored.url, "https://example.com/o/moved.git");
    assert_eq!(stored.username.as_deref(), Some("sync-bot"));
    assert_eq!(
        encryption::decrypt(
            stored.password_encrypted.as_deref().expect("a credential"),
            &encryption::derive_key(ENCRYPTION_KEY)
        )
        .unwrap(),
        TOKEN
    );
}

#[tokio::test]
async fn an_ambiguous_credential_is_refused_with_an_instruction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fresh_db(dir.path()).await;
    let (_user, repo_id) = owner_and_repo(&db).await;

    let error = rg_core::mirror::service::create_mirror(
        &db,
        repo_id,
        format!("https://{TOKEN}@example.com/o/upstream.git"),
        None,
        None,
        3600,
        ENCRYPTION_KEY,
    )
    .await
    .expect_err("a lone userinfo could be a login or a token — it must not be guessed at");

    let rendered = format!("{error:#}");
    assert!(rendered.contains("password field"), "{rendered}");
    assert!(
        !rendered.contains(TOKEN),
        "the refusal quoted the credential back: {rendered}"
    );
    // And nothing was written.
    assert!(rg_core::mirror::service::get_mirror(&db, repo_id)
        .await
        .expect("read")
        .is_none());
}

#[tokio::test]
async fn the_startup_pass_converts_a_legacy_mirror_url_and_repeats_harmlessly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fresh_db(dir.path()).await;
    let (_user, repo_id) = owner_and_repo(&db).await;

    // A row as it was written before create/update split the URL.
    let now = chrono::Utc::now();
    let legacy = rg_db::ops::mirror_ops::create(
        &db,
        rg_db::entities::mirror::ActiveModel {
            repo_id: Set(repo_id),
            url: Set(format!(
                "https://sync-bot:{TOKEN}@example.com/o/upstream.git"
            )),
            username: Set(None),
            password_encrypted: Set(None),
            sync_interval_seconds: Set(3600),
            next_sync_at: Set(None),
            last_sync_at: Set(None),
            last_sync_error: Set(None),
            status: Set("active".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("write the legacy row");

    let lifted = rg_core::mirror::service::lift_legacy_url_credentials(&db, ENCRYPTION_KEY)
        .await
        .expect("the startup pass runs");
    assert_eq!(lifted, 1, "the legacy row was not converted");

    let stored = stored_mirror(&db, legacy.id).await;
    assert_eq!(stored.url, "https://example.com/o/upstream.git");
    assert_eq!(stored.username.as_deref(), Some("sync-bot"));
    assert_eq!(
        encryption::decrypt(
            stored.password_encrypted.as_deref().expect("a credential"),
            &encryption::derive_key(ENCRYPTION_KEY)
        )
        .unwrap(),
        TOKEN,
        "the conversion lost the credential instead of moving it"
    );

    // A restart must cost one query and rewrite nothing.
    assert_eq!(
        rg_core::mirror::service::lift_legacy_url_credentials(&db, ENCRYPTION_KEY)
            .await
            .expect("the pass runs again"),
        0
    );
}

#[tokio::test]
async fn an_scp_like_remote_is_left_alone_by_the_startup_pass() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fresh_db(dir.path()).await;
    let (_user, repo_id) = owner_and_repo(&db).await;

    // `git@host:path` has an `@` and no secret: the pass must not touch it, or
    // every ssh mirror on the instance loses its login on the next restart.
    const SSH_REMOTE: &str = "git@github.com:owner/repo.git";
    let now = chrono::Utc::now();
    let mirror = rg_db::ops::mirror_ops::create(
        &db,
        rg_db::entities::mirror::ActiveModel {
            repo_id: Set(repo_id),
            url: Set(SSH_REMOTE.to_string()),
            username: Set(None),
            password_encrypted: Set(None),
            sync_interval_seconds: Set(3600),
            next_sync_at: Set(None),
            last_sync_at: Set(None),
            last_sync_error: Set(None),
            status: Set("active".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("write the ssh mirror");

    assert_eq!(
        rg_core::mirror::service::lift_legacy_url_credentials(&db, ENCRYPTION_KEY)
            .await
            .expect("the startup pass runs"),
        0
    );
    assert_eq!(stored_mirror(&db, mirror.id).await.url, SSH_REMOTE);
}

// ── import tasks ────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_import_source_url_is_stored_without_its_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fresh_db(dir.path()).await;
    let (user_id, _repo) = owner_and_repo(&db).await;

    // `.invalid` never resolves, so the worker this spawns fails at the SSRF
    // guard instead of reaching the network. What is under test is the row it
    // was started from.
    let task = rg_core::import::service::start_import(
        &db,
        user_id,
        "git".to_string(),
        format!("https://importer:{TOKEN}@source.invalid/o/repo.git"),
        "alice".to_string(),
        "imported".to_string(),
        None,
        true,
        false,
        false,
        false,
        false,
        false,
        false,
        &Default::default(),
        dir.path(),
    )
    .await
    .expect("the import is accepted");

    assert!(
        !task.source_url.contains(TOKEN),
        "the token was written to `import_tasks.source_url`: {}",
        task.source_url
    );
    // The login half is not a secret and is what git pairs the token with, so
    // it stays — this table has no column to move it to.
    assert_eq!(
        task.source_url,
        "https://importer@source.invalid/o/repo.git"
    );
}

#[tokio::test]
async fn the_startup_pass_strips_a_legacy_import_source_url() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fresh_db(dir.path()).await;
    let (user_id, _repo) = owner_and_repo(&db).await;

    let now = chrono::Utc::now();
    let legacy = rg_db::ops::import_task_ops::create(
        &db,
        rg_db::entities::import_task::ActiveModel {
            user_id: Set(user_id),
            repo_id: Set(None),
            platform: Set("git".to_string()),
            source_url: Set(format!(
                "https://importer:{TOKEN}@source.invalid/o/repo.git"
            )),
            target_owner: Set("alice".to_string()),
            target_name: Set("imported".to_string()),
            status: Set("completed".to_string()),
            progress: Set(100),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("write the legacy task");

    assert_eq!(
        rg_core::import::service::strip_legacy_source_url_credentials(&db)
            .await
            .expect("the startup pass runs"),
        1
    );
    let stored = rg_db::entities::import_task::Entity::find_by_id(legacy.id)
        .one(&db)
        .await
        .expect("read")
        .expect("the task is there");
    assert_eq!(
        stored.source_url,
        "https://importer@source.invalid/o/repo.git"
    );

    // Idempotent.
    assert_eq!(
        rg_core::import::service::strip_legacy_source_url_credentials(&db)
            .await
            .expect("the pass runs again"),
        0
    );
}

// ── webhooks ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_webhook_url_may_not_carry_a_credential() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fresh_db(dir.path()).await;
    let (_user, repo_id) = owner_and_repo(&db).await;

    let request = |url: &str| CreateWebhookRequest {
        url: url.to_string(),
        content_type: None,
        secret: Some("signing-secret".to_string()),
        active: Some(true),
        events: vec!["push".to_string()],
    };

    let error = rg_core::webhook::service::create_webhook(
        &db,
        repo_id,
        &request(&format!(
            "https://receiver:{TOKEN}@hooks.example.invalid/fk"
        )),
        ENCRYPTION_KEY,
    )
    .await
    .expect_err("there is nowhere to store a webhook credential, so it must be refused");
    let rendered = format!("{error:#}");
    assert!(rendered.contains("signing secret"), "{rendered}");
    assert!(!rendered.contains(TOKEN), "{rendered}");

    // A plain URL still registers, so the rejection is about the credential and
    // not about webhooks in general.
    let hook = rg_core::webhook::service::create_webhook(
        &db,
        repo_id,
        &request("https://hooks.example.invalid/fk"),
        ENCRYPTION_KEY,
    )
    .await
    .expect("a URL without a credential is fine");

    // …and the same rule applies to moving an existing hook to such a URL.
    let error = rg_core::webhook::service::update_webhook(
        &db,
        &hook,
        &UpdateWebhookRequest {
            url: Some(format!("https://receiver:{TOKEN}@hooks.example.invalid/fk")),
            content_type: None,
            secret: None,
            active: None,
            events: None,
        },
        ENCRYPTION_KEY,
    )
    .await
    .expect_err("update is the other door into the same column");
    assert!(!format!("{error:#}").contains(TOKEN));
}

#[tokio::test]
async fn the_startup_pass_strips_a_legacy_webhook_url() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fresh_db(dir.path()).await;
    let (_user, repo_id) = owner_and_repo(&db).await;

    let now = chrono::Utc::now();
    let legacy = rg_db::ops::webhook_ops::create_webhook(
        &db,
        rg_db::entities::webhook::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            url: Set(format!("https://receiver:{TOKEN}@hooks.example.invalid/fk")),
            content_type: Set("json".to_string()),
            secret_encrypted: Set(None),
            active: Set(true),
            events: Set("push".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .expect("write the legacy hook");

    assert_eq!(
        rg_core::webhook::service::strip_legacy_url_credentials(&db)
            .await
            .expect("the startup pass runs"),
        1
    );
    let stored = rg_db::entities::webhook::Entity::find_by_id(legacy.id)
        .one(&db)
        .await
        .expect("read")
        .expect("the hook is there");
    assert_eq!(stored.url, "https://hooks.example.invalid/fk");

    // Idempotent.
    assert_eq!(
        rg_core::webhook::service::strip_legacy_url_credentials(&db)
            .await
            .expect("the pass runs again"),
        0
    );
}
