//! Ids under `/users/...` are global primary keys, so the account check that
//! guards them decides what the response says about rows the caller may not
//! touch. Answering `403` on someone else's id — while an unused id answers
//! `404` — makes the pair an existence oracle: walking `{id}` enumerates every
//! access token and SSH key on the instance.
//!
//! The doctrine is already written down for repository-scoped ids in
//! `api::boards` ("a mismatch answers 404, not 403: a 403 would confirm the id
//! exists"); these tests hold the user-scoped half of it. Each one carries a
//! live baseline — the owner's own id still works in the same run — so the
//! test cannot pass by having broken the route for everybody.

use crate::common::{register_user, spawn_test_app};

const OWNER_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA owner@forgekeep";
const OTHER_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB other@forgekeep";

/// An id that no row has ever carried — the reference answer a stranger's id
/// has to be indistinguishable from.
const UNUSED_ID: i64 = 999_999;

/// The distinguishing part of an error body. `request_id` is fresh on every
/// response by design, so comparing whole bodies would never match; what must
/// not differ between "someone else's row" and "no such row" is the code and
/// the message.
fn error_shape(body: &serde_json::Value) -> (Option<&str>, Option<&str>) {
    (
        body["error"]["code"].as_str(),
        body["error"]["message"].as_str(),
    )
}

#[tokio::test]
async fn another_accounts_token_id_is_indistinguishable_from_an_unused_one() {
    let base = spawn_test_app().await;
    let owner = register_user(&base, "tokowner", "tokowner@example.com", "Qz7$wRtm").await;
    let other = register_user(&base, "tokother", "tokother@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({ "name": "laptop" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.unwrap();
    let token_id = created["id"].as_i64().unwrap();

    let stranger = client
        .delete(format!("{base}/api/v1/users/tokens/{token_id}"))
        .bearer_auth(&other)
        .send()
        .await
        .unwrap();
    let stranger_status = stranger.status();
    let stranger_body: serde_json::Value = stranger.json().await.unwrap();
    assert_eq!(
        stranger_status, 404,
        "another account's token id must not be confirmed with a 403"
    );

    let unused = client
        .delete(format!("{base}/api/v1/users/tokens/{UNUSED_ID}"))
        .bearer_auth(&other)
        .send()
        .await
        .unwrap();
    let unused_status = unused.status();
    let unused_body: serde_json::Value = unused.json().await.unwrap();
    assert_eq!(unused_status, 404);
    assert_eq!(
        error_shape(&stranger_body),
        error_shape(&unused_body),
        "an existing token and an unused id must answer the same thing"
    );

    // Baseline: the route still deletes the caller's own token, so the two
    // 404s above are a scope decision and not a dead handler.
    let own = client
        .delete(format!("{base}/api/v1/users/tokens/{token_id}"))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(own.status(), 204);
}

#[tokio::test]
async fn another_accounts_ssh_key_id_is_indistinguishable_from_an_unused_one() {
    let base = spawn_test_app().await;
    let owner = register_user(&base, "keyidowner", "keyidowner@example.com", "Qz7$wRtm").await;
    let other = register_user(&base, "keyidother", "keyidother@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({ "title": "Laptop", "key": OWNER_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.unwrap();
    let key_id = created["id"].as_i64().unwrap();

    // The other account holds a key of its own, so the run also proves the
    // stranger is a legitimate user of the route rather than one without keys.
    let own_key = client
        .post(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&other)
        .json(&serde_json::json!({ "title": "Desktop", "key": OTHER_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(own_key.status(), 201);
    let own_key: serde_json::Value = own_key.json().await.unwrap();
    let own_key_id = own_key["id"].as_i64().unwrap();

    let stranger = client
        .delete(format!("{base}/api/v1/users/ssh-keys/{key_id}"))
        .bearer_auth(&other)
        .send()
        .await
        .unwrap();
    let stranger_status = stranger.status();
    let stranger_body: serde_json::Value = stranger.json().await.unwrap();
    assert_eq!(
        stranger_status, 404,
        "another account's SSH key id must not be confirmed with a 403"
    );

    let unused = client
        .delete(format!("{base}/api/v1/users/ssh-keys/{UNUSED_ID}"))
        .bearer_auth(&other)
        .send()
        .await
        .unwrap();
    let unused_status = unused.status();
    let unused_body: serde_json::Value = unused.json().await.unwrap();
    assert_eq!(unused_status, 404);
    assert_eq!(
        error_shape(&stranger_body),
        error_shape(&unused_body),
        "an existing SSH key and an unused id must answer the same thing"
    );

    // Baseline: the owner's key survived the stranger's attempt, and each
    // account can still delete its own.
    let own = client
        .delete(format!("{base}/api/v1/users/ssh-keys/{own_key_id}"))
        .bearer_auth(&other)
        .send()
        .await
        .unwrap();
    assert_eq!(own.status(), 204);

    let owners_own = client
        .delete(format!("{base}/api/v1/users/ssh-keys/{key_id}"))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(owners_own.status(), 204);
}
