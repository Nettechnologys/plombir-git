//! Regression coverage for card_d33afb82797f: the mirror feature was dead.
//!
//! `m20260607_000001_create_mirrors` derived its table name from a bare
//! `#[derive(Iden)] enum Mirror`, producing `mirror`, while `entities::mirror`
//! reads `mirrors`. Every mirror endpoint answered 500 with `no such table:
//! mirrors`, and nothing caught it: the mirror handlers validate the remote URL
//! before touching the database, so the only mirror requests any test ever made
//! — the ones with a deliberately bad URL — returned an honest 400 without
//! reaching the table, and the background `sync_due_mirrors` logged its error
//! and moved on.
//!
//! These tests take the happy path all the way to the row, which is the one
//! thing that was never exercised.

use crate::common::{create_repo, register_full, spawn_test_app, spawn_test_app_with_db};

const REMOTE: &str = "https://example.com/upstream.git";

/// The core of the card: a mirror can be created and read back.
#[tokio::test]
async fn a_mirror_can_be_created_and_read_back() {
    let base = spawn_test_app().await;
    let (token, _user_id) = register_full(&base, "mirror-owner", "mirror-owner@example.com").await;
    create_repo(&base, &token, "mirrored").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-owner/mirrored/mirror");

    // Nothing configured yet — the read is an honest empty state, not a 500.
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        404,
        "a repository with no mirror must report 'not configured', not a server error"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": REMOTE, "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 201, "creating a mirror failed: {body}");
    assert_eq!(body["url"], REMOTE);
    assert_eq!(body["sync_interval_seconds"], 3600);
    assert_eq!(body["status"], "active");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 200, "reading the mirror back failed: {body}");
    assert_eq!(body["url"], REMOTE);
    assert_eq!(body["sync_interval_seconds"], 3600);
}

/// The rest of the surface — update and delete — reaches the same table, so it
/// was just as dead. Walking the whole lifecycle keeps every verb honest.
#[tokio::test]
async fn a_mirror_can_be_updated_and_deleted() {
    let base = spawn_test_app().await;
    let (token, _user_id) = register_full(&base, "mirror-admin", "mirror-admin@example.com").await;
    create_repo(&base, &token, "lifecycle").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-admin/lifecycle/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": REMOTE, "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");

    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"sync_interval_seconds": 7200, "status": "inactive"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 200, "updating the mirror failed: {body}");
    assert_eq!(body["sync_interval_seconds"], 7200);
    assert_eq!(body["status"], "inactive");

    // The change is persisted, not just echoed back.
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["sync_interval_seconds"], 7200);
    assert_eq!(body["status"], "inactive");

    let resp = client
        .delete(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 204, "deleting the mirror failed");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "the deleted mirror is still readable");
}

/// card_c974e2bc1d73: the handlers used to answer `Json(json!(mirror))` — the
/// whole entity row, `password_encrypted` included. Every verb that hands a
/// mirror back is checked, because the leak was in the shared shape, not in one
/// handler.
#[tokio::test]
async fn a_mirror_reply_never_carries_the_stored_password() {
    let base = spawn_test_app().await;
    let (token, _user_id) = register_full(&base, "mirror-creds", "mirror-creds@example.com").await;
    create_repo(&base, &token, "credentialed").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-creds/credentialed/mirror");

    let created: serde_json::Value = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": REMOTE,
            "username": "sync-bot",
            "password": "hunter2",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");

    for (verb, body) in [
        ("POST", created.clone()),
        ("GET", get_mirror(&client, &url, &token).await),
        (
            "PATCH",
            client
                .patch(&url)
                .bearer_auth(&token)
                .json(&serde_json::json!({"sync_interval_seconds": 7200}))
                .send()
                .await
                .expect("request")
                .json()
                .await
                .expect("json body"),
        ),
    ] {
        assert!(
            body.get("password_encrypted").is_none(),
            "{verb} leaked the stored mirror password: {body}"
        );
        assert!(
            !body.to_string().contains("hunter2"),
            "{verb} leaked the mirror password verbatim: {body}"
        );
        assert_eq!(
            body["has_credentials"], true,
            "{verb} must still report that a credential is configured: {body}"
        );
        assert_eq!(body["username"], "sync-bot", "{verb} lost the username");
    }
}

/// The other half of the same card: the reply is administrative, so reading it
/// takes push access. `require_read` let any visitor to a *public* repo pull the
/// remote's username and credential state out of the settings.
#[tokio::test]
async fn reading_a_mirror_requires_write_access() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "mirror-pub", "mirror-pub@example.com").await;
    let (outsider_token, _) = register_full(&base, "mirror-nosy", "mirror-nosy@example.com").await;
    create_repo(&base, &owner_token, "public-mirror").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-pub/public-mirror/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"url": REMOTE, "username": "sync-bot", "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");

    let resp = client.get(&url).send().await.expect("request");
    assert_eq!(
        resp.status(),
        401,
        "an anonymous reader reached the mirror settings of a public repo"
    );

    let resp = client
        .get(&url)
        .bearer_auth(&outsider_token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        403,
        "a reader without push access reached the mirror settings"
    );

    let resp = client
        .get(&url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "the owner lost access to the mirror");
}

/// card_c29cb3416941: `password_encrypted` was a name, not a fact — the column
/// held the password exactly as the operator typed it, so a database dump was a
/// list of other people's remote credentials. Modelled on
/// `admin_sso_audit_tests`' check of `ldap_bind_password_enc`: it is not enough
/// that the stored value *differs* from the input, it has to decrypt back to it
/// with the server's key.
#[tokio::test]
async fn a_stored_mirror_password_is_encrypted_at_rest() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "mirror-rest", "mirror-rest@example.com").await;
    create_repo(&base, &token, "at-rest").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-rest/at-rest/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": REMOTE,
            "username": "sync-bot",
            "password": "hunter2",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");
    let created: serde_json::Value = resp.json().await.expect("json body");
    let repo_id = created["repo_id"].as_i64().expect("repo_id");

    let key = rg_core::auth::encryption::derive_key("test-secret-key");
    let stored = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("query")
        .expect("the mirror row")
        .password_encrypted
        .expect("a password was supplied, so one must be stored");
    assert_ne!(stored, "hunter2", "the password is stored in the clear");
    assert_eq!(
        rg_core::auth::encryption::decrypt(&stored, &key).expect("decrypt"),
        "hunter2",
        "the stored ciphertext is not the password we were given"
    );

    // A replacement password goes through the same door...
    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"password": "correct horse"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "updating the credential failed");
    let stored = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("query")
        .expect("the mirror row")
        .password_encrypted
        .expect("the replacement password must be stored");
    assert_ne!(stored, "correct horse", "the update stored it in the clear");
    assert_eq!(
        rg_core::auth::encryption::decrypt(&stored, &key).expect("decrypt"),
        "correct horse"
    );

    // ...and an empty one is how a credential is taken back off a mirror.
    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"password": ""}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 200, "clearing the credential failed: {body}");
    assert_eq!(
        body["has_credentials"], false,
        "the mirror still reports a stored credential: {body}"
    );
    assert!(
        rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
            .await
            .expect("query")
            .expect("the mirror row")
            .password_encrypted
            .is_none(),
        "the cleared credential is still in the database"
    );
}

/// The other half of card_c29cb3416941: the credential is not just stored, it
/// is *read* by the sync. Proving that without a network is easy from the
/// failure side — a value the server cannot decrypt (one written under a
/// different secret, or a plaintext leftover from before encryption existed)
/// has to stop the sync with a message that says so, which it can only do by
/// having looked at the column at all. Before the fix the column was never
/// read and this sync would have sailed past it.
#[tokio::test]
async fn a_sync_reports_a_credential_it_cannot_decrypt() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "mirror-stale", "mirror-stale@example.com").await;
    create_repo(&base, &token, "stale-secret").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-stale/stale-secret/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": REMOTE,
            "username": "sync-bot",
            "password": "hunter2",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");
    let created: serde_json::Value = resp.json().await.expect("json body");
    let repo_id = created["repo_id"].as_i64().expect("repo_id");

    // Stand in for a row written before encryption existed: plaintext where
    // ciphertext is expected.
    let stored = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("query")
        .expect("the mirror row");
    let mut model: rg_db::entities::mirror::ActiveModel = stored.into();
    model.password_encrypted = sea_orm::ActiveValue::Set(Some("hunter2".to_string()));
    rg_db::ops::mirror_ops::update(&db, model)
        .await
        .expect("plant the legacy value");

    let resp = client
        .post(format!("{url}/sync"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "the sync trigger itself must not fail");

    let mirror = get_mirror(&client, &url, &token).await;
    assert_eq!(
        mirror["status"], "error",
        "an unusable credential left the mirror looking healthy: {mirror}"
    );
    let reason = mirror["last_sync_error"].as_str().unwrap_or_default();
    assert!(
        reason.contains("could not be decrypted"),
        "the sync did not report the unreadable credential: {mirror}"
    );
    assert!(
        !mirror.to_string().contains("hunter2"),
        "the sync echoed the stored credential back: {mirror}"
    );
}

async fn get_mirror(client: &reqwest::Client, url: &str, token: &str) -> serde_json::Value {
    client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body")
}

/// One mirror per repository — the unique index the create migration builds is
/// on `mirrors`, so this also proves the index followed the table rename.
#[tokio::test]
async fn a_second_mirror_for_the_same_repo_is_refused_as_a_bad_request() {
    let base = spawn_test_app().await;
    let (token, _user_id) = register_full(&base, "mirror-dup", "mirror-dup@example.com").await;
    create_repo(&base, &token, "once-only").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirror-dup/once-only/mirror");
    let payload = serde_json::json!({"url": REMOTE, "sync_interval_seconds": 3600});

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&payload)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline create");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&payload)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "a duplicate mirror is the caller's mistake, not a server failure"
    );
}
