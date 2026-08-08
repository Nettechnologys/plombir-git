//! What a `CI_JOB_TOKEN` opens, and for how long.
//!
//! The token's signature is fixed when it is minted and stays in date for the
//! whole hour of its TTL, so every test here seeds a *real* pipeline, stage and
//! job: a token naming rows that were never written is not a token the gate
//! should honour, and a fixture that skipped them would have been asserting
//! against a gate that never looked.

use crate::common::{register_full, spawn_test_app_with_db};

/// A running job on `repo_id`, and a token minted for it.
///
/// Jobs are born `pending`; the gate accepts `assigned` and `running`, which is
/// the window a job actually talks to the API in.
async fn running_job_token(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    scopes: &str,
) -> (String, i64) {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "0000000000000000000000000000000000000000",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("fixture: pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "build", 0)
        .await
        .expect("fixture: stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "build", "true", None, None, None, None, None, false, None, None, None,
    )
    .await
    .expect("fixture: job");
    rg_db::ops::pipeline_ops::update_job_result(db, job.id, "running", None, None, None, None)
        .await
        .expect("fixture: mark the job running");

    let token = rg_core::auth::ci_token::generate_ci_job_token_with_ttl(
        repo_id,
        pipeline.id,
        job.id,
        scopes,
        "test-secret-key",
        3600,
    )
    .expect("generate ci job token");
    (token, job.id)
}

async fn create_private_repo(base: &str, token: &str, name: &str) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "is_private": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create private repo failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn publish_generic_package(base: &str, token: &str, owner: &str, repo: &str) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/packages/generic/publish?name=sample&version=1.0.0",
            base, owner, repo
        ))
        .bearer_auth(token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"sample.bin\"",
        )
        .body("package-bytes")
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap();
    assert_eq!(status, 201, "publish package failed: {body}");
}

#[tokio::test]
async fn ci_job_token_can_read_scoped_private_packages() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "ci_pkg_owner", "ci_pkg_owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "ci-private-packages").await;
    let other_repo_id = create_private_repo(&base, &owner_token, "ci-other-packages").await;
    publish_generic_package(&base, &owner_token, "ci_pkg_owner", "ci-private-packages").await;

    let (matching_token, _) = running_job_token(&db, repo_id, "packages:read").await;
    let matching_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_pkg_owner/ci-private-packages/packages/generic/list",
            base
        ))
        .bearer_auth(&matching_token)
        .send()
        .await
        .unwrap();
    assert_eq!(matching_resp.status(), 200);
    let body: serde_json::Value = matching_resp.json().await.unwrap();
    assert_eq!(body["packages"].as_array().unwrap().len(), 1);

    let (wrong_repo_token, _) = running_job_token(&db, other_repo_id, "packages:read").await;
    let wrong_repo_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_pkg_owner/ci-private-packages/packages/generic/list",
            base
        ))
        .bearer_auth(&wrong_repo_token)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_repo_resp.status(), 401);
}

/// A token outlives the job it was minted for — the gate must not.
///
/// The signature carries an hour of TTL and keeps verifying for all of it, so
/// until the gate re-read the row, cancelling a pipeline did nothing to the key
/// it had already handed out: the same token went on opening the private
/// repository, including a token that had leaked into the job's own log. The
/// baseline sits in the middle of the run on purpose — the same request, the
/// same token, answered before and after the cancellation.
#[tokio::test]
async fn a_cancelled_job_stops_opening_the_repository_with_its_token() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "ci_cancel_owner", "ci_cancel_owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "ci-cancelled").await;
    let (job_token, job_id) = running_job_token(&db, repo_id, "repo:read").await;

    let url = format!("{base}/api/v1/repos/ci_cancel_owner/ci-cancelled/tree");
    let while_running = client
        .get(&url)
        .bearer_auth(&job_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        while_running.status(),
        200,
        "baseline: a running job reads the repository it was minted for"
    );

    rg_db::ops::pipeline_ops::update_job_result(&db, job_id, "canceled", None, None, None, None)
        .await
        .expect("cancel the job");

    let after_cancel = client
        .get(&url)
        .bearer_auth(&job_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        after_cancel.status(),
        401,
        "a cancelled job kept its key to the repository until the token expired"
    );
}

/// Deleting the pipeline takes the key with it, the same way cancelling does —
/// the binding re-reads stage and pipeline, not only the job row.
#[tokio::test]
async fn a_token_naming_rows_that_were_never_written_opens_nothing() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "ci_ghost_owner", "ci_ghost_owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "ci-ghost").await;

    // Well-formed, correctly signed, in date, right repository, right scope —
    // and naming a job that does not exist. Signature alone used to be enough.
    let ghost = rg_core::auth::ci_token::generate_ci_job_token_with_ttl(
        repo_id,
        999_999,
        999_999,
        "repo:read",
        "test-secret-key",
        3600,
    )
    .expect("generate ci job token");

    let resp = client
        .get(format!("{base}/api/v1/repos/ci_ghost_owner/ci-ghost/tree"))
        .bearer_auth(&ghost)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "a signature naming no persisted job opened a private repository"
    );
}

#[tokio::test]
async fn ci_job_token_can_read_scoped_private_repo_content() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "ci_repo_owner", "ci_repo_owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "ci-private-content").await;
    let other_repo_id = create_private_repo(&base, &owner_token, "ci-other-content").await;

    let (matching_token, _) = running_job_token(&db, repo_id, "repo:read").await;
    let matching_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_repo_owner/ci-private-content/tree",
            base
        ))
        .bearer_auth(&matching_token)
        .send()
        .await
        .unwrap();
    assert_eq!(matching_resp.status(), 200);

    let (wrong_repo_token, _) = running_job_token(&db, other_repo_id, "repo:read").await;
    let wrong_repo_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_repo_owner/ci-private-content/tree",
            base
        ))
        .bearer_auth(&wrong_repo_token)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_repo_resp.status(), 401);
}
