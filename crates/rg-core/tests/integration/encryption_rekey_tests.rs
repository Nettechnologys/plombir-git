//! Rotating the at-rest encryption key itself (card_e7574c9de39f).
//!
//! The gap these close: card_d740512de0a8 made rotating the *signing* secret
//! safe and made a wrong encryption key refuse the start — but left the
//! encryption key itself permanent for the life of a database. A leaked
//! `encryption_key` had exactly one answer, "wipe every encrypted value and
//! have MFA re-enrolled and every stored credential re-entered by hand", for an
//! incident that is otherwise routine.
//!
//! The contract now: the pass moves every registered column onto the new key in
//! one transaction, reports what it could not open instead of destroying it,
//! and refuses to commit a rotation whose old key opens nothing.

use std::path::Path;

use rg_core::auth::encrypted_columns::LABELS;
use rg_core::auth::encryption;
use rg_core::auth::key_check::verify_encryption_key;
use rg_core::auth::rekey::{rekey, RekeyReport};
use sea_orm::{ActiveValue::NotSet, DatabaseConnection, Set};

const OLD_SECRET: &str = "the-secret-this-database-was-built-with";
const NEW_SECRET: &str = "a-freshly-generated-secret-after-a-leak";

const TOTP_PLAINTEXT: &str = "JBSWY3DPEHPK3PXP";
const CI_SECRET_PLAINTEXT: &str = "ghp_a-deploy-token-nobody-wants-to-retype";
const WEBHOOK_SECRET_PLAINTEXT: &str = "the-key-every-delivery-is-signed-with";

async fn fresh_db(dir: &Path) -> DatabaseConnection {
    let db = crate::common::migrated_sqlite(&dir.join("test.db"), 2).await;
    db
}

/// Land an encrypted TOTP secret in `users.totp_secret` the way enrolment does.
///
/// Since card_08400088bb40 the setup step only *stages* the secret
/// (`users.pending_totp_secret`); the column the login path verifies against is
/// written by the enable step, which promotes what was staged. A test that
/// wants a live factor has to walk both halves — writing the column by hand
/// would be testing a state the server can no longer produce.
async fn store_live_totp_secret(db: &sea_orm::DatabaseConnection, user_id: i64, ciphertext: &str) {
    rg_db::ops::user_ops::stage_pending_totp_secret(db, user_id, ciphertext)
        .await
        .expect("stage the TOTP secret")
        .expect("the account is open");
    rg_db::ops::user_ops::enable_mfa_with_backup_codes(db, user_id, &[])
        .await
        .expect("promote the staged TOTP secret")
        .expect("the account is open");
}

/// A database that looks like a live instance: a user enrolled in MFA, a CI
/// secret and a signed webhook on a repository, and the instance signing key
/// established — the things an operator would lose if a key rotation quietly
/// skipped a column.
async fn live_instance(db: &DatabaseConnection, secret: &str) {
    let user = rg_db::ops::user_ops::create_user(db, "alice", "alice@example.invalid", "", "Alice")
        .await
        .expect("create user");
    let stored = encryption::encrypt(TOTP_PLAINTEXT, &encryption::derive_key(secret))
        .expect("encrypt totp secret");
    store_live_totp_secret(db, user.id, &stored).await;

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

    let ci = encryption::encrypt(CI_SECRET_PLAINTEXT, &encryption::derive_key(secret))
        .expect("encrypt ci secret");
    rg_db::ops::ci_secret_ops::upsert(db, repo.id, "DEPLOY_TOKEN", &ci, user.id)
        .await
        .expect("store ci secret");

    // A webhook whose deliveries are signed. Registered through the service so
    // the secret is sealed exactly the way a running server seals it.
    rg_core::webhook::service::create_webhook(
        db,
        repo.id,
        &rg_core::webhook::service::CreateWebhookRequest {
            url: "https://hooks.example.invalid/forgekeep".to_string(),
            content_type: None,
            secret: Some(WEBHOOK_SECRET_PLAINTEXT.to_string()),
            active: Some(true),
            events: vec!["push".to_string()],
        },
        secret,
        rg_core::webhook::transport::WebhookTransportPolicy::default(),
    )
    .await
    .expect("register webhook");

    // Establishes `instance_signing_key.seed_encrypted` the way the first
    // server start does.
    rg_core::auth::instance_key::load_or_adopt(db, "a-signing-secret", secret)
        .await
        .expect("establish the instance signing key");
}

async fn stored_totp(db: &DatabaseConnection) -> String {
    rg_db::ops::user_ops::find_by_username(db, "alice")
        .await
        .expect("read user")
        .expect("user exists")
        .totp_secret
        .expect("totp secret stored")
}

async fn stored_webhook_secret(db: &DatabaseConnection) -> String {
    use sea_orm::EntityTrait;
    rg_db::entities::webhook::Entity::find()
        .one(db)
        .await
        .expect("read webhook")
        .expect("webhook exists")
        .secret_encrypted
        .expect("webhook secret stored")
}

fn opens(value: &str, secret: &str) -> bool {
    encryption::decrypt(value, &encryption::derive_key(secret)).is_ok()
}

fn column<'a>(report: &'a RekeyReport, label: &str) -> &'a rg_core::auth::rekey::ColumnRekey {
    report
        .columns
        .iter()
        .find(|c| c.column == label)
        .unwrap_or_else(|| panic!("{label} missing from the report: {report:?}"))
}

/// The card's acceptance, end to end: a live database with MFA and a CI secret
/// moves onto the new key, starts under it, and refuses the old one.
#[tokio::test]
async fn a_live_database_moves_onto_the_new_key() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    live_instance(&db, OLD_SECRET).await;

    let report = rekey(&db, OLD_SECRET, NEW_SECRET, false)
        .await
        .expect("rotate the encryption key");

    assert_eq!(column(&report, "users.totp_secret").rewritten, 1);
    assert_eq!(column(&report, "ci_secrets.encrypted_value").rewritten, 1);
    assert_eq!(
        column(&report, "instance_signing_key.seed_encrypted").rewritten,
        1
    );
    assert_eq!(column(&report, "webhooks.secret_encrypted").rewritten, 1);
    assert_eq!(report.unreadable(), 0, "{report:?}");

    // The values are the same secrets, just sealed differently.
    let totp = stored_totp(&db).await;
    assert!(
        !opens(&totp, OLD_SECRET),
        "the old key must no longer open it"
    );
    assert_eq!(
        encryption::decrypt(&totp, &encryption::derive_key(NEW_SECRET)).unwrap(),
        TOTP_PLAINTEXT,
        "re-encryption must preserve the plaintext, not just the shape"
    );

    // Same for the signing secret: a rotation that changed it would leave every
    // receiver rejecting deliveries it used to accept.
    let webhook_secret = stored_webhook_secret(&db).await;
    assert!(
        !opens(&webhook_secret, OLD_SECRET),
        "the old key must no longer open it"
    );
    assert_eq!(
        encryption::decrypt(&webhook_secret, &encryption::derive_key(NEW_SECRET)).unwrap(),
        WEBHOOK_SECRET_PLAINTEXT,
    );

    verify_encryption_key(&db, NEW_SECRET)
        .await
        .expect("the server must start under the new key");
    verify_encryption_key(&db, OLD_SECRET)
        .await
        .expect_err("the old key must now be refused");
}

/// The instance's provenance identity has to survive the rotation *unchanged*:
/// a re-encryption that minted a new key would invalidate every attestation
/// this server ever signed, silently, as a side effect of a config chore.
#[tokio::test]
async fn the_instance_signing_key_keeps_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    live_instance(&db, OLD_SECRET).await;

    let before = rg_core::auth::instance_key::load_or_adopt(&db, "a-signing-secret", OLD_SECRET)
        .await
        .expect("read the established key");

    rekey(&db, OLD_SECRET, NEW_SECRET, false)
        .await
        .expect("rotate the encryption key");

    let after = rg_core::auth::instance_key::load_or_adopt(&db, "a-signing-secret", NEW_SECRET)
        .await
        .expect("read the key under the new encryption key");
    assert_eq!(
        before.kid(),
        after.kid(),
        "re-encrypting the seed must not change the instance's identity"
    );
}

/// `--dry-run` reports the same numbers the real run would produce and leaves
/// every byte where it was.
#[tokio::test]
async fn a_dry_run_reports_everything_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    live_instance(&db, OLD_SECRET).await;
    let before = stored_totp(&db).await;

    let report = rekey(&db, OLD_SECRET, NEW_SECRET, true)
        .await
        .expect("dry run");
    assert!(report.dry_run);
    assert_eq!(report.rewritten(), 4, "{report:?}");

    assert_eq!(before, stored_totp(&db).await, "a dry run must not write");
    verify_encryption_key(&db, OLD_SECRET)
        .await
        .expect("the database must still be readable with the old key");
}

/// A key that opens nothing is a mistyped `--old`, not an instruction to seal
/// the database away. The transaction must roll back, not commit.
#[tokio::test]
async fn an_old_key_that_opens_nothing_is_refused_and_rolled_back() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    live_instance(&db, OLD_SECRET).await;
    let before = stored_totp(&db).await;

    let error = rekey(&db, "a-key-this-database-never-saw", NEW_SECRET, false)
        .await
        .expect_err("a key that opens nothing must not be committed");
    let message = format!("{error:#}");
    assert!(
        message.contains("Nothing has been changed"),
        "the refusal must say the database is intact: {message}"
    );

    assert_eq!(before, stored_totp(&db).await, "the pass must roll back");
    verify_encryption_key(&db, OLD_SECRET)
        .await
        .expect("the database must still open with its real key");
}

/// Legacy plaintext predates encryption and its readers still expect it in the
/// clear; ciphertext neither key opens is per-row damage. Both are reported and
/// left alone — and neither stops the rest of the rotation.
#[tokio::test]
async fn plaintext_and_damaged_values_are_reported_not_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    live_instance(&db, OLD_SECRET).await;

    let bob = rg_db::ops::user_ops::create_user(&db, "bob", "bob@example.invalid", "", "Bob")
        .await
        .expect("create user");
    store_live_totp_secret(&db, bob.id, TOTP_PLAINTEXT).await;

    let carol = rg_db::ops::user_ops::create_user(&db, "carol", "carol@example.invalid", "", "C")
        .await
        .expect("create user");
    let damaged = encryption::encrypt("x", &encryption::derive_key("some-third-key")).unwrap();
    store_live_totp_secret(&db, carol.id, &damaged).await;

    let report = rekey(&db, OLD_SECRET, NEW_SECRET, false)
        .await
        .expect("one damaged row must not abort the rotation");

    let totp = column(&report, "users.totp_secret");
    assert_eq!(
        (totp.rewritten, totp.plaintext, totp.unreadable),
        (1, 1, 1),
        "{report:?}"
    );
    assert_eq!(report.unreadable_columns(), vec!["users.totp_secret"]);

    let bob_after = rg_db::ops::user_ops::find_by_id(&db, bob.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        bob_after.totp_secret.as_deref(),
        Some(TOTP_PLAINTEXT),
        "plaintext must not be encrypted by a pass that was told to re-encrypt"
    );
    let carol_after = rg_db::ops::user_ops::find_by_id(&db, carol.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        carol_after.totp_secret.as_deref(),
        Some(damaged.as_str()),
        "a value nothing opens must be left for a human, not destroyed"
    );
}

/// Re-running the command — after a crash, or because the operator is not sure
/// it finished — must be a no-op rather than an alarm about unreadable data.
#[tokio::test]
async fn a_second_run_recognises_the_already_rotated_values() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    live_instance(&db, OLD_SECRET).await;

    rekey(&db, OLD_SECRET, NEW_SECRET, false)
        .await
        .expect("first run");
    let second = rekey(&db, OLD_SECRET, NEW_SECRET, false)
        .await
        .expect("a re-run must not fail");

    assert_eq!(second.rewritten(), 0, "{second:?}");
    assert_eq!(second.already_new(), 4, "{second:?}");
    assert_eq!(second.unreadable(), 0, "{second:?}");
}

/// A fresh install has nothing encrypted yet, so there is nothing to rotate and
/// nothing to complain about.
#[tokio::test]
async fn an_empty_database_rotates_to_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;

    let report = rekey(&db, OLD_SECRET, NEW_SECRET, false)
        .await
        .expect("an empty database must not be an error");
    assert_eq!(report.rewritten(), 0);
    assert!(!report.old_key_is_wrong());
}

#[tokio::test]
async fn rotating_onto_the_same_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;

    rekey(&db, OLD_SECRET, OLD_SECRET, false)
        .await
        .expect_err("rotating onto the same key is a mistake, not a no-op");
    rekey(&db, OLD_SECRET, "   ", false)
        .await
        .expect_err("an empty new key must never be accepted");
}

/// The drift guard the whole registry exists for: the rotation must cover
/// exactly the columns the startup preflight watches. If someone adds an
/// encrypted column to one and not the other, a rotation reports success while
/// leaving that column sealed under the discarded key.
#[tokio::test]
async fn the_rotation_covers_every_registered_column() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;

    let report = rekey(&db, OLD_SECRET, NEW_SECRET, true)
        .await
        .expect("dry run");
    let covered: Vec<&str> = report.columns.iter().map(|c| c.column).collect();
    assert_eq!(covered, LABELS.to_vec(), "{report:?}");
    assert!(
        LABELS.contains(&"instance_signing_key.seed_encrypted"),
        "the registry must include the stored signing key, not just user data"
    );
}
