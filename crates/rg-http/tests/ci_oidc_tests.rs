mod common;

use common::{register_full, spawn_test_app_with_db};

#[tokio::test]
async fn oidc_exchange_is_audience_bound_and_persisted_job_bound() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, owner_id) = register_full(&base, "oidc_owner", "oidc@example.com").await;
    let repo_response = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"name":"oidc-repo","is_private":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(repo_response.status(), 201);
    let repo_id = repo_response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "refs/heads/main",
        "push",
        Some(owner_id),
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "deploy", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        &db, stage.id, "federate", "true", None, None, None, None, None, false, None, None, None,
    )
    .await
    .unwrap();
    rg_db::ops::pipeline_ops::update_job_result(
        &db,
        job.id,
        "running",
        None,
        None,
        Some(chrono::Utc::now().naive_utc()),
        None,
    )
    .await
    .unwrap();
    let ci_token = rg_core::auth::ci_token::generate_ci_job_token(
        repo_id,
        pipeline.id,
        job.id,
        "repo:read",
        "test-secret-key",
    )
    .unwrap();

    let discovery = client
        .get(format!(
            "{base}/api/v1/ci/oidc/.well-known/openid-configuration"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(discovery.status(), 200);
    let discovery = discovery.json::<serde_json::Value>().await.unwrap();
    assert_eq!(discovery["issuer"], format!("{base}/api/v1/ci/oidc"));
    let jwks = client
        .get(format!("{base}/api/v1/ci/oidc/jwks"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(jwks["keys"][0]["alg"], "EdDSA");
    assert_eq!(jwks["keys"][0]["kty"], "OKP");

    let denied = client
        .get(format!("{base}/api/v1/ci/oidc/token?audience=sts.example"))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);
    let issued = client
        .get(format!("{base}/api/v1/ci/oidc/token?audience=sts.example"))
        .bearer_auth(&ci_token)
        .send()
        .await
        .unwrap();
    assert_eq!(issued.status(), 200);
    assert_eq!(issued.headers()["cache-control"], "no-store");
    let token = issued.json::<serde_json::Value>().await.unwrap()["value"]
        .as_str()
        .unwrap()
        .to_string();
    let public = rg_core::auth::ci_oidc::jwk("test-secret-key");
    let key = jsonwebtoken::DecodingKey::from_ed_components(&public.x).unwrap();
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
    validation.set_audience(&["sts.example"]);
    validation.set_issuer(&[format!("{base}/api/v1/ci/oidc")]);
    let claims =
        jsonwebtoken::decode::<rg_core::auth::ci_oidc::CiOidcClaims>(&token, &key, &validation)
            .unwrap()
            .claims;
    assert_eq!(claims.repository_id, repo_id);
    assert_eq!(claims.pipeline_id, pipeline.id);
    assert_eq!(claims.job_id, job.id);

    let forged_binding = rg_core::auth::ci_token::generate_ci_job_token(
        repo_id,
        pipeline.id + 1,
        job.id,
        "repo:read",
        "test-secret-key",
    )
    .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/api/v1/ci/oidc/token?audience=sts.example"))
            .bearer_auth(forged_binding)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    rg_db::ops::pipeline_ops::update_job_result(
        &db,
        job.id,
        "success",
        Some(0),
        None,
        None,
        Some(chrono::Utc::now().naive_utc()),
    )
    .await
    .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/api/v1/ci/oidc/token?audience=sts.example"))
            .bearer_auth(ci_token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
}
