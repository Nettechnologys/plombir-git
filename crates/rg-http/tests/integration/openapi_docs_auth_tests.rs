use crate::common::{register_user, spawn_test_app};

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

/// card_fb094ba6d323: the docs gate used to read `Authorization: Bearer` and
/// nothing else.
///
/// Swagger UI is a browser surface, and a browser cannot put a header on a
/// plain navigation — it sends the HttpOnly `forgekeep_token` cookie. So the one
/// audience `/api-docs/` exists for was answered `401` on the HTML itself,
/// before any script ran, while `curl -H "Authorization: Bearer …"` worked
/// fine. The gate is `AuthUser` now, which reads the cookie first.
///
/// The anonymous request is in this test rather than only in
/// [`api_docs_ui_requires_auth`] on purpose: a `401` proves the gate turned
/// somebody away only next to a `200` proving it lets the right caller in. A
/// broken fixture would otherwise read as a passing security test.
#[tokio::test]
async fn api_docs_accepts_a_cookie_session_and_still_refuses_an_anonymous_caller() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "docscookie", "docscookie@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    for path in ["/api-docs/", "/api-docs/openapi.json"] {
        let with_cookie = client
            .get(format!("{}{}", base, path))
            .header("cookie", format!("forgekeep_token={jwt}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            with_cookie.status(),
            200,
            "{path} refused a browser carrying the session cookie"
        );

        let anonymous = client
            .get(format!("{}{}", base, path))
            .send()
            .await
            .unwrap();
        assert_eq!(
            anonymous.status(),
            401,
            "{path} let an anonymous caller in — the baseline above proves the fixture works"
        );
    }
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
