use crate::common::{create_repo, register_full, spawn_test_app_with_db};

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
    assert_eq!(
        claims.sub, "repo:oidc_owner/oidc-repo:ref:refs/heads/main",
        "a branch job is addressed by repository and ref"
    );
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

/// Security audit finding #11: the subject scopes the token to the environment
/// or the ref instead of a bare repository id, so a production IAM role can
/// require `sub = repo:owner/name:environment:production` rather than accept
/// every branch of the repository. The ref and numeric ids stay as claims.
#[allow(clippy::too_many_arguments)] // test helper: one argument per claim keeps the call sites readable
async fn claims_for(
    db: &rg_db::DatabaseConnection,
    base: &str,
    client: &reqwest::Client,
    repo_id: i64,
    owner_id: i64,
    ref_name: &str,
    trigger_type: &str,
    environment: Option<&rg_db::entities::ci_environment::Model>,
) -> rg_core::auth::ci_oidc::CiOidcClaims {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        ref_name,
        trigger_type,
        Some(owner_id),
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "deploy", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "federate", "true", None, None, None, None, None, None, false, None, None,
        None,
    )
    .await
    .unwrap();
    if let Some(environment) = environment {
        rg_db::ops::ci_environment_ops::attach_job(
            db,
            job.id,
            Some(environment),
            &environment.name,
        )
        .await
        .unwrap();
    }
    rg_db::ops::pipeline_ops::update_job_result(
        db,
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
    let token = client
        .get(format!("{base}/api/v1/ci/oidc/token?audience=sts.example"))
        .bearer_auth(&ci_token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["value"]
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
    let public_x = jwks["keys"][0]["x"].as_str().unwrap();
    let key = jsonwebtoken::DecodingKey::from_ed_components(public_x).unwrap();
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
    validation.set_audience(&["sts.example"]);
    validation.set_issuer(&[format!("{base}/api/v1/ci/oidc")]);
    validation.required_spec_claims.clear();
    jsonwebtoken::decode::<rg_core::auth::ci_oidc::CiOidcClaims>(&token, &key, &validation)
        .unwrap()
        .claims
}

#[tokio::test]
async fn environment_and_pull_request_jobs_get_their_own_subject_shapes() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, owner_id) =
        register_full(&base, "oidc_env_owner", "oidc_env@example.com").await;
    let repo_id = create_repo(&base, &owner_token, "oidc-env-repo").await;
    let environment_response = client
        .post(format!(
            "{base}/api/v1/repos/oidc_env_owner/oidc-env-repo/actions/environments"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"name": "production"}))
        .send()
        .await
        .unwrap();
    assert_eq!(environment_response.status(), 201);
    let production = rg_db::ops::ci_environment_ops::find_by_name(&db, repo_id, "production")
        .await
        .unwrap()
        .unwrap();

    let production_claims = claims_for(
        &db,
        &base,
        &client,
        repo_id,
        owner_id,
        "refs/heads/main",
        "push",
        Some(&production),
    )
    .await;
    assert_eq!(
        production_claims.sub,
        "repo:oidc_env_owner/oidc-env-repo:environment:production"
    );
    assert_eq!(production_claims.environment.as_deref(), Some("production"));
    // The ref still travels alongside, but no branch spells this subject.
    assert_eq!(production_claims.ref_name, "refs/heads/main");
    assert_eq!(production_claims.ref_type, "branch");
    assert_eq!(production_claims.repository, "oidc_env_owner/oidc-env-repo");
    assert_eq!(production_claims.repository_owner, "oidc_env_owner");
    assert_eq!(production_claims.repository_id, repo_id);
    assert_eq!(production_claims.actor.as_deref(), Some("oidc_env_owner"));

    let pull_request_claims = claims_for(
        &db,
        &base,
        &client,
        repo_id,
        owner_id,
        "refs/pull/7/head",
        "pull_request",
        None,
    )
    .await;
    assert_eq!(
        pull_request_claims.sub,
        "repo:oidc_env_owner/oidc-env-repo:ref:refs/pull/7/head"
    );
    assert_eq!(pull_request_claims.ref_type, "pull_request");
    assert_eq!(pull_request_claims.environment, None);
}
