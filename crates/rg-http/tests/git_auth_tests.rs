//! End-to-end tests for git-over-HTTP authentication.
//!
//! Verifies that a private repository can be accessed over the Git Smart HTTP
//! protocol using a Personal Access Token (PAT) — both via HTTP Basic auth
//! (`git clone https://user:<token>@host/...`) and `Authorization: Bearer`.
//! Previously the git path only accepted JWT session tokens, so PAT-based
//! clone/push of private repos silently failed with 401.

mod common;
use common::{register_user, spawn_test_app};

use base64::Engine as _;

const INFO_REFS: &str = "info/refs?service=git-upload-pack";

async fn create_private_repo(base: &str, jwt: &str, name: &str) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(jwt)
        .json(&serde_json::json!({ "name": name, "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create private repo failed");
}

async fn create_pat(base: &str, jwt: &str) -> String {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/users/tokens", base))
        .bearer_auth(jwt)
        .json(&serde_json::json!({ "name": "git-cli" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create token failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn private_repo_info_refs_requires_auth() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "gauth1", "gauth1@example.com", "Qz7$wRtm").await;
    create_private_repo(&base, &jwt, "secret").await;

    let client = reqwest::Client::new();
    // Anonymous access to a private repo must be rejected.
    let resp = client
        .get(format!("{}/git/gauth1/secret/{}", base, INFO_REFS))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "anonymous access to private repo should be 401"
    );
}

#[tokio::test]
async fn private_repo_clone_with_pat_basic_auth() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "gauth2", "gauth2@example.com", "Qz7$wRtm").await;
    create_private_repo(&base, &jwt, "secret").await;
    let pat = create_pat(&base, &jwt).await;

    let client = reqwest::Client::new();

    // PAT in the password field: `git clone https://user:<token>@host/...`
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("gauth2:{}", pat));
    let resp = client
        .get(format!("{}/git/gauth2/secret/{}", base, INFO_REFS))
        .header("Authorization", format!("Basic {}", basic))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "PAT via Basic auth should grant access");

    // Same PAT presented as a Bearer token.
    let resp = client
        .get(format!("{}/git/gauth2/secret/{}", base, INFO_REFS))
        .bearer_auth(&pat)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "PAT via Bearer should grant access");
}

#[tokio::test]
async fn private_repo_rejects_invalid_pat() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "gauth3", "gauth3@example.com", "Qz7$wRtm").await;
    create_private_repo(&base, &jwt, "secret").await;

    let client = reqwest::Client::new();
    let basic = base64::engine::general_purpose::STANDARD.encode("gauth3:not-a-real-token");
    let resp = client
        .get(format!("{}/git/gauth3/secret/{}", base, INFO_REFS))
        .header("Authorization", format!("Basic {}", basic))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "invalid PAT should be rejected");
}
