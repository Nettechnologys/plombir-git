//! A 204 on a credential revocation must mean *this* request removed the row.
//!
//! Every one of these routes looks the row up, checks who owns it, and then
//! issues a `DELETE` — two separate statements. When two requests race, both
//! lookups succeed, both pass the ownership check, and only one `DELETE` can
//! actually remove anything. While the op functions returned `Result<()>` the
//! loser could not tell: it answered 204 as well, so the client was told a
//! revocation had happened that this request never performed.
//!
//! The assertion is deliberately about the *set* of answers rather than about
//! which task wins: exactly one 204, the rest 404, and never a 5xx. That holds
//! whichever way the runtime interleaves them, and it cannot hold at all if the
//! response is derived from the lookup instead of from `rows_affected`.

use crate::common::{
    create_repo, register_full, register_user, spawn_test_app, spawn_test_app_with_db,
};

const VALID_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA race@plombir-git";

/// Number of DELETEs fired at the same row. One is enough to state the
/// property; several make an interleaving that only `rows_affected` can catch
/// actually likely to occur.
const RACERS: usize = 8;

/// Fire `RACERS` concurrent `DELETE`s at `url` and return the status codes.
async fn race_deletes(url: &str, jwt: &str) -> Vec<u16> {
    let client = reqwest::Client::new();
    let mut tasks = Vec::with_capacity(RACERS);
    for _ in 0..RACERS {
        let client = client.clone();
        let url = url.to_string();
        let jwt = jwt.to_string();
        tasks.push(tokio::spawn(async move {
            client
                .delete(url)
                .bearer_auth(jwt)
                .send()
                .await
                .expect("the delete request itself must not fail")
                .status()
                .as_u16()
        }));
    }
    let mut statuses = Vec::with_capacity(RACERS);
    for task in tasks {
        statuses.push(task.await.expect("delete task panicked"));
    }
    statuses
}

fn assert_exactly_one_deleter(statuses: &[u16], what: &str) {
    assert_one_confirmation(statuses, 204, what);
}

/// The same property for the routes that confirm with a body instead of a 204
/// — `{"deleted": true}`, `{"message": "webhook deleted"}`. The status code
/// differs; the claim those bodies make does not.
fn assert_one_confirmation(statuses: &[u16], success: u16, what: &str) {
    let confirmed = statuses.iter().filter(|s| **s == success).count();
    let absent = statuses.iter().filter(|s| **s == 404).count();
    assert_eq!(
        confirmed, 1,
        "{what}: exactly one request may confirm the deletion, got {statuses:?}"
    );
    assert_eq!(
        absent,
        statuses.len() - 1,
        "{what}: every request that removed nothing must answer 404, got {statuses:?}"
    );
}

#[tokio::test]
async fn concurrent_token_revocations_confirm_exactly_one_deletion() {
    let base = spawn_test_app().await;
    let owner = register_user(&base, "tokracer", "tokracer@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({ "name": "raced" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.unwrap();
    let token_id = created["id"].as_i64().unwrap();

    let statuses = race_deletes(&format!("{base}/api/v1/users/tokens/{token_id}"), &owner).await;
    assert_exactly_one_deleter(&statuses, "access token");

    // And the row really is gone — the 204 was not the only thing that moved.
    let listed = client
        .get(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    let listed: serde_json::Value = listed.json().await.unwrap();
    assert!(
        listed
            .as_array()
            .map(|rows| rows.is_empty())
            .unwrap_or(false),
        "the token survived the race: {listed}"
    );
}

#[tokio::test]
async fn concurrent_ssh_key_revocations_confirm_exactly_one_deletion() {
    let base = spawn_test_app().await;
    let owner = register_user(&base, "keyracer", "keyracer@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({ "title": "raced", "key": VALID_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.unwrap();
    let key_id = created["id"].as_i64().unwrap();

    let statuses = race_deletes(&format!("{base}/api/v1/users/ssh-keys/{key_id}"), &owner).await;
    assert_exactly_one_deleter(&statuses, "SSH key");

    let listed = client
        .get(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    let listed: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert!(
        listed.is_empty(),
        "the SSH key survived the race: {listed:?}"
    );
}

// ── card_4e8df0264c53: the second batch ──────────────────────────────────
//
// The same shape, found by sweeping for the op signature rather than for the
// route list: `board`, `webhook`, `sso_provider`, `ci_environment` and `org`
// all had a `delete` returning `Result<()>`, so their handlers could not tell a
// deletion from a no-op either. Two of them said so in the response body.

#[tokio::test]
async fn concurrent_board_deletions_confirm_exactly_one_deletion() {
    let base = spawn_test_app().await;
    let owner = register_user(&base, "boardracer", "boardracer@example.com", "Qz7$wRtm").await;
    create_repo(&base, &owner, "boardrace").await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/v1/repos/boardracer/boardrace/boards"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({ "name": "raced" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.unwrap();
    let board_id = created["id"].as_i64().expect("created board carries an id");

    let statuses = race_deletes(
        &format!("{base}/api/v1/repos/boardracer/boardrace/boards/{board_id}"),
        &owner,
    )
    .await;
    assert_exactly_one_deleter(&statuses, "board");
}

#[tokio::test]
async fn concurrent_webhook_deletions_confirm_exactly_one_deletion() {
    let base = spawn_test_app().await;
    let owner = register_user(&base, "hookracer", "hookracer@example.com", "Qz7$wRtm").await;
    create_repo(&base, &owner, "hookrace").await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/v1/repos/hookracer/hookrace/hooks"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({
            "url": "https://example.test/hook",
            "events": ["push"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "create webhook");
    let created: serde_json::Value = created.json().await.unwrap();
    let hook_id = created["id"]
        .as_i64()
        .expect("created webhook carries an id");

    // 200 with `{"message": "webhook deleted"}` — the loser used to send that
    // sentence about a row it never touched.
    let statuses = race_deletes(
        &format!("{base}/api/v1/repos/hookracer/hookrace/hooks/{hook_id}"),
        &owner,
    )
    .await;
    assert_one_confirmation(&statuses, 200, "webhook");
}

#[tokio::test]
async fn concurrent_sso_provider_deletions_confirm_exactly_one_deletion() {
    let (base, db) = spawn_test_app_with_db().await;
    let (admin, admin_id) = register_full(&base, "ssoracer", "ssoracer@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .expect("promote test user")
        .expect("registered user must exist");
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/v1/admin/sso/providers"))
        .bearer_auth(&admin)
        .json(&serde_json::json!({
            "name": "Raced IdP",
            "slug": "raced-idp",
            "provider_type": "oauth2",
            "enabled": false,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "create SSO provider");
    let created: serde_json::Value = created.json().await.unwrap();
    let provider_id = created["id"]
        .as_i64()
        .expect("created provider carries an id");

    // This one said it outright: `{"deleted": true}` from every racer.
    let statuses = race_deletes(
        &format!("{base}/api/v1/admin/sso/providers/{provider_id}"),
        &admin,
    )
    .await;
    assert_one_confirmation(&statuses, 200, "SSO provider");
}
