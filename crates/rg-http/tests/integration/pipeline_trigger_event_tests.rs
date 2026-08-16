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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};
use tokio::sync::Notify;

struct TriggerGate {
    entered: Notify,
    release: Notify,
}

#[derive(Default)]
struct RecordingCiEngine {
    triggered: Mutex<Vec<String>>,
    refs: Mutex<Vec<String>>,
    dispatch_inputs: Mutex<Vec<std::collections::HashMap<String, String>>>,
    /// The other two values a retry has to replay off the pipeline row
    /// (card_74d58ec3ac1e). Recorded as `Option` because "the run had none" is
    /// the answer that has to survive too: `Some(default branch)` would be the
    /// engine inventing one.
    base_branches: Mutex<Vec<Option<String>>>,
    previous_shas: Mutex<Vec<Option<String>>>,
    dispatch_schema: Mutex<rg_core::ci::WorkflowDispatchSchema>,
    schema_queries: Mutex<Vec<(PathBuf, String)>>,
    pipeline_ids: Mutex<Vec<i64>>,
    gate: Mutex<Option<Arc<TriggerGate>>>,
    no_match_ref: Mutex<Option<String>>,
}

impl RecordingCiEngine {
    fn gate_next_trigger(&self) -> Arc<TriggerGate> {
        let gate = Arc::new(TriggerGate {
            entered: Notify::new(),
            release: Notify::new(),
        });
        let previous = self.gate.lock().unwrap().replace(gate.clone());
        assert!(
            previous.is_none(),
            "only one trigger can be gated at a time"
        );
        gate
    }

    fn refuse_next_trigger_for_ref(&self, ref_name: &str) {
        let previous = self
            .no_match_ref
            .lock()
            .unwrap()
            .replace(ref_name.to_string());
        assert!(
            previous.is_none(),
            "only one refusal can be armed at a time"
        );
    }
}

impl rg_core::ci::CiTrigger for RecordingCiEngine {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        true
    }

    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
        true
    }

    fn workflow_dispatch_schema(
        &self,
        query: rg_core::ci::WorkflowDispatchSchemaQuery<'_>,
    ) -> anyhow::Result<rg_core::ci::WorkflowDispatchSchema> {
        self.schema_queries
            .lock()
            .unwrap()
            .push((query.repo_path.to_path_buf(), query.commit_sha.to_owned()));
        Ok(self.dispatch_schema.lock().unwrap().clone())
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        let event = params.trigger_type.to_string();
        let ref_name = params.ref_name.to_string();
        let inputs = params.inputs.cloned().unwrap_or_default();
        let base_branch = params.base_branch.map(str::to_string);
        let previous_sha = params.previous_sha.map(str::to_string);
        let gate = self.gate.lock().unwrap().take();
        let no_match_ref = self.no_match_ref.lock().unwrap().take();
        Box::pin(async move {
            if let Some(ref_name) = no_match_ref {
                return Err(anyhow::Error::new(rg_core::ci::NoMatchingCiJobs::new(
                    ref_name,
                )));
            }
            let build_full_graph = gate.is_some();
            if let Some(gate) = &gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            self.triggered.lock().unwrap().push(event);
            self.refs.lock().unwrap().push(ref_name);
            // The real engine records the caller's dispatch inputs on the row
            // so a retry can replay them; the double has to do the same or the
            // retry handler is tested against a row production never writes.
            let stored_inputs = (!inputs.is_empty())
                .then(|| serde_json::to_string(&inputs))
                .transpose()?;
            self.dispatch_inputs.lock().unwrap().push(inputs);
            self.base_branches.lock().unwrap().push(base_branch.clone());
            self.previous_shas
                .lock()
                .unwrap()
                .push(previous_sha.clone());
            let pipeline = rg_db::ops::pipeline_ops::create_pipeline_row(
                params.db,
                rg_db::ops::pipeline_ops::NewPipeline {
                    repo_id: params.repo_id,
                    commit_sha: params.commit_sha,
                    ref_name: params.ref_name,
                    trigger_type: params.trigger_type,
                    triggered_by: params.triggered_by,
                    concurrency_group: None,
                    dispatch_inputs: stored_inputs.as_deref(),
                    base_branch: base_branch.as_deref(),
                    previous_sha: previous_sha.as_deref(),
                },
            )
            .await?;
            if build_full_graph {
                let stage =
                    rg_db::ops::pipeline_ops::create_stage(params.db, pipeline.id, "race-stage", 0)
                        .await?;
                rg_db::ops::pipeline_ops::create_job(
                    params.db,
                    stage.id,
                    "race-job",
                    "echo should-not-run",
                    None,
                    Some(r#"["linux"]"#),
                    None,
                    None,
                    None,
                    false,
                    None,
                    None,
                    None,
                )
                .await?;
            }
            self.pipeline_ids.lock().unwrap().push(pipeline.id);
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

struct Harness {
    base: String,
    token: String,
    db: rg_db::DatabaseConnection,
    repo_id: i64,
    owner: String,
    repo_path: PathBuf,
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
        self.engine.refs.lock().unwrap().clear();
        self.engine.dispatch_inputs.lock().unwrap().clear();
        self.engine.base_branches.lock().unwrap().clear();
        self.engine.previous_shas.lock().unwrap().clear();
        self.engine.schema_queries.lock().unwrap().clear();
        self.engine.pipeline_ids.lock().unwrap().clear();
        *self.engine.gate.lock().unwrap() = None;
        *self.engine.no_match_ref.lock().unwrap() = None;
    }
}

async fn harness(suffix: &str) -> Harness {
    harness_on_branch(suffix, None).await
}

async fn harness_on_branch(suffix: &str, default_branch: Option<&str>) -> Harness {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let engine = Arc::new(RecordingCiEngine::default());
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
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
    let repo_id = match default_branch {
        None => crate::common::create_repo(&base, &token, "trg-repo").await,
        Some(branch) => {
            let created = reqwest::Client::new()
                .post(format!("{base}/api/v1/repos"))
                .bearer_auth(&token)
                .json(&serde_json::json!({"name": "trg-repo", "default_branch": branch}))
                .send()
                .await
                .expect("create repo");
            assert_eq!(created.status(), 201);
            created.json::<serde_json::Value>().await.expect("body")["id"]
                .as_i64()
                .expect("repo id")
        }
    };

    Harness {
        base,
        token,
        db,
        repo_id,
        repo_path: repo_root.join(&owner).join("trg-repo.git"),
        owner,
        engine,
        delivery_tracker,
    }
}

async fn write_default_branch(h: &Harness, filename: &str) {
    let response = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/{}/trg-repo/contents/{filename}",
            h.base, h.owner
        ))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({"content": "hello\n", "message": "fixture commit"}))
        .send()
        .await
        .expect("write fixture commit");
    assert_eq!(response.status(), 200, "the fixture commit must land");
}

fn ref_sha(repo_path: &Path, ref_name: &str) -> String {
    let repo = gix::open(repo_path).expect("open fixture repository");
    let mut reference = repo
        .find_reference(ref_name)
        .expect("fixture ref must exist");
    reference
        .peel_to_id()
        .expect("fixture ref must peel to a commit")
        .to_string()
}

fn delete_ref(repo_path: &Path, ref_name: &str) {
    use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};

    let repo = gix::open(repo_path).expect("open fixture repository");
    repo.edit_reference(RefEdit {
        change: Change::Delete {
            expected: PreviousValue::Any,
            log: RefLog::AndReference,
        },
        name: ref_name.try_into().expect("valid fixture ref name"),
        deref: false,
    })
    .expect("delete fixture ref");
}

/// card_d0e36a8901d8: the web form must inspect the same immutable revision the
/// subsequent manual trigger will execute, through the injected CI engine rather
/// than by teaching `rg-http` to parse Actions YAML itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_manual_run_form_reads_the_selected_refs_dispatch_schema() {
    let h = harness("schema").await;
    write_default_branch(&h, "README.md").await;
    h.settle().await;
    let expected_sha = ref_sha(&h.repo_path, "refs/heads/main");
    let input = rg_core::ci::WorkflowDispatchInput {
        name: "target".into(),
        description: Some("Where to deploy".into()),
        required: true,
        input_type: "choice".into(),
        default: Some("staging".into()),
        options: vec!["staging".into(), "production".into()],
    };
    *h.engine.dispatch_schema.lock().unwrap() = rg_core::ci::WorkflowDispatchSchema {
        inputs: vec![input.clone()],
        workflows: vec![rg_core::ci::WorkflowDispatchWorkflow {
            path: ".gitea/workflows/deploy.yml".into(),
            name: "Deploy".into(),
            inputs: vec![input],
        }],
    };

    let response = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/{}/trg-repo/pipelines/workflow-dispatch?ref=main",
            h.base, h.owner
        ))
        .bearer_auth(&h.token)
        .send()
        .await
        .expect("load the manual-run form");
    assert_eq!(response.status(), 200, "the schema route must outrank /:id");
    let body = response
        .json::<serde_json::Value>()
        .await
        .expect("schema body");
    assert_eq!(body["ref_name"], "refs/heads/main");
    assert_eq!(body["commit_sha"], expected_sha);
    assert_eq!(body["inputs"][0]["name"], "target");
    assert_eq!(body["inputs"][0]["type"], "choice");
    assert_eq!(body["workflows"][0]["path"], ".gitea/workflows/deploy.yml");
    assert_eq!(body["workflows"][0]["inputs"][0]["name"], "target");
    assert_eq!(body["workflows"][0]["inputs"][0]["type"], "choice");
    assert_eq!(body["workflows"][0]["inputs"][0]["required"], true);
    assert_eq!(body["workflows"][0]["inputs"][0]["default"], "staging");
    assert_eq!(
        body["workflows"][0]["inputs"][0]["options"],
        serde_json::json!(["staging", "production"])
    );
    assert_eq!(
        h.engine.schema_queries.lock().unwrap().as_slice(),
        [(h.repo_path.clone(), expected_sha)],
        "the handler inspected a different repository revision than the selected ref"
    );
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
        .json(&serde_json::json!({
            "inputs": {
                "deploy": "true",
                "target": "staging"
            }
        }))
        .send()
        .await
        .expect("trigger pipeline");
    assert_eq!(resp.status(), 201, "the Run button must start a pipeline");

    assert_eq!(
        h.engine.triggered.lock().unwrap().as_slice(),
        [rg_core::ci::WORKFLOW_DISPATCH_EVENT.to_string()],
        "the manual run asked for an event no `on:` clause can declare"
    );
    assert_eq!(
        h.engine.dispatch_inputs.lock().unwrap().as_slice(),
        [std::collections::HashMap::from([
            ("deploy".to_string(), "true".to_string()),
            ("target".to_string(), "staging".to_string()),
        ])],
        "the HTTP request discarded workflow_dispatch inputs before the CI engine"
    );
}

/// card_64804da48693: the branch the caller asks for is the branch that is built.
///
/// The struct field was `ref_name` with no rename while every client sends
/// `ref`, so `serde` filled it with `None` for every request and the handler
/// substituted a literal `refs/heads/main`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manual_run_builds_the_ref_the_caller_named() {
    let h = harness("ref").await;
    let client = reqwest::Client::new();

    let written = client
        .post(format!(
            "{}/api/v1/repos/trgref/trg-repo/contents/README.md",
            h.base
        ))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({"content": "hello\n", "message": "init"}))
        .send()
        .await
        .expect("write a file");
    assert_eq!(written.status(), 200, "the fixture commit must land");

    // A second branch, so "the ref the caller named" is distinguishable from
    // "whatever the repository would have picked". A write to a branch that
    // does not exist yet creates it.
    let branched = client
        .post(format!(
            "{}/api/v1/repos/trgref/trg-repo/contents/FEATURE.md",
            h.base
        ))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({
            "content": "feature\n",
            "message": "branch off",
            "branch": "feature",
        }))
        .send()
        .await
        .expect("write a file on a new branch");
    assert_eq!(branched.status(), 200, "the fixture branch must be created");
    h.settle().await;

    // The wire name every client uses.
    let resp = client
        .post(format!("{}/api/v1/repos/trgref/trg-repo/pipelines", h.base))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({"ref": "refs/heads/feature"}))
        .send()
        .await
        .expect("trigger pipeline");
    assert_eq!(resp.status(), 201, "a named ref must start a pipeline");
    assert_eq!(
        h.engine.refs.lock().unwrap().as_slice(),
        ["refs/heads/feature".to_string()],
        "the ref the caller named did not reach the engine"
    );

    // A bare branch name is a branch, and it reaches the engine in the
    // canonical form the `on:` filters read.
    h.engine.refs.lock().unwrap().clear();
    let resp = client
        .post(format!("{}/api/v1/repos/trgref/trg-repo/pipelines", h.base))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({"ref": "feature"}))
        .send()
        .await
        .expect("trigger pipeline");
    assert_eq!(resp.status(), 201);
    assert_eq!(
        h.engine.refs.lock().unwrap().as_slice(),
        ["refs/heads/feature".to_string()],
        "a bare branch name must reach the engine as a full ref"
    );

    // The old struct name stays accepted, so a caller written against it is not
    // broken by the rename.
    h.engine.refs.lock().unwrap().clear();
    let resp = client
        .post(format!("{}/api/v1/repos/trgref/trg-repo/pipelines", h.base))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({"ref_name": "refs/heads/feature"}))
        .send()
        .await
        .expect("trigger pipeline");
    assert_eq!(resp.status(), 201);
    assert_eq!(
        h.engine.refs.lock().unwrap().as_slice(),
        ["refs/heads/feature".to_string()],
        "the legacy `ref_name` spelling stopped being accepted"
    );
}

/// card_64804da48693: no ref asked for means *this repository's* default branch.
///
/// The literal `refs/heads/main` answered `400 cannot resolve commit SHA for
/// ref` on a repository whose default branch is `develop` — about a branch the
/// caller never named, while the right one sat in the row the access gate had
/// already read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manual_run_without_a_ref_builds_the_repositorys_default_branch() {
    let h = harness_on_branch("dflt", Some("develop")).await;
    let client = reqwest::Client::new();

    let written = client
        .post(format!(
            "{}/api/v1/repos/trgdflt/trg-repo/contents/README.md",
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
        .post(format!(
            "{}/api/v1/repos/trgdflt/trg-repo/pipelines",
            h.base
        ))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("trigger pipeline");
    assert_eq!(
        resp.status(),
        201,
        "an empty body must build the repository's own default branch"
    );
    assert_eq!(
        h.engine.refs.lock().unwrap().as_slice(),
        ["refs/heads/develop".to_string()],
        "the fallback built a branch the repository does not use"
    );
}

/// card_8ce245b79376: the deleted-ref cancellation pass and a manual producer
/// overlap. The pass sees everything that existed before the request publishes;
/// the producer must close the other ordering after its own graph is durable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manual_run_published_after_ref_deletion_cancels_its_exact_graph() {
    let h = harness("delete-race").await;
    write_default_branch(&h, "README.md").await;
    h.settle().await;

    let gate = h.engine.gate_next_trigger();
    let base = h.base.clone();
    let token = h.token.clone();
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!(
                "{base}/api/v1/repos/trgdelete-race/trg-repo/pipelines"
            ))
            .bearer_auth(token)
            .json(&serde_json::json!({"ref": "refs/heads/main"}))
            .send()
            .await
            .expect("manual pipeline request")
    });

    tokio::time::timeout(std::time::Duration::from_secs(10), gate.entered.notified())
        .await
        .expect("manual producer reached the pre-publication barrier");

    delete_ref(&h.repo_path, "refs/heads/main");
    let visible =
        rg_db::ops::pipeline_ops::find_active_pipelines_by_ref(&h.db, h.repo_id, "refs/heads/main")
            .await
            .expect("deletion cancellation lookup");
    assert!(
        !visible.is_empty(),
        "the fixture's earlier push must prove the cancellation pass really ran"
    );
    for pipeline in visible {
        rg_db::ops::pipeline_ops::cancel_pipeline_chain(&h.db, pipeline.id)
            .await
            .expect("cancel graph visible during ref deletion");
    }
    assert!(
        rg_db::ops::pipeline_ops::find_active_pipelines_by_ref(
            &h.db,
            h.repo_id,
            "refs/heads/main",
        )
        .await
        .expect("read active pipelines after deletion pass")
        .is_empty(),
        "the deletion pass must finish before the delayed producer resumes"
    );

    gate.release.notify_one();
    let response = tokio::time::timeout(std::time::Duration::from_secs(10), request)
        .await
        .expect("manual request finished after release")
        .expect("manual request task did not panic");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::CONFLICT,
        "a vanished ref is a resource-state conflict"
    );
    let body = response.text().await.expect("conflict body");
    assert!(
        body.contains("pipeline ref no longer exists: refs/heads/main"),
        "the conflict must name the ref whose lifetime ended: {body}"
    );

    let pipeline_ids = h.engine.pipeline_ids.lock().unwrap().clone();
    assert_eq!(pipeline_ids.len(), 1, "the delayed producer made one graph");
    let pipeline_id = pipeline_ids[0];
    let pipeline = rg_db::ops::pipeline_ops::get_pipeline(&h.db, pipeline_id)
        .await
        .expect("read compensated pipeline")
        .expect("compensated pipeline still records its history");
    assert_eq!(pipeline.status, "canceled");
    let stages = rg_db::ops::pipeline_ops::list_stages_by_pipeline(&h.db, pipeline_id)
        .await
        .expect("read compensated stages");
    assert_eq!(stages.len(), 1);
    assert_eq!(stages[0].status, "canceled");
    let jobs = rg_db::ops::pipeline_ops::list_jobs_by_stage(&h.db, stages[0].id)
        .await
        .expect("read compensated jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, "canceled");
    assert!(
        rg_db::ops::pipeline_ops::find_active_pipelines_by_ref(
            &h.db,
            h.repo_id,
            "refs/heads/main",
        )
        .await
        .expect("read final active pipelines")
        .is_empty(),
        "the producer must not reopen work after the deletion pass"
    );
}

/// A durable pipeline row is history, not proof that its branch still exists.
/// Retry must refuse before it invokes the engine or creates another graph.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retrying_a_pipeline_whose_ref_is_gone_is_a_conflict_without_a_graph() {
    let h = harness("retry-gone").await;
    let original = rg_db::ops::pipeline_ops::create_pipeline(
        &h.db,
        h.repo_id,
        "2222222222222222222222222222222222222222",
        "refs/heads/deleted",
        "push",
        None,
    )
    .await
    .expect("seed historical pipeline");

    let response = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/{}/trg-repo/pipelines/{}/retry",
            h.base, h.owner, original.id
        ))
        .bearer_auth(&h.token)
        .send()
        .await
        .expect("retry missing ref");
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body = response.text().await.expect("conflict body");
    assert!(
        body.contains("pipeline ref no longer exists: refs/heads/deleted"),
        "the state conflict must name the missing ref: {body}"
    );
    assert!(
        h.engine.triggered.lock().unwrap().is_empty(),
        "a stable missing-ref retry must stop before invoking the engine"
    );
    assert!(
        h.engine.pipeline_ids.lock().unwrap().is_empty(),
        "a stable missing-ref retry must not create a graph"
    );
}

/// card_6860d4a03e16: automatic producers silently omit a valid native config
/// that selects no work, but an operator explicitly pressing Run or Retry must
/// get a precise refusal. Both handlers have to preserve the typed engine
/// outcome through the HTTP boundary, and neither may leave a graph behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_and_retry_no_match_are_precise_bad_requests_without_new_graphs() {
    let h = harness("no-match-http").await;
    let client = reqwest::Client::new();
    write_default_branch(&h, "README.md").await;
    h.settle().await;
    let commit_sha = ref_sha(&h.repo_path, "refs/heads/main");
    let (_, baseline) =
        rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&h.db, h.repo_id, 0, 20)
            .await
            .expect("record fixture pipeline baseline");

    h.engine.refuse_next_trigger_for_ref("refs/heads/main");
    let manual = client
        .post(format!(
            "{}/api/v1/repos/{}/trg-repo/pipelines",
            h.base, h.owner
        ))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("manual no-match request");
    assert_eq!(manual.status(), reqwest::StatusCode::BAD_REQUEST);
    let manual_body = manual.text().await.expect("manual refusal body");
    for expected in ["refs/heads/main", "only"] {
        assert!(
            manual_body.contains(expected),
            "manual refusal omitted {expected:?}: {manual_body}"
        );
    }

    let original = rg_db::ops::pipeline_ops::create_pipeline(
        &h.db,
        h.repo_id,
        &commit_sha,
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("seed the pipeline being retried");
    h.engine.refuse_next_trigger_for_ref("refs/heads/main");
    let retry = client
        .post(format!(
            "{}/api/v1/repos/{}/trg-repo/pipelines/{}/retry",
            h.base, h.owner, original.id
        ))
        .bearer_auth(&h.token)
        .send()
        .await
        .expect("retry no-match request");
    assert_eq!(retry.status(), reqwest::StatusCode::BAD_REQUEST);
    let retry_body = retry.text().await.expect("retry refusal body");
    for expected in ["refs/heads/main", "only"] {
        assert!(
            retry_body.contains(expected),
            "retry refusal omitted {expected:?}: {retry_body}"
        );
    }

    let (_, total) =
        rg_db::ops::pipeline_ops::list_pipelines_by_repo_paginated(&h.db, h.repo_id, 0, 20)
            .await
            .expect("list pipelines after both refusals");
    assert_eq!(
        total,
        baseline + 1,
        "a refused manual run or retry published a graph"
    );
    assert!(
        h.engine.pipeline_ids.lock().unwrap().is_empty(),
        "the refusing engine published a graph"
    );
}

/// A retry re-runs the pipeline, so it re-runs its event — `"retry"` was a name
/// the matcher answered `false` to for every workflow ever written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retry_runs_under_the_event_that_produced_the_pipeline() {
    let h = harness("retry").await;
    let client = reqwest::Client::new();
    write_default_branch(&h, "README.md").await;
    h.settle().await;
    let commit_sha = ref_sha(&h.repo_path, "refs/heads/main");
    let mut retried_ids = Vec::new();

    for original in ["push", "pull_request", rg_core::ci::WORKFLOW_DISPATCH_EVENT] {
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &h.db,
            h.repo_id,
            &commit_sha,
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
        let retried_id = resp.json::<serde_json::Value>().await.expect("retry body")["id"]
            .as_i64()
            .expect("retry pipeline id");
        retried_ids.push(retried_id);

        assert_eq!(
            h.engine.triggered.lock().unwrap().as_slice(),
            [original.to_string()],
            "the retry of a {original} pipeline ran under a different event"
        );
    }

    retried_ids.sort_unstable();
    retried_ids.dedup();
    assert_eq!(
        retried_ids.len(),
        3,
        "a live ref keeps each retry as a distinct pipeline run"
    );
}

/// card_24f475c09a17: the same run again means the same *inputs* again.
///
/// The retry re-reads the workflow at the original commit, so a run started
/// with `deploy: true` and no `target` has to arrive back at the engine that
/// way — otherwise the workflow's `default:` silently stands in for a value the
/// caller chose, and a `required:` input turns the retry of a perfectly good
/// pipeline into a refusal about a value already supplied once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retry_replays_the_dispatch_inputs_of_the_run_it_repeats() {
    let h = harness("retryinputs").await;
    let client = reqwest::Client::new();
    write_default_branch(&h, "README.md").await;
    h.settle().await;

    let started = client
        .post(format!(
            "{}/api/v1/repos/{}/trg-repo/pipelines",
            h.base, h.owner
        ))
        .bearer_auth(&h.token)
        .json(&serde_json::json!({"inputs": {"deploy": "true"}}))
        .send()
        .await
        .expect("start a manual run");
    assert_eq!(started.status(), 201, "the manual run must start");
    let original_id = started.json::<serde_json::Value>().await.expect("body")["id"]
        .as_i64()
        .expect("manual pipeline id");
    let supplied = std::collections::HashMap::from([("deploy".to_string(), "true".to_string())]);
    assert_eq!(
        h.engine.dispatch_inputs.lock().unwrap().as_slice(),
        std::slice::from_ref(&supplied),
        "the manual run itself did not carry its inputs"
    );
    h.engine.dispatch_inputs.lock().unwrap().clear();

    let retried = client
        .post(format!(
            "{}/api/v1/repos/{}/trg-repo/pipelines/{}/retry",
            h.base, h.owner, original_id
        ))
        .bearer_auth(&h.token)
        .send()
        .await
        .expect("retry the manual run");
    assert_eq!(retried.status(), 201, "the retry must start a pipeline");
    assert_eq!(
        h.engine.dispatch_inputs.lock().unwrap().as_slice(),
        [supplied],
        "the retry reached the engine with different workflow_dispatch inputs than the run it \
         repeats — a declared default would stand in for the caller's own value"
    );

    // …and a retry does not invent inputs for a run that had none: a push
    // pipeline is repeated exactly as empty as it was.
    let commit_sha = ref_sha(&h.repo_path, "refs/heads/main");
    let pushed = rg_db::ops::pipeline_ops::create_pipeline(
        &h.db,
        h.repo_id,
        &commit_sha,
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("seed a push pipeline");
    h.engine.dispatch_inputs.lock().unwrap().clear();
    let retried_push = client
        .post(format!(
            "{}/api/v1/repos/{}/trg-repo/pipelines/{}/retry",
            h.base, h.owner, pushed.id
        ))
        .bearer_auth(&h.token)
        .send()
        .await
        .expect("retry the push run");
    assert_eq!(retried_push.status(), 201, "the push retry must start");
    assert_eq!(
        h.engine.dispatch_inputs.lock().unwrap().as_slice(),
        [std::collections::HashMap::new()],
        "the retry of a push pipeline arrived carrying workflow_dispatch inputs"
    );
}

/// card_74d58ec3ac1e: the same run again means the same *filters* again.
///
/// `base_branch` and `previous_sha` were hardcoded `None` here, which is not
/// "this run had none" but "work it out from the repository as it stands now":
/// the matcher then judges a retried pull request against the default branch —
/// so a PR into `develop` selects no workflow and the run falls through to
/// `.forgekeep-ci.yml`, a different graph under the same `201` — and a `paths:`
/// filter diffs against the head commit's first parent instead of the range the
/// push covered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_retry_replays_the_event_context_of_the_run_it_repeats() {
    let h = harness("retryctx").await;
    let client = reqwest::Client::new();
    write_default_branch(&h, "README.md").await;
    h.settle().await;
    let commit_sha = ref_sha(&h.repo_path, "refs/heads/main");
    let previous_sha = "1111111111111111111111111111111111111111";

    // A row as its producer wrote it: a pull-request run into a branch that is
    // not this repository's default, pushed from a known earlier revision.
    let original = rg_db::ops::pipeline_ops::create_pipeline_row(
        &h.db,
        rg_db::ops::pipeline_ops::NewPipeline {
            repo_id: h.repo_id,
            commit_sha: &commit_sha,
            ref_name: "refs/heads/main",
            trigger_type: "pull_request",
            triggered_by: None,
            concurrency_group: None,
            dispatch_inputs: None,
            base_branch: Some("develop"),
            previous_sha: Some(previous_sha),
        },
    )
    .await
    .expect("seed the pipeline being retried");

    let retried = client
        .post(format!(
            "{}/api/v1/repos/{}/trg-repo/pipelines/{}/retry",
            h.base, h.owner, original.id
        ))
        .bearer_auth(&h.token)
        .send()
        .await
        .expect("retry the recorded run");
    assert_eq!(retried.status(), 201, "the retry must start a pipeline");
    assert_eq!(
        h.engine.base_branches.lock().unwrap().as_slice(),
        [Some("develop".to_string())],
        "the retry reached the engine without the branch its filters were matched against, so \
         it is judged against whatever this repository defaults to today"
    );
    assert_eq!(
        h.engine.previous_shas.lock().unwrap().as_slice(),
        [Some(previous_sha.to_string())],
        "the retry reached the engine without the revision the original diff was taken from, \
         so its `paths:` filters see a narrower range than the run it repeats"
    );

    // …and a retry invents neither for a run that carried neither: a manual run
    // targets no branch and has no previous revision, and must stay that way.
    let manual = rg_db::ops::pipeline_ops::create_pipeline(
        &h.db,
        h.repo_id,
        &commit_sha,
        "refs/heads/main",
        rg_core::ci::WORKFLOW_DISPATCH_EVENT,
        None,
    )
    .await
    .expect("seed a manual run");
    h.engine.base_branches.lock().unwrap().clear();
    h.engine.previous_shas.lock().unwrap().clear();

    let retried_manual = client
        .post(format!(
            "{}/api/v1/repos/{}/trg-repo/pipelines/{}/retry",
            h.base, h.owner, manual.id
        ))
        .bearer_auth(&h.token)
        .send()
        .await
        .expect("retry the manual run");
    assert_eq!(
        retried_manual.status(),
        201,
        "the manual retry must start a pipeline"
    );
    assert_eq!(
        h.engine.base_branches.lock().unwrap().as_slice(),
        [None],
        "the retry of a run that targeted no branch arrived carrying one"
    );
    assert_eq!(
        h.engine.previous_shas.lock().unwrap().as_slice(),
        [None],
        "the retry of a run with no previous revision arrived carrying one"
    );
}
