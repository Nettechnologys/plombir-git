//! Regression coverage for card_4f69a21fc139: `?per_page=N` answered `400` on
//! every handler that pulls `PaginationParams` in through `#[serde(flatten)]`.
//!
//! `crates/rg-http/src/pagination.rs` opens with "All list endpoints accept
//! `page` and `per_page`", the parameter is declared, and the OpenAPI document
//! hands it to clients — but under `flatten` serde reads the inner struct via
//! `deserialize_any`, and `serde_urlencoded` only ever produces strings there.
//! A bare `u64` field was therefore unreachable: `per_page=2` came back as
//! `400 invalid type: string "2", expected u64`, and the default of 20 was the
//! only page size any of these five routes could ever serve.
//!
//! `page` had a custom string-parsing deserializer and worked, which is what
//! made the defect read like a broken client rather than a broken type.
//!
//! Unit coverage of the parsing itself lives next to the code, in
//! `pagination::tests`. What cannot be asserted there is that a real request
//! reaches a real handler and that the page it gets back is the size it asked
//! for — so there is one test per flattening route here, plus the two boundary
//! answers (`abc` and `1000`) that the fix must not have traded away.

use crate::common::{create_issue, create_repo, register_full, spawn_test_app_with_db};
use sea_orm::ActiveValue::{NotSet, Set};

/// `(data length, pagination.per_page, pagination.total)` of a list response.
async fn page_of(url: &str, token: &str) -> (usize, u64, u64) {
    let resp = reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .expect("send list request");
    let status = resp.status();
    let body = resp.text().await.expect("read list body");
    assert_eq!(status, 200, "GET {url} answered {status}: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("list body is JSON");
    (
        json["data"].as_array().expect("data array").len(),
        json["pagination"]["per_page"]
            .as_u64()
            .expect("pagination.per_page"),
        json["pagination"]["total"].as_u64().expect("total"),
    )
}

async fn status_of(url: &str, token: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .expect("send list request")
        .status()
}

async fn seed_pull_request(db: &rg_db::DatabaseConnection, repo_id: i64, author_id: i64, n: i64) {
    let now = chrono::Utc::now();
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(n),
            title: Set(format!("page size {n}")),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(author_id),
            reviewer_id: Set(None),
            head_branch: Set(format!("feature-{n}")),
            base_branch: Set("main".to_string()),
            head_sha: Set(None),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("seed pull request");
}

// ── One test per handler that flattens `PaginationParams` ───────────────

#[tokio::test]
async fn issues_serve_the_requested_page_size() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "pagesize-issues", "pagesize-issues@example.com").await;
    create_repo(&base, &token, "issue-pages").await;
    for n in 1..=3 {
        create_issue(
            &base,
            &token,
            "pagesize-issues",
            "issue-pages",
            &format!("issue {n}"),
        )
        .await;
    }

    let (len, per_page, total) = page_of(
        &format!("{base}/api/v1/repos/pagesize-issues/issue-pages/issues?per_page=2"),
        &token,
    )
    .await;
    assert_eq!(len, 2, "a page of 2 must carry 2 issues");
    assert_eq!(per_page, 2);
    assert_eq!(total, 3, "the page shrinks, the total does not");
}

#[tokio::test]
async fn pull_requests_serve_the_requested_page_size() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) =
        register_full(&base, "pagesize-pulls", "pagesize-pulls@example.com").await;
    let repo_id = create_repo(&base, &token, "pull-pages").await;
    for n in 1..=3 {
        seed_pull_request(&db, repo_id, user_id, n).await;
    }

    let (len, per_page, total) = page_of(
        &format!("{base}/api/v1/repos/pagesize-pulls/pull-pages/pulls?per_page=2"),
        &token,
    )
    .await;
    assert_eq!(len, 2, "a page of 2 must carry 2 pull requests");
    assert_eq!(per_page, 2);
    assert_eq!(total, 3);
}

#[tokio::test]
async fn notifications_serve_the_requested_page_size() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) =
        register_full(&base, "pagesize-notif", "pagesize-notif@example.com").await;
    for n in 1..=3 {
        rg_db::ops::notification_ops::create_notification(
            &db,
            user_id,
            "issue",
            &format!("notification {n}"),
            None,
            None,
        )
        .await
        .expect("seed notification");
    }

    let (len, per_page, total) =
        page_of(&format!("{base}/api/v1/notifications?per_page=2"), &token).await;
    assert_eq!(len, 2, "a page of 2 must carry 2 notifications");
    assert_eq!(per_page, 2);
    assert_eq!(total, 3);
}

#[tokio::test]
async fn pipelines_serve_the_requested_page_size() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "pagesize-ci", "pagesize-ci@example.com").await;
    let repo_id = create_repo(&base, &token, "pipeline-pages").await;
    for n in 1..=3 {
        rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            repo_id,
            &format!("{n}").repeat(40),
            "refs/heads/main",
            "manual",
            None,
        )
        .await
        .expect("seed pipeline");
    }

    let (len, per_page, total) = page_of(
        &format!("{base}/api/v1/repos/pagesize-ci/pipeline-pages/pipelines?per_page=2"),
        &token,
    )
    .await;
    assert_eq!(len, 2, "a page of 2 must carry 2 pipelines");
    assert_eq!(per_page, 2);
    assert_eq!(total, 3);
}

#[tokio::test]
async fn repository_listings_serve_the_requested_page_size() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "pagesize-repos", "pagesize-repos@example.com").await;
    for n in 1..=3 {
        create_repo(&base, &token, &format!("repo-pages-{n}")).await;
    }

    let (len, per_page, total) = page_of(
        &format!("{base}/api/v1/repos/pagesize-repos?per_page=2"),
        &token,
    )
    .await;
    assert_eq!(len, 2, "a page of 2 must carry 2 repositories");
    assert_eq!(per_page, 2);
    assert_eq!(total, 3);
}

// ── The two boundaries the fix must not have traded away ────────────────

/// Parsing the string ourselves must not turn junk into a default: a caller who
/// asked for `per_page=abc` is told so, rather than silently served 20 items.
#[tokio::test]
async fn a_non_numeric_page_size_is_still_a_bad_request() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "pagesize-junk", "pagesize-junk@example.com").await;
    create_repo(&base, &token, "junk-pages").await;

    for route in [
        format!("{base}/api/v1/repos/pagesize-junk/junk-pages/issues?per_page=abc"),
        format!("{base}/api/v1/repos/pagesize-junk/junk-pages/pulls?per_page=abc"),
        format!("{base}/api/v1/repos/pagesize-junk/junk-pages/pipelines?per_page=abc"),
        format!("{base}/api/v1/repos/pagesize-junk?per_page=abc"),
        format!("{base}/api/v1/notifications?per_page=abc"),
    ] {
        assert_eq!(
            status_of(&route, &token).await,
            400,
            "a non-numeric page size must stay a bad request: {route}"
        );
    }
}

/// A page size above the cap is a request the server can honour approximately,
/// so it is clamped rather than refused — and the response says which size it
/// actually served, because that is the number a client pages with.
#[tokio::test]
async fn an_oversized_page_size_is_clamped_rather_than_rejected() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "pagesize-cap", "pagesize-cap@example.com").await;
    create_repo(&base, &token, "cap-pages").await;

    for route in [
        format!("{base}/api/v1/repos/pagesize-cap/cap-pages/issues?per_page=1000"),
        format!("{base}/api/v1/repos/pagesize-cap/cap-pages/pulls?per_page=1000"),
        format!("{base}/api/v1/repos/pagesize-cap/cap-pages/pipelines?per_page=1000"),
        format!("{base}/api/v1/repos/pagesize-cap?per_page=1000"),
        format!("{base}/api/v1/notifications?per_page=1000"),
    ] {
        let (_len, per_page, _total) = page_of(&route, &token).await;
        assert_eq!(
            per_page, 100,
            "page size must be clamped to the cap: {route}"
        );
    }

    // Zero is the other end of the same range, and the one that matters most:
    // `PaginationMeta::from_params` divides `total` by `per_page`.
    let (_len, per_page, _total) = page_of(
        &format!("{base}/api/v1/repos/pagesize-cap/cap-pages/issues?per_page=0"),
        &token,
    )
    .await;
    assert_eq!(per_page, 1, "zero must be clamped, not divided by");
}

// ── An absurd page NUMBER, not page size (card_8973efdcbea5) ────────────
//
// `page` is a bare `u64` off the query string and `offset()` multiplied it out
// raw. Under `overflow-checks` — every test and debug build — the handler
// panicked mid-request and the client got a dropped connection with no status
// at all; in release the product wrapped, and `?page=4611686018427387905` came
// back `200` carrying the *first* page's rows beside a `pagination.page` that
// said it was somewhere else entirely. The unit tests next to `offset()` pin the
// arithmetic; what they cannot show is that a real request now gets an answer.

/// The release-build symptom, asserted where it was visible: this page number
/// times `per_page=100` wraps to an offset of exactly 0.
#[tokio::test]
async fn a_page_number_that_wraps_the_offset_does_not_serve_the_first_page() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "pagenum-wrap", "pagenum-wrap@example.com").await;
    create_repo(&base, &token, "wrap-pages").await;
    for n in 1..=3 {
        create_issue(
            &base,
            &token,
            "pagenum-wrap",
            "wrap-pages",
            &format!("issue {n}"),
        )
        .await;
    }

    let url = format!(
        "{base}/api/v1/repos/pagenum-wrap/wrap-pages/issues?page=4611686018427387905&per_page=100"
    );
    let (len, _per_page, total) = page_of(&url, &token).await;
    assert_eq!(
        len, 0,
        "a page past the end must be empty, not the first page's rows"
    );
    assert_eq!(total, 3, "the total still describes the whole listing");
}

/// The debug-build symptom: the largest page number there is must produce a
/// status line, not a transport error. `status_of` panics on a dropped
/// connection, which is precisely the failure being guarded.
#[tokio::test]
async fn the_largest_page_number_gets_an_http_status_rather_than_a_dropped_connection() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _id) = register_full(&base, "pagenum-max", "pagenum-max@example.com").await;
    create_repo(&base, &token, "max-pages").await;

    for route in [
        format!("{base}/api/v1/repos/pagenum-max/max-pages/issues?page=18446744073709551615"),
        format!("{base}/api/v1/repos/pagenum-max/max-pages/pulls?page=18446744073709551615"),
        format!("{base}/api/v1/repos/pagenum-max/max-pages/pipelines?page=18446744073709551615"),
        format!("{base}/api/v1/repos/pagenum-max?page=18446744073709551615"),
        format!("{base}/api/v1/notifications?page=18446744073709551615"),
        format!("{base}/api/v1/repos/explore?page=18446744073709551615"),
    ] {
        assert_eq!(
            status_of(&route, &token).await,
            200,
            "an absurd page number must be answered, not aborted: {route}"
        );
    }
}

/// The audit-log handlers build their own paginator instead of going through
/// `PaginationParams`, and hand the index to sea_orm, which multiplies it by the
/// page size with no check of its own — the same overflow one layer down.
#[tokio::test]
async fn the_admin_audit_listings_survive_an_absurd_page_number() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, id) = register_full(&base, "pagenum-audit", "pagenum-audit@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, id, None, None, Some(true), None)
        .await
        .expect("promote to instance admin")
        .expect("registered user must exist");

    for route in [
        format!("{base}/api/v1/admin/audit/logs?page=18446744073709551615&per_page=100"),
        format!("{base}/api/v1/admin/login-attempts?page=18446744073709551615&per_page=100"),
    ] {
        assert_eq!(
            status_of(&route, &token).await,
            200,
            "an absurd page number must be answered, not aborted: {route}"
        );
    }
}
