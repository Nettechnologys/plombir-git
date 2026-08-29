//! Merge queue speculative merge-group CI regression coverage.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Barrier, Notify};

use crate::common::{build_test_app_state, register_full, setup_test_db};

struct PendingMergeGroupCi;

impl rg_core::ci::CiTrigger for PendingMergeGroupCi {
    fn has_ci_config(&self, _repo_path: &std::path::Path, _commit_sha: &str) -> bool {
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
        Box::pin(async move {
            Ok(rg_db::ops::pipeline_ops::create_pipeline(
                params.db,
                params.repo_id,
                params.commit_sha,
                params.ref_name,
                params.trigger_type,
                params.triggered_by,
            )
            .await?
            .id)
        })
    }

    fn resume_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// Holds the first merge-group producer before it publishes a real graph. A
/// cancel and immediate re-enqueue can then complete in the gap, which proves
/// both halves of the ownership protocol without timing guesses.
struct DelayedMergeGroupCi {
    delay_first: AtomicBool,
    entered: Notify,
    release: Notify,
    graphs: Mutex<Vec<(String, i64, i64, i64)>>,
}

/// Holds both same-attempt producers after each has observed that no pipeline
/// exists. Releasing them together forces two complete graphs to be published;
/// the queue row's ownership CAS must elect one and retire the other.
struct ConcurrentMergeGroupCi {
    before_publication: Barrier,
    graphs: Mutex<Vec<(String, i64, i64, i64)>>,
}

impl ConcurrentMergeGroupCi {
    fn new() -> Self {
        Self {
            before_publication: Barrier::new(2),
            graphs: Mutex::new(Vec::new()),
        }
    }
}

impl rg_core::ci::CiTrigger for ConcurrentMergeGroupCi {
    fn has_ci_config(&self, _repo_path: &std::path::Path, _commit_sha: &str) -> bool {
        true
    }

    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
        false
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        Box::pin(async move {
            self.before_publication.wait().await;
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
            self.graphs.lock().unwrap().push((
                params.commit_sha.to_string(),
                pipeline.id,
                stage.id,
                job.id,
            ));
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

impl DelayedMergeGroupCi {
    fn new() -> Self {
        Self {
            delay_first: AtomicBool::new(true),
            entered: Notify::new(),
            release: Notify::new(),
            graphs: Mutex::new(Vec::new()),
        }
    }
}

impl rg_core::ci::CiTrigger for DelayedMergeGroupCi {
    fn has_ci_config(&self, _repo_path: &std::path::Path, _commit_sha: &str) -> bool {
        true
    }

    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
        // Keep PR-open CI out of this recorder: the barrier is specifically
        // between merge-group publication and queue-attempt ownership.
        false
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        let delay =
            params.trigger_type == "merge_group" && self.delay_first.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            if delay {
                self.entered.notify_one();
                self.release.notified().await;
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
            self.graphs.lock().unwrap().push((
                params.commit_sha.to_string(),
                pipeline.id,
                stage.id,
                job.id,
            ));
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

/// A repository with `main`, a `feature` branch one commit ahead, and an open
/// PR between them — the state every merge-queue question starts from.
struct QueueFixture {
    db: rg_db::DatabaseConnection,
    base: String,
    token: String,
    repo_id: i64,
    pr_id: i64,
    bare: std::path::PathBuf,
    worktree: tempfile::TempDir,
}

impl QueueFixture {
    async fn build(owner: &str, repo: &str) -> Self {
        Self::build_with_ci(owner, repo, Arc::new(PendingMergeGroupCi)).await
    }

    async fn build_with_ci(
        owner: &str,
        repo: &str,
        ci_engine: Arc<dyn rg_core::ci::CiTrigger>,
    ) -> Self {
        let (db, app_dir) = setup_test_db().await;
        let repo_root = app_dir.path().join("repos");
        std::fs::create_dir_all(&repo_root).unwrap();
        let mut state = build_test_app_state(db.clone(), repo_root.clone());
        state.ci_engine = ci_engine;
        let app = rg_http::create_router_for_test(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let base = format!("http://{addr}");
        tokio::spawn(async move {
            let _app_dir = app_dir;
            axum::serve(listener, app).await.unwrap();
        });
        crate::common::wait_for_listener(&addr).await;

        let (token, _) = register_full(&base, owner, &format!("{owner}@example.com")).await;
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .post(format!("{base}/api/v1/repos"))
                .bearer_auth(&token)
                .json(&serde_json::json!({"name": repo, "is_private": true}))
                .send()
                .await
                .unwrap()
                .status(),
            201
        );

        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let bare = repo_root.join(format!("{owner}/{repo}.git"));
        let worktree = tempfile::tempdir().unwrap();
        let worktree_path = worktree.path().to_path_buf();
        let worktree_arg = worktree_path.to_string_lossy().into_owned();
        git.run(&["init", "--initial-branch=main", &worktree_arg], None)
            .unwrap()
            .ensure_success()
            .unwrap();
        let bare_arg = bare.to_string_lossy().into_owned();
        std::fs::write(worktree_path.join("value.txt"), "base\n").unwrap();
        for args in [
            vec!["config", "user.name", "Queue Fixture"],
            vec!["config", "user.email", &format!("{owner}@example.com")],
            vec!["add", "."],
            vec!["commit", "-m", "base"],
            vec!["remote", "add", "origin", &bare_arg],
            vec!["push", "origin", "main"],
            vec!["checkout", "-b", "feature"],
        ] {
            git.run(&args, Some(&worktree_path))
                .unwrap()
                .ensure_success()
                .unwrap();
        }
        std::fs::write(worktree_path.join("value.txt"), "feature\n").unwrap();
        for args in [
            vec!["commit", "-am", "feature"],
            vec!["push", "origin", "feature"],
        ] {
            git.run(&args, Some(&worktree_path))
                .unwrap()
                .ensure_success()
                .unwrap();
        }

        let pr = client
            .post(format!("{base}/api/v1/repos/{owner}/{repo}/pulls"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "title": "Queued change",
                "head": "feature",
                "base": "main"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(pr.status(), 201, "{}", pr.text().await.unwrap());
        let pr_id = pr.json::<serde_json::Value>().await.unwrap()["id"]
            .as_i64()
            .expect("the created pull request must carry its row id");

        let repo_id = rg_db::ops::repo_ops::find_personal_by_owner_and_name(
            &db,
            rg_db::ops::user_ops::find_by_username(&db, owner)
                .await
                .unwrap()
                .unwrap()
                .id,
            repo,
        )
        .await
        .unwrap()
        .unwrap()
        .id;

        Self {
            db,
            base,
            token,
            repo_id,
            pr_id,
            bare,
            worktree,
        }
    }

    fn queue_url(&self, owner: &str, repo: &str) -> String {
        format!(
            "{}/api/v1/repos/{owner}/{repo}/pulls/1/merge-queue",
            self.base
        )
    }

    /// Looked up by PR rather than listed by repository: `list_by_repo` shows
    /// only `queued`/`running` rows, and half of what these tests assert is
    /// what a *finished* entry left behind.
    async fn entry(&self) -> rg_db::entities::merge_queue_entry::Model {
        rg_db::ops::merge_queue_ops::find_by_pr(&self.db, self.pr_id)
            .await
            .unwrap()
            .expect("the pull request must have a merge-queue entry")
    }

    async fn pipeline_status(&self, pipeline_id: i64) -> String {
        rg_db::ops::pipeline_ops::get_pipeline(&self.db, pipeline_id)
            .await
            .unwrap()
            .unwrap()
            .status
    }
}

/// A merge-group pipeline outlives its reason when the PR's head moves under
/// it: the queue builds a new group commit and the old run keeps its jobs, which
/// real runners pick up and spend real minutes on for a merge nobody will make.
///
/// The count is the sharper half of the assertion. A cancel that fired but
/// triggered nothing new, or one that left both runs active, both satisfy "the
/// old one is canceled" — only "exactly one active pipeline on this entry" says
/// the queue is still doing its job (card_13d8ebde295b).
#[tokio::test]
async fn a_moved_head_cancels_the_merge_group_pipeline_it_left_behind() {
    let fixture = QueueFixture::build("queue-stale-owner", "stale").await;
    let client = reqwest::Client::new();
    let queue_url = fixture.queue_url("queue-stale-owner", "stale");

    let queued = client
        .put(&queue_url)
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(queued.status(), 200, "{}", queued.text().await.unwrap());
    let first = fixture.entry().await;
    let stale_pipeline = first
        .merge_group_pipeline_id
        .expect("the first pass must own a merge-group pipeline");

    // Move the PR's head the way a push to the branch would.
    let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let worktree_path = fixture.worktree.path();
    std::fs::write(worktree_path.join("value.txt"), "feature again\n").unwrap();
    for args in [
        vec!["commit", "-am", "feature again"],
        vec!["push", "origin", "feature"],
    ] {
        git.run(&args, Some(worktree_path))
            .unwrap()
            .ensure_success()
            .unwrap();
    }
    let moved_head = git
        .run(&["rev-parse", "refs/heads/feature"], Some(&fixture.bare))
        .unwrap()
        .stdout_str()
        .trim()
        .to_string();
    rg_db::ops::pull_request_ops::update_open_head_sha(
        &fixture.db,
        fixture.repo_id,
        "feature",
        Some(&moved_head),
    )
    .await
    .unwrap();

    let second = client
        .put(&queue_url)
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200, "{}", second.text().await.unwrap());

    let rebuilt = fixture.entry().await;
    let fresh_pipeline = rebuilt
        .merge_group_pipeline_id
        .expect("the rebuilt group must own a pipeline");
    assert_ne!(
        fresh_pipeline, stale_pipeline,
        "the moved head must produce a new merge-group pipeline"
    );
    assert_eq!(
        fixture.pipeline_status(stale_pipeline).await,
        "canceled",
        "the merge-group pipeline for the head that moved is still burning runner time"
    );

    let active = rg_db::ops::pipeline_ops::find_active_pipelines_by_ref(
        &fixture.db,
        fixture.repo_id,
        &format!("refs/merge-queue/{}", rebuilt.id),
    )
    .await
    .unwrap();
    assert_eq!(
        active.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![fresh_pipeline],
        "the merge-group ref must carry exactly one active pipeline"
    );
}

/// Taking a PR out of the queue ends the question its merge-group pipeline was
/// asked. Before this, the entry went to `canceled` and the run it named kept
/// going — with nothing left in the system that would ever come back for it.
#[tokio::test]
async fn leaving_the_queue_cancels_the_merge_group_pipeline() {
    let fixture = QueueFixture::build("queue-leave-owner", "leave").await;
    let client = reqwest::Client::new();
    let queue_url = fixture.queue_url("queue-leave-owner", "leave");

    let queued = client
        .put(&queue_url)
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(queued.status(), 200, "{}", queued.text().await.unwrap());
    let pipeline_id = fixture
        .entry()
        .await
        .merge_group_pipeline_id
        .expect("the queued entry must own a merge-group pipeline");
    assert_eq!(
        fixture.pipeline_status(pipeline_id).await,
        "pending",
        "the fixture must leave a live pipeline for the cancel to act on"
    );

    let canceled = client
        .delete(&queue_url)
        .bearer_auth(&fixture.token)
        .send()
        .await
        .unwrap();
    assert!(
        canceled.status().is_success(),
        "{}",
        canceled.text().await.unwrap()
    );

    assert_eq!(fixture.entry().await.status, "canceled");
    assert_eq!(
        fixture.pipeline_status(pipeline_id).await,
        "canceled",
        "the merge-group pipeline outlived the queue entry that was waiting for it"
    );
}

/// The cancellation query cannot cancel a graph that has not been inserted
/// yet. The producer must therefore prove that the exact queue attempt it
/// started for still owns the result, and compensate only its own graph when a
/// cancel plus re-enqueue won the race.
#[tokio::test]
async fn a_delayed_merge_group_producer_cannot_publish_after_its_attempt_was_canceled() {
    let ci = Arc::new(DelayedMergeGroupCi::new());
    let fixture = QueueFixture::build_with_ci("queue-race-owner", "race", ci.clone()).await;
    let client = reqwest::Client::new();
    let queue_url = fixture.queue_url("queue-race-owner", "race");

    let old_request = tokio::spawn({
        let client = client.clone();
        let queue_url = queue_url.clone();
        let token = fixture.token.clone();
        async move {
            client
                .put(queue_url)
                .bearer_auth(token)
                .json(&serde_json::json!({"strategy": "merge"}))
                .send()
                .await
                .unwrap()
        }
    });
    tokio::time::timeout(Duration::from_secs(30), ci.entered.notified())
        .await
        .expect("the first queue pass reached the CI publication barrier");

    let canceled = client
        .delete(&queue_url)
        .bearer_auth(&fixture.token)
        .send()
        .await
        .unwrap();
    assert!(
        canceled.status().is_success(),
        "{}",
        canceled.text().await.unwrap()
    );
    let canceled_entry = fixture.entry().await;
    assert_eq!(canceled_entry.status, "canceled");
    assert!(canceled_entry.merge_group_pipeline_id.is_none());
    let group_ref = format!("refs/merge-queue/{}", canceled_entry.id);
    let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    assert!(
        !git.run(&["rev-parse", "--verify", &group_ref], Some(&fixture.bare))
            .unwrap()
            .success(),
        "the terminal attempt must own neither a pipeline nor a synthetic ref"
    );

    // Recycle the same row before the stale producer resumes. The attempt
    // number, not the row id, is what keeps its later write out of this run.
    let requeued = client
        .put(&queue_url)
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(requeued.status(), 200, "{}", requeued.text().await.unwrap());
    let current_entry = fixture.entry().await;
    assert_eq!(
        current_entry.attempt_number,
        canceled_entry.attempt_number + 1
    );
    let current_pipeline_id = current_entry
        .merge_group_pipeline_id
        .expect("the re-enqueued attempt must own its pipeline");
    let current_group_sha = current_entry
        .merge_group_sha
        .clone()
        .expect("the re-enqueued attempt must own its group commit");

    ci.release.notify_one();
    let old_response = tokio::time::timeout(Duration::from_secs(30), old_request)
        .await
        .expect("the delayed queue request drained")
        .expect("the delayed queue task did not panic");
    assert_eq!(
        old_response.status(),
        200,
        "{}",
        old_response.text().await.unwrap()
    );

    let graphs = ci.graphs.lock().unwrap().clone();
    assert_eq!(graphs.len(), 2, "one graph per queue attempt");
    let (_, stale_pipeline_id, stale_stage_id, stale_job_id) = graphs
        .iter()
        .find(|(_, pipeline_id, _, _)| *pipeline_id != current_pipeline_id)
        .cloned()
        .expect("the delayed producer's graph was recorded");
    assert_eq!(fixture.pipeline_status(stale_pipeline_id).await, "canceled");
    assert_eq!(
        rg_db::ops::pipeline_ops::get_stage_by_id(&fixture.db, stale_stage_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "canceled"
    );
    let stale_jobs =
        rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&fixture.db, stale_pipeline_id)
            .await
            .unwrap();
    assert_eq!(stale_jobs.len(), 1);
    assert_eq!(stale_jobs[0].id, stale_job_id);
    assert_eq!(stale_jobs[0].status, "canceled");

    let final_entry = fixture.entry().await;
    assert_eq!(final_entry.attempt_number, current_entry.attempt_number);
    assert_eq!(
        final_entry.merge_group_pipeline_id,
        Some(current_pipeline_id)
    );
    assert_eq!(
        final_entry.merge_group_sha.as_deref(),
        Some(current_group_sha.as_str())
    );
    assert_eq!(
        fixture.pipeline_status(current_pipeline_id).await,
        "pending"
    );
    let ref_sha = git
        .run(&["rev-parse", "--verify", &group_ref], Some(&fixture.bare))
        .unwrap();
    ref_sha.ensure_success().unwrap();
    assert_eq!(ref_sha.stdout_str().trim(), current_group_sha);
    assert_eq!(
        rg_db::ops::pipeline_ops::find_active_pipelines_by_ref(
            &fixture.db,
            fixture.repo_id,
            &group_ref,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|pipeline| pipeline.id)
        .collect::<Vec<_>>(),
        vec![current_pipeline_id],
        "stale cleanup must preserve the new attempt's ref and graph"
    );
}

/// Both requests cross the lookup boundary before either publishes its graph.
/// The queue entry must therefore act as the election point: one graph remains
/// pending and owned, while the other is canceled all the way through its job.
#[tokio::test]
async fn concurrent_queue_passes_keep_one_pipeline_for_the_same_attempt() {
    let ci = Arc::new(ConcurrentMergeGroupCi::new());
    let fixture = QueueFixture::build_with_ci("queue-dual-owner", "dual", ci.clone()).await;
    let client = reqwest::Client::new();
    let queue_url = fixture.queue_url("queue-dual-owner", "dual");

    let request = |client: reqwest::Client, token: String, queue_url: String| async move {
        client
            .put(queue_url)
            .bearer_auth(token)
            .json(&serde_json::json!({"strategy": "merge"}))
            .send()
            .await
            .unwrap()
    };
    let (first, second) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(
            request(client.clone(), fixture.token.clone(), queue_url.clone()),
            request(client, fixture.token.clone(), queue_url)
        )
    })
    .await
    .expect("both same-attempt queue passes crossed and drained");
    assert_eq!(first.status(), 200, "{}", first.text().await.unwrap());
    assert_eq!(second.status(), 200, "{}", second.text().await.unwrap());

    let entry = fixture.entry().await;
    let winner_id = entry
        .merge_group_pipeline_id
        .expect("one pipeline must own the live queue attempt");
    let group_sha = entry
        .merge_group_sha
        .clone()
        .expect("the winning graph must own the deterministic merge commit");
    let graphs = ci.graphs.lock().unwrap().clone();
    assert_eq!(graphs.len(), 2, "the barrier must publish two full graphs");
    assert!(graphs.iter().all(|(sha, _, _, _)| sha == &group_sha));

    let (_, _, winner_stage_id, winner_job_id) = graphs
        .iter()
        .find(|(_, pipeline_id, _, _)| *pipeline_id == winner_id)
        .cloned()
        .expect("the queue row's pipeline must be one of the published graphs");
    let (_, loser_id, loser_stage_id, loser_job_id) = graphs
        .iter()
        .find(|(_, pipeline_id, _, _)| *pipeline_id != winner_id)
        .cloned()
        .expect("the other publication must be identifiable");

    for (kind, status) in [
        ("winning pipeline", fixture.pipeline_status(winner_id).await),
        (
            "winning stage",
            rg_db::ops::pipeline_ops::get_stage_by_id(&fixture.db, winner_stage_id)
                .await
                .unwrap()
                .unwrap()
                .status,
        ),
        (
            "winning job",
            rg_db::ops::pipeline_ops::get_job(&fixture.db, winner_job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
        ),
    ] {
        assert_eq!(status, "pending", "{kind} must remain runnable");
    }
    for (kind, status) in [
        ("losing pipeline", fixture.pipeline_status(loser_id).await),
        (
            "losing stage",
            rg_db::ops::pipeline_ops::get_stage_by_id(&fixture.db, loser_stage_id)
                .await
                .unwrap()
                .unwrap()
                .status,
        ),
        (
            "losing job",
            rg_db::ops::pipeline_ops::get_job(&fixture.db, loser_job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
        ),
    ] {
        assert_eq!(status, "canceled", "{kind} must be fully retired");
    }

    let group_ref = format!("refs/merge-queue/{}", entry.id);
    let active = rg_db::ops::pipeline_ops::find_active_pipelines_by_ref(
        &fixture.db,
        fixture.repo_id,
        &group_ref,
    )
    .await
    .unwrap();
    assert_eq!(
        active
            .iter()
            .map(|pipeline| pipeline.id)
            .collect::<Vec<_>>(),
        vec![winner_id],
        "the attempt must expose exactly its winning active graph"
    );
    let ref_sha = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .unwrap()
        .run(&["rev-parse", "--verify", &group_ref], Some(&fixture.bare))
        .unwrap();
    ref_sha.ensure_success().unwrap();
    assert_eq!(ref_sha.stdout_str().trim(), group_sha);
}

#[tokio::test]
async fn queue_waits_for_speculative_merge_group_ci_before_updating_base() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = Arc::new(PendingMergeGroupCi);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let (token, _) = register_full(&base, "queue-ci-owner", "queue-ci@example.com").await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "speculative", "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);

    let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let bare = repo_root.join("queue-ci-owner/speculative.git");
    let worktree = tempfile::tempdir().unwrap();
    let worktree_path = worktree.path();
    let worktree_arg = worktree_path.to_string_lossy();
    git.run(&["init", "--initial-branch=main", &worktree_arg], None)
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["config", "user.name", "Queue CI"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(
        &["config", "user.email", "queue-ci@example.com"],
        Some(worktree_path),
    )
    .unwrap()
    .ensure_success()
    .unwrap();
    std::fs::write(worktree_path.join("value.txt"), "base\n").unwrap();
    git.run(&["add", "."], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["commit", "-m", "base"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    let bare_arg = bare.to_string_lossy();
    git.run(&["remote", "add", "origin", &bare_arg], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["push", "origin", "main"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["checkout", "-b", "feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    std::fs::write(worktree_path.join("value.txt"), "feature\n").unwrap();
    git.run(&["commit", "-am", "feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["push", "origin", "feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    let base_before = git
        .run(&["rev-parse", "refs/heads/main"], Some(&bare))
        .unwrap()
        .stdout_str()
        .trim()
        .to_string();

    let client = reqwest::Client::new();
    let pr = client
        .post(format!(
            "{base}/api/v1/repos/queue-ci-owner/speculative/pulls"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "Speculative merge",
            "head": "feature",
            "base": "main"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pr.status(), 201, "{}", pr.text().await.unwrap());

    let queue_url = format!("{base}/api/v1/repos/queue-ci-owner/speculative/pulls/1/merge-queue");
    let queued = client
        .put(&queue_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(queued.status(), 200, "{}", queued.text().await.unwrap());
    let queued = rg_db::ops::merge_queue_ops::list_by_repo(
        &db,
        rg_db::ops::repo_ops::find_personal_by_owner_and_name(
            &db,
            rg_db::ops::user_ops::find_by_username(&db, "queue-ci-owner")
                .await
                .unwrap()
                .unwrap()
                .id,
            "speculative",
        )
        .await
        .unwrap()
        .unwrap()
        .id,
    )
    .await
    .unwrap()
    .remove(0);
    assert!(queued.merge_group_sha.is_some());
    let pipeline_id = queued.merge_group_pipeline_id.unwrap();
    assert_eq!(
        git.run(&["rev-parse", "refs/heads/main"], Some(&bare))
            .unwrap()
            .stdout_str()
            .trim(),
        base_before
    );

    rg_db::ops::pipeline_ops::update_pipeline_status(
        &db,
        pipeline_id,
        "success",
        None,
        Some(chrono::Utc::now().naive_utc()),
    )
    .await
    .unwrap();
    let processed = client
        .put(&queue_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        processed.status(),
        200,
        "{}",
        processed.text().await.unwrap()
    );
    let pr = client
        .get(format!(
            "{base}/api/v1/repos/queue-ci-owner/speculative/pulls/1"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(pr["state"], "merged");
    assert_ne!(
        git.run(&["rev-parse", "refs/heads/main"], Some(&bare))
            .unwrap()
            .stdout_str()
            .trim(),
        base_before
    );
}

/// card_55282a865b8e: the merge-group pipeline is created before the row that
/// owns it, so a failed `set_merge_group` left the entry owning nothing — and
/// because the reuse branch recognises a group only by the SHAs on that row,
/// every later pass built another group commit and triggered another pipeline
/// while the PR waited forever. `clear_merge_group` reproduces exactly the state
/// the failed write leaves behind, and two passes must still leave one pipeline.
#[tokio::test]
async fn a_queue_pass_adopts_the_merge_group_pipeline_instead_of_triggering_another() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = Arc::new(PendingMergeGroupCi);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let (token, _) = register_full(&base, "queue-adopt-owner", "queue-adopt@example.com").await;
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": "adopt", "is_private": true}))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );

    let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let bare = repo_root.join("queue-adopt-owner/adopt.git");
    let worktree = tempfile::tempdir().unwrap();
    let worktree_path = worktree.path();
    let worktree_arg = worktree_path.to_string_lossy();
    for args in [
        vec!["init", "--initial-branch=main", &worktree_arg],
        vec!["-C", &worktree_arg, "config", "user.name", "Queue Adopt"],
        vec![
            "-C",
            &worktree_arg,
            "config",
            "user.email",
            "queue-adopt@example.com",
        ],
    ] {
        git.run(&args, None).unwrap().ensure_success().unwrap();
    }
    std::fs::write(worktree_path.join("value.txt"), "base\n").unwrap();
    let bare_arg = bare.to_string_lossy();
    for args in [
        vec!["add", "."],
        vec!["commit", "-m", "base"],
        vec!["remote", "add", "origin", &bare_arg],
        vec!["push", "origin", "main"],
        vec!["checkout", "-b", "feature"],
    ] {
        git.run(&args, Some(worktree_path))
            .unwrap()
            .ensure_success()
            .unwrap();
    }
    std::fs::write(worktree_path.join("value.txt"), "feature\n").unwrap();
    for args in [
        vec!["commit", "-am", "feature"],
        vec!["push", "origin", "feature"],
    ] {
        git.run(&args, Some(worktree_path))
            .unwrap()
            .ensure_success()
            .unwrap();
    }

    assert_eq!(
        client
            .post(format!("{base}/api/v1/repos/queue-adopt-owner/adopt/pulls"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "title": "Adopted merge group",
                "head": "feature",
                "base": "main"
            }))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );

    let repo_id = rg_db::ops::repo_ops::find_personal_by_owner_and_name(
        &db,
        rg_db::ops::user_ops::find_by_username(&db, "queue-adopt-owner")
            .await
            .unwrap()
            .unwrap()
            .id,
        "adopt",
    )
    .await
    .unwrap()
    .unwrap()
    .id;

    // Enqueue without running the queue, then move the entry an hour into the
    // past: the group commit's date is pinned to `created_at`, so a wall-clock
    // date is now distinguishable from the pinned one instead of landing in the
    // same second as it would if the entry were fresh.
    let pr_id = rg_db::ops::pull_request_ops::find_by_repo_and_number(&db, repo_id, 1)
        .await
        .unwrap()
        .unwrap()
        .id;
    let owner_id = rg_db::ops::user_ops::find_by_username(&db, "queue-adopt-owner")
        .await
        .unwrap()
        .unwrap()
        .id;
    let entry = rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr_id, owner_id, "merge")
        .await
        .unwrap()
        .expect("the fixture repository and pull request remain live");
    let enqueued_at = chrono::Utc::now() - chrono::Duration::hours(1);
    let mut backdated: rg_db::entities::merge_queue_entry::ActiveModel = entry.into();
    backdated.created_at = sea_orm::Set(enqueued_at);
    sea_orm::ActiveModelTrait::update(backdated, &db)
        .await
        .unwrap();

    let queue_url = format!("{base}/api/v1/repos/queue-adopt-owner/adopt/pulls/1/merge-queue");
    assert_eq!(
        client
            .put(&queue_url)
            .bearer_auth(&token)
            .json(&serde_json::json!({"strategy": "merge"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    let queued = rg_db::ops::merge_queue_ops::list_by_repo(&db, repo_id)
        .await
        .unwrap()
        .remove(0);
    let group_sha = queued
        .merge_group_sha
        .clone()
        .expect("the first pass must have built a merge group");
    let pipeline_id = queued
        .merge_group_pipeline_id
        .expect("the first pass must have triggered a merge-group pipeline");
    // Opening the PR triggers a `pull_request` pipeline of its own, so only the
    // merge-group ones are the queue's doing.
    let merge_group_pipelines = |db: sea_orm::DatabaseConnection| async move {
        rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&db, repo_id, 0, 100)
            .await
            .unwrap()
            .0
            .into_iter()
            .filter(|pipeline| pipeline.trigger_type == "merge_group")
            .map(|pipeline| pipeline.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(merge_group_pipelines(db.clone()).await, vec![pipeline_id]);

    // The commit date is what makes the group SHA reproducible, and it must come
    // from the entry, not from the clock.
    assert_eq!(
        git.run(&["show", "-s", "--format=%ct", &group_sha], Some(&bare))
            .unwrap()
            .stdout_str()
            .trim(),
        enqueued_at.timestamp().to_string(),
        "the group commit must be dated by the queue entry, or its SHA drifts on every pass"
    );

    // The state a failed `set_merge_group` leaves: the pipeline is running, the
    // row that pointed at it is gone.
    rg_db::ops::merge_queue_ops::clear_merge_group(&db, queued.id, queued.attempt_number)
        .await
        .unwrap();

    let second = client
        .put(&queue_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200);
    let second: serde_json::Value = second.json().await.unwrap();

    assert_eq!(
        merge_group_pipelines(db.clone()).await,
        vec![pipeline_id],
        "the second pass must adopt the merge-group pipeline, not trigger another one"
    );

    let entry = rg_db::ops::merge_queue_ops::find_by_pr(&db, queued.pr_id)
        .await
        .unwrap()
        .expect("the entry must still be there");
    assert_eq!(
        entry.merge_group_pipeline_id,
        Some(pipeline_id),
        "the entry must own the pipeline that already exists"
    );
    assert_eq!(
        entry.merge_group_sha.as_deref(),
        Some(group_sha.as_str()),
        "the group commit must be rebuilt byte-identically, or nothing can find its pipeline"
    );
    assert_eq!(
        entry.status, "queued",
        "the PR is still waiting for CI, not failed"
    );
    assert_eq!(
        second["process"]["waiting_reason"],
        serde_json::json!(format!("merge-group pipeline #{pipeline_id} is pending")),
        "the queue must name the pipeline it is actually waiting for"
    );
}

/// A merge-queue failure must persist the reason a PR can no longer merge.
/// A deleted head branch is now detected before the gix merge path, so the
/// stored answer must name that missing branch rather than a later git failure.
#[tokio::test]
async fn a_failed_merge_persists_the_missing_head_branch_reason() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = Arc::new(PendingMergeGroupCi);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let (token, _) = register_full(&base, "queue-fail-owner", "queue-fail@example.com").await;
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": "flatten", "is_private": true}))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );

    let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let bare = repo_root.join("queue-fail-owner/flatten.git");
    let worktree = tempfile::tempdir().unwrap();
    let worktree_path = worktree.path();
    let worktree_arg = worktree_path.to_string_lossy();
    for args in [
        vec!["init", "--initial-branch=main", &worktree_arg],
        vec!["-C", &worktree_arg, "config", "user.name", "Queue Fail"],
        vec![
            "-C",
            &worktree_arg,
            "config",
            "user.email",
            "queue-fail@example.com",
        ],
    ] {
        git.run(&args, None).unwrap().ensure_success().unwrap();
    }
    std::fs::write(worktree_path.join("value.txt"), "base\n").unwrap();
    let bare_arg = bare.to_string_lossy();
    for args in [
        vec!["add", "."],
        vec!["commit", "-m", "base"],
        vec!["remote", "add", "origin", &bare_arg],
        vec!["push", "origin", "main"],
        vec!["checkout", "-b", "feature"],
    ] {
        git.run(&args, Some(worktree_path))
            .unwrap()
            .ensure_success()
            .unwrap();
    }
    std::fs::write(worktree_path.join("value.txt"), "feature\n").unwrap();
    for args in [
        vec!["commit", "-am", "feature"],
        vec!["push", "origin", "feature"],
    ] {
        git.run(&args, Some(worktree_path))
            .unwrap()
            .ensure_success()
            .unwrap();
    }

    assert_eq!(
        client
            .post(format!(
                "{base}/api/v1/repos/queue-fail-owner/flatten/pulls"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "title": "Doomed merge",
                "head": "feature",
                "base": "main"
            }))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );

    let queue_url = format!("{base}/api/v1/repos/queue-fail-owner/flatten/pulls/1/merge-queue");
    assert_eq!(
        client
            .put(&queue_url)
            .bearer_auth(&token)
            .json(&serde_json::json!({"strategy": "merge"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    let repo_id = rg_db::ops::repo_ops::find_personal_by_owner_and_name(
        &db,
        rg_db::ops::user_ops::find_by_username(&db, "queue-fail-owner")
            .await
            .unwrap()
            .unwrap()
            .id,
        "flatten",
    )
    .await
    .unwrap()
    .unwrap()
    .id;
    let queued = rg_db::ops::merge_queue_ops::list_by_repo(&db, repo_id)
        .await
        .unwrap()
        .remove(0);
    rg_db::ops::pipeline_ops::update_pipeline_status(
        &db,
        queued.merge_group_pipeline_id.unwrap(),
        "success",
        None,
        Some(chrono::Utc::now().naive_utc()),
    )
    .await
    .unwrap();

    // Drop the branch the merge will need. The merge-group CI step only reads
    // the base ref and the stored head SHA, so it still reports Ready; the
    // merge itself must then persist the early head-branch validation failure.
    git.run(&["update-ref", "-d", "refs/heads/feature"], Some(&bare))
        .unwrap()
        .ensure_success()
        .unwrap();

    assert_eq!(
        client
            .put(&queue_url)
            .bearer_auth(&token)
            .json(&serde_json::json!({"strategy": "merge"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // `list_by_repo` only returns queued/running entries, so a finished one has
    // to be looked up by PR.
    let entry = rg_db::ops::merge_queue_ops::find_by_pr(&db, queued.pr_id)
        .await
        .unwrap()
        .expect("the queue entry must still be there");
    assert_eq!(entry.status, "failed", "the merge was expected to fail");
    let reason = entry
        .failure_reason
        .expect("a failed entry must carry a reason");
    assert!(
        reason.contains("pull request head branch 'feature' no longer exists"),
        "the persisted reason must name the deleted head branch: {reason}"
    );
}
