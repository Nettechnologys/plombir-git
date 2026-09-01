//! The webhook HMAC secret at rest (card_9e46363314f4).
//!
//! `webhooks.secret` used to store the operator's signing key verbatim: a
//! database dump handed out the key every delivery to that receiver is signed
//! with, and `forgekeep rotate-encryption-key` reported success without ever
//! looking at the column. It cannot be hashed the way a runner token is — the
//! dispatcher reads it back on every delivery — so the fix is the treatment
//! every other recoverable secret here gets: AES-256-GCM under the instance's
//! at-rest key, registered in `auth::encrypted_columns`.
//!
//! What these tests hold down: the column never contains what the operator
//! typed, the value still round-trips, an upgrade does not take existing hooks
//! off the air, and the rotation report names the column.

use std::path::Path;

use rg_core::auth::encrypted_columns::LABELS;
use rg_core::auth::encryption;
use rg_core::webhook::service::{seal_legacy_secrets, CreateWebhookRequest, UpdateWebhookRequest};
use sea_orm::{ActiveValue::NotSet, DatabaseConnection, EntityTrait, Set};

const ENCRYPTION_KEY: &str = "the-at-rest-key-this-instance-was-built-with";
const SECRET: &str = "s3cr3t-the-receiver-also-knows";

async fn fresh_db(dir: &Path) -> DatabaseConnection {
    let db = crate::common::migrated_sqlite(&dir.join("test.db"), 2).await;
    db
}

/// A repository to hang webhooks off.
async fn repo_id(db: &DatabaseConnection) -> i64 {
    let user = rg_db::ops::user_ops::create_user(db, "alice", "alice@example.invalid", "", "Alice")
        .await
        .expect("create user");
    let now = chrono::Utc::now();
    rg_db::ops::repo_ops::create(
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
    .expect("create repository")
    .id
}

fn create_request(secret: Option<&str>) -> CreateWebhookRequest {
    CreateWebhookRequest {
        url: "https://hooks.example.invalid/forgekeep".to_string(),
        content_type: None,
        secret: secret.map(str::to_string),
        active: Some(true),
        events: vec!["push".to_string()],
    }
}

/// Read the column as the database holds it, not as a service returns it.
async fn stored_secret(db: &DatabaseConnection, id: i64) -> Option<String> {
    rg_db::entities::webhook::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("read webhook")
        .expect("webhook row")
        .secret_encrypted
}

fn open(stored: &str) -> String {
    encryption::decrypt(stored, &encryption::derive_key(ENCRYPTION_KEY)).expect("open the secret")
}

/// The defect: a dump of the `webhooks` row must not carry a usable signing
/// key, while the value the receiver verifies against stays the same.
#[tokio::test]
async fn the_row_never_holds_the_secret_the_operator_typed() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo = repo_id(&db).await;

    let hook = rg_core::webhook::service::create_webhook(
        &db,
        repo,
        &create_request(Some(SECRET)),
        ENCRYPTION_KEY,
        rg_core::webhook::transport::WebhookTransportPolicy::default(),
    )
    .await
    .expect("register webhook");

    let stored = stored_secret(&db, hook.id).await.expect("a secret is set");
    assert_ne!(stored, SECRET, "the plaintext secret is in the database");
    assert!(
        encryption::looks_like_ciphertext(&stored),
        "stored value is not our ciphertext: {stored}"
    );
    assert_eq!(open(&stored), SECRET, "the secret no longer round-trips");
}

/// A hook without a secret keeps signing nothing — the column stays `NULL`
/// rather than becoming the ciphertext of an empty string, which is what both
/// `has_secret` and the dispatcher read.
#[tokio::test]
async fn a_hook_without_a_secret_stores_null() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo = repo_id(&db).await;

    let hook = rg_core::webhook::service::create_webhook(
        &db,
        repo,
        &create_request(None),
        ENCRYPTION_KEY,
        rg_core::webhook::transport::WebhookTransportPolicy::default(),
    )
    .await
    .expect("register webhook");

    assert_eq!(stored_secret(&db, hook.id).await, None);
}

/// The secret is write-only over the API, so an update that does not mention it
/// must keep the stored one — re-encrypting the ciphertext it read back would
/// double-seal it, and dropping it would silently unsign every later delivery.
#[tokio::test]
async fn an_update_that_omits_the_secret_keeps_the_sealed_one() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo = repo_id(&db).await;

    let hook = rg_core::webhook::service::create_webhook(
        &db,
        repo,
        &create_request(Some(SECRET)),
        ENCRYPTION_KEY,
        rg_core::webhook::transport::WebhookTransportPolicy::default(),
    )
    .await
    .expect("register webhook");

    let updated = rg_core::webhook::service::update_webhook(
        &db,
        &hook,
        &UpdateWebhookRequest {
            url: None,
            content_type: None,
            secret: None,
            active: Some(false),
            events: None,
        },
        ENCRYPTION_KEY,
        rg_core::webhook::transport::WebhookTransportPolicy::default(),
    )
    .await
    .expect("update webhook");

    let stored = stored_secret(&db, updated.id).await.expect("still set");
    assert_eq!(open(&stored), SECRET);

    // A supplied secret replaces it, still sealed.
    let replaced = rg_core::webhook::service::update_webhook(
        &db,
        &updated,
        &UpdateWebhookRequest {
            url: None,
            content_type: None,
            secret: Some("a-rotated-secret".to_string()),
            active: None,
            events: None,
        },
        ENCRYPTION_KEY,
        rg_core::webhook::transport::WebhookTransportPolicy::default(),
    )
    .await
    .expect("update webhook");

    let stored = stored_secret(&db, replaced.id).await.expect("still set");
    assert_ne!(stored, "a-rotated-secret");
    assert_eq!(open(&stored), "a-rotated-secret");
}

/// The upgrade path. The migration can only rename the column — the key it
/// would need to seal the values lives outside the migration runner — so the
/// server runs this pass at startup, and it has to be safe to run on every
/// start.
#[tokio::test]
async fn the_startup_pass_seals_what_the_migration_could_not_and_repeats_harmlessly() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo = repo_id(&db).await;

    // A row exactly as an instance that upgraded into the rename holds it:
    // renamed column, plaintext value.
    let hook = rg_core::webhook::service::create_webhook(
        &db,
        repo,
        &create_request(None),
        ENCRYPTION_KEY,
        rg_core::webhook::transport::WebhookTransportPolicy::default(),
    )
    .await
    .expect("register webhook");
    let mut legacy: rg_db::entities::webhook::ActiveModel = hook.clone().into();
    legacy.secret_encrypted = Set(Some(SECRET.to_string()));
    sea_orm::ActiveModelTrait::update(legacy, &db)
        .await
        .expect("write a legacy plaintext secret");

    let sealed = seal_legacy_secrets(&db, ENCRYPTION_KEY)
        .await
        .expect("seal legacy secrets");
    assert_eq!(sealed, 1);

    let stored = stored_secret(&db, hook.id).await.expect("still set");
    assert_ne!(stored, SECRET);
    assert_eq!(open(&stored), SECRET, "sealing changed the signing key");

    // A restart must not re-encrypt what is already ciphertext — doing so once
    // per boot would bury the value under layers nothing can open.
    let sealed_again = seal_legacy_secrets(&db, ENCRYPTION_KEY)
        .await
        .expect("seal legacy secrets");
    assert_eq!(sealed_again, 0);
    assert_eq!(
        stored_secret(&db, hook.id).await.as_deref(),
        Some(stored.as_str())
    );
}

/// The registry is the gate: membership is what makes the startup preflight
/// sample this column and `forgekeep rotate-encryption-key` rewrite it. Being
/// encrypted but unregistered is the half-fix that leaves a rotation reporting
/// success over a column still sealed under the discarded key.
#[test]
fn the_column_is_registered_as_an_encrypted_column() {
    assert!(
        LABELS.contains(&"webhooks.secret_encrypted"),
        "not in the registry: {LABELS:?}"
    );
}
