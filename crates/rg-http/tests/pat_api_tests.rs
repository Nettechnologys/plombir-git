//! Verifies that a Personal Access Token authenticates REST API calls, not
//! just git-over-HTTP. The API handlers validate JWTs; a middleware translates
//! a PAT into an equivalent Bearer JWT so PAT-based API access works.

mod common;
use common::{register_user, spawn_test_app};

use base64::Engine as _;

async fn create_pat(base: &str, jwt: &str) -> String {
    create_pat_with_scopes(base, jwt, None).await
}

async fn create_pat_with_scopes(base: &str, jwt: &str, scopes: Option<&str>) -> String {
    let client = reqwest::Client::new();
    let mut body = serde_json::json!({ "name": "api-cli" });
    if let Some(scopes) = scopes {
        body["scopes"] = serde_json::Value::String(scopes.to_string());
    }
    let resp = client
        .post(format!("{}/api/v1/users/tokens", base))
        .bearer_auth(jwt)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create token failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn pat_scopes_are_enforced_by_api_family() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "patscope", "patscope@example.com", "Qz7$wRtm").await;
    let user_pat = create_pat_with_scopes(&base, &jwt, Some("user")).await;
    let repo_pat = create_pat_with_scopes(&base, &jwt, Some("repo")).await;
    let combined_pat = create_pat_with_scopes(&base, &jwt, Some("user, repo")).await;
    let client = reqwest::Client::new();

    let denied_repo = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(&user_pat)
        .json(&serde_json::json!({ "name": "scope-denied" }))
        .send()
        .await
        .unwrap();
    assert_eq!(denied_repo.status(), 403);

    let allowed_user = client
        .get(format!("{}/api/v1/users/me", base))
        .bearer_auth(&user_pat)
        .send()
        .await
        .unwrap();
    assert_eq!(allowed_user.status(), 200);

    let denied_user = client
        .get(format!("{}/api/v1/users/me", base))
        .bearer_auth(&repo_pat)
        .send()
        .await
        .unwrap();
    assert_eq!(denied_user.status(), 403);

    let allowed_repo = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(&combined_pat)
        .json(&serde_json::json!({ "name": "scope-allowed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed_repo.status(), 201);
}

#[tokio::test]
async fn unknown_pat_scope_is_rejected_at_creation() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "patscope2", "patscope2@example.com", "Qz7$wRtm").await;
    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/users/tokens", base))
        .bearer_auth(jwt)
        .json(&serde_json::json!({
            "name": "bad-scope",
            "scopes": "repo,delete_everything"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

/// Creating a repo requires auth; do it with a PAT as a Bearer token.
#[tokio::test]
async fn pat_authenticates_api_via_bearer() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "patapi1", "patapi1@example.com", "Qz7$wRtm").await;
    let pat = create_pat(&base, &jwt).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(&pat) // PAT, not JWT
        .json(&serde_json::json!({ "name": "via-pat" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "PAT should authenticate repo creation");
}

/// Same, but the PAT presented via HTTP Basic auth (`user:token`).
#[tokio::test]
async fn pat_authenticates_api_via_basic() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "patapi2", "patapi2@example.com", "Qz7$wRtm").await;
    let pat = create_pat(&base, &jwt).await;

    let client = reqwest::Client::new();
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("patapi2:{}", pat));
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .header("Authorization", format!("Basic {}", basic))
        .json(&serde_json::json!({ "name": "via-basic" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "PAT via Basic auth should authenticate the API"
    );
}

/// An invalid/garbage token must NOT authenticate.
#[tokio::test]
async fn invalid_token_is_rejected_by_api() {
    let base = spawn_test_app().await;
    let _ = register_user(&base, "patapi3", "patapi3@example.com", "Qz7$wRtm").await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth("not-a-real-token")
        .json(&serde_json::json!({ "name": "nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "garbage token must be rejected");
}
