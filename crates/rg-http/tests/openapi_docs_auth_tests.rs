mod common;

use common::{register_user, spawn_test_app};

async fn create_pat(base: &str, jwt: &str) -> String {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/users/tokens", base))
        .bearer_auth(jwt)
        .json(&serde_json::json!({"name": "docs-cli"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create token failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn api_docs_openapi_requires_auth() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api-docs/openapi.json", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn api_docs_openapi_accepts_jwt_and_pat() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "docview", "docview@example.com", "Qz7$wRtm").await;
    let pat = create_pat(&base, &jwt).await;
    let client = reqwest::Client::new();

    let jwt_resp = client
        .get(format!("{}/api-docs/openapi.json", base))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(jwt_resp.status(), 200);
    assert!(jwt_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .starts_with("application/json"));

    let pat_resp = client
        .get(format!("{}/api-docs/openapi.json", base))
        .bearer_auth(&pat)
        .send()
        .await
        .unwrap();
    assert_eq!(pat_resp.status(), 200);
    assert!(pat_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .starts_with("application/json"));
}

#[tokio::test]
async fn api_docs_ui_requires_auth() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api-docs/", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}
