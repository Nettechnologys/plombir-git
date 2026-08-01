use crate::common::{register_full, spawn_test_app_with_db};

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

#[tokio::test]
async fn private_pipeline_list_requires_read_access() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) = register_full(&base, "ci_owner", "ci_owner@example.com").await;
    let (other_token, _other_id) = register_full(&base, "ci_other", "ci_other@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "private-ci").await;

    rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        "0123456789012345678901234567890123456789",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .unwrap();

    let anon_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_owner/private-ci/pipelines",
            base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anon_resp.status(), 401);

    let other_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_owner/private-ci/pipelines",
            base
        ))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(other_resp.status(), 403);

    let owner_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_owner/private-ci/pipelines",
            base
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(owner_resp.status(), 200);
    let body: serde_json::Value = owner_resp.json().await.unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn cancel_pipeline_requires_write_access() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "ci_cancel_owner", "ci_cancel_owner@example.com").await;
    let (other_token, _other_id) =
        register_full(&base, "ci_cancel_other", "ci_cancel_other@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "private-cancel").await;

    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .unwrap();

    let other_resp = client
        .post(format!(
            "{}/api/v1/repos/ci_cancel_owner/private-cancel/pipelines/{}/cancel",
            base, pipeline.id
        ))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(other_resp.status(), 403);

    let owner_resp = client
        .post(format!(
            "{}/api/v1/repos/ci_cancel_owner/private-cancel/pipelines/{}/cancel",
            base, pipeline.id
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(owner_resp.status(), 200);
}

async fn seed_cancel_target(
    base: &str,
    db: &rg_db::DatabaseConnection,
    suffix: &str,
) -> (String, String, i64, i64, i64) {
    let owner = format!("ci_cancel_atomic_{suffix}");
    let email = format!("{owner}@example.com");
    let (token, _owner_id) = register_full(base, &owner, &email).await;
    let repo_name = format!("cancel-atomic-{suffix}");
    let repo_id = create_private_repo(base, &token, &repo_name).await;
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .expect("create cancellation pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create cancellation stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db,
        stage.id,
        "test-job",
        "echo test",
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
    .expect("create cancellation job");
    (
        token,
        format!(
            "{base}/api/v1/repos/{owner}/{repo_name}/pipelines/{}/cancel",
            pipeline.id
        ),
        pipeline.id,
        stage.id,
        job.id,
    )
}

async fn assert_cancel_rollback_on_child_update(target: &str) {
    use sea_orm::ConnectionTrait;

    let (base, db) = spawn_test_app_with_db().await;
    let (token, url, pipeline_id, stage_id, job_id) = seed_cancel_target(&base, &db, target).await;
    let (table, id, trigger) = match target {
        "stage" => ("pipeline_stages", stage_id, "abort_cancel_stage"),
        "job" => ("pipeline_jobs", job_id, "abort_cancel_job"),
        other => panic!("unknown cancellation update target {other}"),
    };
    db.execute_unprepared(&format!(
        "CREATE TRIGGER {trigger} BEFORE UPDATE OF status ON {table}\n\
         WHEN NEW.id = {id} AND NEW.status = 'canceled'\n\
         BEGIN SELECT RAISE(ABORT, 'faulted {target} cancellation'); END;"
    ))
    .await
    .expect("install cancellation fault trigger");

    let client = reqwest::Client::new();
    let failed = client
        .post(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("cancel pipeline through production router");
    assert!(
        failed.status().is_server_error(),
        "a failed {target} cancellation must not acknowledge success: {}",
        failed.status()
    );

    for (name, status) in [
        (
            "pipeline",
            rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline_id)
                .await
                .expect("reload pipeline after failed cancellation")
                .expect("pipeline remains after rollback")
                .status,
        ),
        (
            "stage",
            rg_db::ops::pipeline_ops::get_stage_by_id(&db, stage_id)
                .await
                .expect("reload stage after failed cancellation")
                .expect("stage remains after rollback")
                .status,
        ),
        (
            "job",
            rg_db::ops::pipeline_ops::get_job(&db, job_id)
                .await
                .expect("reload job after failed cancellation")
                .expect("job remains after rollback")
                .status,
        ),
    ] {
        assert_eq!(
            status, "pending",
            "{name} must be rolled back after {target} fault"
        );
    }

    db.execute_unprepared(&format!("DROP TRIGGER {trigger};"))
        .await
        .expect("remove cancellation fault trigger");
    let retry = client
        .post(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("retry cancellation through production router");
    assert_eq!(
        retry.status(),
        200,
        "retry after clearing the fault succeeds"
    );
    for (name, status) in [
        (
            "pipeline",
            rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline_id)
                .await
                .expect("reload canceled pipeline")
                .expect("pipeline exists")
                .status,
        ),
        (
            "stage",
            rg_db::ops::pipeline_ops::get_stage_by_id(&db, stage_id)
                .await
                .expect("reload canceled stage")
                .expect("stage exists")
                .status,
        ),
        (
            "job",
            rg_db::ops::pipeline_ops::get_job(&db, job_id)
                .await
                .expect("reload canceled job")
                .expect("job exists")
                .status,
        ),
    ] {
        assert_eq!(status, "canceled", "retry cancels the {name}");
    }
}

/// A cancellation response is a claim about the entire pipeline graph. Break a
/// child write after the root update and prove the routed request fails, rolls
/// every prior write back, and remains safely retryable once the database recovers.
#[tokio::test]
async fn cancel_pipeline_rolls_back_stage_and_job_update_failures() {
    assert_cancel_rollback_on_child_update("stage").await;
    assert_cancel_rollback_on_child_update("job").await;
}

/// The stage read sits after the pipeline update. Dropping only that table
/// therefore distinguishes the cancellation cascade from an earlier auth or
/// ownership failure and proves a read failure cannot be acknowledged as 200.
#[tokio::test]
async fn cancel_pipeline_does_not_acknowledge_stage_lookup_failure() {
    use sea_orm::ConnectionTrait;

    let (base, db) = spawn_test_app_with_db().await;
    let (token, url, pipeline_id, _stage_id, _job_id) =
        seed_cancel_target(&base, &db, "stage-read").await;
    db.execute_unprepared("PRAGMA foreign_keys = OFF;\nDROP TABLE pipeline_stages;")
        .await
        .expect("break only the stage lookup after the pipeline write");

    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("cancel pipeline through production router");
    assert!(
        response.status().is_server_error(),
        "a failed stage lookup must not acknowledge cancellation: {}",
        response.status()
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline_id)
            .await
            .expect("reload pipeline after failed stage lookup")
            .expect("pipeline remains after rollback")
            .status,
        "pending",
        "the root pipeline write must roll back when stage enumeration fails"
    );
}

#[tokio::test]
async fn manual_job_play_requires_write_access_and_is_atomic() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "ci_play_owner", "ci_play_owner@example.com").await;
    let (other_token, _other_id) =
        register_full(&base, "ci_play_other", "ci_play_other@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "private-play").await;
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "deploy", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        &db,
        stage.id,
        "production",
        "echo deploy",
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        Some("manual"),
        None,
    )
    .await
    .unwrap();
    rg_db::ops::pipeline_ops::update_stage_status(&db, stage.id, "manual", None, None)
        .await
        .unwrap();
    rg_db::ops::pipeline_ops::update_pipeline_status(&db, pipeline.id, "manual", None, None)
        .await
        .unwrap();
    let url = format!(
        "{base}/api/v1/repos/ci_play_owner/private-play/pipelines/{}/jobs/{}/play",
        pipeline.id, job.id
    );

    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&other_token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&owner_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&owner_token)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
}

#[tokio::test]
async fn protected_environment_requires_authorized_approval_before_release() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) = register_full(&base, "env_owner", "env_owner@example.com").await;
    let (other_token, _other_id) = register_full(&base, "env_other", "env_other@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "protected-deploy").await;
    let environment_response = client
        .post(format!(
            "{base}/api/v1/repos/env_owner/protected-deploy/actions/environments"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "production",
            "protected": true,
            "required_approvals": 1,
            "allowed_approver_ids": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(environment_response.status(), 201);
    let environment_id = environment_response
        .json::<serde_json::Value>()
        .await
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let environment = rg_db::ops::ci_environment_ops::find_by_id(&db, environment_id)
        .await
        .unwrap()
        .unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "deploy", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        &db,
        stage.id,
        "production",
        "echo deploy",
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
    rg_db::ops::ci_environment_ops::attach_job(&db, job.id, Some(&environment), "production")
        .await
        .unwrap();
    assert!(
        rg_db::ops::pipeline_ops::try_pause_stage_at_manual(&db, stage.id)
            .await
            .unwrap()
    );
    let approve_url = format!(
        "{base}/api/v1/repos/env_owner/protected-deploy/pipelines/{}/jobs/{}/approve",
        pipeline.id, job.id
    );

    assert_eq!(
        client
            .post(&approve_url)
            .bearer_auth(&other_token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let approved = client
        .post(&approve_url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), 200);
    assert!(
        approved.json::<serde_json::Value>().await.unwrap()["released"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
    assert_eq!(
        client
            .delete(format!(
                "{base}/api/v1/repos/env_owner/protected-deploy/actions/environments/{environment_id}"
            ))
            .bearer_auth(&owner_token)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
}
