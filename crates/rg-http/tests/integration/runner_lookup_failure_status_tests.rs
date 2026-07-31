//! Regression coverage for card_4791042b81e2.
//!
//! Runner requests do several database operations before the lookup whose
//! failure matters here. A closed-pool test over the production router would
//! therefore be vacuous: authentication would answer first. The trigger-backed
//! cases below keep all earlier work healthy, then corrupt a mandatory text
//! column at the exact write boundary named by the test. SQLx subsequently
//! rejects that row as invalid UTF-8, giving the target lookup a real database
//! error after assignment or after the job-result write.

use axum::http::StatusCode;
use sea_orm::ConnectionTrait;

use crate::common::{register_full, spawn_test_app_with_db, spawn_test_app_with_state};

struct SeededJob {
    runner_id: i64,
    runner_token: String,
    pipeline_id: i64,
    stage_id: i64,
    job_id: i64,
}

async fn seed_job(base: &str, db: &rg_db::DatabaseConnection, suffix: &str) -> SeededJob {
    let client = reqwest::Client::new();
    let username = format!("rf-{suffix}");
    let email = format!("{username}@example.test");
    let (owner_token, _) = register_full(base, &username, &email).await;
    let repo_name = format!("rf-{suffix}");
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(owner_token)
        .json(&serde_json::json!({
            "name": repo_name,
            "is_private": true,
        }))
        .send()
        .await
        .expect("create runner fault-injection repository");
    let created_status = created.status();
    let created_body = created
        .json::<serde_json::Value>()
        .await
        .expect("repository response is JSON");
    assert_eq!(created_status, StatusCode::CREATED, "{created_body}");
    let repo_id = created_body["id"]
        .as_i64()
        .expect("repository response carries its id");

    let runner = rg_db::ops::runner_ops::register_runner(
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
        false,
        None,
        None,
        None,
    )
    .await
    .expect("create job");

    SeededJob {
        runner_id: runner.id,
        runner_token: runner.token,
        pipeline_id: pipeline.id,
        stage_id: stage.id,
        job_id: job.id,
    }
}

async fn install_trigger(db: &rg_db::DatabaseConnection, sql: &str) {
    db.execute_unprepared(sql)
        .await
        .expect("install targeted database fault trigger");
}

/// Exercise `poll_job` without runner-auth middleware so a deliberately broken
/// runner row reaches the lookup named by the card instead of failing earlier
/// in `find_by_token`. The rest of the database stays healthy: before the fix,
/// the decode error became an empty label set and the tagged job below was
/// actually assigned.
#[tokio::test]
async fn runner_lookup_failure_never_becomes_an_empty_label_set() {
    use tower::ServiceExt as _;

    let (base, db, state) = spawn_test_app_with_state().await;
    let seeded = seed_job(&base, &db, "runner-row").await;
    db.execute_unprepared(&format!(
        "UPDATE runners SET labels = CAST(X'80' AS TEXT) WHERE id = {}",
        seeded.runner_id
    ))
    .await
    .expect("make only the runner-row lookup fail to decode");

    let request = axum::http::Request::builder()
        .uri(format!(
            "/api/v1/runners/{}/jobs/poll?timeout=1",
            seeded.runner_id
        ))
        .body(axum::body::Body::empty())
        .expect("build runner poll request");
    let response = axum::Router::new()
        .route(
            "/api/v1/runners/{id}/jobs/poll",
            axum::routing::get(rg_http::api::runners::poll_job),
        )
        .with_state(state)
        .oneshot(request)
        .await
        .expect("runner poll response");

    assert!(
        response.status().is_server_error(),
        "a failed runner lookup must not schedule as an unlabelled runner: {}",
        response.status()
    );
    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload pending job")
        .expect("pending job still exists");
    assert_eq!(persisted.status, "pending");
    assert_eq!(persisted.runner_id, None);
}

async fn assert_poll_context_lookup_failure(target: &str) {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, &format!("poll-{target}")).await;
    let trigger = match target {
        "stage" => format!(
            "CREATE TRIGGER break_poll_stage_after_assignment\n\
             AFTER UPDATE OF runner_id ON pipeline_jobs\n\
             WHEN NEW.id = {} AND NEW.runner_id IS NOT NULL\n\
             BEGIN\n\
               UPDATE pipeline_stages\n\
               SET name = CAST(X'80' AS TEXT)\n\
               WHERE id = NEW.stage_id;\n\
             END;",
            seeded.job_id
        ),
        "pipeline" => format!(
            "CREATE TRIGGER break_poll_pipeline_after_assignment\n\
             AFTER UPDATE OF runner_id ON pipeline_jobs\n\
             WHEN NEW.id = {} AND NEW.runner_id IS NOT NULL\n\
             BEGIN\n\
               UPDATE pipelines\n\
               SET ref_name = CAST(X'80' AS TEXT)\n\
               WHERE id = {};\n\
             END;",
            seeded.job_id, seeded.pipeline_id
        ),
        other => panic!("unknown poll fault target {other}"),
    };
    install_trigger(&db, &trigger).await;

    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/poll?timeout=1",
            seeded.runner_id
        ))
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .expect("poll assigned job");
    assert!(
        response.status().is_server_error(),
        "the {target} lookup failed after assignment but poll returned {}",
        response.status()
    );

    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload assigned job")
        .expect("assigned job still exists");
    assert_eq!(persisted.runner_id, Some(seeded.runner_id));
    assert_eq!(persisted.status, "assigned");
}

#[tokio::test]
async fn assigned_job_stage_and_pipeline_lookup_failures_are_not_context_free_jobs() {
    // Each case gets its own database: an invalid UTF-8 value is deliberately
    // unrecoverable through the typed model and must not leak into another case.
    assert_poll_context_lookup_failure("stage").await;
    assert_poll_context_lookup_failure("pipeline").await;
}

async fn assert_finish_rollup_failure(target: &str) {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, &format!("finish-{target}")).await;
    rg_db::ops::pipeline_ops::assign_job(&db, seeded.job_id, seeded.runner_id)
        .await
        .expect("assign job before finish");
    rg_db::ops::pipeline_ops::update_job_result(
        &db,
        seeded.job_id,
        "running",
        None,
        None,
        Some(chrono::Utc::now().naive_utc()),
        None,
    )
    .await
    .expect("start job before finish");

    let trigger = match target {
        "stage-rollup" => format!(
            "CREATE TRIGGER break_stage_rollup_after_job_finish\n\
             AFTER UPDATE OF finished_at ON pipeline_jobs\n\
             WHEN NEW.id = {} AND NEW.finished_at IS NOT NULL\n\
             BEGIN\n\
               UPDATE pipeline_stages\n\
               SET name = CAST(X'80' AS TEXT)\n\
               WHERE id = NEW.stage_id;\n\
             END;",
            seeded.job_id
        ),
        "stage-context" => format!(
            "CREATE TRIGGER break_stage_context_after_rollup\n\
             AFTER UPDATE OF status ON pipeline_stages\n\
             WHEN NEW.id = {} AND NEW.status IN ('success', 'failed')\n\
             BEGIN\n\
               UPDATE pipeline_stages\n\
               SET name = CAST(X'80' AS TEXT)\n\
               WHERE id = NEW.id;\n\
             END;",
            seeded.stage_id
        ),
        "pipeline-rollup" => format!(
            "CREATE TRIGGER break_pipeline_rollup_after_stage_finish\n\
             AFTER UPDATE OF status ON pipeline_stages\n\
             WHEN NEW.id = {} AND NEW.status IN ('success', 'failed')\n\
             BEGIN\n\
               UPDATE pipelines\n\
               SET ref_name = CAST(X'80' AS TEXT)\n\
               WHERE id = {};\n\
             END;",
            seeded.stage_id, seeded.pipeline_id
        ),
        "pipeline-context" => format!(
            "CREATE TRIGGER break_pipeline_context_after_rollup\n\
             AFTER UPDATE OF status ON pipelines\n\
             WHEN NEW.id = {} AND NEW.status IN ('success', 'failed')\n\
             BEGIN\n\
               UPDATE pipelines\n\
               SET ref_name = CAST(X'80' AS TEXT)\n\
               WHERE id = NEW.id;\n\
             END;",
            seeded.pipeline_id
        ),
        other => panic!("unknown finish fault target {other}"),
    };
    install_trigger(&db, &trigger).await;

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{}/finish",
            seeded.runner_id, seeded.job_id
        ))
        .bearer_auth(&seeded.runner_token)
        .json(&serde_json::json!({"status": "success", "exit_code": 0}))
        .send()
        .await
        .expect("finish job through production router");
    assert!(
        response.status().is_server_error(),
        "the {target} fault happened after the job-result write but finish returned {}",
        response.status()
    );

    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload finished job")
        .expect("finished job still exists");
    assert_eq!(persisted.status, "success");
    assert_eq!(persisted.exit_code, Some(0));
    assert!(persisted.finished_at.is_some());
}

#[tokio::test]
async fn finish_reports_every_post_write_rollup_and_context_failure() {
    for target in [
        "stage-rollup",
        "stage-context",
        "pipeline-rollup",
        "pipeline-context",
    ] {
        assert_finish_rollup_failure(target).await;
    }
}
