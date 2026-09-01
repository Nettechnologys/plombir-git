use super::common::{create_repo, register_full, spawn_test_app_with_overrides, StateOverrides};

async fn app_with_repo(
    prefix: &str,
    policy: Option<rg_core::mirror::transport::MirrorTransportPolicy>,
) -> (String, String, sea_orm::DatabaseConnection) {
    let (base, db) = spawn_test_app_with_overrides(StateOverrides {
        mirror_transport_policy: policy,
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
    (base, token, db)
}

#[tokio::test]
async fn plaintext_mirror_urls_are_refused_while_https_round_trips() {
    let (base, token, db) = app_with_repo("secure-mirror", None).await;
    let client = reqwest::Client::new();
    let mirror = format!("{base}/api/v1/repos/secure-mirror-owner/secure-mirror-repo/mirror");

    let response = client
        .post(&mirror)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "http://example.com/upstream.git",
            "username": "sync-bot",
            "password": "hunter2",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("create plaintext mirror");
    assert_eq!(response.status(), 400);
    let body: serde_json::Value = response.json().await.expect("error json");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("[mirror].allow_insecure_http")),
        "the refusal must name the instance-operator escape hatch: {body}"
    );

    let response = client
        .post(&mirror)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "https://example.com/upstream.git",
            "username": "sync-bot",
            "password": "hunter2",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("create HTTPS mirror");
    assert_eq!(response.status(), 201);
    let created: serde_json::Value = response.json().await.expect("created mirror json");
    assert_eq!(created["url"], "https://example.com/upstream.git");
    assert_eq!(created["has_credentials"], true);

    let response = client
        .patch(&mirror)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "https://example.org/repointed.git",
            "sync_interval_seconds": 7200,
        }))
        .send()
        .await
        .expect("update HTTPS mirror");
    assert_eq!(response.status(), 200);

    let response = client
        .patch(&mirror)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": "http://example.net/downgraded.git"}))
        .send()
        .await
        .expect("repoint mirror to plaintext HTTP");
    assert_eq!(
        response.status(),
        400,
        "an update must not bypass the create-time transport rule"
    );

    let current: serde_json::Value = client
        .get(&mirror)
        .bearer_auth(&token)
        .send()
        .await
        .expect("read mirror")
        .json()
        .await
        .expect("mirror json");
    assert_eq!(
        current["url"], "https://example.org/repointed.git",
        "a refused downgrade must not modify the stored HTTPS remote"
    );
    assert_eq!(current["has_credentials"], true);

    // Simulate a row created before the policy existed. A PATCH that omits
    // `url` still operates on this effective remote, and attaching a new
    // credential to it must not preserve the plaintext configuration.
    let repo_id = created["repo_id"].as_i64().expect("repo id");
    let legacy = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("read mirror row")
        .expect("mirror row");
    let mut legacy: rg_db::entities::mirror::ActiveModel = legacy.into();
    legacy.url = sea_orm::ActiveValue::Set("http://example.com/legacy.git".to_string());
    rg_db::ops::mirror_ops::update(&db, legacy)
        .await
        .expect("plant legacy plaintext row");

    let response = client
        .patch(&mirror)
        .bearer_auth(&token)
        .json(&serde_json::json!({"password": "replacement"}))
        .send()
        .await
        .expect("update credential on legacy plaintext mirror");
    assert_eq!(
        response.status(),
        400,
        "omitting url must not bypass validation of a legacy plaintext remote"
    );
}

#[tokio::test]
async fn operator_opt_in_admits_public_http_without_weakening_ssrf() {
    let policy = rg_core::mirror::transport::MirrorTransportPolicy::new(true);
    let (base, token, _db) = app_with_repo("insecure-mirror", Some(policy)).await;
    let client = reqwest::Client::new();
    let mirror = format!("{base}/api/v1/repos/insecure-mirror-owner/insecure-mirror-repo/mirror");

    let response = client
        .post(&mirror)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "http://example.com/upstream.git",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("create explicitly allowed HTTP mirror");
    assert_eq!(response.status(), 201);

    let second_repo = "insecure-mirror-loopback";
    create_repo(&base, &token, second_repo).await;
    let response = client
        .post(format!(
            "{base}/api/v1/repos/insecure-mirror-owner/{second_repo}/mirror"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": "http://127.0.0.1/upstream.git",
            "sync_interval_seconds": 3600,
        }))
        .send()
        .await
        .expect("try loopback mirror under transport opt-in");
    assert_eq!(
        response.status(),
        400,
        "plaintext transport opt-in must not become an SSRF bypass"
    );
}
