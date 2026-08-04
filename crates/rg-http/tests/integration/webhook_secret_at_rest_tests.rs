//! The webhook signing secret goes into the database sealed (card_9e46363314f4).
//!
//! `webhook_authz_tests` proves the secret never comes back *out* of the API.
//! This is the other half, and the one that was actually broken: what the
//! operator posts to `.../hooks` used to land in `webhooks.secret` verbatim, so
//! a database dump handed out the key every delivery to that receiver is signed
//! with — and `forgekeep rotate-encryption-key` reported success without ever
//! touching the column.
//!
//! Driven through the HTTP handler on purpose: the encryption happens in the
//! service, but only the handler knows the instance's at-rest key, and a
//! handler that forgets to pass it is exactly the regression worth catching.

use crate::common::{create_repo, register_full, spawn_test_app_with_db, TEST_ENCRYPTION_KEY};

use sea_orm::EntityTrait;

const HOOK_SECRET: &str = "s3cr3t-hmac-key-the-receiver-also-knows";

async fn stored_secret(db: &rg_db::DatabaseConnection, id: i64) -> Option<String> {
    rg_db::entities::webhook::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("read webhook")
        .expect("webhook row")
        .secret_encrypted
}

fn open(stored: &str) -> String {
    let key = rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY);
    rg_core::auth::encryption::decrypt(stored, &key).expect("open the stored secret")
}

async fn post_hook(base: &str, token: &str, owner: &str, repo: &str, secret: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/hooks"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "url": "https://hooks.example.invalid/forgekeep",
            "secret": secret,
            "events": ["push"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "creating the webhook failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["has_secret"], true,
        "the API must report the secret as configured"
    );
    body["id"].as_i64().expect("webhook id")
}

/// The card's acceptance: the row does not contain what the operator typed, and
/// the value the dispatcher will sign with is still that secret.
#[tokio::test]
async fn a_secret_posted_to_the_api_is_stored_as_ciphertext() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "hookowner", "hookowner@example.com").await;
    create_repo(&base, &token, "hooked").await;

    let id = post_hook(&base, &token, "hookowner", "hooked", HOOK_SECRET).await;

    let stored = stored_secret(&db, id).await.expect("a secret is set");
    assert_ne!(
        stored, HOOK_SECRET,
        "the operator's signing secret is in the database in the clear"
    );
    assert!(
        rg_core::auth::encryption::looks_like_ciphertext(&stored),
        "stored value is not our ciphertext: {stored}"
    );
    assert_eq!(open(&stored), HOOK_SECRET);
}

/// A rewrite through `PATCH` goes through a second code path, and it is the one
/// that used to copy `req.secret` into the column verbatim.
#[tokio::test]
async fn a_secret_replaced_through_patch_is_stored_as_ciphertext() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "patchowner", "patchowner@example.com").await;
    create_repo(&base, &token, "hooked").await;

    let id = post_hook(&base, &token, "patchowner", "hooked", HOOK_SECRET).await;

    let rotated = "a-rotated-signing-secret";
    let resp = reqwest::Client::new()
        .patch(format!("{base}/api/v1/repos/patchowner/hooked/hooks/{id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"secret": rotated}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let stored = stored_secret(&db, id).await.expect("a secret is set");
    assert_ne!(stored, rotated, "PATCH stored the secret in the clear");
    assert_eq!(open(&stored), rotated);
}
