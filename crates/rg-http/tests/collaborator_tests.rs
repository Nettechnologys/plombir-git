mod common;

use common::{create_repo, register_full, spawn_test_app};

#[tokio::test]
async fn add_collaborator_accepts_username_and_email() {
    let base = spawn_test_app().await;
    let (owner_token, _owner_id) =
        register_full(&base, "collab_owner", "collab_owner@example.com").await;
    let (_alice_token, alice_id) =
        register_full(&base, "collab_alice", "collab_alice@example.com").await;
    let (_bob_token, bob_id) = register_full(&base, "collab_bob", "collab_bob@example.com").await;
    create_repo(&base, &owner_token, "collab_repo").await;

    let client = reqwest::Client::new();
    let by_username = client
        .post(format!(
            "{}/api/v1/repos/collab_owner/collab_repo/collaborators",
            base
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "username": "collab_alice",
            "permission": "read"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(by_username.status(), 201);
    let username_body: serde_json::Value = by_username.json().await.unwrap();
    assert_eq!(username_body["user_id"], alice_id);

    let by_email = client
        .post(format!(
            "{}/api/v1/repos/collab_owner/collab_repo/collaborators",
            base
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "email": "collab_bob@example.com",
            "permission": "write"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(by_email.status(), 201);
    let email_body: serde_json::Value = by_email.json().await.unwrap();
    assert_eq!(email_body["user_id"], bob_id);

    let list = client
        .get(format!(
            "{}/api/v1/repos/collab_owner/collab_repo/collaborators",
            base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 200);
    let collaborators: Vec<serde_json::Value> = list.json().await.unwrap();
    let ids: Vec<i64> = collaborators
        .iter()
        .filter_map(|collab| collab["user_id"].as_i64())
        .collect();
    assert!(ids.contains(&alice_id));
    assert!(ids.contains(&bob_id));
}
