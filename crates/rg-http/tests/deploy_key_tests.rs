//! Repository deploy-key CRUD and scoping regression coverage.

mod common;

use common::{register_full, spawn_test_app};

const ED25519_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA deploy";

async fn create_repo(base: &str, token: &str, name: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
}

#[tokio::test]
async fn deploy_key_lifecycle_is_repository_scoped_and_globally_unique() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "deploy-owner", "deploy-owner@example.com").await;
    let (writer_token, _) =
        register_full(&base, "deploy-writer", "deploy-writer@example.com").await;
    create_repo(&base, &token, "one").await;
    create_repo(&base, &token, "two").await;
    let client = reqwest::Client::new();
    let one_keys = format!("{base}/api/v1/repos/deploy-owner/one/keys");
    let collaborator = client
        .post(format!(
            "{base}/api/v1/repos/deploy-owner/one/collaborators"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "username": "deploy-writer",
            "permission": "write"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(collaborator.status(), 201);
    let writer_denied = client
        .get(&one_keys)
        .bearer_auth(&writer_token)
        .send()
        .await
        .unwrap();
    assert_eq!(writer_denied.status(), 403);

    let created = client
        .post(&one_keys)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "production reader",
            "key": ED25519_KEY,
            "read_only": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created = created.json::<serde_json::Value>().await.unwrap();
    let key_id = created["id"].as_i64().unwrap();
    assert_eq!(created["read_only"], true);

    let listed = client
        .get(&one_keys)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    assert_eq!(
        listed.json::<Vec<serde_json::Value>>().await.unwrap().len(),
        1
    );

    let duplicate_user_key = client
        .post(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "duplicate", "key": ED25519_KEY}))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate_user_key.status(), 409);

    let wrong_repo_delete = client
        .delete(format!(
            "{base}/api/v1/repos/deploy-owner/two/keys/{key_id}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_repo_delete.status(), 404);

    let deleted = client
        .delete(format!("{one_keys}/{key_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);
}
