//! Second pass of the sweep in [`super::service_failure_status_tests`], for
//! card_a086f3421ee9 / card_10f706d87a6d.
//!
//! That sweep searched for the literal `AppError::bad_request(e)` and converted
//! 35 handlers. The `.to_string()` spelling of the same defect —
//! `Err(e) => AppError::bad_request(e.to_string())` wrapped around the *whole*
//! result of a service call — was invisible to it and survived in seven more
//! modules: issues, pulls, repos, collaborators, branch_protection, users and
//! admin. The consequence is identical: a dropped table, a dead connection pool
//! and a genuinely malformed request all left as `400`, so the client never
//! retried and the alerts stayed empty, with the `db: …` context chain shipped
//! in a body `IntoResponse` does not sanitize (H-05).
//!
//! Each test asserts both halves for the same reason as the file above: the
//! "still a 400" baseline fails an endpoint that now 500s on everything, and the
//! outage half fails an endpoint that kept the blanket `bad_request`.

use crate::common::{create_repo, register_full, register_user, spawn_test_app_with_db};
use sea_orm::ConnectionTrait;

/// The failure half: a broken write must be a 5xx carrying no internal detail.
fn assert_not_blamed_on_the_client(
    status: reqwest::StatusCode,
    body: &serde_json::Value,
    what: &str,
) {
    assert!(
        status.is_server_error(),
        "a broken {what} write must be a 5xx, not {status} — a 400 tells the \
         client to fix a request that was never wrong and is never retried \
         (body: {body})"
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("no such table"),
        "the {what} response body must not carry internal error detail, got: {message}"
    );
}

async fn app_with_repo(prefix: &str) -> (String, sea_orm::DatabaseConnection, String, i64) {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(
        &base,
        &format!("{prefix}-owner"),
        &format!("{prefix}@example.com"),
    )
    .await;
    let repo_id = create_repo(&base, &token, &format!("{prefix}-repo")).await;
    (base, db, token, repo_id)
}

/// `POST .../issues` — an empty title is the caller's, a dead `issues` table is
/// ours. Same split for `PATCH`, whose extra outcome is an issue that genuinely
/// is not there: that is a 404, and it used to be a 400 as well.
#[tokio::test]
async fn issue_create_separates_an_empty_title_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("issuefail").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/issuefail-owner/issuefail-repo/issues");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "   "}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "a blank issue title is still the caller's mistake"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "real work"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: a healthy create still works");

    // An issue number nobody ever used is a 404, not a bad request.
    let resp = client
        .patch(format!("{url}/4711"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "renamed"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent issue is a 404, not a 400");

    // An unknown target state is the request's fault and stays 400.
    let resp = client
        .patch(format!("{url}/1"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "sideways"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "an unknown issue state is still a 400");

    db.execute_unprepared("DROP TABLE issues")
        .await
        .expect("drop issues");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "after the outage"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "issue");
}

/// `POST .../pulls` — `head == base` is the caller's, a dead `pull_requests`
/// table is ours. Both used to leave through one `400`, and so did the second,
/// nested `bad_request` the handler had around `create_pr` itself.
#[tokio::test]
async fn pr_create_separates_an_identical_head_and_base_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("prfail").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/prfail-owner/prfail-repo/pulls");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "same branch", "head": "main", "base": "main"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "head == base is still the caller's mistake"
    );

    // An owner prefix naming a user that does not exist is the other
    // request-shaped branch, this one inside `resolve_head_ref`.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "from nowhere",
            "head": "ghost:feature",
            "base": "main",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "an unknown head owner is still a 400");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "real", "head": "feature", "base": "main"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: a healthy create still works");

    db.execute_unprepared("DROP TABLE pull_requests")
        .await
        .expect("drop pull_requests");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "after", "head": "other", "base": "main"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "pull request");
}

/// `POST .../collaborators` — an unknown permission and an already-listed user
/// are the caller's, the `repo_collaborators` insert is ours. The handler had
/// *two* blanket `bad_request`s: one on the service, one on the helper that
/// resolves `username`/`email` to an id through a database lookup.
#[tokio::test]
async fn add_collaborator_separates_a_rejected_permission_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("collabfail").await;
    let (_other_token, other_id) =
        register_full(&base, "collabfail-guest", "collabfail-guest@example.com").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/collabfail-owner/collabfail-repo/collaborators");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"user_id": other_id, "permission": "sideways"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "an unknown permission is still the caller's mistake"
    );

    // Naming nobody at all, and naming a user that does not exist, are the two
    // request-shaped branches of the resolve helper.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"permission": "read"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "a request naming no collaborator is still a 400"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"username": "nobody-here", "permission": "read"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "an unknown username is still a 400");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"user_id": other_id, "permission": "write"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: a healthy add still works");

    db.execute_unprepared("DROP TABLE repo_collaborators")
        .await
        .expect("drop repo_collaborators");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"user_id": other_id, "permission": "read"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "collaborator");
}

/// `POST /users/register` — a taken username and a weak password are the
/// caller's, a dead `users` table is ours. This is the worst instance of the
/// class in the sweep: an unauthenticated endpoint that answered "pick another
/// username" to a database outage, so an operator watching only 5xx rates saw a
/// completely broken registration path as perfectly healthy traffic.
#[tokio::test]
async fn register_separates_a_taken_username_from_a_broken_insert() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/users/register");

    register_user(&base, "regfail-first", "regfail-first@example.com", "Qz7$wRtm").await;

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "username": "regfail-first",
            "email": "regfail-other@example.com",
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "a taken username is still a 400");

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "username": "regfail-second",
            "email": "regfail-first@example.com",
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "a taken email is still a 400");

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "username": "regfail-third",
            "email": "not-an-address",
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "a malformed email is still a 400");

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "username": "regfail-fourth",
            "email": "regfail-fourth@example.com",
            "password": "short",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "a rejected password is still a 400");

    db.execute_unprepared("DROP TABLE users")
        .await
        .expect("drop users");

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "username": "regfail-fifth",
            "email": "regfail-fifth@example.com",
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "registration");
}

/// `POST .../branches/protection` — protecting an already-protected branch is
/// the caller's, the `protected_branches` insert is ours. The `PATCH`/`DELETE`
/// pair adds the third outcome: a rule that is not there is a 404, which is
/// what `get_protection` on the same routes already answered while its two
/// siblings called it a bad request.
#[tokio::test]
async fn branch_protection_separates_a_duplicate_rule_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("bpfail").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/bpfail-owner/bpfail-repo/branches/protection");
    let body = serde_json::json!({"branch_name": "main", "require_pr": true});

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: the first rule is created");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "protecting the same branch twice is still the caller's mistake"
    );

    let resp = client
        .patch(format!("{url}/999999"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"require_pr": false}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        404,
        "an absent protection rule is a 404, not a 400"
    );

    let resp = client
        .delete(format!("{url}/999999"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        404,
        "deleting an absent protection rule is a 404, not a 400"
    );

    db.execute_unprepared("DROP TABLE protected_branches")
        .await
        .expect("drop protected_branches");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"branch_name": "release", "require_pr": true}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "branch protection");
}

/// `POST .../statuses/{sha}` — an unknown status state is the caller's, the
/// `commit_statuses` upsert is ours. This route is what CI drives, so a `400`
/// on an outage is a build reported as failed-by-configuration.
#[tokio::test]
async fn commit_status_separates_a_bad_state_from_a_broken_upsert() {
    let (base, db, token, _repo_id) = app_with_repo("statusfail").await;
    let client = reqwest::Client::new();
    let sha = "0".repeat(40);
    let url = format!("{base}/api/v1/repos/statusfail-owner/statusfail-repo/statuses/{sha}");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "sideways", "context": "ci/build"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "an unknown commit status state is still the caller's mistake"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "success", "context": "ci/build"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: a healthy status is recorded");

    db.execute_unprepared("DROP TABLE commit_statuses")
        .await
        .expect("drop commit_statuses");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "failure", "context": "ci/build"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "commit status");
}
