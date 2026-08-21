//! Merging a pull request must run the same post-push automation a `git push`
//! runs (card_87c4912c51ed).
//!
//! A merge advances `refs/heads/<base>` exactly like a push does, but until this
//! card the only thing it fired was the `pull_request.merged` webhook: no CI
//! pipeline on the merge commit, no `push` webhook, nothing for anything
//! subscribed to the branch. "Run CI on every push to main" therefore did not
//! hold for the way most merges happen — through the UI — and the only
//! workaround was to push to main by hand.
//!
//! The second test pins the bound that comes with the fix: the hooks themselves
//! run auto-merge, so a hook-triggered merge feeds its own ref move back into
//! the hooks. That has to converge.
//!
//! The third covers the half card_87c4912c51ed left behind (card_73a1ec5b32f3):
//! a merge started by something other than a push or a REST merge. "CI went
//! green, so the PR goes in" is the flow auto-merge exists for, and it ran the
//! merge and dropped the ref move — so precisely there, the merge commit on
//! `main` got no pipeline, no `push` webhook and no watch notification.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::{ActiveValue::NotSet, Set};

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

/// A CI engine that always claims a config is present and records what it was
/// asked to run. The harness default (`NoopCiEngine`) answers
/// `has_ci_config = false`, which would make "no pipeline" indistinguishable
/// from the bug.
#[derive(Default)]
struct RecordingCiEngine {
    triggered: Mutex<Vec<(String, String, String)>>,
}

impl rg_core::ci::CiTrigger for RecordingCiEngine {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        true
    }

    /// Mirrors `has_ci_config`: this double has no workflow files to
    /// match an event against, so it answers the same for every event.
    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
        true
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        let entry = (
            params.commit_sha.to_string(),
            params.ref_name.to_string(),
            params.trigger_type.to_string(),
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

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

fn commit(worktree: &Path, file: &str, message: &str) {
    std::fs::write(worktree.join(file), format!("{message}\n")).unwrap();
    git(&["add", "."], Some(worktree));
    git(&["commit", "-m", message], Some(worktree));
}

/// Seed `main` + `feature` (and optionally `release`) in the bare repository the
/// API just created, by pushing from a throwaway worktree.
fn seed_branches(bare_path: &Path, extra_branch: Option<&str>) -> tempfile::TempDir {
    let worktree = tempfile::tempdir().unwrap();
    let path = worktree.path();
    git(
        &["init", "--initial-branch=main", &path.to_string_lossy()],
        None,
    );
    git(&["config", "user.name", "Merge Hook Test"], Some(path));
    git(
        &["config", "user.email", "merge-hook@example.invalid"],
        Some(path),
    );

    commit(path, "README.md", "base");
    git(
        &["remote", "add", "origin", &bare_path.to_string_lossy()],
        Some(path),
    );
    git(&["push", "origin", "main"], Some(path));

    if let Some(branch) = extra_branch {
        git(&["checkout", "-b", branch], Some(path));
        git(&["push", "origin", branch], Some(path));
        git(&["checkout", "main"], Some(path));
    }

    git(&["checkout", "-b", "feature"], Some(path));
    commit(path, "feature.txt", "feature work");
    git(&["push", "origin", "feature"], Some(path));
    git(&["checkout", "main"], Some(path));

    worktree
}

/// Drain the detached hook tasks exactly as `rg_http::run` does on shutdown —
/// this is what makes the assertions deterministic instead of a sleep-and-hope,
/// and in the cascade test it is also the "it terminates" assertion.
async fn drain_delivery_tracker(tracker: &rg_core::task_tracker::TaskTracker) {
    tracker.close();
    tokio::time::timeout(Duration::from_secs(120), tracker.wait())
        .await
        .expect("post-push hooks drained within timeout — a cascade that never ends hangs here");
    tracker.reopen();
}

/// The card's headline: a PR merged through the REST API must trigger a
/// pipeline on the merge commit and send the `push` webhook for the base
/// branch. Both used to be missing entirely.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_merged_pull_request_runs_the_post_push_hooks() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let ci_engine = Arc::new(RecordingCiEngine::default());
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = ci_engine.clone();
    let delivery_tracker = state.delivery_tracker.clone();

    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (jwt, _user_id) = register_full(&base, "mergehook", "mergehook@example.com").await;
    let repo_id = crate::common::create_repo(&base, &jwt, "hook-repo").await;
    let _worktree = seed_branches(&repo_root.join("mergehook/hook-repo.git"), None);

    // A subscriber to `push`. The delivery target is unresolvable on purpose:
    // what is under test is that the fan-out ran at all, and a failed POST is
    // still recorded as a delivery row carrying the payload we care about.
    let hook = rg_core::webhook::service::create_webhook(
        &db,
        repo_id,
        &rg_core::webhook::service::CreateWebhookRequest {
            url: "https://hooks.example.invalid/forgekeep".to_string(),
            content_type: None,
            secret: None,
            active: Some(true),
            events: vec!["push".to_string()],
        },
        crate::common::TEST_ENCRYPTION_KEY,
    )
    .await
    .expect("register push webhook");

    let client = reqwest::Client::new();
    let opened = client
        .post(format!("{base}/api/v1/repos/mergehook/hook-repo/pulls"))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "title": "merge me",
            "head": "feature",
            "base": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(opened.status(), 201, "{}", opened.text().await.unwrap());

    // Opening a PR is a CI event of its own since card_074d93bfe327. Drain and
    // clear it, so what the merge assertion sees is the merge's own automation.
    drain_delivery_tracker(&delivery_tracker).await;
    ci_engine.triggered.lock().unwrap().clear();

    let merged = client
        .post(format!(
            "{base}/api/v1/repos/mergehook/hook-repo/pulls/1/merge"
        ))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(merged.status(), 200, "{}", merged.text().await.unwrap());
    let merged: serde_json::Value = merged.json().await.unwrap();
    let merge_sha = merged["merge_commit_sha"].as_str().unwrap().to_string();
    assert_eq!(
        git(
            &["rev-parse", "refs/heads/main"],
            Some(&repo_root.join("mergehook/hook-repo.git"))
        ),
        merge_sha,
        "the merge must actually be the new tip of the base branch"
    );

    drain_delivery_tracker(&delivery_tracker).await;

    let triggered = ci_engine.triggered.lock().unwrap().clone();
    assert_eq!(
        triggered,
        vec![(
            merge_sha.clone(),
            "refs/heads/main".to_string(),
            "push".to_string()
        )],
        "merging a PR must trigger exactly one pipeline, on the merge commit of \
         the base branch — the whole point of 'CI on every push to main'"
    );

    let deliveries = rg_core::webhook::service::list_deliveries(&db, hook.id)
        .await
        .expect("list webhook deliveries");
    let push_delivery = deliveries
        .iter()
        .find(|delivery| delivery.event == "push")
        .expect("a merge must send the `push` webhook for the base branch");
    let payload: serde_json::Value =
        serde_json::from_str(push_delivery.request_payload.as_deref().unwrap_or("null"))
            .expect("delivery payload is JSON");
    assert_eq!(payload["ref"], "refs/heads/main");
    assert_eq!(payload["after"], merge_sha);
    assert_ne!(
        payload["before"],
        serde_json::Value::Null,
        "the payload must carry the pre-merge tip, not a fabricated zero SHA \
         (zeros read as `branch.created` for a branch that is alive)"
    );

    server.abort();
}

/// The card's headline (card_73a1ec5b32f3): a pipeline reported green by an
/// external runner auto-merges the PR whose head it built, and *that* merge owes
/// the post-push hooks just like any other. Before the fix `finish_job` ran the
/// merge and threw the ref move away, so the merge commit on `main` got no
/// pipeline and no `push` webhook — in the one flow auto-merge is for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pipeline_going_green_runs_the_hooks_for_the_merge_it_triggers() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let ci_engine = Arc::new(RecordingCiEngine::default());
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = ci_engine.clone();
    let delivery_tracker = state.delivery_tracker.clone();

    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (jwt, user_id) = register_full(&base, "cimerge", "cimerge@example.com").await;
    let repo_id = crate::common::create_repo(&base, &jwt, "ci-repo").await;
    let bare_path = repo_root.join("cimerge/ci-repo.git");
    let _worktree = seed_branches(&bare_path, None);

    let hook = rg_core::webhook::service::create_webhook(
        &db,
        repo_id,
        &rg_core::webhook::service::CreateWebhookRequest {
            url: "https://hooks.example.invalid/forgekeep".to_string(),
            content_type: None,
            secret: None,
            active: Some(true),
            events: vec!["push".to_string()],
        },
        crate::common::TEST_ENCRYPTION_KEY,
    )
    .await
    .expect("register push webhook");

    // Seeded rather than opened through the API with auto-merge switched on:
    // enabling auto-merge on a PR whose conditions are already met merges it on
    // the spot, through a path this card is not about. What is under test is the
    // PR that is only waiting for its pipeline.
    let now = chrono::Utc::now();
    let feature_sha = git(&["rev-parse", "refs/heads/feature"], Some(&bare_path));
    let waiting = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("merge me once CI is green".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(true),
            auto_merge_strategy: Set(Some("merge".to_string())),
            auto_merge_enabled_by_id: Set(Some(user_id)),
            auto_merge_enabled_at: Set(Some(now)),
            author_id: Set(user_id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some(feature_sha.clone())),
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
    .expect("seed the auto-merge PR");

    // One runner, one job on the PR head — the shape an external-runner CI run
    // has when its last job reports in.
    let (runner, runner_token) =
        rg_db::ops::runner_ops::register_runner(&db, "ci-runner", "[]", None, None, None)
            .await
            .expect("register runner");
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        &feature_sha,
        "refs/heads/feature",
        "push",
        None,
    )
    .await
    .expect("create pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
        .await
        .expect("create stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        &db, stage.id, "test", "true", None, None, None, None, None, None, false, None, None, None,
    )
    .await
    .expect("create job");
    rg_db::ops::pipeline_ops::assign_job(&db, job.id, runner.id)
        .await
        .expect("assign the job to the runner");

    let client = reqwest::Client::new();
    let finished = client
        .post(format!(
            "{base}/api/v1/runners/{}/jobs/{}/finish",
            runner.id, job.id
        ))
        .bearer_auth(&runner_token)
        .json(&serde_json::json!({"status": "success", "exit_code": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(finished.status(), 200, "{}", finished.text().await.unwrap());

    drain_delivery_tracker(&delivery_tracker).await;

    let merged_pr = rg_db::ops::pull_request_ops::find_by_id(&db, waiting.id)
        .await
        .expect("reload the PR")
        .expect("PR still exists");
    assert_eq!(
        merged_pr.state, "merged",
        "a green pipeline on the PR head must trigger the auto-merge"
    );
    let merge_sha = merged_pr
        .merge_commit_sha
        .clone()
        .expect("the merged PR records its merge commit");
    assert_eq!(
        git(&["rev-parse", "refs/heads/main"], Some(&bare_path)),
        merge_sha,
        "the merge must actually be the new tip of the base branch"
    );

    let triggered = ci_engine.triggered.lock().unwrap().clone();
    assert_eq!(
        triggered,
        vec![(
            merge_sha.clone(),
            "refs/heads/main".to_string(),
            "push".to_string()
        )],
        "the merge commit the finished pipeline produced owes a pipeline of its \
         own — this is the half of the class that survived card_87c4912c51ed"
    );

    let deliveries = rg_core::webhook::service::list_deliveries(&db, hook.id)
        .await
        .expect("list webhook deliveries");
    let push_delivery = deliveries
        .iter()
        .find(|delivery| delivery.event == "push")
        .expect("the auto-merge must send the `push` webhook for the base branch");
    let payload: serde_json::Value =
        serde_json::from_str(push_delivery.request_payload.as_deref().unwrap_or("null"))
            .expect("delivery payload is JSON");
    assert_eq!(payload["ref"], "refs/heads/main");
    assert_eq!(payload["after"], merge_sha);

    server.abort();
}

/// The hooks run auto-merge; auto-merge merges; that merge moves a branch and
/// owes the hooks again. This is the cycle the card asks to be proven finite:
/// merging PR #1 into `main` must cascade into the auto-merge of `main` →
/// `release` and then stop, rather than feed itself forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_merge_the_hooks_trigger_cascades_once_and_terminates() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let ci_engine = Arc::new(RecordingCiEngine::default());
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = ci_engine.clone();
    let delivery_tracker = state.delivery_tracker.clone();

    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (jwt, user_id) = register_full(&base, "cascade", "cascade@example.com").await;
    let repo_id = crate::common::create_repo(&base, &jwt, "cascade-repo").await;
    let bare_path = repo_root.join("cascade/cascade-repo.git");
    let _worktree = seed_branches(&bare_path, Some("release"));

    let client = reqwest::Client::new();
    let opened = client
        .post(format!("{base}/api/v1/repos/cascade/cascade-repo/pulls"))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "title": "feature into main",
            "head": "feature",
            "base": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(opened.status(), 201, "{}", opened.text().await.unwrap());

    // Opening the PR triggers its own `pull_request` pipeline
    // (card_074d93bfe327); the cascade under test starts at the merge below.
    drain_delivery_tracker(&delivery_tracker).await;
    ci_engine.triggered.lock().unwrap().clear();

    // The second PR is seeded directly: opening it through the API and enabling
    // auto-merge would merge it immediately (its conditions are already met),
    // and then there would be no cascade left to observe. Its head SHA is
    // whatever `main` currently points at — the hooks rewrite it to the merge
    // commit, which is what makes auto-merge pick it up.
    let now = chrono::Utc::now();
    let main_sha = git(&["rev-parse", "refs/heads/main"], Some(&bare_path));
    let waiting = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(2),
            title: Set("main into release, automatically".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(true),
            auto_merge_strategy: Set(Some("merge".to_string())),
            auto_merge_enabled_by_id: Set(Some(user_id)),
            auto_merge_enabled_at: Set(Some(now)),
            author_id: Set(user_id),
            reviewer_id: Set(None),
            head_branch: Set("main".to_string()),
            base_branch: Set("release".to_string()),
            head_sha: Set(Some(main_sha)),
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
    .expect("seed the auto-merge PR");

    let merged = client
        .post(format!(
            "{base}/api/v1/repos/cascade/cascade-repo/pulls/1/merge"
        ))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(merged.status(), 200, "{}", merged.text().await.unwrap());
    let merged: serde_json::Value = merged.json().await.unwrap();
    let first_merge_sha = merged["merge_commit_sha"].as_str().unwrap().to_string();

    // If the cascade did not terminate, this never returns.
    drain_delivery_tracker(&delivery_tracker).await;

    let auto_merged = rg_db::ops::pull_request_ops::find_by_id(&db, waiting.id)
        .await
        .expect("reload the auto-merge PR")
        .expect("PR still exists");
    assert_eq!(
        auto_merged.state, "merged",
        "the merge of PR #1 must run the hooks, and the hooks must auto-merge \
         the PR whose head branch just moved"
    );
    let second_merge_sha = auto_merged
        .merge_commit_sha
        .clone()
        .expect("the auto-merged PR records its merge commit");

    let triggered = ci_engine.triggered.lock().unwrap().clone();
    assert_eq!(
        triggered,
        vec![
            // `main` is the head branch of the waiting PR #2, so the first merge
            // synchronises it — one `pull_request` pipeline, on its own ref.
            (
                first_merge_sha.clone(),
                "refs/pull/2/head".to_string(),
                "pull_request".to_string()
            ),
            (
                first_merge_sha,
                "refs/heads/main".to_string(),
                "push".to_string()
            ),
            // `release` is nobody's head branch, so its move is a push and
            // nothing else.
            (
                second_merge_sha,
                "refs/heads/release".to_string(),
                "push".to_string()
            ),
        ],
        "each branch the chain moved gets exactly one pipeline per event it \
         raises, and the chain stops there — a repeat would mean the work list \
         re-ran a ref move"
    );

    server.abort();
}
