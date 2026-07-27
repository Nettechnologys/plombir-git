//! card_2e259792f27c: the import API used to hand the user their own
//! GitHub/GitLab PAT back on every call — `import_task::Model` was serialized
//! wholesale, `auth_token_encrypted` and all — and the column held the token
//! exactly as it arrived, the `_encrypted` suffix notwithstanding. The progress
//! page polls `GET /imports/{id}` in a loop, so the token was replayed into
//! devtools, HAR dumps and proxy caches for the whole length of an import, and
//! it outlived it in the database forever.
//!
//! There is no cross-user leak to prove here (all three handlers scope by
//! `task.user_id`); what these tests hold down is that the token does not come
//! back to its *own* owner and is not kept at rest.

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

use crate::common::{register_full, spawn_test_app_with_db};

/// The PAT the import is started with. Distinctive enough that finding it
/// anywhere in a response body is unambiguous.
const SOURCE_TOKEN: &str = "ghp_ZfQ4importtokenmustnotcomeback9182";

/// `.invalid` never resolves (RFC 6761), so the background worker fails fast on
/// the SSRF/DNS guard instead of reaching out to a real host.
const SOURCE_URL: &str = "https://example.invalid/octo/widgets.git";

fn body_carries_the_token(body: &serde_json::Value) -> bool {
    body.to_string().contains(SOURCE_TOKEN)
}

#[tokio::test]
async fn an_import_never_returns_the_source_token_to_its_owner() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) =
        register_full(&base, "import-secrets", "import-secrets@example.com").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/v1/imports"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "platform": "git",
            "source_url": SOURCE_URL,
            "target_owner": "import-secrets",
            "target_name": "widgets",
            "auth_token": SOURCE_TOKEN,
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: the import is accepted");
    let created: serde_json::Value = resp.json().await.expect("json body");

    // Baseline in the same test: the response is a real task, not an error we
    // are trivially finding no token in.
    let task_id = created["id"]
        .as_i64()
        .expect("the created task carries an id");
    assert_eq!(created["status"], "pending");
    assert_eq!(created["progress"], 0);
    assert_eq!(created["source_url"], SOURCE_URL);
    assert!(
        !body_carries_the_token(&created),
        "POST /imports handed the token straight back: {created}"
    );

    // The status endpoint the progress page polls.
    let status: serde_json::Value = client
        .get(format!("{base}/api/v1/imports/{task_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert_eq!(status["id"], task_id, "baseline: the owner still reads it");
    assert!(status["status"].is_string());
    assert!(
        !body_carries_the_token(&status),
        "GET /imports/{{id}} replays the token on every poll: {status}"
    );

    // ...and the list view.
    let listed: serde_json::Value = client
        .get(format!("{base}/api/v1/imports"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    assert_eq!(
        listed.as_array().map(Vec::len),
        Some(1),
        "baseline: the task is listed"
    );
    assert!(
        !body_carries_the_token(&listed),
        "GET /imports carries the token: {listed}"
    );

    // At rest: the column exists (rollback safety) but nothing writes it.
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT COUNT(*) AS n FROM import_tasks WHERE auth_token_encrypted IS NOT NULL"
                .to_string(),
        ))
        .await
        .expect("query")
        .expect("count row");
    assert_eq!(
        row.try_get::<i64>("", "n").expect("count"),
        0,
        "the token was stored on the task row"
    );
}

/// The same guarantee across the *whole* life of an import, not just the moment
/// it is created: the progress page polls until the task is terminal, and the
/// row picks up `stage`, `error` and `stats` along the way.
///
/// This source fails on the DNS guard, before the token reaches `git`, so the
/// masking that keeps a credential out of a platform's own error text is proven
/// where it lives — `rg_core::import::service`'s `failure_reason` unit tests.
/// What this holds down is the shape of the tract: whatever state the task is
/// observed in, the response carries no token.
#[tokio::test]
async fn a_failed_import_does_not_report_the_token_in_its_error() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "import-failure", "import-fail@example.com").await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(format!("{base}/api/v1/imports"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "platform": "github",
            "source_url": SOURCE_URL,
            "target_owner": "import-failure",
            "target_name": "widgets",
            "auth_token": SOURCE_TOKEN,
        }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    let task_id = created["id"]
        .as_i64()
        .expect("the created task carries an id");

    let mut last = created;
    for _ in 0..100 {
        assert!(
            !body_carries_the_token(&last),
            "an import status carried the token: {last}"
        );
        if last["status"] == "failed" || last["status"] == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        last = client
            .get(format!("{base}/api/v1/imports/{task_id}"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("request")
            .json()
            .await
            .expect("json body");
    }

    // An unresolvable host fails the guard well inside the polling window; if it
    // somehow did not, the loop above still checked 100 bodies for the token.
    assert_eq!(
        last["status"], "failed",
        "expected the unresolvable source to fail the import: {last}"
    );
    let error = last["error"].as_str().expect("a failed import states why");
    assert!(
        !error.contains(SOURCE_TOKEN),
        "the token came back inside the failure reason: {error}"
    );
    assert!(!error.is_empty(), "the reason must still say something");
}
