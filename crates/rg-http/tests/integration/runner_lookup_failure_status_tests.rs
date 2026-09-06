//! Regression coverage for card_4791042b81e2, card_b2d69ce9baa6 and
//! card_055582f58b13.
//!
//! The first card is about lookups a runner request reads; the latter two are
//! about lifecycle writes — heartbeat/status transitions and both routed ways
//! to deregister a runner. The families share this file's harness for the same
//! reason: the fault has to be aimed at one table, after the request has already
//! got past authentication.
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
    repo_id: i64,
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

    SeededJob {
        repo_id,
        runner_id: runner.id,
        runner_token,
        pipeline_id: pipeline.id,
        stage_id: stage.id,
        job_id: job.id,
    }
}

async fn admin_token(base: &str, db: &rg_db::DatabaseConnection, suffix: &str) -> String {
    let username = format!("runner-admin-{suffix}");
    let (token, user_id) =
        register_full(base, &username, &format!("{username}@example.test")).await;
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, Some(true), None)
        .await
        .expect("promote runner-test user")
        .expect("registered runner-test user exists");
    token
}

async fn create_additional_job(db: &rg_db::DatabaseConnection, stage_id: i64, name: &str) -> i64 {
    rg_db::ops::pipeline_ops::create_job(
        db,
        stage_id,
        name,
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
    .expect("create additional runner lifecycle job")
    .id
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

/// A corrupt job tag list is not an untagged job. In particular, an unlabelled
/// runner must not receive it merely because the tag parser could not recover
/// the stored requirement.
#[tokio::test]
async fn malformed_job_tags_are_not_routed_as_untagged() {
    use tower::ServiceExt as _;

    let (base, db, state) = spawn_test_app_with_state().await;
    let seeded = seed_job(&base, &db, "malformed-job-tags").await;
    db.execute_unprepared(&format!(
        "UPDATE runners SET labels = '[]' WHERE id = {}",
        seeded.runner_id
    ))
    .await
    .expect("make the runner intentionally unlabelled");
    db.execute_unprepared(&format!(
        "UPDATE pipeline_jobs SET tags = '{{' WHERE id = {}",
        seeded.job_id
    ))
    .await
    .expect("make the job tag list malformed");

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

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload malformed-tag job")
        .expect("malformed-tag job still exists");
    assert_eq!(persisted.status, "pending");
    assert_eq!(persisted.runner_id, None);
}

/// card_2ba27c033497: the job row carries two runner-facing JSON columns, and a
/// runner cannot tell a stored-empty one from an undecodable one. Both used to be
/// decoded *after* the claim and with the error discarded — the runner got a
/// `200`, started work without its mandatory environment (or with caching
/// silently off), and the row was already `running` under its own id, so no other
/// runner could take it either. The claim must not happen at all.
async fn assert_undecodable_job_column_is_refused_before_assignment(column: &str, stored: &str) {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, &format!("corrupt-{column}-{}", stored.len())).await;
    db.execute_unprepared(&format!(
        "UPDATE pipeline_jobs SET {column} = '{stored}' WHERE id = {}",
        seeded.job_id
    ))
    .await
    .expect("store an undecodable runner-facing column");

    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/poll?timeout=1",
            seeded.runner_id
        ))
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .expect("poll through the production router");
    assert!(
        response.status().is_server_error(),
        "stored {column} `{stored}` could not be decoded but poll returned {}",
        response.status()
    );

    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload the refused job")
        .expect("the refused job still exists");
    assert_eq!(
        persisted.status, "pending",
        "the job was claimed although its {column} never decoded"
    );
    assert_eq!(persisted.runner_id, None);
}

#[tokio::test]
async fn poll_refuses_undecodable_runner_facing_job_columns() {
    // Each case gets its own database so one poisoned row cannot mask another.
    assert_undecodable_job_column_is_refused_before_assignment("variables", "{").await;
    // Valid JSON of the wrong shape is undecodable too: `.ok()` erased this
    // exactly like a syntax error did.
    assert_undecodable_job_column_is_refused_before_assignment("variables", "[]").await;
    assert_undecodable_job_column_is_refused_before_assignment("cache_paths", r#"["target""#).await;
    assert_undecodable_job_column_is_refused_before_assignment(
        "cache_paths",
        r#"{"dir":"target"}"#,
    )
    .await;
}

/// The other half of the rule: a stored `NULL` is still a genuinely unset
/// optional column, and a decodable value still reaches the runner unchanged.
#[tokio::test]
async fn poll_keeps_the_wire_contract_for_decodable_and_null_job_columns() {
    for (suffix, variables, cache_paths, expected_paths) in [
        (
            "decodable",
            r#"'{"BUILD_MODE":"release"}'"#,
            r#"'["target","node_modules"]'"#,
            Some(serde_json::json!(["target", "node_modules"])),
        ),
        ("null", "NULL", "NULL", None),
    ] {
        let (base, db) = spawn_test_app_with_db().await;
        let seeded = seed_job(&base, &db, &format!("wire-{suffix}")).await;
        db.execute_unprepared(&format!(
            "UPDATE pipeline_jobs SET variables = {variables}, cache_paths = {cache_paths} \
             WHERE id = {}",
            seeded.job_id
        ))
        .await
        .expect("store the runner-facing columns");

        let response = reqwest::Client::new()
            .get(format!(
                "{base}/api/v1/runners/{}/jobs/poll?timeout=5",
                seeded.runner_id
            ))
            .bearer_auth(&seeded.runner_token)
            .send()
            .await
            .expect("poll through the production router");
        let status = response.status();
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("poll response is JSON");
        assert_eq!(status, StatusCode::OK, "{suffix}: {body}");
        assert_eq!(body["job_id"].as_i64(), Some(seeded.job_id));

        match &expected_paths {
            Some(paths) => assert_eq!(&body["cache_paths"], paths, "{suffix}"),
            None => assert!(body["cache_paths"].is_null(), "{suffix}: {body}"),
        }
        // The injected CI environment is present either way; the stored entry
        // only survives in the decodable case.
        assert_eq!(body["variables"]["CI"], serde_json::json!("true"), "{body}");
        assert_eq!(
            body["variables"]["CI_PIPELINE_ID"].as_i64(),
            Some(seeded.pipeline_id)
        );
        assert_eq!(
            body["variables"]["CI_REPOSITORY"],
            serde_json::json!(format!("rf-wire-{suffix}/rf-wire-{suffix}"))
        );
        assert_eq!(
            body["variables"]["CI_REPOSITORY_OWNER"],
            serde_json::json!(format!("rf-wire-{suffix}"))
        );
        if suffix == "decodable" {
            assert_eq!(
                body["variables"]["BUILD_MODE"],
                serde_json::json!("release")
            );
        }

        let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
            .await
            .expect("reload the assigned job")
            .expect("the assigned job still exists");
        assert_eq!(persisted.runner_id, Some(seeded.runner_id));
    }
}

#[tokio::test]
async fn null_and_empty_job_tags_remain_eligible_without_runner_labels() {
    for (suffix, stored_tags) in [("null-tags", "NULL"), ("empty-tags", "'[]'")] {
        let (base, db) = spawn_test_app_with_db().await;
        let seeded = seed_job(&base, &db, suffix).await;
        db.execute_unprepared(&format!(
            "UPDATE pipeline_jobs SET tags = {stored_tags} WHERE id = {}",
            seeded.job_id
        ))
        .await
        .expect("set the untagged-job fixture");

        let matched =
            rg_db::ops::pipeline_ops::find_pending_job_matching_labels(&db, seeded.repo_id, &[])
                .await
                .expect("find untagged job")
                .expect("untagged job remains eligible");
        assert_eq!(matched.id, seeded.job_id);
    }
}

async fn assert_poll_context_lookup_failure(target: &str) {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, &format!("poll-{target}")).await;
    let damage = match target {
        "stage" => format!(
            "UPDATE pipeline_stages SET name = CAST(X'80' AS TEXT) WHERE id = {}",
            seeded.stage_id
        ),
        "pipeline" => format!(
            "UPDATE pipelines SET ref_name = CAST(X'80' AS TEXT) WHERE id = {}",
            seeded.pipeline_id
        ),
        other => panic!("unknown poll fault target {other}"),
    };
    db.execute_unprepared(&damage)
        .await
        .expect("damage the selected job context");

    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/poll?timeout=1",
            seeded.runner_id
        ))
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .expect("poll candidate job");
    assert!(
        response.status().is_server_error(),
        "the {target} lookup failed during response preparation but poll returned {}",
        response.status()
    );

    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload candidate job")
        .expect("candidate job still exists");
    assert_eq!(persisted.runner_id, None);
    assert_eq!(persisted.status, "pending");
}

#[tokio::test]
async fn job_context_lookup_failures_leave_the_candidate_unassigned() {
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

    let (trigger_name, trigger) = match target {
        "stage-rollup" => (
            "break_stage_rollup_after_job_finish",
            format!(
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
        ),
        "stage-context" => (
            "break_stage_context_after_rollup",
            format!(
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
        ),
        "pipeline-rollup" => (
            "break_pipeline_rollup_after_stage_finish",
            format!(
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
        ),
        "pipeline-context" => (
            "break_pipeline_context_after_rollup",
            format!(
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
    assert_eq!(persisted.status, "running");
    assert_eq!(persisted.exit_code, None);
    assert_eq!(persisted.finished_at, None);
    assert_eq!(
        rg_db::ops::pipeline_ops::get_stage_by_id(&db, seeded.stage_id)
            .await
            .expect("reload stage after failed finish")
            .expect("stage still exists")
            .status,
        "pending"
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_pipeline(&db, seeded.pipeline_id)
            .await
            .expect("reload pipeline after failed finish")
            .expect("pipeline still exists")
            .status,
        "pending"
    );

    db.execute_unprepared(&format!("DROP TRIGGER {trigger_name};"))
        .await
        .expect("remove finish fault trigger");
    let retry = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{}/finish",
            seeded.runner_id, seeded.job_id
        ))
        .bearer_auth(&seeded.runner_token)
        .json(&serde_json::json!({"status": "success", "exit_code": 0}))
        .send()
        .await
        .expect("retry finish after removing the fault");
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
            .await
            .expect("reload retried job")
            .expect("retried job exists")
            .status,
        "success"
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_stage_by_id(&db, seeded.stage_id)
            .await
            .expect("reload retried stage")
            .expect("retried stage exists")
            .status,
        "success"
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_pipeline(&db, seeded.pipeline_id)
            .await
            .expect("reload retried pipeline")
            .expect("retried pipeline exists")
            .status,
        "success"
    );
}

#[tokio::test]
async fn finish_rolls_back_every_post_write_rollup_and_context_failure() {
    for target in [
        "stage-rollup",
        "stage-context",
        "pipeline-rollup",
        "pipeline-context",
    ] {
        assert_finish_rollup_failure(target).await;
    }
}

// ── card_b2d69ce9baa6: writes the response speaks for ──────────────────────
//
// Every case below aborts exactly one write with a SQLite trigger and leaves the
// rest of the database healthy, so the request reaches the handler through
// normal runner-token authentication and fails at the named statement. A closed
// pool would have answered at `find_by_token` instead and proved nothing.

/// Refuse one column's write on the runner row. `UPDATE OF` fires only when that
/// column is in the statement's SET list, which is what keeps the heartbeat
/// refresh (`last_seen_at`) and the status transitions (`status`) separable.
fn refuse_runner_write(name: &str, column: &str, when: &str) -> String {
    format!(
        "CREATE TRIGGER {name}\n\
         BEFORE UPDATE OF {column} ON runners\n\
         WHEN {when}\n\
         BEGIN\n\
           SELECT RAISE(ABORT, 'runner {column} write refused');\n\
         END;"
    )
}

async fn runner_row(
    db: &rg_db::DatabaseConnection,
    runner_id: i64,
) -> Option<rg_db::entities::runner::Model> {
    rg_db::ops::runner_ops::find_by_id(db, runner_id)
        .await
        .expect("reload runner row")
}

/// The `/heartbeat` response is a statement about one write. When that write is
/// refused the endpoint must not answer `200 {"status":"ok"}` — and the same
/// refused refresh must not fail an unrelated report that did land, which is why
/// the finish below runs against the very same broken trigger.
#[tokio::test]
async fn heartbeat_answers_for_the_write_it_reports_without_failing_other_reports() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, "heartbeat").await;
    let client = reqwest::Client::new();
    let heartbeat_url = format!("{base}/api/v1/runners/{}/heartbeat", seeded.runner_id);

    // Non-vacuity: the token, the route and the write all work before the fault.
    let healthy = client
        .post(&heartbeat_url)
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .expect("healthy heartbeat");
    assert_eq!(healthy.status(), StatusCode::OK);

    install_trigger(
        &db,
        &refuse_runner_write(
            "break_runner_heartbeat",
            "last_seen_at",
            &format!("NEW.id = {}", seeded.runner_id),
        ),
    )
    .await;

    let seen_before = runner_row(&db, seeded.runner_id)
        .await
        .expect("runner still registered")
        .last_seen_at;

    let response = client
        .post(&heartbeat_url)
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .expect("heartbeat against the refused write");
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "the heartbeat write was refused but /heartbeat answered {}",
        response.status()
    );
    assert_eq!(
        runner_row(&db, seeded.runner_id)
            .await
            .expect("runner still registered")
            .last_seen_at,
        seen_before,
        "the response claimed a heartbeat that never reached the row"
    );

    // The other half of the rule: the refresh is opportunistic everywhere else.
    // A runner that finished a job has done work it cannot reproduce, so its
    // report must land even while its liveness timestamp is unwritable.
    rg_db::ops::pipeline_ops::assign_job(&db, seeded.job_id, seeded.runner_id)
        .await
        .expect("assign job before finish");
    let finish = client
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{}/finish",
            seeded.runner_id, seeded.job_id
        ))
        .bearer_auth(&seeded.runner_token)
        .json(&serde_json::json!({"status": "success", "exit_code": 0}))
        .send()
        .await
        .expect("finish job while the heartbeat write is refused");
    assert_eq!(
        finish.status(),
        StatusCode::OK,
        "a failed opportunistic heartbeat refresh must not fail an unrelated report"
    );
    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload finished job")
        .expect("finished job still exists");
    assert_eq!(persisted.status, "success");
}

/// `busy` is what stops the scheduler handing this runner a second job. A
/// refused transition may not be reported as a started job — and the report the
/// runner retries has to find the job exactly where it left it.
#[tokio::test]
async fn start_does_not_confirm_a_runner_that_could_not_be_marked_busy() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, "start-busy").await;
    rg_db::ops::pipeline_ops::assign_job(&db, seeded.job_id, seeded.runner_id)
        .await
        .expect("assign job before start");
    install_trigger(
        &db,
        &refuse_runner_write(
            "break_runner_busy_transition",
            "status",
            &format!("NEW.id = {} AND NEW.status = 'busy'", seeded.runner_id),
        ),
    )
    .await;

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{}/start",
            seeded.runner_id, seeded.job_id
        ))
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .expect("start job through the production router");
    assert!(
        response.status().is_server_error(),
        "the runner was never marked busy but start returned {}",
        response.status()
    );
    assert_ne!(
        runner_row(&db, seeded.runner_id)
            .await
            .expect("runner still registered")
            .status,
        "busy"
    );

    // The fault landed after the job-result write — proof it is not a vacuous
    // failure at authentication — and the job is still this runner's, so the
    // retry passes the same gate.
    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload started job")
        .expect("started job still exists");
    assert_eq!(persisted.status, "running");
    assert_eq!(persisted.runner_id, Some(seeded.runner_id));
}

/// The mirror at the other end: a runner left `busy` after finishing is one the
/// scheduler skips until the watchdog notices, so the finish response cannot
/// claim a transition that was refused.
#[tokio::test]
async fn finish_does_not_confirm_a_runner_that_could_not_be_marked_online() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, "finish-online").await;
    rg_db::ops::pipeline_ops::assign_job(&db, seeded.job_id, seeded.runner_id)
        .await
        .expect("assign job before finish");
    rg_db::ops::runner_ops::update_status(&db, seeded.runner_id, "busy")
        .await
        .expect("mark runner busy before finish");
    install_trigger(
        &db,
        &refuse_runner_write(
            "break_runner_online_transition",
            "status",
            &format!("NEW.id = {} AND NEW.status = 'online'", seeded.runner_id),
        ),
    )
    .await;

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{}/finish",
            seeded.runner_id, seeded.job_id
        ))
        .bearer_auth(&seeded.runner_token)
        .json(&serde_json::json!({"status": "success", "exit_code": 0}))
        .send()
        .await
        .expect("finish job through the production router");
    assert!(
        response.status().is_server_error(),
        "the runner was never marked online but finish returned {}",
        response.status()
    );
    assert_eq!(
        runner_row(&db, seeded.runner_id)
            .await
            .expect("runner still registered")
            .status,
        "busy"
    );

    // Runner availability and job settlement are one receipt. Refusing the
    // second write must leave the first one retryable too.
    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload finished job")
        .expect("finished job still exists");
    assert_eq!(persisted.status, "assigned");
    assert_eq!(persisted.exit_code, None);
    assert_eq!(persisted.finished_at, None);

    db.execute_unprepared("DROP TRIGGER break_runner_online_transition;")
        .await
        .expect("remove runner-online fault");
    let retry = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{}/finish",
            seeded.runner_id, seeded.job_id
        ))
        .bearer_auth(&seeded.runner_token)
        .json(&serde_json::json!({"status": "success", "exit_code": 0}))
        .send()
        .await
        .expect("retry finish after runner write recovers");
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        runner_row(&db, seeded.runner_id)
            .await
            .expect("runner still registered")
            .status,
        "online"
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
            .await
            .expect("reload retried job")
            .expect("retried job exists")
            .status,
        "success"
    );
}

/// Deregistration is two writes that are only correct together. Each half is
/// refused in turn; neither may leave the other half committed.
async fn assert_deregistration_is_atomic(target: &str) {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, &format!("deregister-{target}")).await;
    rg_db::ops::pipeline_ops::assign_job(&db, seeded.job_id, seeded.runner_id)
        .await
        .expect("assign job before deregistration");

    let trigger = match target {
        // The job reset fails: the runner must survive, or its jobs are stranded
        // on a row that no longer exists.
        "reset" => format!(
            "CREATE TRIGGER break_runner_job_reset\n\
             BEFORE UPDATE OF status ON pipeline_jobs\n\
             WHEN NEW.id = {} AND NEW.status = 'pending'\n\
             BEGIN\n\
               SELECT RAISE(ABORT, 'job reset refused');\n\
             END;",
            seeded.job_id
        ),
        // The delete fails after the reset succeeded: the reset must roll back,
        // or a live runner is left with the jobs it was executing taken away.
        "delete" => format!(
            "CREATE TRIGGER break_runner_delete\n\
             BEFORE DELETE ON runners\n\
             WHEN OLD.id = {}\n\
             BEGIN\n\
               SELECT RAISE(ABORT, 'runner delete refused');\n\
             END;",
            seeded.runner_id
        ),
        other => panic!("unknown deregistration fault target {other}"),
    };
    install_trigger(&db, &trigger).await;

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{}/deregister",
            seeded.runner_id
        ))
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .expect("deregister through the production router");
    assert!(
        response.status().is_server_error(),
        "the {target} half of deregistration failed but it returned {}",
        response.status()
    );

    assert!(
        runner_row(&db, seeded.runner_id).await.is_some(),
        "the runner was deleted although the {target} half never committed"
    );
    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload assigned job")
        .expect("assigned job still exists");
    assert_eq!(
        persisted.runner_id,
        Some(seeded.runner_id),
        "the {target} failure left the job reset half-applied"
    );
    assert_eq!(persisted.status, "assigned");
}

#[tokio::test]
async fn deregistration_never_commits_one_half_of_its_two_writes() {
    // Each case gets its own database: the triggers are deliberately
    // irreversible for the rows they name.
    assert_deregistration_is_atomic("reset").await;
    assert_deregistration_is_atomic("delete").await;
}

/// The healthy path the fault-injection cases are measured against: the runner's
/// job goes back to the pool and the runner row is gone, in one response.
#[tokio::test]
async fn deregistration_returns_the_jobs_and_deletes_the_runner() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, "deregister-healthy").await;
    rg_db::ops::pipeline_ops::assign_job(&db, seeded.job_id, seeded.runner_id)
        .await
        .expect("assign job before deregistration");

    let client = reqwest::Client::new();
    let deregister_url = format!("{base}/api/v1/runners/{}/deregister", seeded.runner_id);
    let response = client
        .post(&deregister_url)
        .bearer_auth(&seeded.runner_token)
        .send()
        .await
        .expect("deregister through the production router");
    assert_eq!(response.status(), StatusCode::OK);

    assert!(runner_row(&db, seeded.runner_id).await.is_none());
    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload released job")
        .expect("released job still exists");
    assert_eq!(persisted.status, "pending");
    assert_eq!(persisted.runner_id, None);
}

/// The admin route removes the same lifecycle object as self-deregistration.
/// Both assigned and running work must be handed back before the row disappears.
#[tokio::test]
async fn admin_deletion_returns_assigned_and_running_jobs_before_deleting_the_runner() {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, "admin-delete-healthy").await;
    let running_job_id = create_additional_job(&db, seeded.stage_id, "running-job").await;

    for job_id in [seeded.job_id, running_job_id] {
        assert!(
            rg_db::ops::pipeline_ops::assign_job(&db, job_id, seeded.runner_id)
                .await
                .expect("assign job before admin deletion")
        );
    }
    assert!(
        rg_db::ops::pipeline_ops::start_job_if_active(&db, running_job_id, None)
            .await
            .expect("start second job before admin deletion")
    );

    let token = admin_token(&base, &db, "delete-healthy").await;
    let delete_url = format!("{base}/api/v1/admin/runners/{}", seeded.runner_id);
    let client = reqwest::Client::new();
    let response = client
        .delete(&delete_url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete runner through the admin route");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    assert!(runner_row(&db, seeded.runner_id).await.is_none());
    for job_id in [seeded.job_id, running_job_id] {
        let persisted = rg_db::ops::pipeline_ops::get_job(&db, job_id)
            .await
            .expect("reload job released by admin deletion")
            .expect("released job still exists");
        assert_eq!(persisted.status, "pending");
        assert_eq!(persisted.runner_id, None);
    }

    let absent = client
        .delete(&delete_url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("repeat admin deletion for the missing-runner contract");
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
}

/// The admin route must not regress the transaction already used by runner
/// self-deregistration: refusing either write leaves both rows untouched.
async fn assert_admin_deletion_is_atomic(target: &str) {
    let (base, db) = spawn_test_app_with_db().await;
    let seeded = seed_job(&base, &db, &format!("admin-delete-{target}")).await;
    assert!(
        rg_db::ops::pipeline_ops::assign_job(&db, seeded.job_id, seeded.runner_id)
            .await
            .expect("assign job before failed admin deletion")
    );

    let trigger = match target {
        "reset" => format!(
            "CREATE TRIGGER break_admin_runner_job_reset\n\
             BEFORE UPDATE OF status ON pipeline_jobs\n\
             WHEN NEW.id = {} AND NEW.status = 'pending'\n\
             BEGIN\n\
               SELECT RAISE(ABORT, 'admin job reset refused');\n\
             END;",
            seeded.job_id
        ),
        "delete" => format!(
            "CREATE TRIGGER break_admin_runner_delete\n\
             BEFORE DELETE ON runners\n\
             WHEN OLD.id = {}\n\
             BEGIN\n\
               SELECT RAISE(ABORT, 'admin runner delete refused');\n\
             END;",
            seeded.runner_id
        ),
        other => panic!("unknown admin deletion fault target {other}"),
    };
    install_trigger(&db, &trigger).await;

    let token = admin_token(&base, &db, target).await;
    let response = reqwest::Client::new()
        .delete(format!("{base}/api/v1/admin/runners/{}", seeded.runner_id))
        .bearer_auth(token)
        .send()
        .await
        .expect("delete runner through the admin route");
    assert!(
        response.status().is_server_error(),
        "the {target} half of admin deletion failed but it returned {}",
        response.status()
    );

    assert!(
        runner_row(&db, seeded.runner_id).await.is_some(),
        "the runner was deleted although the {target} half never committed"
    );
    let persisted = rg_db::ops::pipeline_ops::get_job(&db, seeded.job_id)
        .await
        .expect("reload job after failed admin deletion")
        .expect("assigned job still exists");
    assert_eq!(persisted.status, "assigned");
    assert_eq!(persisted.runner_id, Some(seeded.runner_id));
}

#[tokio::test]
async fn admin_deletion_never_commits_one_half_of_its_two_writes() {
    assert_admin_deletion_is_atomic("reset").await;
    assert_admin_deletion_is_atomic("delete").await;
}
