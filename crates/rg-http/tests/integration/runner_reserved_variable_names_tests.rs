//! Reserved variable names never reach an external runner (security audit
//! finding #2).
//!
//! The runner reads every polled variable into the environment of its host
//! `docker` CLI (`docker run -e KEY` takes the value from there). Names such as
//! `LD_PRELOAD` or `DOCKER_HOST` therefore act on the runner host, not in the
//! container. The config validator refuses them on the way in; this drives the
//! two stores that can still hold them — a job row and a secret row written
//! before the rule existed — through the poll route and checks they are
//! stripped there, and that the secrets API refuses to write new ones.

use axum::http::StatusCode;

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

const OWNER: &str = "reserved-owner";
const REPO: &str = "reserved-names";

async fn put_secret(base: &str, owner_token: &str, name: &str) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/actions/secrets/{name}"
        ))
        .bearer_auth(owner_token)
        .json(&serde_json::json!({ "value": "secret-value" }))
        .send()
        .await
        .expect("write repository secret")
}

/// A secret row written straight to the store, as one created before the
/// name rule existed would have been.
async fn stored_secret(
    db: &rg_db::DatabaseConnection,
    encryption_key: &str,
    repo_id: i64,
    actor_id: i64,
    name: &str,
    value: &str,
) {
    let key = rg_core::auth::encryption::derive_key(encryption_key);
    let encrypted = rg_core::auth::encryption::encrypt(value, &key).expect("encrypt secret");
    rg_db::ops::ci_secret_ops::upsert(db, repo_id, name, &encrypted, actor_id)
        .await
        .expect("store secret")
        .expect("secret row written");
}

async fn pending_job(db: &rg_db::DatabaseConnection, repo_id: i64, variables: &str) -> i64 {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        &"a".repeat(40),
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("create pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create stage");
    rg_db::ops::pipeline_ops::create_job(
        db,
        stage.id,
        "legacy-variables",
        "echo scoped",
        None,
        Some(r#"["linux"]"#),
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
    .expect("create pending job")
    .id
}

#[tokio::test]
async fn reserved_variable_names_are_refused_as_secrets_and_stripped_from_polls() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    rg_db::ops::user_ops::update_by_id(&db, owner_id, None, None, Some(true), None)
        .await
        .expect("promote runner registrar")
        .expect("runner registrar exists");
    let repo_id = create_repo(&base, &owner_token, REPO).await;

    // The API refuses the names a secret may not take...
    for name in [
        "LD_PRELOAD",
        "DOCKER_HOST",
        "GODEBUG",
        "HTTPS_PROXY",
        "SSL_CERT_FILE",
        "PATH",
        "CI_JOB_TOKEN",
    ] {
        let response = put_secret(&base, &owner_token, name).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "secret {name} must be refused"
        );
    }
    // ...and accepts an ordinary one.
    assert_eq!(
        put_secret(&base, &owner_token, "DEPLOY_TOKEN")
            .await
            .status(),
        StatusCode::CREATED
    );

    // Rows the rule never saw: a secret and a job written before it existed.
    let encryption_key = crate::common::TEST_ENCRYPTION_KEY;
    stored_secret(
        &db,
        encryption_key,
        repo_id,
        owner_id,
        "DOCKER_HOST",
        "tcp://attacker:2375",
    )
    .await;
    let job_id = pending_job(
        &db,
        repo_id,
        r#"{"LD_PRELOAD":"/workspace/evil.so","GODEBUG":"http2debug=2","http_proxy":"http://attacker","CI_JOB_TOKEN":"forged","SAFE_VALUE":"first\nsecond"}"#,
    )
    .await;

    let registration = reqwest::Client::new()
        .post(format!("{base}/api/v1/runners/register"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "repository": format!("{OWNER}/{REPO}"),
            "name": "runner",
            "labels": ["linux"]
        }))
        .send()
        .await
        .expect("register runner")
        .json::<serde_json::Value>()
        .await
        .expect("registration body");
    let runner_id = registration["id"].as_i64().expect("runner id");
    let runner_token = registration["token"].as_str().expect("runner token");

    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/poll?timeout=1"
        ))
        .bearer_auth(runner_token)
        .send()
        .await
        .expect("poll runner job");
    let status = response.status();
    let body = response
        .json::<serde_json::Value>()
        .await
        .expect("poll body");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["job_id"], job_id);
    let variables = body["variables"]
        .as_object()
        .expect("poll body carries variables");
    for denied in ["LD_PRELOAD", "GODEBUG", "http_proxy", "DOCKER_HOST"] {
        assert!(
            !variables.contains_key(denied),
            "{denied} reached the runner: {body}"
        );
    }
    assert_eq!(variables["SAFE_VALUE"], "first\nsecond");
    assert_eq!(variables["DEPLOY_TOKEN"], "secret-value");
    assert_ne!(
        variables["CI_JOB_TOKEN"], "forged",
        "the job's own CI_JOB_TOKEN must not replace the one the server mints"
    );
}
