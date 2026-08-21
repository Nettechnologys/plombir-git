//! card_8973329cd257: an allow-list column that does not decode is a broken
//! row, not an empty allow-list.
//!
//! Four sites read a stored JSON array with
//! `serde_json::from_str(..).ok().unwrap_or_default()`, so a column that failed
//! to decode arrived as `[]` — and `[]` is a meaningful value here, not a
//! neutral one. It went out in two directions:
//!
//!   * the gate told the client about the client — a listed approver got
//!     `403 user is not an allowed environment approver` for a row of ours that
//!     was broken;
//!   * the read told the operator the list was empty, and the obvious next move
//!     (`GET`, edit one field, write the object back) then made it empty for
//!     real.
//!
//! `NULL` still means "no allow-list configured" and must keep its behaviour —
//! each test below pins that half too, so the fix cannot be read as "any
//! absence is now an error".

use axum::http::StatusCode;
use sea_orm::ConnectionTrait;

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

/// Overwrite a stored allow-list with bytes that are valid UTF-8 and not a
/// JSON array of ids — the shape a half-written migration or a hand-edited row
/// actually leaves behind.
async fn corrupt(db: &rg_db::DatabaseConnection, table: &str, column: &str, id: i64) {
    db.execute_unprepared(&format!(
        "UPDATE {table} SET {column} = '{{\"7\": true}}' WHERE id = {id};"
    ))
    .await
    .expect("corrupt the stored allow-list");
}

#[tokio::test]
async fn an_undecodable_tag_allow_list_is_not_served_as_an_empty_one() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) =
        register_full(&base, "tagjson-owner", "tagjson-owner@example.test").await;
    create_repo(&base, &token, "releases").await;
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/v1/repos/tagjson-owner/releases/tags/protection");

    let created = client
        .post(&endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({"pattern": "v*", "allowed_user_ids": [user_id]}))
        .send()
        .await
        .expect("create the rule");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: serde_json::Value = created.json().await.expect("created rule body");
    let rule_id = created["id"].as_i64().expect("created rule carries an id");
    assert_eq!(created["allowed_user_ids"], serde_json::json!([user_id]));

    corrupt(&db, "protected_tags", "allowed_user_ids", rule_id).await;

    let listed = client
        .get(&endpoint)
        .bearer_auth(&token)
        .send()
        .await
        .expect("list the rules");
    assert_eq!(
        listed.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a rule whose allow-list cannot be read is a server fault, not a rule that admits nobody"
    );
    let body = listed.text().await.unwrap_or_default();
    assert!(
        !body.contains("\"allowed_user_ids\":[]"),
        "the unreadable list must not be handed back as an empty one, got: {body}"
    );
}

#[tokio::test]
async fn a_tag_rule_without_an_allow_list_still_reads_as_an_empty_one() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "tagnull-owner", "tagnull-owner@example.test").await;
    create_repo(&base, &token, "releases").await;
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/v1/repos/tagnull-owner/releases/tags/protection");

    // No `allowed_user_ids` at all — the column stays `NULL`, which is a
    // configured absence and has to keep its wire contract.
    let created = client
        .post(&endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({"pattern": "v*"}))
        .send()
        .await
        .expect("create the rule");
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(
        created.json::<serde_json::Value>().await.expect("body")["allowed_user_ids"],
        serde_json::json!([])
    );

    let listed = client
        .get(&endpoint)
        .bearer_auth(&token)
        .send()
        .await
        .expect("list the rules");
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(
        listed.json::<Vec<serde_json::Value>>().await.expect("body")[0]["allowed_user_ids"],
        serde_json::json!([])
    );
}

#[tokio::test]
async fn an_undecodable_approver_list_is_not_served_as_an_empty_one() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) =
        register_full(&base, "envjson-owner", "envjson-owner@example.test").await;
    create_repo(&base, &token, "deploys").await;
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/v1/repos/envjson-owner/deploys/actions/environments");

    let created = client
        .post(&endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "production",
            "protected": true,
            "required_approvals": 1,
            "allowed_approver_ids": [user_id],
        }))
        .send()
        .await
        .expect("create the environment");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: serde_json::Value = created.json().await.expect("created environment body");
    let environment_id = created["id"].as_i64().expect("environment carries an id");
    assert_eq!(
        created["allowed_approver_ids"],
        serde_json::json!([user_id])
    );

    corrupt(
        &db,
        "ci_environments",
        "allowed_approver_ids",
        environment_id,
    )
    .await;

    let listed = client
        .get(&endpoint)
        .bearer_auth(&token)
        .send()
        .await
        .expect("list the environments");
    assert_eq!(
        listed.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "an approver list that cannot be read must not be reported as an absent one"
    );
    let body = listed.text().await.unwrap_or_default();
    assert!(
        !body.contains("\"allowed_approver_ids\":[]"),
        "the unreadable list must not be handed back as an empty one, got: {body}"
    );
}

/// The gate half. A collaborator who is on the approver list — and is not an
/// admin, so the list is what decides — used to be told `403 user is not an
/// allowed environment approver` when the column stopped decoding: a statement
/// about them, made from a broken row of ours.
#[tokio::test]
async fn an_undecodable_approver_list_does_not_answer_the_approver_with_a_403() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _) =
        register_full(&base, "envgate-owner", "envgate-owner@example.test").await;
    let (approver_token, approver_id) =
        register_full(&base, "envgate-appr", "envgate-appr@example.test").await;
    let repo_id = create_repo(&base, &owner_token, "deploys").await;
    let client = reqwest::Client::new();

    let collaborator = client
        .post(format!(
            "{base}/api/v1/repos/envgate-owner/deploys/collaborators"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"username": "envgate-appr", "permission": "read"}))
        .send()
        .await
        .expect("add the collaborator");
    assert_eq!(collaborator.status(), StatusCode::CREATED);

    let created = client
        .post(format!(
            "{base}/api/v1/repos/envgate-owner/deploys/actions/environments"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "production",
            "protected": true,
            "required_approvals": 1,
            "allowed_approver_ids": [approver_id],
        }))
        .send()
        .await
        .expect("create the environment");
    assert_eq!(created.status(), StatusCode::CREATED);
    let environment_id = created.json::<serde_json::Value>().await.expect("body")["id"]
        .as_i64()
        .expect("environment carries an id");
    let environment = rg_db::ops::ci_environment_ops::find_by_id(&db, environment_id)
        .await
        .expect("load the environment")
        .expect("environment exists");

    // A pipeline parked on a job that is waiting for this environment.
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        "1111111111111111111111111111111111111111",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("create pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "deploy", 0)
        .await
        .expect("create stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        &db, stage.id, "deploy", "echo ok", None, None, None, None, None, None, false, None, None,
        None,
    )
    .await
    .expect("create job");
    rg_db::ops::ci_environment_ops::attach_job(&db, job.id, Some(&environment), "production")
        .await
        .expect("park the job on the protected environment");
    rg_db::ops::pipeline_ops::update_pipeline_status(
        &db,
        pipeline.id,
        "waiting_approval",
        None,
        None,
    )
    .await
    .expect("park the pipeline");

    let approve_url = format!(
        "{base}/api/v1/repos/envgate-owner/deploys/pipelines/{}/jobs/{}/approve",
        pipeline.id, job.id
    );

    // Baseline: the listed approver is admitted while the column is readable.
    let approved = client
        .post(&approve_url)
        .bearer_auth(&approver_token)
        .send()
        .await
        .expect("approve");
    assert_eq!(
        approved.status(),
        StatusCode::OK,
        "the approver is on the list, so the gate must let them through"
    );

    corrupt(
        &db,
        "ci_environments",
        "allowed_approver_ids",
        environment_id,
    )
    .await;

    // Second job, same environment, same approver — now with a broken column.
    let job = rg_db::ops::pipeline_ops::create_job(
        &db,
        stage.id,
        "deploy-again",
        "echo ok",
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
    .expect("create the second job");
    rg_db::ops::ci_environment_ops::attach_job(&db, job.id, Some(&environment), "production")
        .await
        .expect("park the second job");
    rg_db::ops::pipeline_ops::update_pipeline_status(
        &db,
        pipeline.id,
        "waiting_approval",
        None,
        None,
    )
    .await
    .expect("re-park the pipeline");

    let response = client
        .post(format!(
            "{base}/api/v1/repos/envgate-owner/deploys/pipelines/{}/jobs/{}/approve",
            pipeline.id, job.id
        ))
        .bearer_auth(&approver_token)
        .send()
        .await
        .expect("approve with a broken allow-list");
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a broken allow-list is our fault, not a statement about the approver"
    );
    let body = response.text().await.unwrap_or_default();
    assert!(
        !body.contains("is not an allowed environment approver"),
        "the refusal must not blame the approver for a row we could not read, got: {body}"
    );
}
