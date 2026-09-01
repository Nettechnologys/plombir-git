//! A runner that is told to stop must say so (card_3027b2187d42).
//!
//! `POST /runners/{id}/deregister` hands a runner's in-flight jobs back to the
//! pool and drops it from the runner list, as one transaction. It is mounted
//! behind `RUNNER_TOKEN`, so a runner is the *only* caller it can ever have —
//! and `rg-runner` never called it. The agent had no stop path at all: no
//! `SIGTERM` handler, no `SIGINT` handler, just a `poll → execute → finish` loop
//! that gets killed. Every planned restart — a deploy, `docker compose down`,
//! a container image bump — therefore looked exactly like a crash: the job the
//! runner was holding sat in `running` until the ten-minute stuck-job sweep
//! noticed, and the runner stayed in the pool until its heartbeat expired,
//! although the transaction that fixes both instantly was already written and
//! tested.
//!
//! ## Why the signal is a parameter
//!
//! The stop path is driven through [`rg_runner::run_jobs_until_shutdown`], which
//! takes the stop signal as a future instead of installing a handler of its own.
//! `cmd_run` supplies the real one (`SIGTERM`/`SIGINT`); a test cannot, because
//! raising either would take the whole test binary down with the runner it is
//! trying to observe. What the signal *is* is not this test's subject anyway —
//! what has to be proven is what the runner does once it arrives.
//!
//! ## Why the job is real
//!
//! The runner is not fed a stub: it long-polls the live router, is assigned the
//! job, and downloads the workspace of the actual commit. It is stopped while
//! that job is `running` and its own script has not returned. Anything less
//! would prove the deregistration call happens, not that the job it was holding
//! comes back — and "comes back *without the watchdog*" is the half an operator
//! feels.

use crate::common::{register_full, spawn_test_app_with_db};

/// A script that outlives the test: the runner has to be interrupted while it
/// is holding the job, not caught in the gap between two of them.
///
/// `PATH` is set by the script itself because `run_job_local` clears the
/// environment — the shell would fall back to its built-in default, and a script
/// whose first assertion depends on that is one platform away from failing for a
/// reason that has nothing to do with this test.
const BLOCKING_SCRIPT: &str = "PATH=/usr/bin:/bin; exec sleep 120";

/// How long the test waits for a state the runner has to reach on its own.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(20);

/// Poll the database until `job` reaches `status`, or give up and say what it
/// was doing instead — a bare timeout here reads as flake and is not.
async fn wait_for_job_status(db: &rg_db::DatabaseConnection, job_id: i64, status: &str) {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        let job = rg_db::ops::pipeline_ops::get_job(db, job_id)
            .await
            .expect("read the job row")
            .expect("the job row exists");
        if job.status == status {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job {job_id} never reached `{status}` — it is `{}` and held by {:?}",
            job.status,
            job.runner_id
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A repository with one real commit, and a pipeline holding one pending job
/// that will not finish on its own.
async fn repo_with_a_blocking_job(
    base: &str,
    db: &rg_db::DatabaseConnection,
    owner: &str,
) -> (i64, i64) {
    let (token, _owner_id) = register_full(base, owner, &format!("{owner}@example.com")).await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name":"stopping","auto_init":true,"readme":"default"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "create the repository");
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    // The job is built against the commit `auto_init` wrote, because the runner
    // downloads that exact snapshot before it runs anything: a job pinned to a
    // sha this repository does not have would fail on the workspace download and
    // never be held at all.
    let log = client
        .get(format!("{base}/api/v1/repos/{owner}/stopping/log"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(log.status(), 200, "read the repository log");
    let sha = log.json::<serde_json::Value>().await.unwrap()["commits"][0]["sha"]
        .as_str()
        .unwrap()
        .to_owned();

    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        &sha,
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
        "blocking",
        BLOCKING_SCRIPT,
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
    (repo_id, job.id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopping_runner_deregisters_and_hands_its_job_straight_back() {
    let (base, db) = spawn_test_app_with_db().await;
    let (repo_id, job_id) = repo_with_a_blocking_job(&base, &db, "stopping-owner").await;
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "stopping-runner",
        "[]",
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let agent = tokio::spawn(rg_runner::run_jobs_until_shutdown(
        reqwest::Client::new(),
        base.clone(),
        runner.id,
        runner_token,
        async {
            // A dropped sender would mean the same thing; the test always sends.
            drop(stopped.await);
        },
    ));

    // The runner has to be *holding* the job when it is stopped, otherwise this
    // proves nothing about what happens to work in flight.
    wait_for_job_status(&db, job_id, "running").await;
    let held = rg_db::ops::pipeline_ops::get_job(&db, job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        held.runner_id,
        Some(runner.id),
        "the job is running but not on this runner — the fixture, not the stop path, is what broke"
    );

    stop.send(()).expect("the runner is still listening");
    tokio::time::timeout(SETTLE, agent)
        .await
        .expect("the runner never returned from its stop path")
        .expect("the runner's stop path panicked");

    // (a) The deregistration landed: nothing but that route deletes a runner row.
    assert!(
        rg_db::ops::runner_ops::find_by_id(&db, runner.id)
            .await
            .expect("read the runner row")
            .is_none(),
        "the runner is still in the pool — it stopped without deregistering, so the server will \
         keep offering it work until its heartbeat expires"
    );

    // (b) …and it took the job with it, back to the pool. Nothing else could
    // have done this: the stuck-job sweep is not running in this test, and the
    // script was still executing when the runner was interrupted.
    let returned = rg_db::ops::pipeline_ops::get_job(&db, job_id)
        .await
        .expect("read the job row")
        .expect("the job row exists");
    assert_eq!(
        returned.status, "pending",
        "the job the runner was holding is `{}`, so it waits out the ten-minute stuck-job sweep \
         instead of being picked up by the next runner",
        returned.status
    );
    assert_eq!(
        returned.runner_id, None,
        "the job is pending but still points at a runner that no longer exists"
    );
}
