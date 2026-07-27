//! A pull request is a CI event, and until card_074d93bfe327 nobody emitted it.
//!
//! `Workflow::matches_event` has handled `pull_request` since it was written,
//! with unit tests covering the branch. What it never had was a caller: every
//! producer in the tree named its event `push`, `merge_group`, `manual` or
//! `retry`, so a repository whose CI is one `.gitea/workflows/pr.yml` with
//! `on: pull_request` got a green test suite and no pipeline — not when a PR was
//! opened, not when its head branch moved.
//!
//! These tests hold both halves of the fix: the event is now produced, and it is
//! produced *only* where a workflow actually asked for it. The second half is
//! not a detail — the gate is what keeps every repository driving CI from the
//! native `.forgekeep-ci.yml` from getting a duplicate of its push pipeline on
//! every PR open and every PR sync, forever.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::common::{
    build_test_app_state, create_repo, register_full, setup_test_db, wait_for_listener,
};

/// What the trigger asked CI to run. `base_branch` is in here because it is the
/// value an `on: pull_request` `branches:` filter is matched against — a
/// pipeline triggered without it is judged against the repository's default
/// branch instead of the branch the PR targets.
type TriggeredPipeline = (String, String, String, Option<i64>, Option<String>);

/// A CI engine that records what it was asked to run.
///
/// `workflow_for_event` is the answer of the gate the producer consults, and it
/// is settable per test: `true` stands for a repository with a matching
/// `on: pull_request` workflow, `false` for one whose only CI is a native
/// `.forgekeep-ci.yml` — which `has_ci_config` reports as "CI present" either
/// way.
struct RecordingCiEngine {
    triggered: Mutex<Vec<TriggeredPipeline>>,
    workflow_for_event: bool,
}

impl RecordingCiEngine {
    fn new(workflow_for_event: bool) -> Self {
        Self {
            triggered: Mutex::new(Vec::new()),
            workflow_for_event,
        }
    }
}

impl rg_core::ci::CiTrigger for RecordingCiEngine {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        true
    }

    fn has_workflow_for_event(
        &self,
        _repo_path: &Path,
        _commit_sha: &str,
        _event: &str,
        _ref_name: &str,
        _base_branch: Option<&str>,
    ) -> bool {
        self.workflow_for_event
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        let entry = (
            params.commit_sha.to_string(),
            params.ref_name.to_string(),
            params.trigger_type.to_string(),
            params.triggered_by,
            params.base_branch.map(str::to_string),
        );
        Box::pin(async move {
            self.triggered.lock().unwrap().push(entry);
            Ok(1)
        })
    }

    fn resume_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// Drain the detached hook tasks the way `rg_http::run` drains them on shutdown,
/// so the assertions are deterministic instead of sleep-and-hope.
async fn drain_delivery_tracker(what: &str) {
    let tracker = rg_core::task_tracker::delivery_tracker();
    tracker.close();
    tokio::time::timeout(Duration::from_secs(120), tracker.wait())
        .await
        .unwrap_or_else(|_| panic!("{what} drained within timeout"));
    tracker.reopen();
}

/// A repository with a seeded commit on `main` and a `feature` branch pointing
/// at it, plus the running server. Returns the pieces the tests assert on.
struct Fixture {
    base: String,
    jwt: String,
    user_id: i64,
    head_sha: String,
    ci_engine: Arc<RecordingCiEngine>,
    db: rg_db::DatabaseConnection,
    server: tokio::task::JoinHandle<()>,
}

async fn fixture(owner: &str, repo_name: &str, workflow_for_event: bool) -> Fixture {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let ci_engine = Arc::new(RecordingCiEngine::new(workflow_for_event));
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = ci_engine.clone();

    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (jwt, user_id) = register_full(&base, owner, &format!("{owner}@example.com")).await;
    create_repo(&base, &jwt, repo_name).await;

    // Seed a commit on the default branch through the web editor, then point a
    // second branch at it: `create_pr` reads the head branch's SHA out of git,
    // and a PR whose head and base are the same branch is rejected.
    let client = reqwest::Client::new();
    let seeded = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo_name}/contents/notes.md"
        ))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "content": "the first line\n",
            "message": "add notes",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(seeded.status(), 200, "seed write must succeed");
    let seeded: serde_json::Value = seeded.json().await.unwrap();
    let head_sha = seeded["commit_sha"].as_str().unwrap().to_string();

    let repo_path = repo_root.join(format!("{owner}/{repo_name}.git"));
    let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    assert!(git
        .run(
            &["update-ref", "refs/heads/feature", &head_sha],
            Some(&repo_path)
        )
        .unwrap()
        .success());

    // The seed write's own post-push hooks are detached; drain and clear them so
    // only the action under test is in the recorder.
    drain_delivery_tracker("the seed write's hooks").await;
    ci_engine.triggered.lock().unwrap().clear();

    Fixture {
        base,
        jwt,
        user_id,
        head_sha,
        ci_engine,
        db,
        server,
    }
}

async fn open_pr(fixture: &Fixture, owner: &str, repo_name: &str) -> serde_json::Value {
    let created = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/{owner}/{repo_name}/pulls",
            fixture.base
        ))
        .bearer_auth(&fixture.jwt)
        .json(&serde_json::json!({
            "title": "review me",
            "head": "feature",
            "base": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "opening the PR must succeed");
    created.json().await.unwrap()
}

/// The card's acceptance: a repository whose only CI is an `on: pull_request`
/// workflow gets a pipeline when a PR is opened — under that exact event name,
/// which is what `Workflow::matches_event` selects workflows by.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_pull_request_triggers_the_pull_request_pipeline() {
    let fixture = fixture("prci", "pr-ci-repo", true).await;
    open_pr(&fixture, "prci", "pr-ci-repo").await;
    drain_delivery_tracker("the PR's CI trigger").await;

    let triggered = fixture.ci_engine.triggered.lock().unwrap().clone();
    assert_eq!(
        triggered,
        vec![(
            fixture.head_sha.clone(),
            "refs/pull/1/head".to_string(),
            "pull_request".to_string(),
            Some(fixture.user_id),
            Some("main".to_string()),
        )],
        "opening a PR must trigger exactly one pipeline, under the `pull_request` \
         event, on the PR's head commit, carrying the base branch its \
         `branches:` filter is matched against"
    );

    fixture.server.abort();
}

/// The other half of the fix. `has_ci_config` is true for every repository with
/// a native `.forgekeep-ci.yml`, so gating on it would give all of them a
/// second, identical pipeline on every PR open. The producer asks the
/// event-aware gate instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repository_without_a_pull_request_workflow_gets_no_pipeline() {
    let fixture = fixture("nopr", "no-pr-workflow", false).await;
    open_pr(&fixture, "nopr", "no-pr-workflow").await;
    drain_delivery_tracker("the PR's CI trigger").await;

    assert!(
        fixture.ci_engine.triggered.lock().unwrap().is_empty(),
        "a repository with no workflow for this event must get no pipeline: {:?}",
        fixture.ci_engine.triggered.lock().unwrap()
    );

    fixture.server.abort();
}

/// Pushing the head branch synchronises the PR, and a synchronised PR owes a
/// pipeline of its own — the `pull_request` half of the pair a forge emits for
/// a branch that has an open PR on it. The `push` pipeline for the same commit
/// is the other half, and the two must not share a ref: they would land in one
/// concurrency group and `cancel_in_progress` would have them cancel each other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pushing_the_head_branch_re_runs_the_pull_request_pipeline() {
    let fixture = fixture("prsync", "pr-sync-repo", true).await;
    let pr = open_pr(&fixture, "prsync", "pr-sync-repo").await;
    let pr_number = pr["number"].as_i64().expect("PR carries a number");
    drain_delivery_tracker("the PR's CI trigger").await;
    fixture.ci_engine.triggered.lock().unwrap().clear();

    // Move the head branch the way a push does — through the web editor, which
    // runs the same post-push hooks (card_13202be354ac).
    let written = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/prsync/pr-sync-repo/contents/review.md",
            fixture.base
        ))
        .bearer_auth(&fixture.jwt)
        .json(&serde_json::json!({
            "content": "addressed\n",
            "message": "address the review",
            "branch": "feature",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(written.status(), 200, "the head-branch write must succeed");
    let written: serde_json::Value = written.json().await.unwrap();
    let new_sha = written["commit_sha"].as_str().unwrap().to_string();
    assert_ne!(new_sha, fixture.head_sha, "the branch must have moved");

    drain_delivery_tracker("the head-branch push hooks").await;

    let triggered = fixture.ci_engine.triggered.lock().unwrap().clone();
    assert!(
        triggered.contains(&(
            new_sha.clone(),
            format!("refs/pull/{pr_number}/head"),
            "pull_request".to_string(),
            Some(fixture.user_id),
            Some("main".to_string()),
        )),
        "the synchronised PR must get a pull_request pipeline on the new head: {triggered:?}"
    );
    assert!(
        triggered.contains(&(
            new_sha.clone(),
            "refs/heads/feature".to_string(),
            "push".to_string(),
            Some(fixture.user_id),
            None,
        )),
        "the push pipeline must still be there, on its own ref: {triggered:?}"
    );

    let refreshed =
        rg_db::ops::pull_request_ops::find_by_id(&fixture.db, pr["id"].as_i64().unwrap())
            .await
            .expect("reload PR")
            .expect("PR still exists");
    assert_eq!(
        refreshed.head_sha.as_deref(),
        Some(new_sha.as_str()),
        "the PR must point at the commit its pipeline was triggered for"
    );

    fixture.server.abort();
}
