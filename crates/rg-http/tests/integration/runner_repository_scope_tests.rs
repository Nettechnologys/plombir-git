//! Repository isolation for runner bearer tokens (card_174154b4ee6c).
//!
//! A runner token used to identify only a machine. The scheduler searched every
//! pending job on the instance and the poll response then decrypted the chosen
//! repository's secrets. Matching labels were therefore enough for a runner
//! registered by one repository to receive another private repository's job and
//! credentials.
//!
//! This drives the public registration and poll routes against two repositories
//! with identical labels. The foreign job is deliberately older, so removing
//! the repository predicate makes the first poll fail on the wrong job and the
//! wrong secret instead of leaving a vacuous all-denied test.

use axum::http::StatusCode;

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn put_secret(base: &str, owner_token: &str, repo: &str, name: &str, value: &str) {
    let response = reqwest::Client::new()
        .put(format!(
            "{base}/api/v1/repos/scope-owner/{repo}/actions/secrets/{name}"
        ))
        .bearer_auth(owner_token)
        .json(&serde_json::json!({ "value": value }))
        .send()
        .await
        .expect("write repository secret");
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn pending_job(db: &rg_db::DatabaseConnection, repo_id: i64, suffix: &str) -> i64 {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        &format!("{suffix:0>40}"),
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("create pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create stage");
    rg_db::ops::pipeline_ops::create_job(
        db,
        stage.id,
        "same-labels",
        "echo scoped",
        None,
        None,
        None,
        None,
        Some(r#"["linux"]"#),
        None,
        false,
        None,
        None,
        None,
    )
    .await
    .expect("create pending job")
    .id
}

async fn register_runner(
    base: &str,
    admin_token: &str,
    repository: &str,
    name: &str,
) -> (i64, String) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/runners/register"))
        .bearer_auth(admin_token)
        .json(&serde_json::json!({
            "repository": repository,
            "name": name,
            "labels": ["linux"]
        }))
        .send()
        .await
        .expect("register repository runner");
    let status = response.status();
    let body = response
        .json::<serde_json::Value>()
        .await
        .expect("registration response is JSON");
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["repository"], repository);
    (
        body["id"].as_i64().expect("runner id"),
        body["token"].as_str().expect("runner token").to_owned(),
    )
}

async fn poll(base: &str, runner_id: i64, runner_token: &str, timeout: u8) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/poll?timeout={timeout}"
        ))
        .bearer_auth(runner_token)
        .send()
        .await
        .expect("poll runner job")
}

#[tokio::test]
async fn runner_can_only_receive_jobs_and_secrets_from_its_repository() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "scope-owner", "scope-owner@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, owner_id, None, None, Some(true), None)
        .await
        .expect("promote runner registrar")
        .expect("runner registrar exists");

    let repo_a = create_repo(&base, &owner_token, "runner-scope-a").await;
    let repo_b = create_repo(&base, &owner_token, "runner-scope-b").await;
    put_secret(
        &base,
        &owner_token,
        "runner-scope-a",
        "SHARED_SECRET",
        "only-repository-a",
    )
    .await;
    put_secret(
        &base,
        &owner_token,
        "runner-scope-b",
        "SHARED_SECRET",
        "only-repository-b",
    )
    .await;
    put_secret(
        &base,
        &owner_token,
        "runner-scope-b",
        "B_ONLY_SECRET",
        "must-not-cross",
    )
    .await;

    let (runner_a, token_a) = register_runner(
        &base,
        &owner_token,
        "scope-owner/runner-scope-a",
        "runner-a",
    )
    .await;
    let (runner_b, token_b) = register_runner(
        &base,
        &owner_token,
        "scope-owner/runner-scope-b",
        "runner-b",
    )
    .await;

    // The foreign job has the lower id. Without the repository predicate it is
    // the first matching-label job and leaks repository B's secrets to runner A.
    let job_b = pending_job(&db, repo_b, "b").await;
    let job_a = pending_job(&db, repo_a, "a").await;
    assert!(job_b < job_a, "foreign job must be considered first");

    let response_a = poll(&base, runner_a, &token_a, 1).await;
    let status_a = response_a.status();
    let body_a = response_a
        .json::<serde_json::Value>()
        .await
        .expect("runner A poll body");
    assert_eq!(status_a, StatusCode::OK, "{body_a}");
    assert_eq!(body_a["job_id"], job_a);
    assert_eq!(body_a["variables"]["SHARED_SECRET"], "only-repository-a");
    assert!(
        body_a["variables"].get("B_ONLY_SECRET").is_none(),
        "runner A received repository B's secret: {body_a}"
    );

    let no_foreign_job = poll(&base, runner_a, &token_a, 1).await;
    assert_eq!(
        no_foreign_job.status(),
        StatusCode::NO_CONTENT,
        "runner A must not receive repository B's still-pending job"
    );
    let persisted_b = rg_db::ops::pipeline_ops::get_job(&db, job_b)
        .await
        .expect("read repository B job")
        .expect("repository B job exists");
    assert_eq!(persisted_b.status, "pending");
    assert_eq!(persisted_b.runner_id, None);

    let response_b = poll(&base, runner_b, &token_b, 1).await;
    let status_b = response_b.status();
    let body_b = response_b
        .json::<serde_json::Value>()
        .await
        .expect("runner B poll body");
    assert_eq!(status_b, StatusCode::OK, "{body_b}");
    assert_eq!(body_b["job_id"], job_b);
    assert_eq!(body_b["variables"]["SHARED_SECRET"], "only-repository-b");
    assert_eq!(body_b["variables"]["B_ONLY_SECRET"], "must-not-cross");
}
