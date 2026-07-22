mod common;

use base64::Engine as _;
use common::{register_full, spawn_test_app_with_db};

async fn create_repo(base: &str, token: &str, name: &str, is_private: bool) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "is_private": is_private
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create repo failed");
}

fn basic_auth(username: &str, password: &str) -> String {
    let raw = format!("{}:{}", username, password);
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

async fn request_oci_token(
    base: &str,
    scope: &str,
    auth_header: Option<String>,
) -> rg_core::auth::oci_token::OciTokenClaims {
    let client = reqwest::Client::new();
    let mut req = client
        .get(format!("{}/v2/auth/token", base))
        .query(&[("service", "ironforge-registry"), ("scope", scope)]);
    if let Some(auth) = auth_header {
        req = req.header(reqwest::header::AUTHORIZATION, auth);
    }

    let resp = req.send().await.unwrap();
    assert_eq!(resp.status(), 200, "token request failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    let token = body["token"].as_str().unwrap();
    rg_core::auth::oci_token::validate_oci_token(token, "test-secret-key").expect("valid OCI token")
}

#[tokio::test]
async fn private_oci_tags_require_read_access() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) = register_full(&base, "oci_owner", "oci_owner@example.com").await;
    let (other_token, _other_id) = register_full(&base, "oci_other", "oci_other@example.com").await;
    create_repo(&base, &owner_token, "private-oci", true).await;

    let anon_resp = client
        .get(format!("{}/v2/oci_owner/private-oci/tags/list", base))
        .send()
        .await
        .unwrap();
    assert_eq!(anon_resp.status(), 401);

    let other_resp = client
        .get(format!("{}/v2/oci_owner/private-oci/tags/list", base))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(other_resp.status(), 401);

    let owner_resp = client
        .get(format!("{}/v2/oci_owner/private-oci/tags/list", base))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(owner_resp.status(), 404);
}

#[tokio::test]
async fn oci_token_endpoint_grants_only_authorized_scopes() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (owner_token, _owner_id) =
        register_full(&base, "oci_token_owner", "oci_token_owner@example.com").await;
    let (_other_token, _other_id) =
        register_full(&base, "oci_token_other", "oci_token_other@example.com").await;
    create_repo(&base, &owner_token, "public-image", false).await;
    create_repo(&base, &owner_token, "private-image", true).await;

    let public_pull =
        request_oci_token(&base, "repository:oci_token_owner/public-image:pull", None).await;
    assert_eq!(
        public_pull.scope.as_deref(),
        Some("repository:oci_token_owner/public-image:pull")
    );

    let private_anon =
        request_oci_token(&base, "repository:oci_token_owner/private-image:pull", None).await;
    assert!(private_anon.scope.is_none());

    let owner_push = request_oci_token(
        &base,
        "repository:oci_token_owner/private-image:pull,push",
        Some(basic_auth("oci_token_owner", "Qz7$wRtm")),
    )
    .await;
    assert_eq!(
        owner_push.scope.as_deref(),
        Some("repository:oci_token_owner/private-image:pull,push")
    );

    let other_push = request_oci_token(
        &base,
        "repository:oci_token_owner/private-image:pull,push",
        Some(basic_auth("oci_token_other", "Qz7$wRtm")),
    )
    .await;
    assert!(other_push.scope.is_none());
}
