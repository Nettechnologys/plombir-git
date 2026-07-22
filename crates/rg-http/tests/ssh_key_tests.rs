mod common;

use common::{register_user, spawn_test_app};

const VALID_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA test@ironforge";

#[tokio::test]
async fn ssh_key_lifecycle_validates_and_enforces_ownership() {
    let base = spawn_test_app().await;
    let owner_jwt = register_user(&base, "keyowner", "keyowner@example.com", "Qz7$wRtm").await;
    let other_jwt = register_user(&base, "keyother", "keyother@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    let unauthenticated = client
        .get(format!("{base}/api/v1/users/ssh-keys"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), 401);

    let invalid = client
        .post(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&owner_jwt)
        .json(&serde_json::json!({
            "title": "broken",
            "public_key": "ssh-ed25519 not-base64"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 400);

    let created = client
        .post(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&owner_jwt)
        .json(&serde_json::json!({ "title": "Laptop", "key": VALID_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: serde_json::Value = created.json().await.unwrap();
    let key_id = created["id"].as_i64().unwrap();
    assert_eq!(created["title"], "Laptop");
    assert!(created["fingerprint"]
        .as_str()
        .unwrap()
        .starts_with("SHA256:"));

    let duplicate = client
        .post(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&other_jwt)
        .json(&serde_json::json!({ "title": "Copied", "public_key": VALID_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), 409);

    let listed = client
        .get(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&owner_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    let listed: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], key_id);

    let forbidden = client
        .delete(format!("{base}/api/v1/users/ssh-keys/{key_id}"))
        .bearer_auth(&other_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403);

    let deleted = client
        .delete(format!("{base}/api/v1/users/ssh-keys/{key_id}"))
        .bearer_auth(&owner_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);

    let listed = client
        .get(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&owner_jwt)
        .send()
        .await
        .unwrap();
    let listed: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert!(listed.is_empty());
}
