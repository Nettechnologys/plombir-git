use crate::common::{register_full, spawn_test_app_with_db};

async fn assert_ci_graph_status(
    db: &rg_db::DatabaseConnection,
    job_id: i64,
    stage_id: i64,
    pipeline_id: i64,
    expected: &str,
) {
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(db, job_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        expected
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_stage_by_id(db, stage_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        expected
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_pipeline(db, pipeline_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        expected
    );
}

async fn assert_ci_parent_finished_at(
    db: &rg_db::DatabaseConnection,
    stage_id: i64,
    pipeline_id: i64,
    expected: Option<chrono::NaiveDateTime>,
) {
    assert_eq!(
        rg_db::ops::pipeline_ops::get_stage_by_id(db, stage_id)
            .await
            .unwrap()
            .unwrap()
            .finished_at,
        expected
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_pipeline(db, pipeline_id)
            .await
            .unwrap()
            .unwrap()
            .finished_at,
        expected
    );
}

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
    use sea_orm::ConnectionTrait;

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
        None,
        false,
        None,
        Some("manual"),
        None,
    )
    .await
    .unwrap();
    let old_finished_at = Some(chrono::Utc::now().naive_utc());
    rg_db::ops::pipeline_ops::update_stage_status(&db, stage.id, "manual", None, old_finished_at)
        .await
        .unwrap();
    rg_db::ops::pipeline_ops::update_pipeline_status(
        &db,
        pipeline.id,
        "manual",
        None,
        old_finished_at,
    )
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
    db.execute_unprepared(&format!(
        "CREATE TRIGGER break_manual_stage_resume BEFORE UPDATE OF status ON pipeline_stages\n\
         WHEN NEW.id = {} AND NEW.status = 'pending'\n\
         BEGIN SELECT RAISE(ABORT, 'faulted manual stage resume'); END;",
        stage.id
    ))
    .await
    .expect("install manual release fault after the job write");
    assert!(client
        .post(&url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .status()
        .is_server_error());
    assert_ci_graph_status(&db, job.id, stage.id, pipeline.id, "manual").await;
    assert_ci_parent_finished_at(&db, stage.id, pipeline.id, old_finished_at).await;
    db.execute_unprepared("DROP TRIGGER break_manual_stage_resume;")
        .await
        .expect("remove manual release fault");
    db.execute_unprepared(&format!(
        "CREATE TRIGGER break_manual_pipeline_resume BEFORE UPDATE OF status ON pipelines\n\
         WHEN NEW.id = {} AND NEW.status = 'pending'\n\
         BEGIN SELECT RAISE(ABORT, 'faulted manual pipeline resume'); END;",
        pipeline.id
    ))
    .await
    .expect("install manual pipeline fault after the job and stage writes");
    assert!(client
        .post(&url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .status()
        .is_server_error());
    assert_ci_graph_status(&db, job.id, stage.id, pipeline.id, "manual").await;
    assert_ci_parent_finished_at(&db, stage.id, pipeline.id, old_finished_at).await;
    db.execute_unprepared("DROP TRIGGER break_manual_pipeline_resume;")
        .await
        .expect("remove manual pipeline fault");
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
        // card_8e5d79bdb8bd: the second release is a well-formed request that
        // arrived after the state moved — 409, not "you sent rubbish".
        409
    );
    assert_ci_graph_status(&db, job.id, stage.id, pipeline.id, "pending").await;
    assert_ci_parent_finished_at(&db, stage.id, pipeline.id, None).await;
}

#[tokio::test]
async fn protected_environment_requires_authorized_approval_before_release() {
    use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, owner_id) = register_full(&base, "env_owner", "env_owner@example.com").await;
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
            "allowed_approver_ids": [owner_id]
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

    let bot = client
        .post(format!("{base}/api/v1/users/bots"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "username": "deploy-bot" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bot.status(), 201, "{}", bot.text().await.unwrap());
    let bot_id = bot.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    // The same list as the live release below becomes impossible when its
    // second human is replaced by a bot: a bot's vote cannot satisfy the gate.
    for (method, url, name) in [
        (
            "POST",
            format!("{base}/api/v1/repos/env_owner/protected-deploy/actions/environments"),
            "unreleasable",
        ),
        (
            "PUT",
            format!(
                "{base}/api/v1/repos/env_owner/protected-deploy/actions/environments/{environment_id}"
            ),
            "production",
        ),
    ] {
        let rejected = client
            .request(method.parse().unwrap(), url)
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({
                "name": name,
                "protected": true,
                "required_approvals": 2,
                "allowed_approver_ids": [owner_id, bot_id]
            }))
            .send()
            .await
            .unwrap();
        let status = rejected.status();
        let body: serde_json::Value = rejected.json().await.unwrap();
        assert_eq!(status, 400, "{body}");
        assert_eq!(
            body["error"]["message"],
            format!("CI environment approver {bot_id} must be a human account")
        );
    }
    assert_eq!(
        rg_db::ops::ci_environment_ops::list(&db, repo_id)
            .await
            .unwrap()
            .len(),
        1,
        "the invalid POST must not leave a second environment"
    );
    let unchanged = rg_db::ops::ci_environment_ops::find_by_id(&db, environment_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.required_approvals, 1);
    assert_eq!(
        rg_db::ops::ci_environment_ops::allowed_approver_ids(&db, &unchanged)
            .await
            .unwrap(),
        vec![owner_id],
        "the invalid PUT must leave the human approval path intact"
    );
    let collaborator = client
        .post(format!(
            "{base}/api/v1/repos/env_owner/protected-deploy/collaborators"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "username": "deploy-bot", "permission": "admin" }))
        .send()
        .await
        .unwrap();
    assert!(
        collaborator.status().is_success(),
        "{}",
        collaborator.text().await.unwrap()
    );
    let bot_token = client
        .post(format!("{base}/api/v1/users/bots/deploy-bot/tokens"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "name": "deploy" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        bot_token.status(),
        201,
        "{}",
        bot_token.text().await.unwrap()
    );
    let bot_token = bot_token.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let denied = client
        .post(&approve_url)
        .bearer_auth(&bot_token)
        .send()
        .await
        .unwrap();
    let denied_status = denied.status();
    let denied_body: serde_json::Value = denied.json().await.unwrap();
    assert_eq!(denied_status, 403, "{denied_body}");
    assert_eq!(denied_body["error"]["code"], "FORBIDDEN");
    let denials = rg_db::entities::audit_log::Entity::find()
        .filter(rg_db::entities::audit_log::Column::Action.eq("agent.scope_denied"))
        .all(&db)
        .await
        .unwrap();
    assert!(denials.iter().any(|row| {
        row.user_id == Some(bot_id)
            && row.details.as_deref().is_some_and(|details| {
                let details: serde_json::Value = serde_json::from_str(details).unwrap();
                details["reason"] == "human_approval_required"
                    && details["action"] == "environment_deploy"
                    && details["token_id"].is_number()
            })
    }));
    // An approval left by an older server remains history, not a release vote.
    rg_db::ops::ci_environment_ops::add_approval(&db, job.id, environment_id, bot_id)
        .await
        .unwrap();
    assert_eq!(
        rg_db::ops::ci_environment_ops::live_approver_ids(&db, job.id)
            .await
            .unwrap()
            .len(),
        0
    );
    assert_ci_graph_status(&db, job.id, stage.id, pipeline.id, "waiting_approval").await;

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
    db.execute_unprepared(&format!(
        "CREATE TRIGGER break_approval_stage_resume BEFORE UPDATE OF status ON pipeline_stages\n\
         WHEN NEW.id = {} AND NEW.status = 'pending'\n\
         BEGIN SELECT RAISE(ABORT, 'faulted approval stage resume'); END;",
        stage.id
    ))
    .await
    .expect("install approval release fault after the job write");
    assert!(client
        .post(&approve_url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .status()
        .is_server_error());
    assert_ci_graph_status(&db, job.id, stage.id, pipeline.id, "waiting_approval").await;
    db.execute_unprepared("DROP TRIGGER break_approval_stage_resume;")
        .await
        .expect("remove approval release fault");
    db.execute_unprepared(&format!(
        "CREATE TRIGGER break_approval_pipeline_resume BEFORE UPDATE OF status ON pipelines\n\
         WHEN NEW.id = {} AND NEW.status = 'pending'\n\
         BEGIN SELECT RAISE(ABORT, 'faulted approval pipeline resume'); END;",
        pipeline.id
    ))
    .await
    .expect("install approval pipeline fault after the job and stage writes");
    assert!(client
        .post(&approve_url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .status()
        .is_server_error());
    assert_ci_graph_status(&db, job.id, stage.id, pipeline.id, "waiting_approval").await;
    db.execute_unprepared("DROP TRIGGER break_approval_pipeline_resume;")
        .await
        .expect("remove approval pipeline fault");
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
    assert_ci_graph_status(&db, job.id, stage.id, pipeline.id, "pending").await;
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

/// A pipeline paused on one job bound for `environment`, ready to be approved.
async fn waiting_protected_job(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    environment: &rg_db::entities::ci_environment::Model,
    commit: char,
) -> (i64, i64, i64) {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        &commit.to_string().repeat(40),
        "refs/heads/main",
        "push",
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
        &environment.name,
        "echo deploy",
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
    rg_db::ops::ci_environment_ops::attach_job(db, job.id, Some(environment), &environment.name)
        .await
        .unwrap();
    assert!(
        rg_db::ops::pipeline_ops::try_pause_stage_at_manual(db, stage.id)
            .await
            .unwrap()
    );
    (pipeline.id, stage.id, job.id)
}

/// card_aa1d374901e3: an approval row records that somebody *was* allowed to
/// approve. Taking the right away — off the allow-list, or out of the
/// administration that approves without one — must take away the vote still
/// waiting for the threshold, or the revoked approver and one newcomer release
/// a deployment neither could alone. The rows stay as history.
#[tokio::test]
async fn a_revoked_approver_no_longer_counts_toward_a_protected_release() {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _) = register_full(&base, "rv_owner", "rv_owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "rv-deploy").await;
    let repo_url = format!("{base}/api/v1/repos/rv_owner/rv-deploy");
    let mut people = Vec::new();
    for (name, permission) in [
        ("rv_first", "read"),
        ("rv_second", "read"),
        ("rv_third", "read"),
        ("rv_admin", "admin"),
    ] {
        let (token, id) = register_full(&base, name, &format!("{name}@example.com")).await;
        let added = client
            .post(format!("{repo_url}/collaborators"))
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({ "username": name, "permission": permission }))
            .send()
            .await
            .unwrap();
        let status = added.status();
        let row: serde_json::Value = added.json().await.unwrap();
        assert!(status.is_success(), "{row}");
        // The permission route takes the collaborator row's id, not the user's.
        people.push((token, id, row["id"].as_i64().unwrap()));
    }
    let [(first, first_id, _), (second, second_id, _), (third, third_id, _), (admin, _, admin_row)] =
        <[_; 4]>::try_from(people).unwrap();
    let environment_url = format!("{repo_url}/actions/environments");
    let created = client
        .post(&environment_url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "production",
            "protected": true,
            "required_approvals": 2,
            "allowed_approver_ids": [first_id, second_id, third_id],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let environment_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let environment = rg_db::ops::ci_environment_ops::find_by_id(&db, environment_id)
        .await
        .unwrap()
        .unwrap();
    let approve = |token: &str, pipeline_id: i64, job_id: i64| {
        client
            .post(format!(
                "{repo_url}/pipelines/{pipeline_id}/jobs/{job_id}/approve"
            ))
            .bearer_auth(token.to_string())
            .send()
    };
    let verdict = |response: reqwest::Response| async move {
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(status, 200, "{body}");
        (
            body["approvals"].as_u64().unwrap(),
            body["released"].as_bool().unwrap(),
        )
    };

    // Baseline: before any revocation, the first approver's vote and the
    // admin's each count, and two of them release.
    let (pipeline, stage, job) = waiting_protected_job(&db, repo_id, &environment, 'a').await;
    assert_eq!(
        verdict(approve(&first, pipeline, job).await.unwrap()).await,
        (1, false)
    );
    assert_eq!(
        verdict(approve(&admin, pipeline, job).await.unwrap()).await,
        (2, true)
    );
    assert_ci_graph_status(&db, job, stage, pipeline, "pending").await;

    // Off the allow-list after voting: the vote stays in history and stops
    // counting, so the second approver alone does not release.
    let (pipeline, stage, job) = waiting_protected_job(&db, repo_id, &environment, 'b').await;
    assert_eq!(
        verdict(approve(&first, pipeline, job).await.unwrap()).await,
        (1, false)
    );
    let narrowed = client
        .put(format!("{environment_url}/{environment_id}"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "production",
            "protected": true,
            "required_approvals": 2,
            "allowed_approver_ids": [second_id, third_id],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(narrowed.status(), 200, "{}", narrowed.text().await.unwrap());
    assert_eq!(
        verdict(approve(&second, pipeline, job).await.unwrap()).await,
        (1, false)
    );
    assert_ci_graph_status(&db, job, stage, pipeline, "waiting_approval").await;
    let history = rg_db::entities::ci_environment_approval::Entity::find()
        .filter(rg_db::entities::ci_environment_approval::Column::JobId.eq(job))
        .all(&db)
        .await
        .unwrap();
    assert!(
        history.iter().any(|row| row.approved_by == Some(first_id)),
        "the revoked vote stays as history"
    );
    // The people still authorized meet the threshold themselves.
    assert_eq!(
        verdict(approve(&third, pipeline, job).await.unwrap()).await,
        (2, true)
    );
    assert_ci_graph_status(&db, job, stage, pipeline, "pending").await;

    // An administrator approves without the allow-list; losing administration
    // after voting drops the vote the same way.
    let (pipeline, stage, job) = waiting_protected_job(&db, repo_id, &environment, 'c').await;
    assert_eq!(
        verdict(approve(&admin, pipeline, job).await.unwrap()).await,
        (1, false)
    );
    let demoted = client
        .patch(format!("{repo_url}/collaborators/{admin_row}"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "permission": "read" }))
        .send()
        .await
        .unwrap();
    assert!(
        demoted.status().is_success(),
        "{}",
        demoted.text().await.unwrap()
    );
    assert_eq!(
        verdict(approve(&second, pipeline, job).await.unwrap()).await,
        (1, false)
    );
    assert_ci_graph_status(&db, job, stage, pipeline, "waiting_approval").await;
    assert_eq!(
        verdict(approve(&third, pipeline, job).await.unwrap()).await,
        (2, true)
    );
    assert_ci_graph_status(&db, job, stage, pipeline, "pending").await;
}
