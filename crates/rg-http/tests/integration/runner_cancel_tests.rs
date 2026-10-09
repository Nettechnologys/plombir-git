//! A cancel has to reach the work, not only the row (card_a0377b61860e).
//!
//! Canceling a pipeline is a transaction on the server. The job it is meant to
//! stop runs on another machine, and until this was fixed the external runner
//! never asked about it again after `start`: a canceled deploy ran on to its
//! timeout — a day by default — then saved its cache and published its
//! artifact, and a `concurrency: cancel-in-progress` group had the old run and
//! its replacement executing side by side.
//!
//! The runner here is the real one ([`rg_runner::run_jobs_until_shutdown_checking_every`]):
//! it long-polls the live router, downloads the commit, runs the script, and
//! asks `GET /runners/{id}/jobs/{job_id}/status` on a period shortened to a
//! fifth of a second. The script sleeps a minute, so a runner that stops on the
//! cancel is done long before the bounds below and one that does not is caught
//! by them.

use sea_orm::{ConnectionTrait, EntityTrait, PaginatorTrait};

use crate::common::{register_full, spawn_test_app_with_db};

/// How long the test waits for a state the runner has to reach on its own.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(20);

/// Liveness period for the runner under test.
const FAST: std::time::Duration = std::time::Duration::from_millis(200);

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

/// Poll the runner row until it reads `status`. `busy` is written by `start`
/// and `online` by `finish` — the last report of a job — so the pair brackets
/// one job's whole life on the runner.
async fn wait_for_runner_status(db: &rg_db::DatabaseConnection, runner_id: i64, status: &str) {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        let current = rg_db::ops::runner_ops::find_by_id(db, runner_id)
            .await
            .unwrap()
            .expect("the runner row exists")
            .status;
        if current == status {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "runner {runner_id} never reached `{status}` — it is `{current}`"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn wait_for_file(path: &std::path::Path) {
    let deadline = tokio::time::Instant::now() + SETTLE;
    while !path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} never appeared — the job's script did not get that far",
            path.display()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn process_is_alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Kill whatever a failed run left behind, so a red test does not leak a
/// minute-long `sleep` into the next one.
struct KillOnDrop(std::path::PathBuf);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(&self.0) {
            drop(
                std::process::Command::new("kill")
                    .args(["-KILL", pid.trim()])
                    .status(),
            );
        }
    }
}

/// A repository with one real commit and a pipeline holding one pending job
/// that declares a cache and an artifact over `out/`, so a run that publishes
/// anything leaves a row behind in one of the two tables.
async fn repo_with_a_publishing_job(
    base: &str,
    db: &rg_db::DatabaseConnection,
    owner: &str,
    script: &str,
) -> (i64, i64, i64) {
    let (token, _owner_id) = register_full(base, owner, &format!("{owner}@example.com")).await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name":"deploys","auto_init":true,"readme":"default"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "create the repository");
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let log = client
        .get(format!("{base}/api/v1/repos/{owner}/deploys/log"))
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
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "deploy", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        db,
        stage.id,
        "deploy",
        script,
        None,
        None,
        None,
        Some("deploy-cache"),
        Some(r#"["out"]"#),
        Some(r#"{"name":"bundle","paths":["out"]}"#),
        false,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    // `rg-runner` lays a job's workspace out under the process temp directory
    // by job id alone, and every test database numbers its first job `1` — so
    // two of these tests running at once in sibling processes download into,
    // and clean up, the same directory. A job id no other process holds keeps
    // each run in its own workspace. The row was created a line ago and nothing
    // references it yet.
    let job_id = 1_000_000 + i64::from(std::process::id());
    db.execute_unprepared(&format!(
        "UPDATE pipeline_jobs SET id = {job_id} WHERE id = {}",
        job.id
    ))
    .await
    .expect("move the job to a process-unique id");
    (repo_id, pipeline.id, job_id)
}

async fn assert_nothing_published(db: &rg_db::DatabaseConnection, pipeline_id: i64) {
    assert_eq!(
        rg_db::entities::ci_cache_entry::Entity::find()
            .count(db)
            .await
            .unwrap(),
        0,
        "a canceled job saved its cache — the next run would restore a canceled build"
    );
    assert!(
        rg_db::ops::artifact_ops::list_by_pipeline(db, pipeline_id)
            .await
            .unwrap()
            .is_empty(),
        "a canceled job published its artifact under the name a real run uses"
    );
}

async fn spawn_runner(
    base: &str,
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    liveness: std::time::Duration,
) -> (
    rg_db::entities::runner::Model,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        db,
        repo_id,
        "deploy-runner",
        "[]",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let agent = tokio::spawn(rg_runner::run_jobs_until_shutdown_checking_every(
        reqwest::Client::new(),
        base.to_owned(),
        runner.id,
        runner_token,
        async {
            drop(stopped.await);
        },
        liveness,
    ));
    (runner, stop, agent)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_canceled_job_is_stopped_on_the_runner_and_publishes_nothing() {
    let (base, db) = spawn_test_app_with_db().await;
    let scratch = tempfile::tempdir().unwrap();
    let pid_file = scratch.path().join("deploy.pid");
    let _cleanup = KillOnDrop(pid_file.clone());
    // `exec`, so the recorded PID is the sleep itself — the process a runner
    // that ignores the cancel would keep alive for the whole minute.
    let script = format!(
        "PATH=/usr/bin:/bin; mkdir -p out && echo built > out/bundle.txt; \
         echo $$ > '{}'; exec sleep 60",
        pid_file.display()
    );
    let (repo_id, pipeline_id, job_id) =
        repo_with_a_publishing_job(&base, &db, "cancel-owner", &script).await;
    let (runner, stop, agent) = spawn_runner(&base, &db, repo_id, FAST).await;

    wait_for_job_status(&db, job_id, "running").await;
    wait_for_runner_status(&db, runner.id, "busy").await;
    wait_for_file(&pid_file).await;
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_owned();
    assert!(process_is_alive(&pid), "the job's sleep is not running");

    assert!(
        rg_db::ops::pipeline_ops::cancel_pipeline_chain(&db, pipeline_id)
            .await
            .unwrap(),
        "the pipeline was not active to cancel"
    );
    let deadline = tokio::time::Instant::now() + SETTLE;
    while process_is_alive(&pid) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the canceled job's process {pid} is still running {SETTLE:?} after the cancel — \
             the runner never heard about it"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // The runner reported back, which is what returns it to the pool: the
    // finish is refused for the settled job, but the same transaction moves the
    // runner off `busy`.
    wait_for_runner_status(&db, runner.id, "online").await;

    stop.send(()).expect("the runner is still listening");
    tokio::time::timeout(SETTLE, agent)
        .await
        .expect("the runner never returned from its stop path")
        .expect("the runner panicked");

    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(&db, job_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "canceled",
        "the runner's report overwrote the cancellation"
    );
    assert_nothing_published(&db, pipeline_id).await;
}

/// The script finishes `0` after the cancel landed and before any periodic
/// check asked — the period is an hour here — so only the check in front of
/// publication can see it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_canceled_while_it_ran_publishes_nothing_even_when_it_exits_zero() {
    let (base, db) = spawn_test_app_with_db().await;
    let scratch = tempfile::tempdir().unwrap();
    let started = scratch.path().join("started");
    let release = scratch.path().join("release");
    let script = format!(
        "PATH=/usr/bin:/bin; mkdir -p out && echo built > out/bundle.txt; touch '{}'; \
         while [ ! -e '{}' ]; do sleep 0.05; done",
        started.display(),
        release.display()
    );
    let (repo_id, pipeline_id, job_id) =
        repo_with_a_publishing_job(&base, &db, "late-cancel-owner", &script).await;
    let (runner, stop, agent) =
        spawn_runner(&base, &db, repo_id, std::time::Duration::from_secs(3600)).await;

    wait_for_job_status(&db, job_id, "running").await;
    wait_for_runner_status(&db, runner.id, "busy").await;
    wait_for_file(&started).await;
    assert!(
        rg_db::ops::pipeline_ops::cancel_pipeline_chain(&db, pipeline_id)
            .await
            .unwrap()
    );
    std::fs::write(&release, b"go").unwrap();

    // `finish` is the runner's last word on a job, so once it is back to
    // `online` every decision about the output has been taken. Stopping it any
    // earlier would drop the cycle mid-way and prove nothing.
    wait_for_runner_status(&db, runner.id, "online").await;
    stop.send(()).expect("the runner is still listening");
    tokio::time::timeout(SETTLE, agent)
        .await
        .expect("the runner never returned from its stop path")
        .expect("the runner panicked");

    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(&db, job_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "canceled"
    );
    // The server refuses both uploads on its own (see the next test), so the
    // tables alone cannot tell whether the runner asked first. Its log can: a
    // runner that went ahead collects two refusals into the job log and
    // uploads it; one that asked uploads nothing for a job it no longer owns.
    let log = rg_db::ops::pipeline_ops::get_job(&db, job_id)
        .await
        .unwrap()
        .unwrap()
        .log;
    assert!(
        log.is_none(),
        "the runner went on to publish a canceled job's output and reported the refusals: {log:?}"
    );
    assert_nothing_published(&db, pipeline_id).await;
}

/// The server half, independent of what the runner asks first: a runner older
/// than the status route — or one that skips the question — is still refused
/// when it tries to publish for a job that was canceled, and the status route
/// says what it says.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_server_refuses_to_publish_for_a_canceled_job_and_says_so() {
    let (base, db) = spawn_test_app_with_db().await;
    let (repo_id, pipeline_id, job_id) =
        repo_with_a_publishing_job(&base, &db, "gate-owner", "true").await;
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
    let client = reqwest::Client::new();
    let polled = client
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/poll?timeout=5",
            runner.id
        ))
        .bearer_auth(&runner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(polled.status(), 200, "the runner is assigned the job");
    let start = client
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{job_id}/start",
            runner.id
        ))
        .bearer_auth(&runner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 200);

    let status_url = format!("{base}/api/v1/runners/{}/jobs/{job_id}/status", runner.id);
    let live: serde_json::Value = client
        .get(&status_url)
        .bearer_auth(&runner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(live["active"], true, "{live}");
    assert_eq!(live["status"], "running", "{live}");

    assert!(
        rg_db::ops::pipeline_ops::cancel_pipeline_chain(&db, pipeline_id)
            .await
            .unwrap()
    );
    let settled: serde_json::Value = client
        .get(&status_url)
        .bearer_auth(&runner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(settled["active"], false, "{settled}");
    assert_eq!(settled["status"], "canceled", "{settled}");

    let cache = client
        .put(format!(
            "{base}/api/v1/runners/{}/jobs/{job_id}/cache",
            runner.id
        ))
        .bearer_auth(&runner_token)
        .header("x-cache-key", "deploy-cache")
        .header("content-type", "application/x-tar")
        .body(vec![1_u8; 512])
        .send()
        .await
        .unwrap();
    assert_eq!(cache.status(), 409, "a canceled job stored a cache entry");
    let staged = client
        .put(format!(
            "{base}/api/v1/runners/{}/jobs/{job_id}/artifacts/staging",
            runner.id
        ))
        .bearer_auth(&runner_token)
        .header("content-type", "application/x-tar")
        .body(vec![1_u8; 512])
        .send()
        .await
        .unwrap();
    assert_eq!(staged.status(), 409, "a canceled job staged an artifact");
    let published = client
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{job_id}/artifacts",
            runner.id
        ))
        .bearer_auth(&runner_token)
        .json(&serde_json::json!({"name": "bundle", "file_path": "/nonexistent.tar"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        published.status(),
        409,
        "a canceled job published an artifact"
    );
    assert_nothing_published(&db, pipeline_id).await;

    // Somebody else's job is answered exactly as an unknown one.
    let foreign = client
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/{}/status",
            runner.id,
            job_id + 1000
        ))
        .bearer_auth(&runner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 404);
}
