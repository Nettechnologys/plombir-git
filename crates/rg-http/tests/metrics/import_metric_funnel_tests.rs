//! card_4f2a72c62d95: an imported tracker has to reach the business counters.
//!
//! `plombir_git_issues_opened_total` and `plombir_git_prs_opened_total` each had a
//! single producer, and both sat in a REST handler. The import subsystem builds
//! its `issue` / `pull_request` rows through the same repository-local number
//! allocators the handlers' services use, but it owns no handler of its own —
//! so migrating a tracker with four hundred issues moved the counters by
//! nothing, and `prs_merged_total` never saw a merged MR at all because an
//! import writes `state = "merged"` in one insert instead of going through
//! `merge_pr`.
//!
//! The rule this settles, and the one `repos_created_total` already follows: an
//! entity that came into existence on this instance is counted, whichever door
//! it came through, **in the state it arrived in**. So an imported issue that
//! arrives closed counts as closed too — otherwise `opened - closed`, which is
//! how the backlog is read off these two series, is permanently wrong by the
//! whole imported history.
//!
//! Driven through the real thing: the real registry, the real `rg-core` →
//! `rg-http` observers, a real `POST /api/v1/imports` against a fake GitHub API
//! served from this test, and the numbers read back off `GET /metrics`. Only in
//! that wiring is "recorded twice" distinguishable from "recorded once".
//!
//! Everything lives in one test on purpose: the registry is process-wide, so a
//! sibling test filing an issue in the same process would be indistinguishable
//! from this one's work.

use std::time::Duration;

use crate::common::{
    build_test_app_state_with, register_full, setup_test_db, wait_for_listener, StateOverrides,
};

/// How many issues, and how many of them arrive already closed.
const IMPORTED_ISSUES: usize = 3;
const IMPORTED_CLOSED_ISSUES: usize = 1;
/// How many pull requests, and how many of them arrive already merged.
const IMPORTED_PRS: usize = 2;
const IMPORTED_MERGED_PRS: usize = 1;

/// A GitHub Enterprise API that serves exactly what an issues-and-PRs import
/// asks for. `parse_github_url` turns any non-`github.com` source host into
/// `<origin>/api/v3`, so that is where the routes live.
async fn spawn_fake_github() -> String {
    let issues: Vec<serde_json::Value> = (1..=IMPORTED_ISSUES)
        .map(|number| {
            let closed = number <= IMPORTED_CLOSED_ISSUES;
            serde_json::json!({
                "number": number,
                "title": format!("imported issue {number}"),
                "body": "from the source tracker",
                "state": if closed { "closed" } else { "open" },
                "labels": [],
                "milestone": null,
                "user": null,
                "assignees": [],
                "comments": 0,
                "created_at": "2026-01-02T03:04:05Z",
                "updated_at": "2026-01-02T03:04:05Z",
                "closed_at": if closed { serde_json::json!("2026-01-03T03:04:05Z") } else { serde_json::Value::Null },
                "pull_request": null,
            })
        })
        .collect();

    let pulls: Vec<serde_json::Value> = (1..=IMPORTED_PRS)
        .map(|number| {
            let merged = number <= IMPORTED_MERGED_PRS;
            serde_json::json!({
                "number": number,
                "title": format!("imported pull request {number}"),
                "body": "from the source forge",
                "state": if merged { "closed" } else { "open" },
                "merged": merged,
                "merged_at": if merged { serde_json::json!("2026-01-04T03:04:05Z") } else { serde_json::Value::Null },
                "draft": false,
                "user": null,
                "head": {"ref": format!("feature-{number}"), "sha": "0".repeat(40), "label": null, "repo": null},
                "base": {"ref": "main", "sha": "1".repeat(40), "label": null, "repo": null},
                "labels": [],
                "milestone": null,
                "created_at": "2026-01-02T03:04:05Z",
                "updated_at": "2026-01-02T03:04:05Z",
                "closed_at": serde_json::Value::Null,
            })
        })
        .collect();

    let empty = || axum::Json(serde_json::json!([]));
    let app = axum::Router::new()
        .route(
            "/api/v3/repos/{owner}/{repo}/issues",
            axum::routing::get(move || {
                let issues = issues.clone();
                async move { axum::Json(serde_json::Value::Array(issues)) }
            }),
        )
        .route(
            "/api/v3/repos/{owner}/{repo}/issues/{number}/comments",
            axum::routing::get(move || async move { empty() }),
        )
        .route(
            "/api/v3/repos/{owner}/{repo}/pulls",
            axum::routing::get(move || {
                let pulls = pulls.clone();
                async move { axum::Json(serde_json::Value::Array(pulls)) }
            }),
        )
        .route(
            "/api/v3/repos/{owner}/{repo}/pulls/{number}/reviews",
            axum::routing::get(move || async move { empty() }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fake source forge");
    let address = listener.local_addr().expect("fake source forge address");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve the fake source forge");
    });
    wait_for_listener(&address.to_string()).await;
    format!("http://{address}")
}

/// The Plombir Git instance under test, trusting the loopback origin the fake
/// forge listens on (a private address is refused without an explicit entry).
async fn spawn_app(source_origin: &str) -> String {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create repo root");
    let trusted = rg_core::import::trust::TrustedImportOrigins::parse(&[source_origin.to_owned()])
        .expect("trusted origin config");
    let state = build_test_app_state_with(
        db,
        repo_root,
        StateOverrides {
            trusted_import_origins: Some(trusted),
            ..Default::default()
        },
    );
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test app");
    let address = listener.local_addr().expect("test app address");
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app)
            .await
            .expect("serve the instance under test");
    });
    wait_for_listener(&address.to_string()).await;
    format!("http://{address}")
}

/// The value an operator would scrape for one counter.
///
/// A counter Prometheus has never been given a value for is absent from the
/// exposition entirely, which reads as zero.
async fn scraped(base: &str, metric: &str) -> u64 {
    let body = reqwest::Client::new()
        .get(format!("{base}/metrics"))
        .send()
        .await
        .expect("scrape /metrics")
        .text()
        .await
        .expect("read the exposition body");

    let prefix = format!("{metric} ");
    body.lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .map_or(0, |value| {
            value
                .trim()
                .parse::<f64>()
                .expect("the counter's exposed value is a number") as u64
        })
}

/// Every business counter this test watches, read in one pass so the four
/// values describe the same moment.
async fn counters(base: &str) -> [u64; 4] {
    [
        scraped(base, "plombir_git_issues_opened_total").await,
        scraped(base, "plombir_git_issues_closed_total").await,
        scraped(base, "plombir_git_prs_opened_total").await,
        scraped(base, "plombir_git_prs_merged_total").await,
    ]
}

async fn run_import_to_completion(base: &str, token: &str, source_url: &str, owner: &str) {
    let client = reqwest::Client::new();
    let accepted = client
        .post(format!("{base}/api/v1/imports"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "platform": "github",
            "source_url": source_url,
            "target_owner": owner,
            "target_name": "widgets",
            "import_repo": false,
            "import_issues": true,
            "import_pull_requests": true,
            "import_wiki": false,
            "import_releases": false,
            "import_labels": false,
            "import_milestones": false,
        }))
        .send()
        .await
        .expect("start import");
    assert_eq!(
        accepted.status(),
        201,
        "baseline: the import was accepted: {:?}",
        accepted.text().await
    );
    let task: serde_json::Value = accepted.json().await.expect("import task response");
    let task_id = task["id"].as_i64().expect("task id");

    for _ in 0..300 {
        let task: serde_json::Value = client
            .get(format!("{base}/api/v1/imports/{task_id}"))
            .bearer_auth(token)
            .send()
            .await
            .expect("read import task")
            .json()
            .await
            .expect("import task body");
        match task["status"].as_str().unwrap_or_default() {
            "completed" => return,
            "failed" => panic!(
                "the import failed before it could count anything: {}",
                task["error_message"]
            ),
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    panic!("the import never finished");
}

async fn file_one_issue(base: &str, token: &str, owner: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/widgets/issues"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "title": "filed by hand" }))
        .send()
        .await
        .expect("file an issue")
        .status();
    assert_eq!(status, 201, "baseline: the issue was filed");
}

#[tokio::test]
async fn an_imported_tracker_moves_the_same_counters_a_filed_issue_does() {
    // The counters below are process-global, so this test owns the process
    // while it reads them.
    let _serial = crate::METRIC_SERIAL.lock().await;

    // The same wiring `run_with_listener` builds. Installing it is idempotent,
    // and it stays installed for the rest of the process — which is why this
    // binary exists and why the lock above is held.
    rg_http::metrics::init_registry().expect("the metrics registry could not be built");
    rg_core::metrics_hook::set_issue_opened_observer(rg_http::metrics::recorder::issue_opened);
    rg_core::metrics_hook::set_issue_closed_observer(rg_http::metrics::recorder::issue_closed);
    rg_core::metrics_hook::set_pr_opened_observer(rg_http::metrics::recorder::pr_opened);
    rg_core::metrics_hook::set_pr_merged_observer(rg_http::metrics::recorder::pr_merged);

    let source_origin = spawn_fake_github().await;
    let base = spawn_app(&source_origin).await;
    let (token, _) = register_full(&base, "importmetrics", "importmetrics@example.invalid").await;

    // ── The path with no handler. ──
    let before = counters(&base).await;
    run_import_to_completion(
        &base,
        &token,
        &format!("{source_origin}/acme/widgets.git"),
        "importmetrics",
    )
    .await;
    let after = counters(&base).await;

    assert_eq!(
        [
            after[0] - before[0],
            after[1] - before[1],
            after[2] - before[2],
            after[3] - before[3],
        ],
        [
            IMPORTED_ISSUES as u64,
            IMPORTED_CLOSED_ISSUES as u64,
            IMPORTED_PRS as u64,
            IMPORTED_MERGED_PRS as u64,
        ],
        "an imported tracker is invisible to the business counters: the rows exist, the panels \
         built on `rate(issues_opened_total)` and `rate(prs_opened_total)` say nothing happened"
    );

    // ── The path with a handler: exactly one, not two. ──
    let before = counters(&base).await;
    file_one_issue(&base, &token, "importmetrics").await;
    let after = counters(&base).await;

    assert_eq!(
        after[0] - before[0],
        1,
        "the REST handler must leave the recording to the allocator it calls; counting it in both \
         places doubles every issue a human files"
    );
    assert_eq!(after[1], before[1], "filing an open issue is not a closure");
}
