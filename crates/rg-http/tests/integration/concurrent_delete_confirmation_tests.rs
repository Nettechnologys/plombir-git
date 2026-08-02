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

use crate::common::{register_user, spawn_test_app};

const VALID_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA race@forgekeep";

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
    let confirmed = statuses.iter().filter(|s| **s == 204).count();
    let absent = statuses.iter().filter(|s| **s == 404).count();
    assert_eq!(
        confirmed, 1,
        "{what}: exactly one request may confirm the revocation, got {statuses:?}"
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
