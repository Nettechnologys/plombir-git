//! Regression coverage for card_e202a48838f9: a cancellation that has already
//! answered must survive the worker that was mid-flight when it landed.
//!
//! `cancel_pipeline_chain` is transactional, so the chain is consistent at the
//! moment the caller reads `200 {"status":"canceled"}`. What it cannot do is
//! stop a runner that already holds the job: that runner comes back with a
//! result computed from a snapshot older than the cascade, and an
//! unconditional write walked job → stage → pipeline back out of `canceled`.
//! The caller's confirmation then contradicted what the UI showed, and the
//! success roll-up released the auto-merge and post-push hooks the
//! cancellation existed to prevent.
//!
//! What each test guards:
//!
//! * A late `/finish` neither moves the chain nor answers as though it had.
//! * The cascade reaches a job that was only *assigned* — the window between
//!   the poll and the runner's `/start` used not to be covered at all.
//! * The refusal is narrow: without a cancellation, a repeated `/finish` (the
//!   one report the runner retries) still lands and still rolls the pipeline
//!   up to `success`.

use axum::http::StatusCode;

use crate::common::{register_full, spawn_test_app_with_db};

struct Fixture {
    owner: String,
    repo: String,
    runner_id: i64,
    runner_token: String,
    pipeline_id: i64,
    stage_id: i64,
    job_id: i64,
    owner_token: String,
}

async fn seed(base: &str, db: &rg_db::DatabaseConnection, suffix: &str) -> Fixture {
    let client = reqwest::Client::new();
    let username = format!("pc-{suffix}");
    let email = format!("{username}@example.test");
    let (owner_token, _) = register_full(base, &username, &email).await;
    let repo_name = format!("pc-{suffix}");
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "name": repo_name, "is_private": true }))
        .send()
        .await
        .expect("create cancellation-test repository");
    let created_status = created.status();
    let created_body = created
        .json::<serde_json::Value>()
        .await
        .expect("repository response is JSON");
    assert_eq!(created_status, StatusCode::CREATED, "{created_body}");
    let repo_id = created_body["id"]
        .as_i64()
        .expect("repository response carries its id");

    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        db,
        &format!("runner-{suffix}"),
        r#"["linux"]"#,
        None,
        None,
        None,
    )
    .await
    .expect("register runner");
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "1111111111111111111111111111111111111111",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("create pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db,
        stage.id,
        "test",
        "echo ok",
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
    .expect("create job");

    Fixture {
        owner: username,
        repo: repo_name,
        runner_id: runner.id,
        runner_token,
        pipeline_id: pipeline.id,
        stage_id: stage.id,
        job_id: job.id,
        owner_token,
    }
}

/// Take the job the way a real runner does — the poll is what moves it to
/// `assigned` and binds it to this runner.
async fn poll(base: &str, fixture: &Fixture) -> StatusCode {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/poll?timeout=2",
            fixture.runner_id
        ))
        .bearer_auth(&fixture.runner_token)
        .send()
        .await
        .expect("runner poll")
        .status()
}

async fn start(base: &str, fixture: &Fixture) -> StatusCode {
    reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{}/start",
            fixture.runner_id, fixture.job_id
        ))
        .bearer_auth(&fixture.runner_token)
        .send()
        .await
        .expect("runner start")
        .status()
}

async fn finish(base: &str, fixture: &Fixture, status: &str, exit_code: i32) -> StatusCode {
    reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{}/finish",
            fixture.runner_id, fixture.job_id
        ))
        .bearer_auth(&fixture.runner_token)
        .json(&serde_json::json!({ "status": status, "exit_code": exit_code }))
        .send()
        .await
        .expect("runner finish")
        .status()
}

async fn cancel(base: &str, fixture: &Fixture) -> StatusCode {
    reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{}/{}/pipelines/{}/cancel",
            fixture.owner, fixture.repo, fixture.pipeline_id
        ))
        .bearer_auth(&fixture.owner_token)
        .send()
        .await
        .expect("cancel pipeline")
        .status()
}

async fn chain_statuses(
    db: &rg_db::DatabaseConnection,
    fixture: &Fixture,
) -> (String, String, String) {
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(db, fixture.pipeline_id)
        .await
        .expect("reload pipeline")
        .expect("pipeline still exists");
    let stage = rg_db::ops::pipeline_ops::get_stage_by_id(db, fixture.stage_id)
        .await
        .expect("reload stage")
        .expect("stage still exists");
    let job = rg_db::ops::pipeline_ops::get_job(db, fixture.job_id)
        .await
        .expect("reload job")
        .expect("job still exists");
    (pipeline.status, stage.status, job.status)
}

/// The card's scenario: the job is held between assignment and completion, the
/// pipeline is canceled while it is held, and only then is the runner released.
#[tokio::test]
async fn a_late_finish_cannot_walk_a_canceled_pipeline_back_to_success() {
    let (base, db) = spawn_test_app_with_db().await;
    let fixture = seed(&base, &db, "late-finish").await;

    assert_eq!(poll(&base, &fixture).await, StatusCode::OK);
    assert_eq!(start(&base, &fixture).await, StatusCode::OK);

    assert_eq!(cancel(&base, &fixture).await, StatusCode::OK);
    assert_eq!(
        chain_statuses(&db, &fixture).await,
        (
            "canceled".to_string(),
            "canceled".to_string(),
            "canceled".to_string()
        ),
        "the cancellation must settle the whole chain before the runner reports"
    );

    // The runner was executing when the cancellation landed; this is the report
    // it computed from the pre-cancellation snapshot.
    assert_eq!(
        finish(&base, &fixture, "success", 0).await,
        StatusCode::CONFLICT,
        "a late completion must not be confirmed as a state change that happened"
    );

    assert_eq!(
        chain_statuses(&db, &fixture).await,
        (
            "canceled".to_string(),
            "canceled".to_string(),
            "canceled".to_string()
        ),
        "the runner's late success must not resurrect the canceled chain"
    );
}

/// The window the cascade used to walk straight past: `assigned` was not in the
/// set of statuses cancellation considered active, so the job stayed assigned
/// under a canceled pipeline and the runner was told to go ahead and run it.
#[tokio::test]
async fn cancellation_reaches_a_job_that_was_only_assigned() {
    let (base, db) = spawn_test_app_with_db().await;
    let fixture = seed(&base, &db, "assigned-only").await;

    assert_eq!(poll(&base, &fixture).await, StatusCode::OK);
    let job = rg_db::ops::pipeline_ops::get_job(&db, fixture.job_id)
        .await
        .expect("reload polled job")
        .expect("polled job still exists");
    assert_eq!(job.status, "assigned");

    assert_eq!(cancel(&base, &fixture).await, StatusCode::OK);
    assert_eq!(
        chain_statuses(&db, &fixture).await,
        (
            "canceled".to_string(),
            "canceled".to_string(),
            "canceled".to_string()
        ),
        "an assigned job is work in flight and belongs to the cascade"
    );

    assert_eq!(
        start(&base, &fixture).await,
        StatusCode::CONFLICT,
        "a runner must not be told to start a job the server has canceled"
    );
    let job = rg_db::ops::pipeline_ops::get_job(&db, fixture.job_id)
        .await
        .expect("reload canceled job")
        .expect("canceled job still exists");
    assert_eq!(job.status, "canceled");
}

/// The guard refuses a *different* terminal status, not a repeat of the same
/// one — `finish` is the report the runner retries, and a retry has to keep
/// driving the roll-up rather than answering `409`.
#[tokio::test]
async fn a_repeated_finish_still_rolls_the_pipeline_up() {
    let (base, db) = spawn_test_app_with_db().await;
    let fixture = seed(&base, &db, "retry").await;

    assert_eq!(poll(&base, &fixture).await, StatusCode::OK);
    assert_eq!(start(&base, &fixture).await, StatusCode::OK);
    assert_eq!(finish(&base, &fixture, "success", 0).await, StatusCode::OK);
    assert_eq!(
        finish(&base, &fixture, "success", 0).await,
        StatusCode::OK,
        "the runner's retry of the same report must stay idempotent"
    );

    assert_eq!(
        chain_statuses(&db, &fixture).await,
        (
            "success".to_string(),
            "success".to_string(),
            "success".to_string()
        )
    );
}

/// card_39bf6a755499: the same endpoint, one step earlier. `status` was written
/// into the column exactly as the runner typed it, and the roll-up recognised a
/// fixed list of strings — so a runner that reported `succes` got `200 OK`, the
/// job settled, and the stage and pipeline stayed `running` for good. Nothing
/// logged anything, and the PR's required checks never unblocked.
#[tokio::test]
async fn an_unreadable_finish_status_is_refused_instead_of_hanging_the_pipeline() {
    let (base, db) = spawn_test_app_with_db().await;
    let fixture = seed(&base, &db, "bad-status").await;

    assert_eq!(poll(&base, &fixture).await, StatusCode::OK);
    assert_eq!(start(&base, &fixture).await, StatusCode::OK);

    for typo in ["succes", "SUCCESS", "done", ""] {
        assert_eq!(
            finish(&base, &fixture, typo, 0).await,
            StatusCode::BAD_REQUEST,
            "`status: {typo}` was accepted as a job outcome"
        );
    }
    assert_eq!(
        chain_statuses(&db, &fixture).await,
        (
            "pending".to_string(),
            "pending".to_string(),
            "running".to_string()
        ),
        "a refused report must leave the chain exactly where it was"
    );

    // The words the server decides for itself are not the runner's to report:
    // a runner claiming `canceled` would be answering a cancellation nobody
    // issued, and `skipped` a condition the server evaluates.
    for reserved in ["canceled", "skipped"] {
        assert_eq!(
            finish(&base, &fixture, reserved, 0).await,
            StatusCode::BAD_REQUEST,
            "`status: {reserved}` is the server's verdict, not a runner report"
        );
    }

    assert_eq!(finish(&base, &fixture, "success", 0).await, StatusCode::OK);
    assert_eq!(
        chain_statuses(&db, &fixture).await,
        (
            "success".to_string(),
            "success".to_string(),
            "success".to_string()
        ),
        "the guard must not cost the endpoint its actual job"
    );
}
