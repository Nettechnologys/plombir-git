use crate::common::{register_full, spawn_test_app_with_db};

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
        &db, stage.id, "federate", "true", None, None, None, None, None, None, false, None, None,
        None,
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
    let ci_token = rg_core::auth::ci_token::generate_ci_job_token_with_ttl(
        repo_id,
        pipeline.id,
        job.id,
        "repo:read",
        "test-secret-key",
        3600,
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
    // Verify through the published JWKS, the way an external relying party
    // would — and never by re-deriving the key from a secret, which is the
    // coupling card_3aecf3708ebe removed.
    let public_x = jwks["keys"][0]["x"].as_str().unwrap();
    let key = jsonwebtoken::DecodingKey::from_ed_components(public_x).unwrap();
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
    assert_eq!(claims.sub, "repo:oidc_owner/oidc-repo:ref:refs/heads/main");
    assert_eq!(claims.repository, "oidc_owner/oidc-repo");
    assert_eq!(claims.repository_owner, "oidc_owner");
    assert_eq!(claims.ref_type, "branch");
    assert_eq!(claims.environment, None);
    assert_eq!(claims.actor.as_deref(), Some("oidc_owner"));

    let forged_binding = rg_core::auth::ci_token::generate_ci_job_token_with_ttl(
        repo_id,
        pipeline.id + 1,
        job.id,
        "repo:read",
        "test-secret-key",
        3600,
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

/// PR #16: a job released by an environment's gate is issued
/// `repo:<owner>/<name>:environment:<env>`, whatever ref it runs from, so a
/// cloud role can trust the environment instead of every branch of the
/// repository.
#[tokio::test]
async fn an_environment_job_is_issued_the_environment_subject() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _) = register_full(&base, "oidc_env", "oidc-env@example.com").await;
    let repo_id = crate::common::create_repo(&base, &owner_token, "deploys").await;
    let created = client
        .post(format!(
            "{base}/api/v1/repos/oidc_env/deploys/actions/environments"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "name": "production", "protected": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let environment = rg_db::ops::ci_environment_ops::find_by_name(&db, repo_id, "production")
        .await
        .unwrap()
        .unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "refs/pull/7/head",
        "pull_request",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "deploy", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        &db, stage.id, "deploy", "true", None, None, None, None, None, None, false, None, None,
        None,
    )
    .await
    .unwrap();
    rg_db::ops::ci_environment_ops::attach_job(&db, job.id, Some(&environment), "production")
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
    let ci_token = rg_core::auth::ci_token::generate_ci_job_token_with_ttl(
        repo_id,
        pipeline.id,
        job.id,
        "repo:read",
        "test-secret-key",
        3600,
    )
    .unwrap();

    let issued = client
        .get(format!("{base}/api/v1/ci/oidc/token?audience=sts.example"))
        .bearer_auth(&ci_token)
        .send()
        .await
        .unwrap();
    assert_eq!(issued.status(), 200);
    let token = issued.json::<serde_json::Value>().await.unwrap()["value"]
        .as_str()
        .unwrap()
        .to_string();
    let jwks = client
        .get(format!("{base}/api/v1/ci/oidc/jwks"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let key = jsonwebtoken::DecodingKey::from_ed_components(jwks["keys"][0]["x"].as_str().unwrap())
        .unwrap();
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
    validation.set_audience(&["sts.example"]);
    let claims =
        jsonwebtoken::decode::<rg_core::auth::ci_oidc::CiOidcClaims>(&token, &key, &validation)
            .unwrap()
            .claims;
    assert_eq!(claims.sub, "repo:oidc_env/deploys:environment:production");
    assert_eq!(claims.environment.as_deref(), Some("production"));
    assert_eq!(claims.ref_name, "refs/pull/7/head");
    assert_eq!(claims.ref_type, "pull_request");
    assert_eq!(
        claims.actor, None,
        "a pipeline nobody triggered names no actor"
    );
}
