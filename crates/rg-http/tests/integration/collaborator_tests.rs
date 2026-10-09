use crate::common::{create_repo, register_full, spawn_test_app};

#[tokio::test]
/// card_9e97b992d4a6: an e-mail is refused, not resolved. Addresses are not
/// confirmed here, so the account holding `collab_bob@example.com` is whoever
/// registered it first — a grant addressed to it must not land anywhere.
async fn add_collaborator_accepts_a_username_and_refuses_an_email() {
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
    assert_eq!(by_email.status(), 400);
    let email_body: serde_json::Value = by_email.json().await.unwrap();
    assert!(
        email_body
            .to_string()
            .contains("cannot be named by e-mail address"),
        "the refusal has to say why an e-mail is not enough, got: {email_body}"
    );

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
    assert!(
        !ids.contains(&bob_id),
        "a grant addressed by e-mail reached the account holding that address"
    );
}
