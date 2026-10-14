//! Environment-scoped CI secrets (PR #16, security audit finding #11): a
//! secret meant for `production` must not reach every job of every branch.
//!
//! These drive the halves that keep it out: the settings API that writes the
//! scope, and the external runner's job body that assembles it.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn create_environment(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    repo: &str,
    name: &str,
    protected: bool,
) -> i64 {
    let response = client
        .post(format!(
            "{base}/api/v1/repos/scope-owner/{repo}/actions/environments"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "protected": protected }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "create environment {name}");
    response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn put_secret(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    repo: &str,
    name: &str,
    value: &str,
    environment: Option<&str>,
) -> reqwest::Response {
    client
        .put(format!(
            "{base}/api/v1/repos/scope-owner/{repo}/actions/secrets/{name}"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({ "value": value, "environment": environment }))
        .send()
        .await
        .unwrap()
}

async fn delete_secret(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    repo: &str,
    query: &str,
) -> u16 {
    client
        .delete(format!(
            "{base}/api/v1/repos/scope-owner/{repo}/actions/secrets/DEPLOY_TOKEN{query}"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// One pending job in a fresh pipeline.
async fn pending_job(db: &rg_db::DatabaseConnection, repo_id: i64, name: &str) -> i64 {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "deploy", 0)
        .await
        .unwrap();
    rg_db::ops::pipeline_ops::create_job(
        db, stage.id, name, "true", None, None, None, None, None, None, false, None, None, None,
    )
    .await
    .unwrap()
    .id
}

async fn poll(
    client: &reqwest::Client,
    base: &str,
    runner_id: i64,
    token: &str,
) -> Option<serde_json::Value> {
    let response = client
        .get(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/poll?timeout=1"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    match response.status().as_u16() {
        200 => Some(response.json().await.unwrap()),
        204 => None,
        status => panic!("unexpected poll status {status}"),
    }
}

/// The settings API addresses a secret by name and scope, keeps the scopes
/// apart, and refuses an environment the repository does not have instead of
/// falling back to the repository-wide scope.
#[tokio::test]
async fn secrets_are_addressed_by_name_and_scope() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(&base, "scope-owner", "scope-owner@example.com").await;
    create_repo(&base, &token, "vault").await;
    create_environment(&client, &base, &token, "vault", "staging", false).await;

    let scoped = put_secret(
        &client,
        &base,
        &token,
        "vault",
        "DEPLOY_TOKEN",
        "staging-v",
        Some("staging"),
    )
    .await;
    assert_eq!(scoped.status(), 201);
    assert_eq!(
        scoped.json::<serde_json::Value>().await.unwrap()["environment"],
        "staging"
    );
    let wide = put_secret(
        &client,
        &base,
        &token,
        "vault",
        "DEPLOY_TOKEN",
        "wide-v",
        None,
    )
    .await;
    assert_eq!(
        wide.status(),
        201,
        "the same name in another scope is another secret"
    );
    assert!(wide.json::<serde_json::Value>().await.unwrap()["environment"].is_null());

    let listed = client
        .get(format!(
            "{base}/api/v1/repos/scope-owner/vault/actions/secrets"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let scopes = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["environment"].clone())
        .collect::<Vec<_>>();
    assert_eq!(scopes.len(), 2, "{listed}");
    assert!(scopes.contains(&serde_json::Value::Null) && scopes.contains(&"staging".into()));
    assert!(
        !listed.to_string().contains("-v\""),
        "a listing carries no value: {listed}"
    );

    let unknown = put_secret(
        &client,
        &base,
        &token,
        "vault",
        "OTHER",
        "value",
        Some("nowhere"),
    )
    .await;
    assert_eq!(unknown.status(), 404);
    assert_eq!(
        delete_secret(&client, &base, &token, "vault", "?environment=nowhere").await,
        404
    );

    // Deleting one scope leaves the other: the unscoped route names the
    // repository-wide row, not whichever came first.
    assert_eq!(
        delete_secret(&client, &base, &token, "vault", "?environment=staging").await,
        204
    );
    assert_eq!(
        delete_secret(&client, &base, &token, "vault", "?environment=staging").await,
        404
    );
    assert_eq!(
        delete_secret(&client, &base, &token, "vault", "").await,
        204
    );
    assert_eq!(
        delete_secret(&client, &base, &token, "vault", "").await,
        404
    );
}

/// A job without `environment:` reads the repository-wide scope only; a job
/// that declares one reads that scope too, and no other; a protected
/// environment's job reads nothing until its gate releases it. Where both
/// scopes set a name, the environment's value is the one the job gets.
#[tokio::test]
async fn a_job_reads_its_own_environment_scope_and_the_environment_value_wins() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, owner_id) = register_full(&base, "scope-owner", "scope-owner@example.com").await;
    let repo_id = create_repo(&base, &token, "deploys").await;
    create_environment(&client, &base, &token, "deploys", "staging", false).await;
    let production_id =
        create_environment(&client, &base, &token, "deploys", "production", true).await;

    for (name, value, environment) in [
        ("REPO_KEY", "repo-value", None),
        ("DEPLOY_TOKEN", "repository-deploy", None),
        ("STAGING_KEY", "staging-value", Some("staging")),
        ("PROD_KEY", "prod-value", Some("production")),
        ("DEPLOY_TOKEN", "production-deploy", Some("production")),
    ] {
        let response =
            put_secret(&client, &base, &token, "deploys", name, value, environment).await;
        assert_eq!(response.status(), 201, "store {name} in {environment:?}");
    }
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "scope-runner",
        "[]",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let plain = pending_job(&db, repo_id, "plain").await;
    let polled = poll(&client, &base, runner.id, &runner_token)
        .await
        .expect("plain job");
    assert_eq!(polled["job_id"], plain);
    let variables = &polled["variables"];
    assert_eq!(variables["REPO_KEY"], "repo-value");
    assert_eq!(variables["DEPLOY_TOKEN"], "repository-deploy");
    assert!(
        variables.get("STAGING_KEY").is_none() && variables.get("PROD_KEY").is_none(),
        "a job without an environment read an environment scope: {variables}"
    );

    let staging = rg_db::ops::ci_environment_ops::find_by_name(&db, repo_id, "staging")
        .await
        .unwrap()
        .unwrap();
    let staged = pending_job(&db, repo_id, "staged").await;
    rg_db::ops::ci_environment_ops::attach_job(&db, staged, Some(&staging), "staging")
        .await
        .unwrap();
    let polled = poll(&client, &base, runner.id, &runner_token)
        .await
        .expect("staging job");
    assert_eq!(polled["job_id"], staged);
    let variables = &polled["variables"];
    assert_eq!(variables["STAGING_KEY"], "staging-value");
    assert_eq!(variables["DEPLOY_TOKEN"], "repository-deploy");
    assert!(
        variables.get("PROD_KEY").is_none(),
        "staging read production: {variables}"
    );

    let production = rg_db::ops::ci_environment_ops::find_by_id(&db, production_id)
        .await
        .unwrap()
        .unwrap();
    let deploy = pending_job(&db, repo_id, "deploy").await;
    rg_db::ops::ci_environment_ops::attach_job(&db, deploy, Some(&production), "production")
        .await
        .unwrap();
    assert!(
        poll(&client, &base, runner.id, &runner_token)
            .await
            .is_none(),
        "a job the protected environment is holding reached a runner"
    );
    rg_db::ops::ci_environment_ops::add_approval(&db, deploy, production_id, owner_id)
        .await
        .unwrap();
    assert!(
        rg_db::ops::ci_environment_ops::release_approved_job(&db, deploy)
            .await
            .unwrap()
    );
    let polled = poll(&client, &base, runner.id, &runner_token)
        .await
        .expect("approved job");
    assert_eq!(polled["job_id"], deploy);
    let variables = &polled["variables"];
    assert_eq!(variables["PROD_KEY"], "prod-value");
    assert_eq!(
        variables["DEPLOY_TOKEN"], "production-deploy",
        "the repository-wide value of a name the environment sets reached the deploy"
    );
    assert!(
        variables.get("STAGING_KEY").is_none(),
        "production read staging: {variables}"
    );
}

/// Deleting an environment takes its secrets with it, and the journal names
/// them: the rows themselves are gone afterwards.
#[tokio::test]
async fn deleting_an_environment_records_the_secrets_it_removed() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(&base, "scope-owner", "scope-owner@example.com").await;
    let repo_id = create_repo(&base, &token, "retired").await;
    let environment_id =
        create_environment(&client, &base, &token, "retired", "legacy", false).await;
    for name in ["LEGACY_B", "LEGACY_A"] {
        let response = put_secret(
            &client,
            &base,
            &token,
            "retired",
            name,
            "value",
            Some("legacy"),
        )
        .await;
        assert_eq!(response.status(), 201);
    }
    let response = put_secret(&client, &base, &token, "retired", "KEPT", "value", None).await;
    assert_eq!(response.status(), 201);

    let deleted = client
        .delete(format!(
            "{base}/api/v1/repos/scope-owner/retired/actions/environments/{environment_id}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);

    let remaining = rg_db::ops::ci_secret_ops::list_by_repo(&db, repo_id)
        .await
        .unwrap();
    assert_eq!(
        remaining
            .iter()
            .map(|secret| secret.name.as_str())
            .collect::<Vec<_>>(),
        ["KEPT"]
    );
    let entry = rg_db::entities::audit_log::Entity::find()
        .filter(rg_db::entities::audit_log::Column::Action.eq("repo.environment_delete"))
        .order_by_desc(rg_db::entities::audit_log::Column::Id)
        .one(&db)
        .await
        .unwrap()
        .expect("the deletion is journalled");
    let details: serde_json::Value =
        serde_json::from_str(entry.details.as_deref().unwrap_or("{}")).unwrap();
    assert_eq!(
        details["secrets"],
        serde_json::json!(["LEGACY_A", "LEGACY_B"]),
        "{details}"
    );
}
