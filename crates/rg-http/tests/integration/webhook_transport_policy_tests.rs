use crate::common::{create_repo, register_full, spawn_test_app_with_overrides, StateOverrides};

async fn app_with_repo(
    prefix: &str,
    policy: Option<rg_core::webhook::transport::WebhookTransportPolicy>,
) -> (String, String) {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides {
        webhook_transport_policy: policy,
        ..Default::default()
    })
    .await;
    let (token, _) = register_full(
        &base,
        &format!("{prefix}-owner"),
        &format!("{prefix}@example.com"),
    )
    .await;
    create_repo(&base, &token, &format!("{prefix}-repo")).await;
    (base, token)
}

#[tokio::test]
async fn plaintext_webhook_targets_are_refused_on_create_and_update_by_default() {
    let (base, token) = app_with_repo("secure-hook", None).await;
    let client = reqwest::Client::new();
    let hooks = format!("{base}/api/v1/repos/secure-hook-owner/secure-hook-repo/hooks");

    let response = client
        .post(&hooks)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "http://example.com/hook",
            "events": ["push"]
        }))
        .send()
        .await
        .expect("create plaintext webhook");
    assert_eq!(response.status(), 400);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("allow_insecure_http")),
        "the refusal must name the explicit operator escape hatch: {body}"
    );

    let response = client
        .post(&hooks)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "https://example.com/hook",
            "events": ["push"]
        }))
        .send()
        .await
        .expect("create HTTPS webhook");
    assert_eq!(response.status(), 201);
    let hook: serde_json::Value = response.json().await.expect("created webhook json");
    let hook_id = hook["id"].as_i64().expect("webhook id");

    let response = client
        .patch(format!("{hooks}/{hook_id}"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": "http://example.org/repointed"}))
        .send()
        .await
        .expect("repoint webhook to plaintext HTTP");
    assert_eq!(
        response.status(),
        400,
        "an update must not bypass the create-time transport rule"
    );
}

#[tokio::test]
async fn the_operator_opt_in_admits_public_http_without_weakening_ssrf() {
    let policy = rg_core::webhook::transport::WebhookTransportPolicy::new(true);
    let (base, token) = app_with_repo("insecure-hook", Some(policy)).await;
    let client = reqwest::Client::new();
    let hooks = format!("{base}/api/v1/repos/insecure-hook-owner/insecure-hook-repo/hooks");

    let response = client
        .post(&hooks)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "http://example.com/hook",
            "events": ["push"]
        }))
        .send()
        .await
        .expect("create explicitly allowed HTTP webhook");
    assert_eq!(response.status(), 201);

    let response = client
        .post(&hooks)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "http://127.0.0.1/hook",
            "events": ["push"]
        }))
        .send()
        .await
        .expect("try loopback webhook under transport opt-in");
    assert_eq!(
        response.status(),
        400,
        "plaintext transport opt-in must not become an SSRF bypass"
    );
}
