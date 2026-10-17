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

/// A complete commit history is either returned as a snapshot or rejected.
/// Keep the documented client and storage failures alongside that live contract
/// so a response annotation cannot accidentally land on a neighbouring handler.
#[tokio::test]
async fn commit_log_openapi_documents_client_and_storage_outcomes() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "logdocs", "logdocs@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    let spec: serde_json::Value = client
        .get(format!("{}/api-docs/openapi.json", base))
        .bearer_auth(jwt)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let responses = &spec["paths"]["/repos/{owner}/{name}/log"]["get"]["responses"];

    for status in ["400", "404", "500"] {
        assert!(
            responses[status].is_object(),
            "the published commit-log contract must document HTTP {status}: {responses}"
        );
    }
}

/// card_fb094ba6d323: the docs gate used to read `Authorization: Bearer` and
/// nothing else, so a browser — which cannot put a header on a plain
/// navigation and sends the HttpOnly `plombir_git_token` cookie — was refused.
/// The Swagger UI routes that exercised this are gone; the document they read
/// is still opened from a browser tab, so it must accept that cookie, a JWT and
/// a PAT, and must still refuse an anonymous caller. Redirects are disabled
/// deliberately: a redirect to a login page is not the document answering.
#[tokio::test]
async fn the_openapi_document_shares_the_live_auth_contract_without_redirects() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "docscookie", "docscookie@example.com", "Qz7$wRtm").await;
    let pat = create_pat(&base, &jwt).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let path = "/api-docs/openapi.json";
    let with_cookie = client
        .get(format!("{}{}", base, path))
        .header("cookie", format!("plombir_git_token={jwt}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        with_cookie.status(),
        200,
        "{path} refused a browser carrying the session cookie"
    );

    let with_jwt = client
        .get(format!("{}{}", base, path))
        .bearer_auth(&jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(with_jwt.status(), 200, "{path} refused a valid JWT");

    let with_pat = client
        .get(format!("{}{}", base, path))
        .bearer_auth(&pat)
        .send()
        .await
        .unwrap();
    assert_eq!(with_pat.status(), 200, "{path} refused a valid PAT");

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
