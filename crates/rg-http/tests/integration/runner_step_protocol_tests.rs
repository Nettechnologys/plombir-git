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

/// card_5013aa8fca9e: a job of steps hands its resolved `outputs:` over with
/// its finish. The server masks them like a log before any job that needs
/// this one can read them, refuses more than a job may leave by size rather
/// than storing them cut, and takes a full MiB even when JSON escaping makes
/// the body six times that.
#[tokio::test]
async fn a_finished_job_leaves_its_outputs_masked_and_bounded() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed(&base, &db, "outputs").await;
    let job_id = job(&db, seeded.stage_id, "build", &["Version"]).await;
    rg_db::ops::pipeline_step_ops::set_job_context(
        &db,
        job_id,
        r#"{"needs":{},"outputs":{"version":"${{ steps.v.outputs.version }}"}}"#,
    )
    .await
    .unwrap();
    let polled = poll(&base, &seeded, Some("2"))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(
        polled["outputs"],
        serde_json::json!({ "version": "${{ steps.v.outputs.version }}" }),
        "the runner is told what to resolve: {polled}"
    );
    assert_eq!(polled["needs"], serde_json::json!({}));
    let job_token = polled["variables"]["CI_JOB_TOKEN"]
        .as_str()
        .unwrap()
        .to_string();
    let client = reqwest::Client::new();
    let runner = |path: &str| {
        format!(
            "{base}/api/v1/runners/{}/jobs/{job_id}/{path}",
            seeded.runner_id
        )
    };
    let started = client
        .post(runner("start"))
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::OK);

    let finish = |outputs: serde_json::Value| {
        client
            .post(runner("finish"))
            .bearer_auth(&seeded.runner_token)
            .json(&serde_json::json!({ "status": "success", "exit_code": 0, "outputs": outputs }))
            .send()
    };
    let limit = rg_core::ci::MAX_JOB_OUTPUT_BYTES;
    let over = finish(serde_json::json!({ "big": "a".repeat(limit) }))
        .await
        .unwrap();
    assert_eq!(over.status(), StatusCode::BAD_REQUEST);
    assert!(over.text().await.unwrap().contains("more than the 1048576"));
    let still = rg_db::ops::pipeline_ops::get_job(&db, job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.status, "running", "a refused finish settles nothing");
    assert_eq!(still.outputs, None);

    // A control character travels as `\u0001`, six bytes of JSON for one of
    // output: the body is past Axum's 2 MiB default, the output within 1 MiB.
    let controls = "\u{1}".repeat(limit - 4096);
    let done = finish(serde_json::json!({
        "version": "1.2.3",
        "token": format!("t={job_token}"),
        "controls": controls,
    }))
    .await
    .unwrap();
    assert_eq!(
        done.status(),
        StatusCode::OK,
        "{}",
        done.text().await.unwrap_or_default()
    );
    let stored = rg_db::ops::pipeline_ops::get_job(&db, job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, "success");
    let outputs: std::collections::BTreeMap<String, String> =
        serde_json::from_str(stored.outputs.as_deref().expect("outputs stored")).unwrap();
    assert_eq!(outputs["version"], "1.2.3");
    assert_eq!(outputs["controls"], controls);
    assert!(outputs["token"].starts_with("t="));
    assert!(
        !outputs["token"].contains(&job_token),
        "an output is masked like a log: {}",
        outputs["token"]
    );
}

/// The card's acceptance through the real external runner: job A writes
/// `version=1.2.3` to `$GITHUB_OUTPUT`, and job B, a stage later and needing
/// A, reads `${{ needs.A.outputs.version }}` in its step.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_external_runner_hands_a_jobs_outputs_to_the_job_that_needs_it() {
    use sea_orm::ConnectionTrait;

    let (base, db) = spawn_test_app_with_db().await;
    let owner = "outputs-runner";
    let (token, _) = register_full(&base, owner, &format!("{owner}@example.com")).await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name":"outputs","auto_init":true,"readme":"default"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let sha = client
        .get(format!("{base}/api/v1/repos/{owner}/outputs/log"))
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
    // Process-unique ids keep a sibling test process out of these jobs'
    // workspaces (see `the_external_runner_runs_and_reports_each_step`).
    let base_id = 3_000_000 + 2 * i64::from(std::process::id());
    let mut ids = Vec::new();
    for (position, (name, steps, context)) in [
        (
            "A",
            serde_json::json!([{ "name": "Version", "id": "v", "run": "echo version=1.2.3 >> \"$GITHUB_OUTPUT\"" }]),
            serde_json::json!({ "outputs": { "version": "${{ steps.v.outputs.version }}" } }),
        ),
        (
            "B",
            serde_json::json!([{ "name": "Use", "run": "echo \"got=${{ needs.A.outputs.version }} result=${{ needs.A.result }}\"" }]),
            serde_json::json!({ "needs": { "A": "A" } }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, name, position as i32)
            .await
            .unwrap();
        let created = rg_db::ops::pipeline_ops::create_job(
            &db, stage.id, name, "echo flat", None, None, None, None, None, None, false, None,
            None, None,
        )
        .await
        .unwrap();
        let job_id = base_id + position as i64;
        db.execute_unprepared(&format!(
            "UPDATE pipeline_jobs SET id = {job_id} WHERE id = {}",
            created.id
        ))
        .await
        .unwrap();
        rg_db::ops::pipeline_step_ops::create_job_steps(
            &db,
            job_id,
            &steps.to_string(),
            &[steps[0]["name"].as_str().unwrap().to_string()],
        )
        .await
        .unwrap();
        rg_db::ops::pipeline_step_ops::set_job_context(&db, job_id, &context.to_string())
            .await
            .unwrap();
        ids.push(job_id);
    }

    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "outputs-runner",
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

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
    let b = loop {
        let b = rg_db::ops::pipeline_ops::get_job(&db, ids[1])
            .await
            .unwrap()
            .unwrap();
        if !rg_db::ops::pipeline_ops::is_active_pipeline_work(&b.status) {
            break b;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job B never ended: {}",
            b.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    stop.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), agent)
        .await
        .unwrap()
        .unwrap();

    let a = rg_db::ops::pipeline_ops::get_job(&db, ids[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a.status, "success", "{:?}", a.log);
    assert_eq!(a.outputs.as_deref(), Some(r#"{"version":"1.2.3"}"#));
    assert_eq!(b.status, "success", "{:?}", b.log);
    let used = rg_db::ops::pipeline_step_ops::find_job_step(&db, ids[1], 1)
        .await
        .unwrap()
        .unwrap();
    let log = used.log.unwrap_or_default();
    assert!(log.contains("got=1.2.3 result=success"), "{log}");
}

/// card_5e09bc40b020's acceptance on the external runner: a variant's
/// `working-directory: ${{ matrix.dir }}`, stored as `${MATRIX_DIR}` beside
/// the variant's `MATRIX_DIR`, runs the step in that variant's directory, and
/// `${{ inputs.dir }}` that became `../..` is refused by name — the runner
/// hands no shell this field, so it is the one that reads the value.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_external_runner_reads_a_working_directory_from_the_jobs_variables() {
    use sea_orm::ConnectionTrait;

    let (base, db) = spawn_test_app_with_db().await;
    let owner = "directory-runner";
    let (token, _) = register_full(&base, owner, &format!("{owner}@example.com")).await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name":"directory","auto_init":true,"readme":"default"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let sha = client
        .get(format!("{base}/api/v1/repos/{owner}/directory/log"))
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
    // Process-unique ids keep a sibling test process out of these jobs'
    // workspaces (see `the_external_runner_runs_and_reports_each_step`).
    let base_id = 30_000_000 + 3 * i64::from(std::process::id());
    let in_variant = serde_json::json!([
        { "name": "Make", "run": "mkdir -p a b" },
        {
            "name": "Where",
            "run": "echo \"in=[$(basename \"$PWD\")]\"",
            "working_directory": "${MATRIX_DIR}",
        },
    ]);
    let climbing = serde_json::json!([
        { "name": "Climb", "run": "pwd", "working_directory": "${INPUT_DIR}" },
    ]);
    let mut ids = Vec::new();
    for (position, (name, steps, variables)) in [
        (
            "test [dir=a]",
            &in_variant,
            r#"{"MATRIX_DIR":"a","dir":"a"}"#,
        ),
        (
            "test [dir=b]",
            &in_variant,
            r#"{"MATRIX_DIR":"b","dir":"b"}"#,
        ),
        ("climb", &climbing, r#"{"INPUT_DIR":"../.."}"#),
    ]
    .into_iter()
    .enumerate()
    {
        let created = rg_db::ops::pipeline_ops::create_job(
            &db,
            stage.id,
            name,
            "echo flat",
            None,
            None,
            Some(variables),
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
        let job_id = base_id + position as i64;
        db.execute_unprepared(&format!(
            "UPDATE pipeline_jobs SET id = {job_id} WHERE id = {}",
            created.id
        ))
        .await
        .unwrap();
        let names = steps
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        rg_db::ops::pipeline_step_ops::create_job_steps(&db, job_id, &steps.to_string(), &names)
            .await
            .unwrap();
        ids.push(job_id);
    }

    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "directory-runner",
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

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
    let mut finished = Vec::new();
    for &job_id in &ids {
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
                "job {} never ended: {}",
                job.name,
                job.status
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };
        finished.push(job);
    }
    stop.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), agent)
        .await
        .unwrap()
        .unwrap();

    for (position, dir) in [(0, "a"), (1, "b")] {
        assert_eq!(
            finished[position].status, "success",
            "{:?}",
            finished[position].log
        );
        let log = rg_db::ops::pipeline_step_ops::find_job_step(&db, ids[position], 2)
            .await
            .unwrap()
            .unwrap()
            .log
            .unwrap_or_default();
        assert!(log.contains(&format!("in=[{dir}]")), "{dir}: {log}");
    }
    assert_eq!(finished[2].status, "failure", "{:?}", finished[2].log);
    let log = rg_db::ops::pipeline_step_ops::find_job_step(&db, ids[2], 1)
        .await
        .unwrap()
        .unwrap()
        .log
        .unwrap_or_default();
    assert!(
        log.contains("working-directory `../..` must stay inside the workspace"),
        "{log}"
    );
}

/// card_bc6676ec0ae3: a job's `services:` travel to the runner with the job,
/// and a runner that runs steps but does not start services — protocol 2 — is
/// refused such a job by name instead of running its steps against a
/// `postgres` that is not there.
#[tokio::test]
async fn a_job_with_services_goes_only_to_a_runner_that_starts_them() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed(&base, &db, "services").await;
    let job_id = job(&db, seeded.stage_id, "integration", &["Query"]).await;
    let context = serde_json::json!({
        "services": [{
            "key": "postgres",
            "image": "postgres:16",
            "env": {"POSTGRES_PASSWORD": "${PG_PASSWORD}"},
            "health": ["--health-cmd=pg_isready"],
        }],
    });
    rg_db::ops::pipeline_step_ops::set_job_context(&db, job_id, &context.to_string())
        .await
        .unwrap();

    let response = poll(&base, &seeded, Some("2")).await;
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    let body = response.json::<serde_json::Value>().await.unwrap();
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains(&format!("job #{job_id}")) && error.contains("services"),
        "the refusal names the job and what it needs: {body}"
    );
    let stored = rg_db::ops::pipeline_ops::get_job(&db, job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (stored.status.as_str(), stored.runner_id),
        ("pending", None),
        "a refused job stays for a runner that can run it"
    );

    let response = poll(
        &base,
        &seeded,
        Some(&rg_core::ci::SERVICES_PROTOCOL.to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["job_id"].as_i64(), Some(job_id));
    assert_eq!(body["services"], context["services"], "{body}");
}

/// card_e42a6d0af271: an Actions job is given only the secrets it names, the
/// runner is told which they are, and a runner that would hand them to every
/// step — protocol 3 — is refused the job by name. A job that names none goes
/// to that runner with no secret at all.
#[tokio::test]
async fn an_actions_job_is_given_only_the_secrets_it_names() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed(&base, &db, "secrets").await;
    for (name, value) in [("ALPHA", "alpha-value"), ("BRAVO", "bravo-value")] {
        let stored = reqwest::Client::new()
            .put(format!(
                "{base}/api/v1/repos/{owner}/{owner}/actions/secrets/{name}",
                owner = seeded.owner
            ))
            .bearer_auth(&seeded.owner_token)
            .json(&serde_json::json!({ "value": value }))
            .send()
            .await
            .unwrap();
        assert_eq!(stored.status(), StatusCode::CREATED);
    }
    let quiet = job(&db, seeded.stage_id, "quiet", &["Build"]).await;
    rg_db::ops::pipeline_step_ops::set_job_context(&db, quiet, r#"{"secrets":[]}"#)
        .await
        .unwrap();

    // Names none: an old runner takes it, and it carries neither secret.
    let response = poll(&base, &seeded, Some("3")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["job_id"].as_i64(), Some(quiet));
    let variables = body["variables"].as_object().unwrap();
    assert!(
        !variables.contains_key("ALPHA") && !variables.contains_key("BRAVO"),
        "{body}"
    );
    assert_eq!(body["secrets"], serde_json::json!([]));

    let deploy = job(&db, seeded.stage_id, "deploy", &["Deploy"]).await;
    rg_db::ops::pipeline_step_ops::set_job_context(&db, deploy, r#"{"secrets":["ALPHA"]}"#)
        .await
        .unwrap();
    let response = poll(&base, &seeded, Some("3")).await;
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    let error = response.json::<serde_json::Value>().await.unwrap()["error"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        error.contains(&format!("job #{deploy}")) && error.contains("secrets"),
        "{error}"
    );

    let response = poll(
        &base,
        &seeded,
        Some(&rg_core::ci::SCOPED_SECRETS_PROTOCOL.to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["job_id"].as_i64(), Some(deploy));
    let variables = body["variables"].as_object().unwrap();
    assert_eq!(variables["ALPHA"], "alpha-value", "{body}");
    assert!(!variables.contains_key("BRAVO"), "{body}");
    assert_eq!(body["secrets"], serde_json::json!(["ALPHA"]));
}

/// card_a1290adf28fd: a job that runs a JavaScript action goes only to a
/// runner that runs them — protocol 4 is refused it by name — and that runner
/// fetches the action's files by the key its step names, and by no other.
#[tokio::test]
async fn a_javascript_action_goes_only_to_a_runner_that_runs_it_with_its_files() {
    let (base, db, repo_root) = crate::common::spawn_test_app_with_db_and_repo_root().await;
    let seeded = seed(&base, &db, "javascript").await;
    let job_id = job(&db, seeded.stage_id, "build", &[]).await;
    let key = "c".repeat(64);
    let other = "d".repeat(64);
    let steps = serde_json::json!([{
        "name": "Set up Node",
        "run": "",
        "action": {
            "files": { "kind": "archive", "key": key },
            "repository": "actions/setup-node",
            "git_ref": "v4",
            "using": "node20",
            "entry": "dist/setup/index.js",
            "state": "node",
        },
    }]);
    rg_db::ops::pipeline_step_ops::create_job_steps(
        &db,
        job_id,
        &steps.to_string(),
        &["Set up Node".to_string()],
    )
    .await
    .unwrap();
    let archives = repo_root.join(rg_core::ci::ACTION_ARCHIVES_DIR);
    std::fs::create_dir_all(&archives).unwrap();
    std::fs::write(archives.join(format!("{key}.tar")), b"archive-bytes").unwrap();
    std::fs::write(archives.join(format!("{other}.tar")), b"someone-else").unwrap();

    let response = poll(
        &base,
        &seeded,
        Some(&rg_core::ci::SCOPED_SECRETS_PROTOCOL.to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    let error = response.json::<serde_json::Value>().await.unwrap()["error"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        error.contains(&format!("job #{job_id}")) && error.contains("JavaScript action"),
        "{error}"
    );

    let response = poll(
        &base,
        &seeded,
        Some(&rg_core::ci::JS_ACTIONS_PROTOCOL.to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["job_id"].as_i64(), Some(job_id));
    assert_eq!(body["steps"][0]["action"]["files"]["key"], key.as_str());

    let fetch = |key: String| {
        let base = base.clone();
        let token = seeded.runner_token.clone();
        let runner_id = seeded.runner_id;
        async move {
            reqwest::Client::new()
                .get(format!(
                    "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/actions/{key}"
                ))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
        }
    };
    let fetched = fetch(key.clone()).await;
    assert_eq!(fetched.status(), StatusCode::OK);
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), b"archive-bytes");
    assert_eq!(fetch(other).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        fetch("../../etc".into()).await.status(),
        StatusCode::NOT_FOUND
    );
}

/// card_3e5298bfab47: a composite action's step that has the action's files
/// beside it goes only to a runner that fetches them — a JavaScript-actions
/// runner would start Node on it — and that runner may fetch the archive the
/// step names.
#[tokio::test]
async fn a_composite_action_with_its_files_goes_only_to_a_runner_that_fetches_them() {
    let (base, db, repo_root) = crate::common::spawn_test_app_with_db_and_repo_root().await;
    let seeded = seed(&base, &db, "composite").await;
    let job_id = job(&db, seeded.stage_id, "build", &[]).await;
    let key = "e".repeat(64);
    let steps = serde_json::json!([{
        "name": "Scripts / Run sh ${GITHUB_ACTION_PATH}/hello.sh",
        "run": "sh ${GITHUB_ACTION_PATH}/hello.sh",
        "shell": "sh",
        "action": {
            "files": { "kind": "archive", "key": key },
            "repository": "tools/scripts",
            "git_ref": "v1",
            "using": rg_core::ci::COMPOSITE_ACTION_USING,
            "entry": "",
            "state": "__action1",
        },
    }]);
    rg_db::ops::pipeline_step_ops::create_job_steps(
        &db,
        job_id,
        &steps.to_string(),
        &["Scripts / Run sh ${GITHUB_ACTION_PATH}/hello.sh".to_string()],
    )
    .await
    .unwrap();
    let archives = repo_root.join(rg_core::ci::ACTION_ARCHIVES_DIR);
    std::fs::create_dir_all(&archives).unwrap();
    std::fs::write(archives.join(format!("{key}.tar")), b"composite-bytes").unwrap();

    for older in [
        rg_core::ci::SCOPED_SECRETS_PROTOCOL,
        rg_core::ci::JS_ACTIONS_PROTOCOL,
    ] {
        let response = poll(&base, &seeded, Some(&older.to_string())).await;
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED, "{older}");
        let error = response.json::<serde_json::Value>().await.unwrap()["error"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            error.contains(&format!("job #{job_id}"))
                && error.contains("composite action")
                && !error.contains("JavaScript"),
            "{older}: {error}"
        );
    }

    let response = poll(
        &base,
        &seeded,
        Some(&rg_core::ci::COMPOSITE_FILES_PROTOCOL.to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["job_id"].as_i64(), Some(job_id));
    let fetched = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/{job_id}/actions/{key}",
            seeded.runner_id
        ))
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(fetched.status(), StatusCode::OK);
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), b"composite-bytes");
}

/// card_1535b3af81ac: the job page reads an Actions job's `jobs.<id>.name`
/// beside the job's own name, which stays its id in the pipeline; a job the
/// workflow gave no name carries none.
#[tokio::test]
async fn the_job_page_shows_a_jobs_name_beside_its_id() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed(&base, &db, "named").await;
    let named = job(&db, seeded.stage_id, "build/b [os=linux]", &[]).await;
    job(&db, seeded.stage_id, "build/plain", &[]).await;
    rg_db::ops::pipeline_ops::set_job_display_name(&db, named, "Build on linux")
        .await
        .unwrap();

    let detail = reqwest::Client::new()
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
    let jobs = detail["stages"][0]["jobs"]
        .as_array()
        .expect("the stage lists its jobs")
        .iter()
        .map(|job| {
            (
                job["name"].as_str().unwrap().to_string(),
                job["display_name"].as_str().map(str::to_string),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        jobs,
        [
            (
                "build/b [os=linux]".to_string(),
                Some("Build on linux".to_string())
            ),
            ("build/plain".to_string(), None),
        ],
        "{detail}"
    );
}

/// card_9f345eccf1e3: a run's `run-name:` reaches the API on the pipeline
/// itself — in the list and in the detail — so the pipeline list can show it;
/// a pipeline whose workflows named nothing answers `null`.
#[tokio::test]
async fn the_pipeline_api_shows_a_runs_name() {
    use sea_orm::ConnectionTrait;

    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed(&base, &db, "run-named").await;
    let client = reqwest::Client::new();
    let get = |path: String| {
        let client = client.clone();
        let token = seeded.owner_token.clone();
        async move {
            client
                .get(path)
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };
    let repo = format!("{base}/api/v1/repos/{owner}/{owner}", owner = seeded.owner);
    let detail_url = format!("{repo}/pipelines/{}", seeded.pipeline_id);

    let unnamed = get(detail_url.clone()).await;
    assert!(unnamed["pipeline"]["run_name"].is_null(), "{unnamed}");

    db.execute_unprepared(&format!(
        "UPDATE pipelines SET run_name = 'Deploy staging' WHERE id = {}",
        seeded.pipeline_id
    ))
    .await
    .unwrap();
    let detail = get(detail_url).await;
    assert_eq!(detail["pipeline"]["run_name"], "Deploy staging", "{detail}");
    let list = get(format!("{repo}/pipelines")).await;
    let listed = list["data"]
        .as_array()
        .expect("the pipeline list")
        .iter()
        .find(|pipeline| pipeline["id"] == seeded.pipeline_id)
        .unwrap_or_else(|| panic!("the pipeline is listed: {list}"));
    assert_eq!(listed["run_name"], "Deploy staging", "{list}");
}
