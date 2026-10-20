//! card_c8b3fa9f0ebc: the step model on the runner API.
//!
//! A job translated from an Actions workflow runs its steps one by one, and an
//! external runner reports each of them. A runner built before the step model
//! would read such a job through the fields it knows — the flat `script` — and
//! run it as one program with every step's policy gone, so it is never handed
//! one: it is told why, and the job waits for a runner that can run it.

use axum::http::StatusCode;

use crate::common::{register_full, spawn_test_app_with_db};

struct Seeded {
    owner: String,
    owner_token: String,
    runner_id: i64,
    runner_token: String,
    pipeline_id: i64,
    stage_id: i64,
}

async fn seed(base: &str, db: &rg_db::DatabaseConnection, suffix: &str) -> Seeded {
    let client = reqwest::Client::new();
    let owner = format!("steps-{suffix}");
    let (owner_token, _) = register_full(base, &owner, &format!("{owner}@example.test")).await;
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "name": owner, "is_private": true }))
        .send()
        .await
        .expect("create repository");
    assert_eq!(created.status(), StatusCode::CREATED);
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        db,
        repo_id,
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
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .unwrap();
    Seeded {
        owner,
        owner_token,
        runner_id: runner.id,
        runner_token,
        pipeline_id: pipeline.id,
        stage_id: stage.id,
    }
}

/// A job; one of steps when `steps` names them.
async fn job(db: &rg_db::DatabaseConnection, stage_id: i64, name: &str, steps: &[&str]) -> i64 {
    let job = rg_db::ops::pipeline_ops::create_job(
        db,
        stage_id,
        name,
        "echo flat",
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
    if !steps.is_empty() {
        let specs = steps
            .iter()
            .map(|name| serde_json::json!({ "name": name, "run": format!("echo {name}") }))
            .collect::<Vec<_>>();
        rg_db::ops::pipeline_step_ops::create_job_steps(
            db,
            job.id,
            &serde_json::to_string(&specs).unwrap(),
            &steps
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
    }
    job.id
}

async fn poll(base: &str, seeded: &Seeded, protocol: Option<&str>) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/poll?timeout=1",
            seeded.runner_id
        ))
        .bearer_auth(&seeded.runner_token);
    if let Some(protocol) = protocol {
        request = request.header(rg_core::ci::RUNNER_PROTOCOL_HEADER, protocol);
    }
    request.send().await.expect("poll")
}

#[tokio::test]
async fn a_runner_that_does_not_run_steps_is_refused_a_job_of_steps_by_name() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed(&base, &db, "refused").await;
    let job_id = job(&db, seeded.stage_id, "build", &["Compile", "Test"]).await;

    for protocol in [None, Some("1"), Some("not-a-number")] {
        let response = poll(&base, &seeded, protocol).await;
        assert_eq!(
            response.status(),
            StatusCode::UPGRADE_REQUIRED,
            "{protocol:?}"
        );
        let body = response.json::<serde_json::Value>().await.unwrap();
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.contains(&format!("job #{job_id}"))
                && error.contains("Upgrade plombir-git-runner"),
            "the refusal names the job and the remedy: {body}"
        );
        let stored = rg_db::ops::pipeline_ops::get_job(&db, job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (stored.status.as_str(), stored.runner_id),
            ("pending", None)
        );
    }

    let response = poll(&base, &seeded, Some("2")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["job_id"].as_i64(), Some(job_id));
    assert_eq!(
        body["steps"].as_array().map(|steps| steps
            .iter()
            .map(|step| step["name"].clone())
            .collect::<Vec<_>>()),
        Some(vec![
            serde_json::json!("Compile"),
            serde_json::json!("Test")
        ])
    );
}

/// The refusal is about jobs of steps only: a runner too old for them keeps
/// taking every native job it can run.
#[tokio::test]
async fn an_old_runner_still_takes_the_native_jobs() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed(&base, &db, "native").await;
    job(&db, seeded.stage_id, "stepped", &["One"]).await;
    let native = job(&db, seeded.stage_id, "native", &[]).await;

    let response = poll(&base, &seeded, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["job_id"].as_i64(), Some(native));
    assert!(body["steps"].is_null(), "{body}");
}

#[tokio::test]
async fn step_reports_land_while_the_job_runs_masked_and_show_on_the_job_page() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed(&base, &db, "reports").await;
    let job_id = job(
        &db,
        seeded.stage_id,
        "build",
        &["Compile", "Test", "Deploy"],
    )
    .await;
    let polled = poll(&base, &seeded, Some("2"))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let job_token = polled["variables"]["CI_JOB_TOKEN"]
        .as_str()
        .expect("the job carries a token")
        .to_string();
    let client = reqwest::Client::new();
    let runner = |path: &str| {
        format!(
            "{base}/api/v1/runners/{}/jobs/{job_id}/{path}",
            seeded.runner_id
        )
    };
    let report = |number: usize, body: serde_json::Value| {
        client
            .post(runner(&format!("steps/{number}")))
            .bearer_auth(&seeded.runner_token)
            .json(&body)
            .send()
    };

    // Before `start` the job is assigned, not running: nothing is recorded.
    let early = report(1, serde_json::json!({ "status": "running" }))
        .await
        .unwrap();
    assert_eq!(early.status(), StatusCode::CONFLICT);

    let started = client
        .post(runner("start"))
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::OK);

    for (number, body, expected) in [
        (
            1,
            serde_json::json!({ "status": "running" }),
            StatusCode::OK,
        ),
        (
            1,
            serde_json::json!({
                "status": "success",
                "conclusion": "success",
                "exit_code": 0,
                "log": format!("compiled with {job_token}\n"),
            }),
            StatusCode::OK,
        ),
        (
            2,
            serde_json::json!({ "status": "failure", "conclusion": "success", "exit_code": 1, "log": "flaky\n" }),
            StatusCode::OK,
        ),
        (
            3,
            serde_json::json!({ "status": "running" }),
            StatusCode::OK,
        ),
        (
            2,
            serde_json::json!({ "status": "done" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            2,
            serde_json::json!({ "status": "failure" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            9,
            serde_json::json!({ "status": "running" }),
            StatusCode::CONFLICT,
        ),
    ] {
        let response = report(number, body.clone()).await.unwrap();
        assert_eq!(response.status(), expected, "step {number} {body}");
    }

    let compile = rg_db::ops::pipeline_step_ops::find_job_step(&db, job_id, 1)
        .await
        .unwrap()
        .unwrap();
    let log = compile.log.unwrap_or_default();
    assert!(log.contains("compiled with"), "{log}");
    assert!(
        !log.contains(&job_token),
        "a step log is masked like the job log: {log}"
    );

    // The job is canceled while step 3 runs: the job page does not keep
    // calling it running.
    rg_db::ops::pipeline_ops::cancel_pipeline_chain(&db, seeded.pipeline_id)
        .await
        .unwrap();
    let late = report(
        3,
        serde_json::json!({ "status": "success", "conclusion": "success" }),
    )
    .await
    .unwrap();
    assert_eq!(
        late.status(),
        StatusCode::CONFLICT,
        "a canceled job takes no reports"
    );

    let detail = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{owner}/pipelines/{}",
            seeded.pipeline_id,
            owner = seeded.owner
        ))
        .bearer_auth(&seeded.owner_token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let steps = &detail["stages"][0]["jobs"][0]["steps"];
    let summary = steps
        .as_array()
        .expect("the job lists its steps")
        .iter()
        .map(|step| {
            (
                step["number"].as_i64().unwrap(),
                step["status"].as_str().unwrap().to_string(),
                step["conclusion"].as_str().map(str::to_string),
                step["has_log"].as_bool().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            (1, "success".into(), Some("success".into()), true),
            (2, "failure".into(), Some("success".into()), true),
            (3, "cancelled".into(), None, false),
        ],
        "{detail}"
    );

    let step_log = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{owner}/pipelines/{}/jobs/{job_id}/steps/1/log",
            seeded.pipeline_id,
            owner = seeded.owner
        ))
        .bearer_auth(&seeded.owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(step_log.status(), StatusCode::OK);
    let step_log = step_log.json::<serde_json::Value>().await.unwrap();
    assert!(step_log["content"]
        .as_str()
        .unwrap()
        .contains("compiled with"));
    let missing = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{owner}/pipelines/{}/jobs/{job_id}/steps/9/log",
            seeded.pipeline_id,
            owner = seeded.owner
        ))
        .bearer_auth(&seeded.owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

/// The whole loop with the real runner: it polls with the steps protocol,
/// runs each step in its own process, carries `$GITHUB_ENV` across, lets a
/// step fail under `continue-on-error`, and reports every step — so the job
/// page shows the same steps an embedded run would.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_external_runner_runs_and_reports_each_step() {
    use sea_orm::ConnectionTrait;

    let (base, db) = spawn_test_app_with_db().await;
    let owner = "steps-runner";
    let (token, _) = register_full(&base, owner, &format!("{owner}@example.com")).await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name":"steps","auto_init":true,"readme":"default"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let sha = client
        .get(format!("{base}/api/v1/repos/{owner}/steps/log"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["commits"][0]["sha"]
        .as_str()
        .unwrap()
        .to_owned();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        &sha,
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "build", 0)
        .await
        .unwrap();
    let created_job = rg_db::ops::pipeline_ops::create_job(
        &db,
        stage.id,
        "build",
        "echo flat",
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
    // `rg-runner` lays a job's workspace out by job id, and every test
    // database numbers its first job `1`: a process-unique id keeps a sibling
    // test process out of this one's workspace (see `runner_cancel_tests`).
    let job_id = 2_000_000 + i64::from(std::process::id());
    db.execute_unprepared(&format!(
        "UPDATE pipeline_jobs SET id = {job_id} WHERE id = {}",
        created_job.id
    ))
    .await
    .unwrap();
    let steps = serde_json::json!([
        { "name": "Set", "run": "echo SHARED=carried >> \"$GITHUB_ENV\"\nexport LEAKED=yes" },
        { "name": "Flaky", "run": "echo flaky; exit 7", "continue_on_error": true },
        { "name": "Read", "run": "echo \"shared=$SHARED leaked=${LEAKED:-no}\"" },
    ]);
    rg_db::ops::pipeline_step_ops::create_job_steps(
        &db,
        job_id,
        &steps.to_string(),
        &["Set".into(), "Flaky".into(), "Read".into()],
    )
    .await
    .unwrap();

    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "steps-runner",
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
        base.clone(),
        runner.id,
        runner_token,
        rg_runner::JobLimits::default(),
        async {
            drop(stopped.await);
        },
        std::time::Duration::from_millis(200),
    ));

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let job = loop {
        let job = rg_db::ops::pipeline_ops::get_job(&db, job_id)
            .await
            .unwrap()
            .unwrap();
        if !rg_db::ops::pipeline_ops::is_active_pipeline_work(&job.status) {
            break job;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the job never ended: {}",
            job.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    stop.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), agent)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(job.status, "success", "{:?}", job.log);
    let mut rows = Vec::new();
    for number in 1..=3 {
        let step = rg_db::ops::pipeline_step_ops::find_job_step(&db, job_id, number)
            .await
            .unwrap()
            .unwrap();
        rows.push((step.status, step.conclusion, step.exit_code));
        if number == 3 {
            let log = step.log.unwrap_or_default();
            assert!(log.contains("shared=carried leaked=no"), "{log}");
        }
    }
    assert_eq!(
        rows,
        [
            ("success".to_string(), Some("success".to_string()), Some(0)),
            ("failure".to_string(), Some("success".to_string()), Some(7)),
            ("success".to_string(), Some("success".to_string()), Some(0)),
        ]
    );
    let log = job.log.unwrap_or_default();
    assert!(
        log.contains("▶ step 2/3: Flaky — failed, continued on error"),
        "{log}"
    );
}
