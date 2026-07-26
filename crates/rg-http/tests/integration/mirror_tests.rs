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

use crate::common::{create_repo, register_full, spawn_test_app};

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
