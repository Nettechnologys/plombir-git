//! Security audit finding #11: repository CI secrets reach every job, and OIDC
//! tokens name only the repository.
//!
//! These tests drive the two halves that keep an environment-scoped secret out
//! of the jobs that did not pass the environment's gate: the settings API that
//! writes the scope, and the runner job body that assembles it into `variables`.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn create_environment(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    name: &str,
    protected: bool,
) -> serde_json::Value {
    let response = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/actions/environments"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "protected": protected}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "create environment {name}");
    response.json().await.unwrap()
}

#[allow(clippy::too_many_arguments)] // test helper: one argument per secret field keeps the call sites readable
async fn put_secret(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    name: &str,
    value: &str,
    environment: Option<&str>,
) -> reqwest::Response {
    client
        .put(format!(
            "{base}/api/v1/repos/{owner}/{repo}/actions/secrets/{name}"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"value": value, "environment": environment}))
        .send()
        .await
        .unwrap()
}

/// A pipeline with one pending job; the caller attaches an environment to it if
/// the job should declare one.
async fn pending_job(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    ref_name: &str,
    trigger_type: &str,
    name: &str,
) -> i64 {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        ref_name,
        trigger_type,
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
        // The long poll elapsed with nothing claimable.
        204 => None,
        status => panic!("unexpected poll status {status}"),
    }
}

/// The settings API addresses a secret by name *and* environment, keeps the
/// scopes separate, and refuses an environment the repository does not have.
#[tokio::test]
async fn environment_scoped_secrets_are_addressed_by_name_and_scope() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(&base, "scope-owner", "scope-owner@example.com").await;
    create_repo(&base, &token, "scoped-vault").await;
    create_environment(
        &client,
        &base,
        &token,
        "scope-owner",
        "scoped-vault",
        "staging",
        false,
    )
    .await;

    let created = put_secret(
        &client,
        &base,
        &token,
        "scope-owner",
        "scoped-vault",
        "DEPLOY_TOKEN",
        "staging-value",
        Some("staging"),
    )
    .await;
    assert_eq!(created.status(), 201);
    assert_eq!(
        created.json::<serde_json::Value>().await.unwrap()["environment"],
        "staging"
    );

    // The same name in the repository-wide scope is a different secret.
    let repository_wide = put_secret(
        &client,
        &base,
        &token,
        "scope-owner",
        "scoped-vault",
        "DEPLOY_TOKEN",
        "repository-value",
        None,
    )
    .await;
    assert_eq!(repository_wide.status(), 201);
    assert!(
        repository_wide.json::<serde_json::Value>().await.unwrap()["environment"].is_null(),
        "a secret without an environment is repository-wide"
    );

    let listed = client
        .get(format!(
            "{base}/api/v1/repos/scope-owner/scoped-vault/actions/secrets"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let entries = listed.as_array().unwrap();
    assert_eq!(entries.len(), 2, "both scopes are listed: {listed}");
    assert!(
        entries
            .iter()
            .any(|entry| entry["environment"] == "staging"),
        "the environment name is shown: {listed}"
    );
    assert!(
        entries.iter().any(|entry| entry["environment"].is_null()),
        "the repository-wide scope is shown: {listed}"
    );
    assert!(
        !listed.to_string().contains("staging-value")
            && !listed.to_string().contains("repository-value"),
        "no listing may carry a value: {listed}"
    );

    let unknown = put_secret(
        &client,
        &base,
        &token,
        "scope-owner",
        "scoped-vault",
        "OTHER_TOKEN",
        "value",
        Some("does-not-exist"),
    )
    .await;
    assert_eq!(unknown.status(), 404);

    let deleted_scoped = client
        .delete(format!(
            "{base}/api/v1/repos/scope-owner/scoped-vault/actions/secrets/DEPLOY_TOKEN?environment=staging"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted_scoped.status(), 204);
    let deleted_again = client
        .delete(format!(
            "{base}/api/v1/repos/scope-owner/scoped-vault/actions/secrets/DEPLOY_TOKEN?environment=staging"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted_again.status(), 404);
    // Deleting the scoped secret left the repository-wide one alone; the
    // unscoped route has to name *that* row, not whichever came first.
    let deleted_repository_wide = client
        .delete(format!(
            "{base}/api/v1/repos/scope-owner/scoped-vault/actions/secrets/DEPLOY_TOKEN"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted_repository_wide.status(), 204);
}

/// A plain job reads repository-wide secrets only; the job that declares an
/// environment reads its environment's scope too, and no other environment's,
/// and only after the environment's gate released it.
#[tokio::test]
async fn a_plain_job_does_not_see_an_environment_secret_while_the_approved_job_does() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, owner_id) = register_full(&base, "gate-owner", "gate-owner@example.com").await;
    let repo_id = create_repo(&base, &token, "gated-deploy").await;
    create_environment(
        &client,
        &base,
        &token,
        "gate-owner",
        "gated-deploy",
        "staging",
        false,
    )
    .await;
    let production = create_environment(
        &client,
        &base,
        &token,
        "gate-owner",
        "gated-deploy",
        "production",
        true,
    )
    .await;
    let staging = rg_db::ops::ci_environment_ops::find_by_name(&db, repo_id, "staging")
        .await
        .unwrap()
        .unwrap();
    let production_id = production["id"].as_i64().unwrap();

    for (name, value, environment) in [
        ("REPO_KEY", "repo-value", None),
        ("STAGING_KEY", "staging-value", Some("staging")),
        ("PROD_KEY", "prod-value", Some("production")),
    ] {
        let response = put_secret(
            &client,
            &base,
            &token,
            "gate-owner",
            "gated-deploy",
            name,
            value,
            environment,
        )
        .await;
        assert_eq!(response.status(), 201, "store {name}");
    }

    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "gate-runner",
        "[]",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    // A job with no `environment:` reads the repository scope and nothing else.
    let plain_job = pending_job(&db, repo_id, "refs/heads/feature", "push", "plain").await;
    let polled = poll(&client, &base, runner.id, &runner_token)
        .await
        .expect("the plain job is runnable");
    assert_eq!(polled["job_id"], plain_job);
    let variables = &polled["variables"];
    assert_eq!(variables["REPO_KEY"], "repo-value");
    assert!(
        variables.get("STAGING_KEY").is_none() && variables.get("PROD_KEY").is_none(),
        "a job without an environment must not read environment scopes: {variables}"
    );

    // The same repository, a job that declares `environment: staging` — its
    // scope joins the repository-wide one, production's does not.
    let staging_job = pending_job(&db, repo_id, "refs/heads/main", "push", "staged").await;
    rg_db::ops::ci_environment_ops::attach_job(&db, staging_job, Some(&staging), "staging")
        .await
        .unwrap();
    let polled = poll(&client, &base, runner.id, &runner_token)
        .await
        .expect("an unprotected environment releases its job immediately");
    assert_eq!(polled["job_id"], staging_job);
    let variables = &polled["variables"];
    assert_eq!(variables["REPO_KEY"], "repo-value");
    assert_eq!(variables["STAGING_KEY"], "staging-value");
    assert!(
        variables.get("PROD_KEY").is_none(),
        "one environment must not read another's scope: {variables}"
    );

    // A protected environment holds its job until an approval releases it: the
    // job is not handed to a runner — and reads nothing — before that.
    let production_job = pending_job(&db, repo_id, "refs/heads/main", "push", "production").await;
    rg_db::ops::ci_environment_ops::attach_job(
        &db,
        production_job,
        Some(
            &rg_db::ops::ci_environment_ops::find_by_name(&db, repo_id, "production")
                .await
                .unwrap()
                .unwrap(),
        ),
        "production",
    )
    .await
    .unwrap();
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(&db, production_job)
            .await
            .unwrap()
            .unwrap()
            .status,
        "waiting_approval"
    );
    assert!(
        poll(&client, &base, runner.id, &runner_token)
            .await
            .is_none(),
        "a job the environment gate is still holding must not reach a runner"
    );

    rg_db::ops::ci_environment_ops::add_approval(&db, production_job, production_id, owner_id)
        .await
        .unwrap();
    assert!(
        rg_db::ops::ci_environment_ops::release_approved_job(&db, production_job)
            .await
            .unwrap()
    );
    let polled = poll(&client, &base, runner.id, &runner_token)
        .await
        .expect("the approved production job is runnable");
    assert_eq!(polled["job_id"], production_job);
    let variables = &polled["variables"];
    assert_eq!(variables["REPO_KEY"], "repo-value");
    assert_eq!(variables["PROD_KEY"], "prod-value");
    assert!(
        variables.get("STAGING_KEY").is_none(),
        "the approved job reads production, not its sibling scope: {variables}"
    );
}

/// A pull-request job from a fork can name a protected environment in its own
/// workflow, but it cannot read that environment's secrets (or the
/// repository's, since no runner can claim it) until the environment gate — an
/// approval that is separate from any fork-PR CI approval — lets it run.
#[tokio::test]
async fn a_fork_pr_job_without_its_environment_gate_sees_neither_scope() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(&base, "fork-owner", "fork-owner@example.com").await;
    let repo_id = create_repo(&base, &token, "fork-deploy").await;
    create_environment(
        &client,
        &base,
        &token,
        "fork-owner",
        "fork-deploy",
        "production",
        true,
    )
    .await;
    for (name, value, environment) in [
        ("REPO_KEY", "repo-value", None),
        ("PROD_KEY", "prod-value", Some("production")),
    ] {
        assert_eq!(
            put_secret(
                &client,
                &base,
                &token,
                "fork-owner",
                "fork-deploy",
                name,
                value,
                environment,
            )
            .await
            .status(),
            201
        );
    }

    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "fork-runner",
        "[]",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let production = rg_db::ops::ci_environment_ops::find_by_name(&db, repo_id, "production")
        .await
        .unwrap()
        .unwrap();

    // The pipeline is what a maintainer's fork-CI approval produces; the job's
    // workflow (from the fork) names production, so nothing runs until the
    // environment gate is satisfied on top of that approval.
    let fork_job = pending_job(
        &db,
        repo_id,
        "refs/pull/42/head",
        "pull_request",
        "deploy-from-fork",
    )
    .await;
    rg_db::ops::ci_environment_ops::attach_job(&db, fork_job, Some(&production), "production")
        .await
        .unwrap();
    assert!(
        poll(&client, &base, runner.id, &runner_token)
            .await
            .is_none(),
        "neither the repository-wide nor the environment scope may reach an ungated fork-PR job"
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(&db, fork_job)
            .await
            .unwrap()
            .unwrap()
            .status,
        "waiting_approval"
    );
}
