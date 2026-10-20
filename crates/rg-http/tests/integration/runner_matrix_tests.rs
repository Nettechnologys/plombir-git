//! card_d97de416d0b0: the variants of a matrix job on the runner API.
//!
//! `strategy.max-parallel` holds a variant back from a polling runner while
//! as many of its siblings already hold one; `strategy.fail-fast` cancels the
//! unfinished siblings in the failing variant's own finish; and a matrix that
//! reads a needed job's outputs is expanded by the poll once its stage may
//! run — its placeholder is never handed out.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;

use crate::common::{register_full, spawn_test_app_with_overrides, StateOverrides};

struct Seeded {
    runner_id: i64,
    runner_token: String,
    pipeline_id: i64,
    stage_ids: Vec<i64>,
}

async fn seed(base: &str, db: &rg_db::DatabaseConnection, suffix: &str, stages: usize) -> Seeded {
    let client = reqwest::Client::new();
    let owner = format!("matrix-{suffix}");
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
        "2222222222222222222222222222222222222222",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .unwrap();
    let mut stage_ids = Vec::new();
    for order in 0..stages {
        let stage = rg_db::ops::pipeline_ops::create_stage(
            db,
            pipeline.id,
            &format!("stage-{order}"),
            order as i32,
        )
        .await
        .unwrap();
        stage_ids.push(stage.id);
    }
    Seeded {
        runner_id: runner.id,
        runner_token,
        pipeline_id: pipeline.id,
        stage_ids,
    }
}

async fn job(
    db: &rg_db::DatabaseConnection,
    stage_id: i64,
    name: &str,
    allow_failure: bool,
) -> i64 {
    rg_db::ops::pipeline_ops::create_job(
        db,
        stage_id,
        name,
        "echo ok",
        None,
        None,
        None,
        None,
        None,
        None,
        allow_failure,
        None,
        None,
        None,
    )
    .await
    .unwrap()
    .id
}

/// Three variants of `test`, with the strategy given.
async fn variants(
    db: &rg_db::DatabaseConnection,
    stage_id: i64,
    fail_fast: bool,
    max_parallel: Option<u32>,
) -> Vec<i64> {
    let mut ids = Vec::new();
    for n in 1..=3 {
        let id = job(db, stage_id, &format!("test [n={n}]"), false).await;
        rg_db::ops::pipeline_matrix_ops::set_matrix_membership(
            db,
            id,
            "test",
            fail_fast,
            max_parallel,
        )
        .await
        .unwrap();
        ids.push(id);
    }
    ids
}

async fn poll(base: &str, seeded: &Seeded) -> Option<i64> {
    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/poll?timeout=1",
            seeded.runner_id
        ))
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .expect("poll");
    match response.status() {
        StatusCode::OK => Some(
            response.json::<serde_json::Value>().await.unwrap()["job_id"]
                .as_i64()
                .unwrap(),
        ),
        StatusCode::NO_CONTENT => None,
        other => panic!("poll answered {other}: {}", response.text().await.unwrap()),
    }
}

async fn run_to(base: &str, seeded: &Seeded, job_id: i64, status: &str) {
    let client = reqwest::Client::new();
    let url = |path: &str| {
        format!(
            "{base}/api/v1/runners/{}/jobs/{job_id}/{path}",
            seeded.runner_id
        )
    };
    let started = client
        .post(url("start"))
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::OK, "start {job_id}");
    let exit_code = if status == "success" { 0 } else { 1 };
    let finished = client
        .post(url("finish"))
        .bearer_auth(&seeded.runner_token)
        .json(&serde_json::json!({ "status": status, "exit_code": exit_code }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        finished.status(),
        StatusCode::OK,
        "finish {job_id}: {}",
        finished.text().await.unwrap_or_default()
    );
}

async fn status(db: &rg_db::DatabaseConnection, job_id: i64) -> String {
    rg_db::ops::pipeline_ops::get_job(db, job_id)
        .await
        .unwrap()
        .unwrap()
        .status
}

/// `max-parallel: 1`: while one variant holds a runner the next one is not
/// handed out — not by the poll, and not by a claim that raced it — and once
/// it finishes the next one is.
#[tokio::test]
async fn max_parallel_holds_the_next_variant_until_one_finishes() {
    let (base, db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let seeded = seed(&base, &db, "parallel", 1).await;
    let ids = variants(&db, seeded.stage_ids[0], false, Some(1)).await;

    assert_eq!(poll(&base, &seeded).await, Some(ids[0]));
    assert_eq!(
        poll(&base, &seeded).await,
        None,
        "the matrix is at its max-parallel"
    );
    assert!(
        !rg_db::ops::pipeline_ops::assign_job(&db, ids[1], seeded.runner_id)
            .await
            .unwrap(),
        "a claim that skipped the poll's check is refused under the graph lock"
    );
    run_to(&base, &seeded, ids[0], "success").await;
    assert_eq!(poll(&base, &seeded).await, Some(ids[1]));
}

/// `fail-fast` (the default): the failing variant's finish cancels a running
/// sibling and a pending one, with the reason in their logs, and the stage
/// and pipeline fail with it. With `fail-fast: false`, or for a variant that
/// may fail, the siblings are left to run.
#[tokio::test]
async fn fail_fast_cancels_the_unfinished_variants_in_the_failing_finish() {
    let (base, db) = spawn_test_app_with_overrides(StateOverrides::default()).await;

    for (suffix, fail_fast, allow_failure, expected) in [
        ("fast", true, false, ["failed", "canceled", "canceled"]),
        ("slow", false, false, ["failed", "running", "pending"]),
        ("allowed", true, true, ["failed", "running", "pending"]),
    ] {
        let seeded = seed(&base, &db, suffix, 1).await;
        let ids = variants(&db, seeded.stage_ids[0], fail_fast, None).await;
        if allow_failure {
            use sea_orm::ConnectionTrait;
            db.execute_unprepared(&format!(
                "UPDATE pipeline_jobs SET allow_failure = true WHERE id = {}",
                ids[0]
            ))
            .await
            .unwrap();
        }
        assert_eq!(poll(&base, &seeded).await, Some(ids[0]));
        assert!(
            rg_db::ops::pipeline_ops::assign_job(&db, ids[1], seeded.runner_id)
                .await
                .unwrap()
        );
        assert!(
            rg_db::ops::pipeline_ops::start_job_if_active(&db, ids[1], None)
                .await
                .unwrap()
        );
        run_to(&base, &seeded, ids[0], "failed").await;

        let mut statuses = Vec::new();
        for id in &ids {
            statuses.push(status(&db, *id).await);
        }
        assert_eq!(statuses, expected, "{suffix}");
        if fail_fast && !allow_failure {
            let canceled = rg_db::ops::pipeline_ops::get_job(&db, ids[2])
                .await
                .unwrap()
                .unwrap();
            let log = canceled.log.unwrap_or_default();
            assert!(
                log.contains("`test [n=1]` failed, and the matrix job's strategy.fail-fast is on"),
                "{log}"
            );
            let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&db, seeded.pipeline_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                pipeline.status, "failed",
                "the stage rolled up with the cancellations"
            );
        }
    }
}

/// A CI engine double whose expansion writes one variant `test [n=x]` in
/// the placeholder's stage, through the same graph transaction the real
/// engine uses, and remembers which placeholders it was asked for.
#[derive(Default)]
struct ExpandingEngine {
    expanded: Mutex<Vec<i64>>,
}

impl rg_core::ci::CiTrigger for ExpandingEngine {
    fn has_ci_config(&self, _repo_path: &std::path::Path, _commit_sha: &str) -> bool {
        false
    }

    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
        false
    }

    fn expand_deferred_matrix<'a>(
        &'a self,
        db: &'a rg_db::DatabaseConnection,
        placeholder: &'a rg_db::entities::pipeline_job::Model,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = anyhow::Result<Option<rg_db::entities::pipeline::Model>>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.expanded.lock().unwrap().push(placeholder.id);
            let stage_id = placeholder.stage_id;
            rg_db::ops::pipeline_matrix_ops::expand_deferred_job(
                db,
                placeholder,
                |tx| async move {
                    let written = rg_db::ops::pipeline_ops::create_job(
                        &tx,
                        stage_id,
                        "test [n=x]",
                        "echo x",
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
                    .map(|_| ());
                    (tx, written)
                },
            )
            .await?;
            Ok(None)
        })
    }

    fn trigger_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        Box::pin(async { Ok(0) })
    }

    fn resume_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// The placeholder of a matrix that reads `setup`'s outputs is never handed
/// to a runner. The poll asks the engine to expand it only once `setup`'s
/// stage succeeded, and then hands out the variant it became.
#[tokio::test]
async fn a_poll_expands_a_ready_matrix_and_hands_out_its_variants() {
    let engine = Arc::new(ExpandingEngine::default());
    let (base, db) = spawn_test_app_with_overrides(StateOverrides {
        ci_engine: Some(engine.clone()),
        ..StateOverrides::default()
    })
    .await;
    let seeded = seed(&base, &db, "deferred", 2).await;
    let setup = job(&db, seeded.stage_ids[0], "setup", false).await;
    let placeholder = job(&db, seeded.stage_ids[1], "test", false).await;
    rg_db::ops::pipeline_matrix_ops::set_deferred_matrix(&db, placeholder, "{}")
        .await
        .unwrap();

    assert_eq!(poll(&base, &seeded).await, Some(setup));
    assert!(
        engine.expanded.lock().unwrap().is_empty(),
        "setup has not finished: nothing to expand from yet"
    );
    run_to(&base, &seeded, setup, "success").await;

    let handed = poll(&base, &seeded)
        .await
        .expect("the variant is handed out");
    assert_ne!(handed, placeholder, "a placeholder is never run");
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(&db, handed)
            .await
            .unwrap()
            .unwrap()
            .name,
        "test [n=x]"
    );
    assert_eq!(*engine.expanded.lock().unwrap(), [placeholder]);
    assert!(
        rg_db::ops::pipeline_ops::get_job(&db, placeholder)
            .await
            .unwrap()
            .is_none(),
        "the placeholder is replaced, not kept beside its variants"
    );
}

/// With an engine that cannot expand it, the placeholder stays where it is:
/// the poll finds no work rather than running it as a job.
#[tokio::test]
async fn a_placeholder_the_engine_cannot_expand_is_never_handed_out() {
    let (base, db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let seeded = seed(&base, &db, "stuck", 1).await;
    let placeholder = job(&db, seeded.stage_ids[0], "test", false).await;
    rg_db::ops::pipeline_matrix_ops::set_deferred_matrix(&db, placeholder, "{}")
        .await
        .unwrap();
    assert_eq!(poll(&base, &seeded).await, None);
    assert_eq!(status(&db, placeholder).await, "pending");
}
