//! Regression coverage for card_45ae7515860a: a write handler must not answer
//! `400` to every error its service returns.
//!
//! Thirty-five handlers across nine modules ended in
//! `Err(e) => AppError::bad_request(e)`, which stringified whatever came back —
//! a duplicate release tag and an unreachable database alike — into the one
//! status a client never retries. The distinction now travels *inside* the
//! error (`rg_core::error::InvalidRequest` / `NotFound` / `Conflict`), and the
//! handlers classify through `AppError::from`.
//!
//! Each test asserts both halves, because either one alone is trivially
//! satisfiable: a genuinely bad request is still a `400` (an endpoint that 500s
//! on everything fails the baseline), and a broken write path is a `5xx` (an
//! endpoint that kept the blanket `bad_request` fails the outage half).
//!
//! The outage is induced by dropping exactly the table the endpoint writes to,
//! so authentication, the repo resolution and every earlier lookup still
//! succeed and only the operation under test fails. Closing the pool would fail
//! authentication first and the assertions would pass unfixed.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
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

/// `POST .../boards` — the board service has no request-shaped branch at all,
/// so *every* `bad_request` it produced was a mislabelled server failure.
#[tokio::test]
async fn broken_board_create_is_not_reported_as_a_bad_request() {
    let (base, db, token, _repo_id) = app_with_repo("boardfail").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/boardfail-owner/boardfail-repo/boards");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "Roadmap"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: a healthy create still works");

    db.execute_unprepared("DROP TABLE boards")
        .await
        .expect("drop boards");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "Roadmap"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "board");
}

/// `POST /orgs` — the name/visibility rules are the caller's, the row insert is
/// ours, and both used to leave through the same `400`.
#[tokio::test]
async fn org_create_separates_a_rejected_name_from_a_broken_insert() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "orgfail-owner", "orgfail@example.com").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/orgs");

    // Genuinely the request's fault: visibility is not one of the two values.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "acme", "visibility": "sideways"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "an unknown visibility is still a bad request"
    );

    // …and so is a name that breaks the username rules.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "acme inc!", "visibility": "public"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "an invalid org name is still a 400");

    db.execute_unprepared("DROP TABLE organizations")
        .await
        .expect("drop organizations");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "acme", "visibility": "public"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "organization");
}

/// `POST .../releases` — a duplicate tag is the caller's (a `409` since
/// card_cfba32a77acd: the request is right, the tag that exists refuses it), a
/// dead `releases` table is ours.
#[tokio::test]
async fn release_create_separates_a_duplicate_tag_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("relfail").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/relfail-owner/relfail-repo/releases");
    let body = serde_json::json!({"tag_name": "v1.0.0", "title": "First"});

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: the first release is created");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        409,
        "a duplicate tag is refused by the release that holds it"
    );

    // An empty title is the other request-shaped branch of the same service.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"tag_name": "v2.0.0", "title": ""}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "an empty title is still a 400");

    db.execute_unprepared("DROP TABLE releases")
        .await
        .expect("drop releases");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"tag_name": "v3.0.0", "title": "Third"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "release");
}

/// `POST .../wiki` — a duplicate page title is the caller's (a `409` since
/// card_cfba32a77acd), a dead `wiki_pages` table is ours. Covers the
/// `resolve_repo_id` helper too: it used to swallow the lookup error with
/// `.ok().flatten()` and answer `404`.
#[tokio::test]
async fn wiki_create_separates_a_duplicate_title_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("wikifail").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/wikifail-owner/wikifail-repo/wiki");
    let body = serde_json::json!({"title": "Home", "content": "hello"});

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: the first page is created");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        409,
        "a duplicate wiki title is refused by the page that holds it"
    );

    // A page that genuinely is not there stays a 404, so the 5xx below cannot
    // be satisfied by an endpoint that lost its not-found branch.
    let resp = client
        .patch(format!("{url}/Absent"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"content": "x"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent wiki page is still a 404");

    db.execute_unprepared("DROP TABLE wiki_pages")
        .await
        .expect("drop wiki_pages");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "Other", "content": "hello"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "wiki page");
}

/// `POST .../hooks` — an internal target URL is the caller's, a dead `webhooks`
/// table is ours.
#[tokio::test]
async fn webhook_create_separates_a_rejected_url_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("hookfail").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/hookfail-owner/hookfail-repo/hooks");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": "http://127.0.0.1/hook", "events": ["push"]}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "a loopback webhook target is still refused as a bad request"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": "https://example.com/hook", "events": ["push"]}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: a public target is accepted");

    db.execute_unprepared("DROP TABLE webhooks")
        .await
        .expect("drop webhooks");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": "https://example.org/hook", "events": ["push"]}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "webhook");
}

/// `POST .../mirror` — a `file://` remote is the caller's; anything past the
/// URL check is ours.
///
/// This test used to induce its outage by doing nothing at all: the mirror
/// table was unreachable for *every* request, because the migration created
/// `mirror` while the entity read `mirrors` (card_d33afb82797f). With that
/// drift fixed the endpoint has a working baseline, so the outage is now
/// induced the same way as everywhere else in this file — by dropping exactly
/// the table the write needs.
#[tokio::test]
async fn mirror_create_separates_a_rejected_remote_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("mirrorfail").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/mirrorfail-owner/mirrorfail-repo/mirror");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": "file:///etc/passwd", "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "a file:// mirror remote is still refused as a bad request"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(
            &serde_json::json!({"url": "https://example.com/x.git", "sync_interval_seconds": 3600}),
        )
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        201,
        "baseline: a healthy mirror create still works"
    );

    db.execute_unprepared("DROP TABLE mirrors")
        .await
        .expect("drop mirrors");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(
            &serde_json::json!({"url": "https://example.com/y.git", "sync_interval_seconds": 3600}),
        )
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "mirror");
}

/// `POST .../time` — a non-positive duration is the caller's, a dead
/// `time_entries` table is ours.
#[tokio::test]
async fn time_entry_separates_a_bad_duration_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("timefail").await;
    let client = reqwest::Client::new();
    let (_issue_id, number) = crate::common::create_issue(
        &base,
        &token,
        "timefail-owner",
        "timefail-repo",
        "needs tracking",
    )
    .await;
    let url = format!("{base}/api/v1/repos/timefail-owner/timefail-repo/issues/{number}/time");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"duration_minutes": 0}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "a zero duration is still the caller's mistake"
    );

    db.execute_unprepared("DROP TABLE time_entries")
        .await
        .expect("drop time_entries");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"duration_minutes": 30}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "time entry");
}

/// `POST /imports` — the platform and source-URL checks are the caller's, the
/// task insert is ours. `start_import` has no request-shaped branch of its own,
/// so its blanket `400` could only ever have been a mislabelled failure.
#[tokio::test]
async fn import_start_separates_a_rejected_source_from_a_broken_insert() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "importfail-owner", "importfail@example.com").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/imports");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "platform": "bitbucket",
            "source_url": "https://example.com/x.git",
            "target_owner": "importfail-owner",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "an unknown platform is still a 400");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "platform": "git",
            "source_url": "file:///srv/repos/secret.git",
            "target_owner": "importfail-owner",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "a file:// source URL is still a 400");

    db.execute_unprepared("DROP TABLE import_tasks")
        .await
        .expect("drop import_tasks");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "platform": "git",
            "source_url": "https://example.com/x.git",
            "target_owner": "importfail-owner",
        }))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "import task");
}

/// `POST .../reviews` — an unknown review action is the caller's, a dead
/// `pr_reviews` table is ours.
#[tokio::test]
async fn review_submit_separates_a_bad_action_from_a_broken_insert() {
    let (base, db, token, repo_id) = app_with_repo("revfail").await;
    let client = reqwest::Client::new();
    let (_token2, author_id) =
        register_full(&base, "revfail-author", "revfail-author@example.com").await;
    insert_open_pr(&db, repo_id, author_id, 1).await;
    let url = format!("{base}/api/v1/repos/revfail-owner/revfail-repo/pulls/1/reviews");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"action": "shrug"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "an unknown review action is still the caller's mistake"
    );

    db.execute_unprepared("DROP TABLE pr_reviews")
        .await
        .expect("drop pr_reviews");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"action": "comment"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "review");
}

/// Reviewing a PR that is not open is a *state* problem: the same request
/// succeeds once it reopens, which is what `409` says and `400` does not.
#[tokio::test]
async fn reviewing_a_closed_pr_is_a_conflict_not_a_bad_request() {
    let (base, db, token, repo_id) = app_with_repo("closedpr").await;
    let client = reqwest::Client::new();
    let (_token2, author_id) =
        register_full(&base, "closedpr-author", "closedpr-author@example.com").await;
    insert_open_pr(&db, repo_id, author_id, 1).await;
    db.execute_unprepared("UPDATE pull_requests SET state = 'closed'")
        .await
        .expect("close pr");

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/closedpr-owner/closedpr-repo/pulls/1/reviews"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"action": "comment"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        409,
        "a closed PR is a state conflict, not a malformed request"
    );
}

async fn insert_open_pr(
    db: &sea_orm::DatabaseConnection,
    repo_id: i64,
    author_id: i64,
    number: i64,
) {
    use chrono::Utc;
    use sea_orm::Set;
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: Set(number),
            title: Set(format!("PR {number}")),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(author_id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some("0".repeat(40))),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            merged_at: Set(None),
            closed_at: Set(None),
        },
    )
    .await
    .expect("insert pr");
}
