//! The startup preflight for the at-rest encryption key (card_d740512de0a8).
//!
//! The defect these cover: `jwt_secret` was also the key for every encrypted
//! column, so rotating it left a server that started happily and then failed
//! MFA login, CI secret injection, mirror sync and LDAP bind one at a time,
//! each with its own unrelated-looking 500. The contract now is that such an
//! instance does not start at all, and that the refusal says what to do.

use std::path::Path;

use rg_core::auth::encryption;
use rg_core::auth::key_check::{probe_encryption_key, verify_encryption_key};

const OLD_SECRET: &str = "the-secret-this-database-was-built-with";
const ROTATED_SECRET: &str = "a-freshly-generated-secret-after-a-leak";

async fn fresh_db(dir: &Path) -> sea_orm::DatabaseConnection {
    let db = crate::common::migrated_sqlite(&dir.join("test.db"), 2).await;
    db
}

/// Enrol a user in MFA the way `POST /users/mfa/setup` does.
async fn user_with_mfa(db: &sea_orm::DatabaseConnection, secret: &str) {
    let user = rg_db::ops::user_ops::create_user(db, "alice", "alice@example.invalid", "", "Alice")
        .await
        .expect("create user");
    let stored = encryption::encrypt("JBSWY3DPEHPK3PXP", &encryption::derive_key(secret))
        .expect("encrypt totp secret");
    store_live_totp_secret(db, user.id, &stored).await;
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

/// A brand-new install has nothing encrypted, so no key can be the wrong one.
/// Blocking here would make the check impossible to adopt.
#[tokio::test]
async fn a_fresh_database_starts_under_any_key() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;

    verify_encryption_key(&db, ROTATED_SECRET)
        .await
        .expect("an empty database must never block a start");
}

#[tokio::test]
async fn the_unchanged_key_still_opens_its_own_data() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    user_with_mfa(&db, OLD_SECRET).await;

    verify_encryption_key(&db, OLD_SECRET)
        .await
        .expect("the key that wrote the data must open it");
}

/// The card's scenario end to end: a live database with MFA enrolled, restarted
/// under a rotated secret. Before the fix this started and then answered 500 on
/// every MFA login; now it refuses, and names the fix.
#[tokio::test]
async fn a_rotated_secret_refuses_the_start_instead_of_serving_500s() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    user_with_mfa(&db, OLD_SECRET).await;

    let error = verify_encryption_key(&db, ROTATED_SECRET)
        .await
        .expect_err("a key that opens nothing must stop the server");
    let message = format!("{error:#}");

    assert!(
        message.contains("users.totp_secret"),
        "the refusal must name what is at stake: {message}"
    );
    assert!(
        message.contains("encryption_key"),
        "the refusal must name the knob that fixes it: {message}"
    );
}

/// The whole point of the split: the signing secret can now be rotated, and as
/// long as `encryption_key` stays pinned to the old value the stored data is
/// still readable. This is the recovery the refusal message prescribes.
#[tokio::test]
async fn pinning_the_previous_secret_as_the_encryption_key_survives_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    user_with_mfa(&db, OLD_SECRET).await;

    // jwt_secret is now ROTATED_SECRET; encryption_key stayed behind.
    verify_encryption_key(&db, OLD_SECRET)
        .await
        .expect("pinning the previous secret must keep the data readable");
}

/// A column that still holds pre-encryption plaintext is not evidence of a
/// wrong key. Counting it as such would refuse the start of a healthy instance
/// that simply predates the column being encrypted.
#[tokio::test]
async fn legacy_plaintext_does_not_masquerade_as_a_failed_decryption() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let user = rg_db::ops::user_ops::create_user(&db, "bob", "bob@example.invalid", "", "Bob")
        .await
        .expect("create user");
    store_live_totp_secret(&db, user.id, "JBSWY3DPEHPK3PXP").await;

    let probe = probe_encryption_key(&db, &encryption::derive_key(ROTATED_SECRET))
        .await
        .expect("probe");
    assert!(probe.is_empty(), "plaintext must not be probed: {probe:?}");

    verify_encryption_key(&db, ROTATED_SECRET)
        .await
        .expect("a legacy plaintext column must not block a start");
}

/// One damaged row among healthy ones is per-row damage, not a wrong key.
/// Refusing to start would not repair it and would take the instance down.
#[tokio::test]
async fn a_single_damaged_row_does_not_take_the_server_down() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    user_with_mfa(&db, OLD_SECRET).await;

    let bob = rg_db::ops::user_ops::create_user(&db, "bob", "bob@example.invalid", "", "Bob")
        .await
        .expect("create user");
    let corrupt = encryption::encrypt("x", &encryption::derive_key("some-other-key")).unwrap();
    store_live_totp_secret(&db, bob.id, &corrupt).await;

    let probe = probe_encryption_key(&db, &encryption::derive_key(OLD_SECRET))
        .await
        .expect("probe");
    assert_eq!((probe.probed, probe.opened), (2, 1), "{probe:?}");
    assert!(!probe.key_is_wrong());

    verify_encryption_key(&db, OLD_SECRET)
        .await
        .expect("one opened value proves the key");
}
