//! Routed acceptance for serialized authorization grants. The deletion runs
//! through the production admin endpoint; the before/after decisions run
//! through the real branch, tag, and protected-environment gates.

use rg_db::sea_orm::{ActiveValue::Set, EntityTrait};

use crate::common::{create_repo, register_full, spawn_test_app_with_state};

async fn promote_user_to_admin(db: &rg_db::DatabaseConnection, user_id: i64) {
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, Some(true), None)
        .await
        .expect("promote user to admin")
        .expect("registered user must exist");
}

async fn seed_push_grants(db: &rg_db::DatabaseConnection, repo_id: i64, user_id: i64) {
    let now = chrono::Utc::now();
    rg_db::ops::protected_branch_ops::create_with_push_grants(
        db,
        rg_db::entities::protected_branch::ActiveModel {
            repo_id: Set(repo_id),
            branch_name: Set("main".to_string()),
            require_pr: Set(true),
            require_status_check: Set(false),
            required_status_checks: Set(None),
            require_approval: Set(false),
            required_approvals: Set(None),
            allow_force_push: Set(false),
            require_signed_commits: Set(false),
            allowed_push_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![user_id]),
    )
    .await
    .expect("seed branch grant");
    rg_db::ops::protected_tag_ops::create_with_push_grants(
        db,
        rg_db::entities::protected_tag::ActiveModel {
            repo_id: Set(repo_id),
            pattern: Set("v*".to_string()),
            allowed_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![user_id]),
    )
    .await
    .expect("seed tag grant");
}

async fn seed_waiting_job(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    environment: &rg_db::entities::ci_environment::Model,
    marker: char,
) -> (i64, i64) {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        &marker.to_string().repeat(40),
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("create approval pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "deploy", 0)
        .await
        .expect("create approval stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db,
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
    .expect("create approval job");
    rg_db::ops::ci_environment_ops::attach_job(db, job.id, Some(environment), "production")
        .await
        .expect("attach protected environment");
    assert!(
        rg_db::ops::pipeline_ops::try_pause_stage_at_manual(db, stage.id)
            .await
            .expect("pause pipeline for environment approval")
    );
    (pipeline.id, job.id)
}

fn assert_push_grants_allow(
    branches: Vec<rg_db::ops::protected_branch_ops::Rule>,
    tags: Vec<rg_db::ops::protected_tag_ops::Rule>,
    user_id: i64,
) {
    assert!(
        rg_core::branch_protection::push_rules::branch_protection_rejected_refs(
            branches,
            Some(user_id)
        )
        .expect("evaluate branch gate")
        .is_empty(),
        "the fixture user was not allow-listed for direct branch push"
    );
    assert!(
        rg_core::branch_protection::push_rules::tag_protection_rejected_refs(tags, Some(user_id))
            .expect("evaluate tag gate")
            .is_empty(),
        "the fixture user was not allow-listed for protected tags"
    );
}

#[tokio::test]
async fn reused_numeric_id_does_not_inherit_deleted_accounts_serialized_grants() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "grant-admin", "grant-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;
    let (owner_token, _owner_id) =
        register_full(&base, "grant-owner", "grant-owner@example.com").await;
    let (victim_token, victim_id) =
        register_full(&base, "grant-victim", "grant-victim@example.com").await;
    let repo_id = create_repo(&base, &owner_token, "shared-grants").await;
    seed_push_grants(&db, repo_id, victim_id).await;

    let environment_response = client
        .post(format!(
            "{base}/api/v1/repos/grant-owner/shared-grants/actions/environments"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "production",
            "protected": true,
            "required_approvals": 1,
            "allowed_approver_ids": [victim_id]
        }))
        .send()
        .await
        .expect("create protected environment");
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

    assert_push_grants_allow(
        rg_db::ops::protected_branch_ops::list_rules_by_repo(&db, repo_id)
            .await
            .unwrap(),
        rg_db::ops::protected_tag_ops::list_rules_by_repo(&db, repo_id)
            .await
            .unwrap(),
        victim_id,
    );
    let (pipeline_id, job_id) = seed_waiting_job(&db, repo_id, &environment, 'a').await;
    let approve_url = format!(
        "{base}/api/v1/repos/grant-owner/shared-grants/pipelines/{pipeline_id}/jobs/{job_id}/approve"
    );
    assert_eq!(
        client
            .post(&approve_url)
            .bearer_auth(&victim_token)
            .send()
            .await
            .expect("approve before account deletion")
            .status(),
        200,
        "the live baseline did not exercise the environment allow-list"
    );

    let deleted = client
        .delete(format!("{base}/api/v1/admin/users/{victim_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("delete allow-listed account");
    assert_eq!(
        deleted.status(),
        200,
        "routed account deletion failed: {}",
        deleted.text().await.unwrap_or_default()
    );

    let now = chrono::Utc::now();
    rg_db::entities::user::Entity::insert(rg_db::entities::user::ActiveModel {
        id: Set(victim_id),
        username: Set("replacement-user".to_string()),
        email: Set("replacement-user@example.com".to_string()),
        password_hash: Set("x".to_string()),
        is_active: Set(true),
        is_admin: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    })
    .exec(&db)
    .await
    .expect("import replacement account with reused numeric id");
    let replacement_token = rg_core::auth::jwt::generate_token(
        victim_id,
        "replacement-user",
        0,
        state.jwt_secret.as_str(),
        1,
    )
    .expect("mint replacement user's test session");

    let branches = rg_db::ops::protected_branch_ops::list_rules_by_repo(&db, repo_id)
        .await
        .unwrap();
    let tags = rg_db::ops::protected_tag_ops::list_rules_by_repo(&db, repo_id)
        .await
        .unwrap();
    let environment = rg_db::ops::ci_environment_ops::find_by_id(&db, environment_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        branches[0].protection.allowed_push_user_ids.as_deref(),
        Some("[]")
    );
    assert_eq!(tags[0].protection.allowed_user_ids.as_deref(), Some("[]"));
    assert_eq!(environment.allowed_approver_ids.as_deref(), Some("[]"));
    assert_eq!(
        rg_core::branch_protection::push_rules::branch_protection_rejected_refs(
            branches,
            Some(victim_id)
        )
        .unwrap()
        .len(),
        1,
        "the replacement account inherited direct-push access"
    );
    assert_eq!(
        rg_core::branch_protection::push_rules::tag_protection_rejected_refs(tags, Some(victim_id))
            .unwrap()
            .len(),
        1,
        "the replacement account inherited protected-tag access"
    );

    let (pipeline_id, job_id) = seed_waiting_job(&db, repo_id, &environment, 'b').await;
    let denied = client
        .post(format!(
            "{base}/api/v1/repos/grant-owner/shared-grants/pipelines/{pipeline_id}/jobs/{job_id}/approve"
        ))
        .bearer_auth(&replacement_token)
        .send()
        .await
        .expect("try environment approval as replacement account");
    assert_eq!(
        denied.status(),
        403,
        "the replacement account inherited environment approval access"
    );
}
