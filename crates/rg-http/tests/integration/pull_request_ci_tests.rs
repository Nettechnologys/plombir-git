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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::ConnectionTrait;
use tokio::sync::Notify;

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
    /// `(pipeline_id, job_id)` of every row this engine actually wrote.
    ///
    /// The rows are real because half of what this file asserts is what happens
    /// to a pipeline *after* it exists — a stub returning a made-up id can be
    /// asked whether a pipeline was requested, but not whether the one that was
    /// left running ever got cancelled.
    created: Mutex<Vec<(i64, i64)>>,
    workflow_for_event: bool,
    refuse_next_pull_request: AtomicBool,
    delay_next: AtomicBool,
    entered_delay: Notify,
    release_delay: Notify,
    graph_created: Notify,
}

impl RecordingCiEngine {
    fn new(workflow_for_event: bool) -> Self {
        Self {
            triggered: Mutex::new(Vec::new()),
            created: Mutex::new(Vec::new()),
            workflow_for_event,
            refuse_next_pull_request: AtomicBool::new(false),
            delay_next: AtomicBool::new(false),
            entered_delay: Notify::new(),
            release_delay: Notify::new(),
            graph_created: Notify::new(),
        }
    }
}

impl rg_core::ci::CiTrigger for RecordingCiEngine {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        true
    }

    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
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
            if self.delay_next.swap(false, Ordering::SeqCst) {
                self.entered_delay.notify_one();
                self.release_delay.notified().await;
            }
            if params.trigger_type == "pull_request"
                && self.refuse_next_pull_request.swap(false, Ordering::SeqCst)
            {
                return Err(rg_core::error::invalid_request(
                    "unsupported key `types` in .gitea/workflows/pr.yml",
                ));
            }
            let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
                params.db,
                params.repo_id,
                params.commit_sha,
                params.ref_name,
                params.trigger_type,
                params.triggered_by,
            )
            .await?;
            let stage =
                rg_db::ops::pipeline_ops::create_stage(params.db, pipeline.id, "test", 0).await?;
            let job = rg_db::ops::pipeline_ops::create_job(
                params.db, stage.id, "test", "true", None, None, None, None, None, None, false,
                None, None, None,
            )
            .await?;
            self.triggered.lock().unwrap().push(entry);
            self.created.lock().unwrap().push((pipeline.id, job.id));
            self.graph_created.notify_waiters();
            Ok(pipeline.id)
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
async fn drain_delivery_tracker(tracker: &rg_core::task_tracker::TaskTracker, what: &str) {
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
    repo_id: i64,
    head_sha: String,
    ci_engine: Arc<RecordingCiEngine>,
    delivery_tracker: rg_core::task_tracker::TaskTracker,
    db: rg_db::DatabaseConnection,
    /// Kept so a test can drive `trigger_pull_request_ci` directly — the fork
    /// gate below has a "nothing happens" outcome, and the only honest way to
    /// assert it is to call the producer and read its `Ok(None)`.
    repo_root: std::path::PathBuf,
    server: tokio::task::JoinHandle<()>,
}

async fn fixture(owner: &str, repo_name: &str, workflow_for_event: bool) -> Fixture {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let ci_engine = Arc::new(RecordingCiEngine::new(workflow_for_event));
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

    let (jwt, user_id) = register_full(&base, owner, &format!("{owner}@example.com")).await;
    let repo_id = create_repo(&base, &jwt, repo_name).await;

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
    drain_delivery_tracker(&delivery_tracker, "the seed write's hooks").await;
    ci_engine.triggered.lock().unwrap().clear();
    ci_engine.created.lock().unwrap().clear();

    Fixture {
        base,
        jwt,
        user_id,
        repo_id,
        head_sha,
        ci_engine,
        delivery_tracker,
        db,
        repo_root,
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

async fn reject_pr_event(fixture: &Fixture, event_type: &str) {
    fixture
        .db
        .execute_unprepared(&format!(
            "CREATE TRIGGER pr_event_outage BEFORE INSERT ON pr_events \
             WHEN new.event_type = '{event_type}' \
             BEGIN SELECT RAISE(ABORT, 'timeline storage is unavailable'); END;"
        ))
        .await
        .expect("install the timeline-event write fault");
}

/// The card's acceptance: a repository whose only CI is an `on: pull_request`
/// workflow gets a pipeline when a PR is opened — under that exact event name,
/// which is what `Workflow::matches_event` selects workflows by.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_pull_request_triggers_the_pull_request_pipeline() {
    let fixture = fixture("prci", "pr-ci-repo", true).await;
    open_pr(&fixture, "prci", "pr-ci-repo").await;
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's CI trigger").await;

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

/// card_19696fe3d6f6: the PR row is committed before its opening timeline row.
/// Losing that secondary write must not answer 5xx or prevent the HTTP layer
/// from starting the `pull_request` automation promised by a successful open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broken_opened_timeline_write_still_creates_the_pr_and_its_pipeline() {
    let fixture = fixture("propenfault", "pr-open-fault-repo", true).await;
    reject_pr_event(&fixture, "pull_request_opened").await;

    let pr = open_pr(&fixture, "propenfault", "pr-open-fault-repo").await;
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's CI trigger").await;

    let pr_id = pr["id"].as_i64().expect("created PR carries its id");
    let persisted = rg_db::ops::pull_request_ops::find_by_id(&fixture.db, pr_id)
        .await
        .expect("reload the created pull request")
        .expect("the pull request row committed before its timeline write failed");
    assert_eq!(persisted.state, "open");
    assert_eq!(
        fixture.ci_engine.created.lock().unwrap().len(),
        1,
        "the successful response must still launch the pull_request pipeline"
    );

    let events = rg_db::ops::pr_event_ops::list_by_pr(&fixture.db, pr_id)
        .await
        .expect("read the degraded timeline");
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "pull_request_opened"),
        "the injected event write must really fail; otherwise this test is vacuous"
    );

    fixture.server.abort();
}

/// A head-branch push commits before its detached PR synchronisation reads the
/// workflow. The write and PR update must stay successful, while the repository
/// gets a terminal pull-request run carrying the safe path/key refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn synchronising_a_pr_with_a_refused_ci_config_records_a_failed_pipeline() {
    let fixture = fixture("prbadci", "pr-bad-ci", true).await;
    let pr = open_pr(&fixture, "prbadci", "pr-bad-ci").await;
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's initial CI trigger").await;
    fixture
        .ci_engine
        .refuse_next_pull_request
        .store(true, Ordering::SeqCst);

    let written = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/prbadci/pr-bad-ci/contents/refusal.md",
            fixture.base
        ))
        .bearer_auth(&fixture.jwt)
        .json(&serde_json::json!({
            "content": "still committed\n",
            "message": "exercise refused PR CI",
            "branch": "feature",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(written.status(), 200, "the head-branch write must succeed");
    let written: serde_json::Value = written.json().await.unwrap();
    let new_sha = written["commit_sha"].as_str().unwrap().to_string();
    drain_delivery_tracker(&fixture.delivery_tracker, "the refused PR sync CI trigger").await;

    let (pipelines, _) = rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(
        &fixture.db,
        fixture.repo_id,
        0,
        100,
    )
    .await
    .expect("list repository pipelines");
    let refused: Vec<_> = pipelines
        .into_iter()
        .filter(|pipeline| {
            pipeline.trigger_type == "pull_request" && pipeline.commit_sha == new_sha
        })
        .collect();
    assert_eq!(
        refused.len(),
        1,
        "one PR operation owes one visible refusal"
    );
    assert_eq!(refused[0].status, "failed");
    assert_eq!(refused[0].commit_sha, new_sha);
    let stages = rg_db::ops::pipeline_ops::list_stages_by_pipeline(&fixture.db, refused[0].id)
        .await
        .expect("list diagnostic stages");
    let jobs = rg_db::ops::pipeline_ops::list_jobs_by_stage(&fixture.db, stages[0].id)
        .await
        .expect("list diagnostic jobs");
    let log = jobs[0].log.as_deref().expect("diagnostic log");
    assert!(log.contains(".gitea/workflows/pr.yml"), "{log}");
    assert!(log.contains("types"), "{log}");
    {
        let triggered = fixture.ci_engine.triggered.lock().unwrap();
        assert!(
            !triggered
                .iter()
                .any(|(sha, _, trigger, _, _)| { sha == &new_sha && trigger == "pull_request" }),
            "a refused PR config must not also create runnable PR work: {triggered:?}"
        );
    }

    let refreshed =
        rg_db::ops::pull_request_ops::find_by_id(&fixture.db, pr["id"].as_i64().unwrap())
            .await
            .expect("reload PR")
            .expect("PR still exists");
    assert_eq!(
        refreshed.head_sha.as_deref(),
        Some(new_sha.as_str()),
        "the PR sync must remain committed despite its CI refusal"
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
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's CI trigger").await;

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
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's CI trigger").await;
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

    drain_delivery_tracker(&fixture.delivery_tracker, "the head-branch push hooks").await;

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

// ── Closing the PR ends the run it started (card_f68eac170fa5) ───────────

impl Fixture {
    /// The single `pull_request` pipeline this fixture's PR produced.
    fn pull_request_pipeline(&self) -> (i64, i64) {
        let created = self.ci_engine.created.lock().unwrap();
        assert_eq!(
            created.len(),
            1,
            "these tests read one pull_request pipeline; got {created:?}"
        );
        created[0]
    }

    async fn pipeline_status(&self, pipeline_id: i64) -> String {
        rg_db::ops::pipeline_ops::get_pipeline(&self.db, pipeline_id)
            .await
            .expect("read the pipeline")
            .expect("the pipeline row must still be there")
            .status
    }

    async fn job_status(&self, job_id: i64) -> String {
        use sea_orm::EntityTrait;
        rg_db::entities::pipeline_job::Entity::find_by_id(job_id)
            .one(&self.db)
            .await
            .expect("read the job")
            .expect("the job row must still be there")
            .status
    }

    async fn set_pr_state(&self, owner: &str, repo_name: &str, state: &str) {
        let patched = reqwest::Client::new()
            .patch(format!(
                "{}/api/v1/repos/{owner}/{repo_name}/pulls/1",
                self.base
            ))
            .bearer_auth(&self.jwt)
            .json(&serde_json::json!({ "state": state }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            patched.status(),
            200,
            "setting the PR state to {state} must succeed: {}",
            patched.text().await.unwrap()
        );
    }
}

/// card_f68eac170fa5: a closed PR's pipeline used to keep going. Real runners
/// pick its jobs up and spend real minutes proving a branch nobody will merge —
/// and because `refs/pull/{n}/head` is stable for the PR's whole life, a
/// repository declaring `concurrency:` without `cancel_in_progress` then has
/// every later trigger on that ref refused for a run that answers a dead
/// question.
///
/// The job half of the assertion is the sharper one: `cancel_pipeline_chain`
/// is what makes the acknowledgement a claim about the whole graph, and a
/// cancel that stopped at the pipeline row would still leave the work for a
/// runner to pick up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_a_pull_request_cancels_the_pipeline_it_left_running() {
    let fixture = fixture("prclose", "pr-close-repo", true).await;
    open_pr(&fixture, "prclose", "pr-close-repo").await;
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's CI trigger").await;

    let (pipeline_id, job_id) = fixture.pull_request_pipeline();
    assert_eq!(
        fixture.pipeline_status(pipeline_id).await,
        "pending",
        "the fixture must leave a live pipeline for the close to act on"
    );

    fixture
        .set_pr_state("prclose", "pr-close-repo", "closed")
        .await;

    assert_eq!(
        fixture.pipeline_status(pipeline_id).await,
        "canceled",
        "the pull_request pipeline outlived the PR it was asked about"
    );
    assert_eq!(
        fixture.job_status(job_id).await,
        "canceled",
        "the pipeline was marked canceled but its job stayed schedulable"
    );

    fixture.server.abort();
}

async fn assert_broken_update_event_still_closes_and_cancels(
    owner: &str,
    repo_name: &str,
    event_type: &str,
    also_convert_to_draft: bool,
) {
    let fixture = fixture(owner, repo_name, true).await;
    let pr = open_pr(&fixture, owner, repo_name).await;
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's CI trigger").await;
    let pr_id = pr["id"].as_i64().expect("opened PR carries its id");
    let (pipeline_id, job_id) = fixture.pull_request_pipeline();
    reject_pr_event(&fixture, event_type).await;

    let patched = reqwest::Client::new()
        .patch(format!(
            "{}/api/v1/repos/{owner}/{repo_name}/pulls/1",
            fixture.base
        ))
        .bearer_auth(&fixture.jwt)
        .json(&serde_json::json!({
            "state": "closed",
            "draft": also_convert_to_draft,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        patched.status(),
        200,
        "a missing {event_type} timeline row cannot turn a committed close into a failure: {}",
        patched.text().await.unwrap()
    );

    let persisted = rg_db::ops::pull_request_ops::find_by_id(&fixture.db, pr_id)
        .await
        .expect("reload the updated pull request")
        .expect("the pull request still exists");
    assert_eq!(persisted.state, "closed");
    assert_eq!(persisted.is_draft, also_convert_to_draft);
    assert_eq!(
        fixture.pipeline_status(pipeline_id).await,
        "canceled",
        "the timeline outage must not skip cancellation after the committed close"
    );
    assert_eq!(
        fixture.job_status(job_id).await,
        "canceled",
        "the pipeline cancellation must still reach its schedulable job"
    );

    let events = rg_db::ops::pr_event_ops::list_by_pr(&fixture.db, pr_id)
        .await
        .expect("read the degraded timeline");
    assert!(
        !events.iter().any(|event| event.event_type == event_type),
        "the injected {event_type} write must really fail; otherwise this test is vacuous"
    );
    if event_type == "pull_request_converted_to_draft" {
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "pull_request_closed"),
            "a failed draft event must not stop the later state event"
        );
    }

    fixture.server.abort();
}

/// The first event emitted by a combined draft+close update is already after
/// both fields were persisted. Its failure must not suppress the later close
/// event or the pipeline cancellation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broken_draft_timeline_write_still_finishes_the_close() {
    assert_broken_update_event_still_closes_and_cancels(
        "prdraftfault",
        "pr-draft-fault-repo",
        "pull_request_converted_to_draft",
        true,
    )
    .await;
}

/// The state transition has likewise committed before its timeline write. A
/// degraded timeline must not keep the old pull_request pipeline runnable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broken_closed_timeline_write_still_cancels_the_pipeline() {
    assert_broken_update_event_still_closes_and_cancels(
        "prclosefault",
        "pr-close-fault-repo",
        "pull_request_closed",
        false,
    )
    .await;
}

/// The other side of the same switch: cancellation must not rewrite a verdict
/// that already exists, and a PR with nothing running must close as it always
/// did. Both are the failure modes a blanket "cancel on close" would introduce.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_a_pull_request_leaves_a_finished_pipeline_alone() {
    let fixture = fixture("prdone", "pr-done-repo", true).await;
    open_pr(&fixture, "prdone", "pr-done-repo").await;
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's CI trigger").await;

    let (pipeline_id, _) = fixture.pull_request_pipeline();
    rg_db::ops::pipeline_ops::update_pipeline_status(
        &fixture.db,
        pipeline_id,
        "success",
        None,
        Some(chrono::Utc::now().naive_utc()),
    )
    .await
    .expect("finish the pipeline");

    fixture
        .set_pr_state("prdone", "pr-done-repo", "closed")
        .await;

    assert_eq!(
        fixture.pipeline_status(pipeline_id).await,
        "success",
        "closing the PR rewrote a verdict its pipeline had already reached"
    );

    fixture.server.abort();
}

/// A repository with no `on: pull_request` workflow has no run to end, and the
/// close path must not have grown a way to fail on that.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_a_pull_request_with_no_pipeline_still_closes() {
    let fixture = fixture("prbare", "pr-bare-repo", false).await;
    open_pr(&fixture, "prbare", "pr-bare-repo").await;
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's CI trigger").await;
    assert!(
        fixture.ci_engine.created.lock().unwrap().is_empty(),
        "this fixture must produce no pull_request pipeline"
    );

    fixture
        .set_pr_state("prbare", "pr-bare-repo", "closed")
        .await;

    fixture.server.abort();
}

/// card_4b114745571b: force the detached PR-open producer to publish only after
/// the close path has completed its (necessarily empty) cancellation query.
/// The producer-side recheck must cancel the graph it just created, while a
/// later reopen must still get a fresh live run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delayed_pr_open_cannot_publish_ci_after_the_pr_was_closed() {
    let fixture = fixture("prlate", "pr-late-repo", true).await;
    fixture.ci_engine.delay_next.store(true, Ordering::SeqCst);
    let entered_delay = fixture.ci_engine.entered_delay.notified();

    open_pr(&fixture, "prlate", "pr-late-repo").await;
    tokio::time::timeout(Duration::from_secs(10), entered_delay)
        .await
        .expect("the PR-open producer reached the pipeline barrier");

    fixture
        .set_pr_state("prlate", "pr-late-repo", "closed")
        .await;
    assert!(
        rg_db::ops::pipeline_ops::find_active_pipelines_by_ref(&fixture.db, 1, "refs/pull/1/head",)
            .await
            .expect("query before the delayed producer resumes")
            .is_empty(),
        "the close-side cancellation pass must finish before the delayed graph exists"
    );

    fixture.ci_engine.release_delay.notify_one();
    drain_delivery_tracker(
        &fixture.delivery_tracker,
        "the delayed PR-open producer after close",
    )
    .await;

    let (stale_pipeline_id, stale_job_id) = fixture.pull_request_pipeline();
    assert_eq!(
        fixture.pipeline_status(stale_pipeline_id).await,
        "canceled",
        "the delayed producer published active CI after the PR was closed"
    );
    assert_eq!(
        fixture.job_status(stale_job_id).await,
        "canceled",
        "producer compensation stopped at the pipeline row"
    );

    let fresh_graph_created = fixture.ci_engine.graph_created.notified();
    fixture.set_pr_state("prlate", "pr-late-repo", "open").await;
    tokio::time::timeout(Duration::from_secs(10), fresh_graph_created)
        .await
        .expect("reopening the PR created current CI");
    drain_delivery_tracker(&fixture.delivery_tracker, "the reopened PR's CI trigger").await;

    let created = fixture.ci_engine.created.lock().unwrap().clone();
    assert_eq!(
        created.len(),
        2,
        "reopen must create exactly one fresh graph"
    );
    assert_eq!(
        fixture.pipeline_status(created[1].0).await,
        "pending",
        "producer compensation suppressed the reopened PR's current run"
    );
    assert_eq!(
        fixture.pipeline_status(stale_pipeline_id).await,
        "canceled",
        "reopening the PR resurrected its stale graph"
    );

    fixture.server.abort();
}

/// The second producer reaches the same shared protocol through post-push head
/// synchronisation. While its old head is blocked, close the PR, advance the
/// stored head, and reopen it so a current run is published first. Releasing
/// the stale producer must cancel only its own id, not that newer graph on the
/// same stable PR ref.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delayed_pr_sync_cancels_only_its_stale_head_pipeline() {
    use sea_orm::Set;

    let fixture = fixture("prstale", "pr-stale-repo", true).await;
    open_pr(&fixture, "prstale", "pr-stale-repo").await;
    drain_delivery_tracker(&fixture.delivery_tracker, "the initial PR's CI trigger").await;

    fixture.ci_engine.delay_next.store(true, Ordering::SeqCst);
    let entered_delay = fixture.ci_engine.entered_delay.notified();
    let written = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/prstale/pr-stale-repo/contents/review.md",
            fixture.base
        ))
        .bearer_auth(&fixture.jwt)
        .json(&serde_json::json!({
            "content": "old delayed head\n",
            "message": "move the delayed head",
            "branch": "feature",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(written.status(), 200, "the head-branch write must succeed");
    let written: serde_json::Value = written.json().await.unwrap();
    let delayed_head = written["commit_sha"].as_str().unwrap().to_string();
    tokio::time::timeout(Duration::from_secs(10), entered_delay)
        .await
        .expect("the post-push PR producer reached the pipeline barrier");
    assert_eq!(
        fixture.pr().await.head_sha.as_deref(),
        Some(delayed_head.as_str()),
        "post-push must synchronise the snapshot before triggering its CI"
    );

    fixture
        .set_pr_state("prstale", "pr-stale-repo", "closed")
        .await;
    let current_head = "c".repeat(40);
    let mut current: rg_db::entities::pull_request::ActiveModel = fixture.pr().await.into();
    current.head_sha = Set(Some(current_head.clone()));
    rg_db::ops::pull_request_ops::update(&fixture.db, current)
        .await
        .expect("advance the closed PR to the head it will reopen on");

    let current_graph_created = fixture.ci_engine.graph_created.notified();
    fixture
        .set_pr_state("prstale", "pr-stale-repo", "open")
        .await;
    tokio::time::timeout(Duration::from_secs(10), current_graph_created)
        .await
        .expect("the reopened current head created CI");
    let current_pipeline_id = fixture.ci_engine.created.lock().unwrap()[1].0;

    fixture.ci_engine.release_delay.notify_one();
    drain_delivery_tracker(
        &fixture.delivery_tracker,
        "the delayed post-push producer after a newer reopen",
    )
    .await;

    let created = fixture.ci_engine.created.lock().unwrap().clone();
    assert_eq!(
        created.len(),
        4,
        "expected initial PR, reopened PR, delayed PR, and ordinary push graphs"
    );
    let stale_pipeline_id = created[2].0;
    assert_eq!(
        fixture.pipeline_status(stale_pipeline_id).await,
        "canceled",
        "the post-push producer kept CI for a head the PR no longer names"
    );
    assert_eq!(
        fixture.pipeline_status(current_pipeline_id).await,
        "pending",
        "stale-snapshot compensation canceled the newer current-head run"
    );
    assert!(
        fixture
            .ci_engine
            .triggered
            .lock()
            .unwrap()
            .iter()
            .any(|triggered| {
                triggered.0 == current_head
                    && triggered.1 == "refs/pull/1/head"
                    && triggered.2 == "pull_request"
            }),
        "the reopened current head never reached the pull_request producer"
    );

    fixture.server.abort();
}

// ── A fork PR's CI waits for a maintainer (card_94834ecee708) ────────────

impl Fixture {
    /// Turn this fixture's PR into a fork PR by pointing its head at another
    /// repository, the way `create_pr` does for a real fork.
    ///
    /// A real fork would need a second checkout and a cross-repository push;
    /// what the gate reads is `head_repo_id`, and the same shortcut is what
    /// `pr_permission_tests` uses for the fork half of its own assertions.
    async fn make_fork_pr(&self, head_repo_id: i64) -> rg_db::entities::pull_request::Model {
        use sea_orm::Set;
        let pr = self.pr().await;
        let mut active: rg_db::entities::pull_request::ActiveModel = pr.into();
        active.head_repo_id = Set(Some(head_repo_id));
        rg_db::ops::pull_request_ops::update(&self.db, active)
            .await
            .expect("point the PR head at the fork")
    }

    async fn pr(&self) -> rg_db::entities::pull_request::Model {
        rg_db::ops::pull_request_ops::find_by_id(&self.db, 1)
            .await
            .expect("read the PR")
            .expect("the fixture's PR must exist")
    }

    /// Ask the producer directly. The unapproved outcome is "no pipeline", and
    /// an HTTP-level assertion cannot tell that apart from "the request did
    /// something else"; `Ok(None)` from the producer can.
    async fn trigger_pr_ci(&self, pr: &rg_db::entities::pull_request::Model) -> Option<i64> {
        let ci = rg_core::pull_request::PipelineCi {
            trigger: self.ci_engine.as_ref(),
            docker_enabled: false,
            external_runners: false,
            allow_host_runner: false,
            jwt_secret: None,
            encryption_key: None,
            external_url: None,
        };
        rg_core::pull_request::trigger_pull_request_ci(&self.db, &self.repo_root, pr, None, &ci)
            .await
            .expect("the producer must not fail, it either runs or holds")
    }
}

/// card_94834ecee708. A pipeline is created under the *base* repository's id and
/// every job it produces is handed that repository's CI secrets, so an
/// unreviewed fork head must not start one on its own. The answer used to be
/// that a fork PR got no CI at all, ever — the one contribution shape that most
/// needs a check was the only one without one.
///
/// All four states in one test, because the defect this guards against is any
/// two of them collapsing: held before approval, released by it, held again once
/// the head moves past what was approved, and not grantable by the person whose
/// code it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fork_pr_runs_ci_only_for_a_head_a_maintainer_approved() {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

    let fixture = fixture("forkci", "fork-ci-repo", true).await;
    open_pr(&fixture, "forkci", "fork-ci-repo").await;
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's CI trigger").await;

    // A second account with a repository of its own — the "fork" the head lives
    // in, and the account that must not be able to approve its own code.
    let client = reqwest::Client::new();
    let (contributor, _) = register_full(&fixture.base, "forker", "forker@example.com").await;
    let fork_repo_id = create_repo(&fixture.base, &contributor, "fork-of-it").await;
    let pr = fixture.make_fork_pr(fork_repo_id).await;

    fixture.ci_engine.triggered.lock().unwrap().clear();
    fixture.ci_engine.created.lock().unwrap().clear();

    assert_eq!(
        fixture.trigger_pr_ci(&pr).await,
        None,
        "an unapproved fork head must not start a pipeline carrying this repository's CI secrets"
    );
    assert!(
        fixture.ci_engine.triggered.lock().unwrap().is_empty(),
        "the CI engine was asked to run something for an unapproved fork head"
    );

    // The contributor holds no write access to the base repository, so the
    // permission is not theirs to grant — which is the entire safety of the gate.
    let approval_url = format!(
        "{}/api/v1/repos/forkci/fork-ci-repo/pulls/1/ci-approval",
        fixture.base
    );
    let self_approved = client
        .post(&approval_url)
        .bearer_auth(&contributor)
        .send()
        .await
        .unwrap();
    assert_eq!(
        self_approved.status(),
        403,
        "the PR's own author approved their unreviewed head: {}",
        self_approved.text().await.unwrap()
    );
    assert_eq!(
        fixture.trigger_pr_ci(&pr).await,
        None,
        "a refused approval must leave the fork head held"
    );

    // A bot has real write access to the base repository, but its own verdict
    // is not the human review that permits fork code to see base CI secrets.
    let bot = client
        .post(format!("{}/api/v1/users/bots", fixture.base))
        .bearer_auth(&fixture.jwt)
        .json(&serde_json::json!({ "username": "fork-ci-bot" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bot.status(), 201, "{}", bot.text().await.unwrap());
    let bot_id = bot.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let collaborator = client
        .post(format!(
            "{}/api/v1/repos/forkci/fork-ci-repo/collaborators",
            fixture.base
        ))
        .bearer_auth(&fixture.jwt)
        .json(&serde_json::json!({ "username": "fork-ci-bot", "permission": "write" }))
        .send()
        .await
        .unwrap();
    assert!(
        collaborator.status().is_success(),
        "{}",
        collaborator.text().await.unwrap()
    );
    let token = client
        .post(format!(
            "{}/api/v1/users/bots/fork-ci-bot/tokens",
            fixture.base
        ))
        .bearer_auth(&fixture.jwt)
        .json(&serde_json::json!({ "name": "ci-review" }))
        .send()
        .await
        .unwrap();
    assert_eq!(token.status(), 201, "{}", token.text().await.unwrap());
    let token = token.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let denied = client
        .post(&approval_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let denied_status = denied.status();
    let denied_body: serde_json::Value = denied.json().await.unwrap();
    assert_eq!(denied_status, 403, "{denied_body}");
    assert_eq!(denied_body["error"]["code"], "FORBIDDEN");
    let denials = rg_db::entities::audit_log::Entity::find()
        .filter(rg_db::entities::audit_log::Column::Action.eq("agent.scope_denied"))
        .all(&fixture.db)
        .await
        .unwrap();
    assert!(denials.iter().any(|row| {
        row.user_id == Some(bot_id)
            && row.details.as_deref().is_some_and(|details| {
                let details: serde_json::Value = serde_json::from_str(details).unwrap();
                details["reason"] == "human_approval_required"
                    && details["action"] == "fork_pr_ci"
                    && details["token_id"].is_number()
            })
    }));
    // A previously stamped approval cannot release the fork after this fix.
    let mut historical: rg_db::entities::pull_request::ActiveModel = fixture.pr().await.into();
    historical.ci_approved_by = Set(Some(bot_id));
    historical.ci_approved_sha = Set(Some(fixture.head_sha.clone()));
    let historical = rg_db::ops::pull_request_ops::update(&fixture.db, historical)
        .await
        .unwrap();
    assert_eq!(fixture.trigger_pr_ci(&historical).await, None);

    let approved = client
        .post(&approval_url)
        .bearer_auth(&fixture.jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(approved.status(), 200, "{}", approved.text().await.unwrap());
    drain_delivery_tracker(&fixture.delivery_tracker, "the approval's CI trigger").await;

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
        "approving must start exactly the run the PR was waiting for: {triggered:?}"
    );

    // The head moves the way a contributor pushing again moves it. The approval
    // names a commit, so it does not follow — otherwise "approve, then push
    // whatever you like" would be the shortest path to the base repository's
    // secrets.
    let moved = "0123456789012345678901234567890123456789";
    let mut active: rg_db::entities::pull_request::ActiveModel = fixture.pr().await.into();
    active.head_sha = Set(Some(moved.to_string()));
    let moved_pr = rg_db::ops::pull_request_ops::update(&fixture.db, active)
        .await
        .unwrap();
    assert_eq!(
        moved_pr.ci_approved_sha.as_deref(),
        Some(fixture.head_sha.as_str()),
        "the approval must still name the commit it was given for"
    );

    fixture.ci_engine.triggered.lock().unwrap().clear();
    assert_eq!(
        fixture.trigger_pr_ci(&moved_pr).await,
        None,
        "a push after the approval must put the fork PR back behind the gate"
    );
    assert!(
        fixture.ci_engine.triggered.lock().unwrap().is_empty(),
        "the CI engine ran the new, unapproved head"
    );

    fixture.server.abort();
}

/// The gate is for fork PRs only. A branch inside the repository is code its
/// writers already own, so making everyone approve their own pushes would be a
/// ceremony that protects nothing — and a check that never runs is what this
/// whole phase is about.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_same_repository_pr_needs_no_approval() {
    let fixture = fixture("ownci", "own-ci-repo", true).await;
    open_pr(&fixture, "ownci", "own-ci-repo").await;
    drain_delivery_tracker(&fixture.delivery_tracker, "the PR's CI trigger").await;

    let pr = fixture.pr().await;
    assert!(
        pr.head_repo_id.is_none(),
        "this fixture's PR must not be a fork PR"
    );
    assert!(
        pr.ci_approved_sha.is_none(),
        "nothing should have approved anything here"
    );

    fixture.ci_engine.triggered.lock().unwrap().clear();
    assert!(
        fixture.trigger_pr_ci(&pr).await.is_some(),
        "a PR whose head lives in this repository must run without an approval"
    );

    fixture.server.abort();
}
