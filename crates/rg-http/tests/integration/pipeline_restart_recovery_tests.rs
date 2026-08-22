//! card_706418977f45: a `pending` job needs somebody whose job it is to run it.
//!
//! `pipeline_jobs.status = 'pending'` means "waiting for an executor". On an
//! instance with `ci.external_runners = false` — the default, and what a fresh
//! install runs — the only executor is the embedded runner, and it does not
//! poll: it walks one pipeline's stages top to bottom and, once its task ends,
//! nothing ever comes back to that pipeline. So a server stopped mid-build left
//! the pipeline `running` and its job `pending` for good. The UI said
//! "building", forever, with no success, no failure and no deadline.
//!
//! Imports already had this recovery (`recover_stuck_imports`, on the same
//! startup path); pipelines had none. These tests hold both halves: the
//! leftovers are picked up, and the runs this process is *itself* executing are
//! not — a second runner on a live pipeline runs somebody's deploy twice.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::common::{build_test_app_state, create_repo, register_full, setup_test_db};

/// One `resume_pipeline` call, as the recovery made it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ResumedPipeline {
    repo_id: i64,
    pipeline_id: i64,
    repo_path: PathBuf,
}

/// A CI engine that records what it was asked to resume.
///
/// The recovery's whole output is that one trait call, so recording it is the
/// only way to see the difference between "handed to a runner" and "left where
/// it was" without linking `rg-ci` into this crate's tests.
#[derive(Default)]
struct RecordingCiEngine {
    resumed: Mutex<Vec<ResumedPipeline>>,
}

impl RecordingCiEngine {
    fn resumed(&self) -> Vec<ResumedPipeline> {
        self.resumed.lock().unwrap().clone()
    }
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
        _params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        Box::pin(async { Ok(0) })
    }

    fn resume_pipeline<'a>(
        &'a self,
        params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        let entry = ResumedPipeline {
            repo_id: params.repo_id,
            pipeline_id: params.pipeline_id,
            repo_path: params.repo_path.to_path_buf(),
        };
        Box::pin(async move {
            self.resumed.lock().unwrap().push(entry);
            Ok(())
        })
    }
}

struct Fixture {
    state: rg_http::AppState,
    ci_engine: Arc<RecordingCiEngine>,
    db: rg_db::DatabaseConnection,
    repo_root: PathBuf,
    owner: String,
    repo_name: String,
    repo_id: i64,
    _dir: tempfile::TempDir,
    _server: tokio::task::JoinHandle<()>,
}

/// A real user and repository, plus an `AppState` whose CI engine records.
///
/// The server is up because `create_repo` is the production path that puts the
/// repository on disk under the owner directory the recovery has to rebuild
/// from the database alone — the point of the `repo_path` assertion below.
async fn fixture(owner: &str, repo_name: &str) -> Fixture {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create the test repo root");

    let ci_engine = Arc::new(RecordingCiEngine::default());
    let mut state = build_test_app_state(db.clone(), repo_root.clone());
    state.ci_engine = ci_engine.clone();

    let app = rg_http::create_router_for_test(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");
    crate::common::wait_for_listener(&addr).await;

    let (token, _) = register_full(&base, owner, &format!("{owner}@example.test")).await;
    let repo_id = create_repo(&base, &token, repo_name).await;

    Fixture {
        state,
        ci_engine,
        db,
        repo_root,
        owner: owner.to_string(),
        repo_name: repo_name.to_string(),
        repo_id,
        _dir: dir,
        _server: server,
    }
}

/// Two jobs in one stage: the first one the dead process was executing, the
/// second one waiting behind it. Returns `(pipeline_id, executing_job_id,
/// waiting_job_id)`.
async fn seed_pipeline(fixture: &Fixture, pipeline_status: &str) -> (i64, i64, i64) {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &fixture.db,
        fixture.repo_id,
        "1111111111111111111111111111111111111111",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("create the pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(&fixture.db, pipeline.id, "test", 0)
        .await
        .expect("create the stage");
    let executing = create_job(fixture, stage.id, "build").await;
    let waiting = create_job(fixture, stage.id, "deploy").await;

    // What a killed process leaves behind: the job it held never reached
    // `hand_job_back`, so it is still `running` in the database.
    rg_db::ops::pipeline_ops::update_job_result(
        &fixture.db,
        executing,
        "running",
        None,
        None,
        None,
        None,
    )
    .await
    .expect("leave the first job as the dead process held it");
    rg_db::ops::pipeline_ops::update_pipeline_status(
        &fixture.db,
        pipeline.id,
        pipeline_status,
        None,
        None,
    )
    .await
    .expect("set the pipeline status");

    (pipeline.id, executing, waiting)
}

async fn create_job(fixture: &Fixture, stage_id: i64, name: &str) -> i64 {
    rg_db::ops::pipeline_ops::create_job(
        &fixture.db,
        stage_id,
        name,
        "echo ok",
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        None,
        None,
    )
    .await
    .expect("create the job")
    .id
}

async fn job_status(db: &rg_db::DatabaseConnection, job_id: i64) -> String {
    rg_db::ops::pipeline_ops::get_job(db, job_id)
        .await
        .expect("read the job")
        .expect("the job still exists")
        .status
}

fn now() -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

#[tokio::test]
async fn a_pipeline_interrupted_by_a_restart_is_handed_back_to_a_runner() {
    let fixture = fixture("recovery-owner", "recovery-repo").await;
    let (pipeline_id, executing, waiting) = seed_pipeline(&fixture, "running").await;

    // The cutoff a fresh process takes at startup: everything seeded above
    // belongs to the process that is no longer running.
    rg_http::recover_interrupted_pipelines(&fixture.state, now()).await;

    assert_eq!(
        fixture.ci_engine.resumed(),
        vec![ResumedPipeline {
            repo_id: fixture.repo_id,
            pipeline_id,
            // Rebuilt from the database alone — there is no request to read the
            // owner off, which is why the recovery resolves it itself.
            repo_path: fixture
                .repo_root
                .join(format!("{}/{}.git", fixture.owner, fixture.repo_name)),
        }],
        "the interrupted pipeline must be handed to a runner"
    );
    assert_eq!(
        job_status(&fixture.db, executing).await,
        "pending",
        "the job the dead process was executing must be handed back — `run_stage` refuses to \
         resume a job that is still `running`, so leaving it there resumes nothing"
    );
    assert_eq!(
        job_status(&fixture.db, waiting).await,
        "pending",
        "the job that was waiting stays waiting"
    );
}

#[tokio::test]
async fn a_pipeline_this_process_started_is_never_resumed_underneath_its_runner() {
    let fixture = fixture("live-owner", "live-repo").await;
    // Taken *before* the pipeline exists, exactly as `run` takes it before
    // anything of this process can create one.
    let created_before = now();
    let (_pipeline_id, executing, _waiting) = seed_pipeline(&fixture, "running").await;

    rg_http::recover_interrupted_pipelines(&fixture.state, created_before).await;

    assert!(
        fixture.ci_engine.resumed().is_empty(),
        "a pipeline created after the cutoff has a runner of its own in this process — resuming \
         it would execute the same job twice"
    );
    assert_eq!(
        job_status(&fixture.db, executing).await,
        "running",
        "and its running job must not be taken away from the runner holding it"
    );
}

#[tokio::test]
async fn an_instance_with_external_runners_leaves_the_work_to_them() {
    let mut fixture = fixture("external-owner", "external-repo").await;
    fixture.state.external_runners = true;
    let (_pipeline_id, executing, _waiting) = seed_pipeline(&fixture, "running").await;

    rg_http::recover_interrupted_pipelines(&fixture.state, now()).await;

    assert!(
        fixture.ci_engine.resumed().is_empty(),
        "a registered runner polls for `pending` jobs, so it is the producer here — spawning an \
         embedded runner beside it is the double execution this sweep exists to avoid"
    );
    assert_eq!(
        job_status(&fixture.db, executing).await,
        "running",
        "the job may still belong to a live external runner; the watchdog reclaims it on its own \
         deadline"
    );
}

#[tokio::test]
async fn a_pipeline_waiting_for_a_person_is_left_where_it_is() {
    let fixture = fixture("manual-owner", "manual-repo").await;
    let (_pipeline_id, _executing, _waiting) = seed_pipeline(&fixture, "manual").await;

    rg_http::recover_interrupted_pipelines(&fixture.state, now()).await;

    assert!(
        fixture.ci_engine.resumed().is_empty(),
        "a restart does not interrupt a pipeline parked on a manual job — it is waiting for a \
         person, and resuming it answers a question nobody asked"
    );
}
