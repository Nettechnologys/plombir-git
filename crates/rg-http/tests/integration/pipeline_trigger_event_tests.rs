//! card_e87a1b6f9633: `pipeline.trigger_type` is the *event name*, not a label.
//!
//! It is what `Workflow::matches_event` is asked about and what reaches the job
//! as `CI_EVENT` / `${{ github.event_name }}`. The Run button used to send
//! `"manual"` and Retry `"retry"` — two words no `on:` clause can carry — so a
//! repository whose CI lives in `.gitea/workflows/` got an error back from both
//! buttons for a completely valid file, and `github.event_name` inside a job was
//! a name that exists nowhere in the Actions vocabulary.
//!
//! `rg-ci`'s own tests cover the matcher; these cover what the handlers *send*,
//! which is the half the matcher cannot fix on its own.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

#[derive(Default)]
struct RecordingCiEngine {
    triggered: Mutex<Vec<String>>,
}

impl rg_core::ci::CiTrigger for RecordingCiEngine {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        true
    }

    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
        true
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        let event = params.trigger_type.to_string();
        Box::pin(async move {
            self.triggered.lock().unwrap().push(event);
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

struct Harness {
    base: String,
    token: String,
    db: rg_db::DatabaseConnection,
    repo_id: i64,
    engine: Arc<RecordingCiEngine>,
    delivery_tracker: rg_core::task_tracker::TaskTracker,
}

impl Harness {
    /// Let every detached post-push hook finish, then forget what it recorded.
    ///
    /// The fixture commit runs the same hooks a `git push` does and triggers a
    /// `push` pipeline of its own, on a task the request does not wait for — so
    /// without this the assertion below races that task instead of reading the
    /// button's own trigger.
    async fn settle(&self) {
        self.delivery_tracker.close();
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            self.delivery_tracker.wait(),
        )
        .await
        .expect("post-push hooks drained within timeout");
        self.delivery_tracker.reopen();
        self.engine.triggered.lock().unwrap().clear();
    }
}

async fn harness(suffix: &str) -> Harness {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let engine = Arc::new(RecordingCiEngine::default());
    let mut state = build_test_app_state(db.clone(), repo_root);
    state.ci_engine = engine.clone();
    let delivery_tracker = state.delivery_tracker.clone();
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let owner = format!("trg{suffix}");
    let (token, _) = register_full(&base, &owner, &format!("{owner}@example.test")).await;
    let repo_id = crate::common::create_repo(&base, &token, "trg-repo").await;

    Harness {
        base,
        token,
        db,
        repo_id,
        engine,
        delivery_tracker,
    }
}

/// The Run button asks for the event the Actions vocabulary calls a manual run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_run_button_triggers_the_workflow_dispatch_event() {
    let h = harness("run").await;
    let client = reqwest::Client::new();

    // A commit for HEAD to resolve to — the handler reads the ref before it
    // reaches the engine.
    let written = client
        .post(format!(
            "{}/api/v1/repos/trgrun/trg-repo/contents/README.md",
            h.base
        ))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({"content": "hello\n", "message": "init"}))
        .send()
        .await
        .expect("write a file");
    assert_eq!(written.status(), 200, "the fixture commit must land");
    h.settle().await;

    let resp = client
        .post(format!("{}/api/v1/repos/trgrun/trg-repo/pipelines", h.base))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("trigger pipeline");
    assert_eq!(resp.status(), 201, "the Run button must start a pipeline");

    assert_eq!(
        h.engine.triggered.lock().unwrap().as_slice(),
        [rg_core::ci::WORKFLOW_DISPATCH_EVENT.to_string()],
        "the manual run asked for an event no `on:` clause can declare"
    );
}

/// A retry re-runs the pipeline, so it re-runs its event — `"retry"` was a name
/// the matcher answered `false` to for every workflow ever written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retry_runs_under_the_event_that_produced_the_pipeline() {
    let h = harness("retry").await;
    let client = reqwest::Client::new();

    for original in ["push", "pull_request", rg_core::ci::WORKFLOW_DISPATCH_EVENT] {
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &h.db,
            h.repo_id,
            "2222222222222222222222222222222222222222",
            "refs/heads/main",
            original,
            None,
        )
        .await
        .expect("seed the pipeline being retried");
        h.engine.triggered.lock().unwrap().clear();

        let resp = client
            .post(format!(
                "{}/api/v1/repos/trgretry/trg-repo/pipelines/{}/retry",
                h.base, pipeline.id
            ))
            .bearer_auth(&h.token)
            .send()
            .await
            .expect("retry pipeline");
        assert_eq!(resp.status(), 201, "retrying a {original} pipeline");

        assert_eq!(
            h.engine.triggered.lock().unwrap().as_slice(),
            [original.to_string()],
            "the retry of a {original} pipeline ran under a different event"
        );
    }
}
