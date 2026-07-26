//! Live HTTP push coverage for the graceful-shutdown drain of post-push hooks.
//!
//! `handle_git_receive_pack` answers the client the moment the pack is stored
//! and leaves the rest — open-PR head-SHA refresh, CI trigger, webhook fan-out
//! — to a detached task. Detached is right; *unowned* is not. The client
//! already holds its `200 OK`, so a SIGTERM in the next few seconds severs that
//! task at its first await: no pipeline, no webhook, no trace that any of it
//! was owed. Routing it through `rg_core::task_tracker::delivery_tracker()`
//! puts it in the drain `rg_http::run` performs after the server stops
//! accepting (`close()` + `wait()`), which is what this test exercises against
//! a real `git push`: once the drain returns, the push's DB effect is there.

use std::path::Path;
use std::time::Duration;

use sea_orm::{ActiveValue::NotSet, Set};

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

/// Run git through the sanctioned gateway (the `test_no_raw_git_command_in_crates`
/// regression guard forbids raw git process construction). Returns trimmed stdout.
fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway must initialize");
    let output = gateway.run(args, cwd).expect("git invocation failed");
    assert!(
        output.success(),
        "git {args:?} failed: {}",
        output.stderr_str().trim()
    );
    output.stdout_str().trim().to_string()
}

async fn create_pat(base: &str, jwt: &str) -> String {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/users/tokens", base))
        .bearer_auth(jwt)
        .json(&serde_json::json!({ "name": "git-push" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create token failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

/// A push whose hooks are still in flight must survive the shutdown drain: the
/// open PR on the pushed branch carries the new head SHA by the time
/// `close()` + `wait()` returns, exactly as `task_tracker`'s own
/// `close_then_wait_drains_a_spawned_task` promises for a tracked task.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn post_push_hooks_are_drained_by_the_delivery_tracker() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let state = build_test_app_state(db.clone(), repo_root.clone());
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (jwt, user_id) = register_full(&base, "pushdrain", "pushdrain@example.com").await;
    let repo_id = crate::common::create_repo(&base, &jwt, "drain-repo").await;
    let pat = create_pat(&base, &jwt).await;

    // An open PR whose head branch is the one about to be pushed. Refreshing
    // its head SHA is the first thing the detached hook task does, and it is a
    // pure DB write — no CI engine, no outbound HTTP, so what the assertion
    // below observes is the hook task itself finishing, nothing else.
    let now = chrono::Utc::now();
    let stale_sha = "1111111111111111111111111111111111111111";
    let pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("drain the push hooks".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(user_id),
            reviewer_id: Set(None),
            head_branch: Set("main".to_string()),
            base_branch: Set("release".to_string()),
            head_sha: Set(Some(stale_sha.to_string())),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("seed open PR");

    // ── A real commit pushed over the live smart-HTTP transport. ──
    let worktree = tempfile::tempdir().unwrap();
    git(&["init", "--initial-branch=main"], Some(worktree.path()));
    git(
        &["config", "user.name", "Push Drain"],
        Some(worktree.path()),
    );
    git(
        &["config", "user.email", "pushdrain@example.com"],
        Some(worktree.path()),
    );
    std::fs::write(worktree.path().join("README.md"), "pushed over HTTP\n").unwrap();
    git(&["add", "."], Some(worktree.path()));
    git(&["commit", "-m", "seed content"], Some(worktree.path()));
    let pushed_sha = git(&["rev-parse", "HEAD"], Some(worktree.path()));

    // PAT in the password field — the same Basic-auth shape `git push` builds
    // from a credential-carrying remote URL.
    let url = format!("http://pushdrain:{pat}@{addr}/pushdrain/drain-repo.git");
    git(&["push", &url, "main"], Some(worktree.path()));

    // ── The shutdown drain, as `rg_http::run` performs it. ──
    let tracker = rg_core::task_tracker::delivery_tracker();
    tracker.close();
    // Hang-guard, not a deadline: an untracked hook task makes `wait()` return
    // instantly (the assertion below is what fails), while a tight bound would
    // just turn machine load into a red suite — same reasoning as
    // `task_tracker`'s own drain test.
    tokio::time::timeout(Duration::from_secs(120), tracker.wait())
        .await
        .expect("delivery tracker drained within timeout");
    // Restore global state: the tracker is process-wide and other tests in this
    // binary spawn into it.
    tracker.reopen();

    let refreshed = rg_db::ops::pull_request_ops::find_by_id(&db, pr.id)
        .await
        .expect("reload PR")
        .expect("PR still exists");
    assert_eq!(
        refreshed.head_sha.as_deref(),
        Some(pushed_sha.as_str()),
        "the detached post-push hooks must have run to completion before the \
         delivery-tracker drain returned — head SHA is still the pre-push value"
    );

    server.abort();
}
