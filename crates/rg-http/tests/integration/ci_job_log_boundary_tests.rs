//! The declared ceiling on a job-log upload, from both ends.
//!
//! `POST /api/v1/runners/{id}/jobs/{job_id}/log` carries a whole build's output
//! in one plain-text body. It used to be mounted with no body limit at all,
//! which is not "no limit": a buffered `String` extractor inherits Axum's 2 MiB
//! `DefaultBodyLimit`, so a verbose build's log was refused and the job kept
//! the empty log it started with. Both halves of the fix are proven here —
//! the server accepting a body above that hidden default, and the runner
//! trimming rather than losing one above the declared ceiling.

use crate::common::{register_full, spawn_test_app_with_db};
use sea_orm::DatabaseConnection;

/// The ceiling `api::runners::JOB_LOG_MAX_BYTES` declares. Spelled out rather
/// than imported because these tests drive the server over HTTP, the same way
/// a runner does, and a client knows the number only from the contract.
const JOB_LOG_MAX_BYTES: usize = 8 * 1024 * 1024;

async fn create_private_repo(base: &str, token: &str, name: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create private repo failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// A job assigned to `runner_id`, which is what the log route's gate measures
/// the uploading runner against.
async fn create_assigned_job(db: &DatabaseConnection, repo_id: i64, runner_id: i64) -> i64 {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "build", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        db,
        stage.id,
        "verbose",
        "cargo build -v",
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    rg_db::ops::pipeline_ops::assign_job(db, job.id, runner_id)
        .await
        .unwrap();
    job.id
}

/// The route answers before the write lands: `LogWriteQueue` persists off the
/// request path, so the assertion has to wait for the row rather than read it
/// once and call an empty column a lost log.
async fn stored_log(db: &DatabaseConnection, job_id: i64) -> String {
    for _ in 0..200 {
        let job = rg_db::ops::pipeline_ops::get_job(db, job_id)
            .await
            .unwrap()
            .expect("the job the log was uploaded for");
        match job.log {
            Some(log) if !log.is_empty() => return log,
            _ => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
        }
    }
    panic!("the log write never reached job {job_id}");
}

/// A repository, a runner holding a token for it, and a job assigned to that
/// runner — the state every upload below starts from.
async fn seed(base: &str, db: &DatabaseConnection, who: &str) -> (i64, i64, String) {
    let (owner_token, _owner_id) = register_full(base, who, &format!("{who}@example.com")).await;
    let repo_id = create_private_repo(base, &owner_token, &format!("{who}-repo")).await;
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        db,
        repo_id,
        &format!("{who}-runner"),
        "",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let job_id = create_assigned_job(db, repo_id, runner.id).await;
    (runner.id, job_id, runner_token)
}

#[tokio::test]
async fn retrying_a_complete_log_replaces_it_once_and_200_means_persisted() {
    let (base, db) = spawn_test_app_with_db().await;
    let (runner_id, job_id, runner_token) = seed(&base, &db, "joblog_retry").await;
    let client = reqwest::Client::new();
    let log = "one complete build log";

    for _ in 0..2 {
        let response = client
            .post(format!(
                "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/log"
            ))
            .bearer_auth(&runner_token)
            .header("x-job-log-mode", "replace")
            .body(log)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let stored = rg_db::ops::pipeline_ops::get_job(&db, job_id)
            .await
            .unwrap()
            .unwrap()
            .log;
        assert_eq!(stored.as_deref(), Some(log));
    }
}

#[tokio::test]
async fn a_log_above_axums_hidden_default_reaches_the_job() {
    let (base, db) = spawn_test_app_with_db().await;
    let (runner_id, job_id, runner_token) = seed(&base, &db, "joblog_big").await;

    // Past the 2 MiB a buffered extractor inherits when the route declares
    // nothing, and well under the 8 MiB this one does declare.
    let log = "compiling something verbose\n".repeat(120_000);
    assert!(log.len() > 2 * 1024 * 1024 && log.len() < JOB_LOG_MAX_BYTES);

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/log"
        ))
        .bearer_auth(&runner_token)
        .body(log.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "a 3 MiB build log is an ordinary log, not an oversized upload"
    );

    assert_eq!(
        stored_log(&db, job_id).await,
        log,
        "the whole log has to survive the round trip, not a 2 MiB prefix of it"
    );
}

#[tokio::test]
async fn a_log_over_the_declared_ceiling_is_refused_rather_than_buffered() {
    let (base, db) = spawn_test_app_with_db().await;
    let (runner_id, job_id, runner_token) = seed(&base, &db, "joblog_over").await;

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/log"
        ))
        .bearer_auth(&runner_token)
        .body(vec![b'x'; JOB_LOG_MAX_BYTES + 1])
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        413,
        "the declared ceiling is the answer above it — not a 400, and not a silent accept"
    );
}

#[tokio::test]
async fn an_oversized_runner_log_arrives_trimmed_instead_of_being_lost() {
    let (base, db) = spawn_test_app_with_db().await;
    let (runner_id, job_id, runner_token) = seed(&base, &db, "joblog_trim").await;

    // What a runner actually holds after a long build: one string, larger than
    // any single upload may carry. Driving `rg_runner::api::upload_log` rather
    // than posting by hand is the point — the runner's trim and the server's
    // ceiling are two constants in two crates, and this is where they meet.
    let tail = "error: could not compile `rg-http`\n";
    let mut log = "warning: unused variable\n".repeat(400_000);
    assert!(log.len() > JOB_LOG_MAX_BYTES);
    log.push_str(tail);

    rg_runner::api::upload_log(
        &reqwest::Client::new(),
        &base,
        runner_id,
        job_id,
        &runner_token,
        &log,
    )
    .await;

    let stored = stored_log(&db, job_id).await;
    assert!(
        stored.len() <= JOB_LOG_MAX_BYTES,
        "an upload the server would refuse is not an upload: {}",
        stored.len()
    );
    assert!(
        stored.starts_with("[plombir-git-runner] log truncated:"),
        "a shortened log has to say so, or it reads as the whole build's output"
    );
    assert!(
        stored.ends_with(tail),
        "the tail carries the failure the person reading this log came for"
    );
}
