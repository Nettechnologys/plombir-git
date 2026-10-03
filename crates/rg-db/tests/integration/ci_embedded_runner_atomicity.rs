//! card_944be22fcd3c: the in-process runner publishes a graph transition whole.
//!
//! The embedded `PipelineRunner` is the executor on a default installation, and
//! it used to walk its own graph one write at a time: settle the job, later
//! settle the stage from a local accumulator, later still settle the pipeline.
//! Every gap between those writes was a state a fault could stop in — a
//! terminal parent owning an active child, or a terminal child under a parent
//! still calling itself `running` — and the runner could not simply be started
//! again from there, because its next pass reads the statuses it had already
//! moved.
//!
//! The fault here is aimed at the *second* write of each transition, not at the
//! whole table: a trigger that aborts the parent update proves the child write
//! that preceded it is undone with it, and that the same call goes through once
//! the database recovers.

use rg_db::ops::pipeline_ops;
use rg_db::sea_orm::{ConnectionTrait, DatabaseConnection};

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-ci-embedded-atomicity-{label}-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
}

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "cleanup must not mask the assertion that failed the test"
    )]
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

/// One repository, one pipeline, one stage — the smallest graph that has all
/// three levels the transitions have to keep in step.
struct Graph {
    pipeline_id: i64,
    stage_id: i64,
}

async fn setup(label: &str) -> (TempDb, DatabaseConnection, Graph) {
    let temp = TempDb::new(label);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    let user = rg_db::ops::user_ops::create_user(
        &db,
        label,
        &format!("{label}@example.invalid"),
        "unused",
        "CI Atomicity Owner",
    )
    .await
    .expect("create the account the pipeline belongs to");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        rg_db::entities::repository::ActiveModel {
            id: rg_db::sea_orm::NotSet,
            owner_id: rg_db::sea_orm::Set(user.id),
            name: rg_db::sea_orm::Set(label.into()),
            description: rg_db::sea_orm::Set(None),
            is_private: rg_db::sea_orm::Set(true),
            default_branch: rg_db::sea_orm::Set("main".into()),
            fork_id: rg_db::sea_orm::Set(None),
            stars_count: rg_db::sea_orm::Set(0),
            forks_count: rg_db::sea_orm::Set(0),
            org_id: rg_db::sea_orm::Set(None),
            created_at: rg_db::sea_orm::Set(now),
            updated_at: rg_db::sea_orm::Set(now),
            deleted_at: rg_db::sea_orm::Set(None),
            origin_repo_id: rg_db::sea_orm::Set(None),
        },
    )
    .await
    .expect("create the repository the pipeline belongs to");
    let pipeline = pipeline_ops::create_pipeline(
        &db,
        repo.id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "push",
        Some(user.id),
    )
    .await
    .expect("create the pipeline");
    let stage = pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
        .await
        .expect("create the stage");
    (
        temp,
        db,
        Graph {
            pipeline_id: pipeline.id,
            stage_id: stage.id,
        },
    )
}

async fn add_stage(db: &DatabaseConnection, pipeline_id: i64, name: &str, order: i32) -> i64 {
    pipeline_ops::create_stage(db, pipeline_id, name, order)
        .await
        .expect("create a stage")
        .id
}

async fn job_log(db: &DatabaseConnection, job_id: i64) -> Option<String> {
    pipeline_ops::get_job(db, job_id)
        .await
        .expect("read the job")
        .expect("the job exists")
        .log
}

async fn add_job(db: &DatabaseConnection, stage_id: i64, name: &str) -> i64 {
    pipeline_ops::create_job(
        db, stage_id, name, "true", None, None, None, None, None, None, false, None, None, None,
    )
    .await
    .expect("create a job")
    .id
}

/// Put the whole graph into the state the runner reaches once it has started.
async fn start_everything(db: &DatabaseConnection, graph: &Graph, job_ids: &[i64]) {
    pipeline_ops::update_pipeline_status(db, graph.pipeline_id, "running", None, None)
        .await
        .expect("start the pipeline");
    pipeline_ops::update_stage_status(db, graph.stage_id, "running", None, None)
        .await
        .expect("start the stage");
    for job_id in job_ids {
        assert!(
            pipeline_ops::start_job_if_active(db, *job_id, None)
                .await
                .expect("start a job"),
            "the fixture could not put job {job_id} to work"
        );
    }
}

/// Abort every UPDATE on one table, and nothing else.
///
/// Narrower than dropping the table: the reads a transition makes before its
/// writes have to keep working for the rollback of the earlier write to be the
/// thing under test.
async fn fail_updates_on(db: &DatabaseConnection, table: &str) {
    db.execute_unprepared(&format!(
        "CREATE TRIGGER fk_fault_{table}_update BEFORE UPDATE ON {table} \
         BEGIN SELECT RAISE(ABORT, 'injected failure: UPDATE on {table}'); END;"
    ))
    .await
    .unwrap_or_else(|error| panic!("arm the UPDATE fault on {table}: {error}"));
}

async fn clear_fault(db: &DatabaseConnection, table: &str) {
    db.execute_unprepared(&format!("DROP TRIGGER IF EXISTS fk_fault_{table}_update"))
        .await
        .unwrap_or_else(|error| panic!("disarm the UPDATE fault on {table}: {error}"));
}

async fn job_status(db: &DatabaseConnection, job_id: i64) -> String {
    pipeline_ops::get_job(db, job_id)
        .await
        .expect("read the job")
        .expect("the job exists")
        .status
}

async fn stage_status(db: &DatabaseConnection, stage_id: i64) -> String {
    pipeline_ops::get_stage_by_id(db, stage_id)
        .await
        .expect("read the stage")
        .expect("the stage exists")
        .status
}

async fn pipeline_status(db: &DatabaseConnection, pipeline_id: i64) -> String {
    pipeline_ops::get_pipeline(db, pipeline_id)
        .await
        .expect("read the pipeline")
        .expect("the pipeline exists")
        .status
}

fn assert_injected(error: &anyhow::Error, table: &str) {
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("injected failure"),
        "the failure that surfaced is not the injected one on {table}: {rendered}"
    );
}

/// The job result and the stage roll-up it makes due are one write or none.
#[tokio::test]
async fn a_job_result_that_cannot_roll_its_stage_up_publishes_nothing() {
    let (_temp, db, graph) = setup("jobfinish").await;
    let job_id = add_job(&db, graph.stage_id, "only").await;
    start_everything(&db, &graph, &[job_id]).await;

    fail_updates_on(&db, "pipeline_stages").await;
    let error = pipeline_ops::finish_embedded_job(
        &db,
        graph.pipeline_id,
        graph.stage_id,
        job_id,
        "success",
        Some(0),
        Some("build log"),
        None,
    )
    .await
    .expect_err("a stage roll-up the database refuses must escape as an error");
    assert_injected(&error, "pipeline_stages");
    clear_fault(&db, "pipeline_stages").await;

    let job = pipeline_ops::get_job(&db, job_id)
        .await
        .expect("read the job")
        .expect("the job exists");
    assert_eq!(
        job.status, "running",
        "the job result was published even though its stage could not follow"
    );
    assert_eq!(
        job.log, None,
        "the job log was published even though the transition rolled back"
    );
    assert_eq!(stage_status(&db, graph.stage_id).await, "running");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "running");

    let transition = pipeline_ops::finish_embedded_job(
        &db,
        graph.pipeline_id,
        graph.stage_id,
        job_id,
        "success",
        Some(0),
        Some("build log"),
        None,
    )
    .await
    .expect("the same call goes through once the database recovers");
    assert!(transition.job_settled);
    assert_eq!(transition.stage_status.as_deref(), Some("success"));
    assert_eq!(transition.pipeline_status.as_deref(), Some("success"));
    assert_eq!(job_status(&db, job_id).await, "success");
    assert_eq!(stage_status(&db, graph.stage_id).await, "success");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "success");
}

/// A stage still holding unfinished work does not drag the pipeline with it.
#[tokio::test]
async fn a_job_result_leaves_its_parents_alone_while_a_sibling_is_still_running() {
    let (_temp, db, graph) = setup("sibling").await;
    let first = add_job(&db, graph.stage_id, "first").await;
    let second = add_job(&db, graph.stage_id, "second").await;
    start_everything(&db, &graph, &[first, second]).await;

    let transition = pipeline_ops::finish_embedded_job(
        &db,
        graph.pipeline_id,
        graph.stage_id,
        first,
        "success",
        Some(0),
        None,
        None,
    )
    .await
    .expect("settle the first job");
    assert!(transition.job_settled);
    assert_eq!(transition.stage_status, None);
    assert_eq!(transition.pipeline_status, None);
    assert_eq!(stage_status(&db, graph.stage_id).await, "running");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "running");
    assert_eq!(job_status(&db, second).await, "running");
}

/// A stage published `skipped` over jobs that still call themselves `pending`
/// is a stage nobody will walk again owning work nobody will do.
#[tokio::test]
async fn a_skipped_stage_never_outruns_the_jobs_underneath_it() {
    let (_temp, db, graph) = setup("skip").await;
    let first = add_job(&db, graph.stage_id, "first").await;
    let second = add_job(&db, graph.stage_id, "second").await;
    pipeline_ops::update_pipeline_status(&db, graph.pipeline_id, "running", None, None)
        .await
        .expect("start the pipeline");

    fail_updates_on(&db, "pipeline_stages").await;
    let error = pipeline_ops::skip_embedded_stage(&db, graph.pipeline_id, graph.stage_id)
        .await
        .expect_err("a stage write the database refuses must escape as an error");
    assert_injected(&error, "pipeline_stages");
    clear_fault(&db, "pipeline_stages").await;

    for job_id in [first, second] {
        assert_eq!(
            job_status(&db, job_id).await,
            "pending",
            "job {job_id} was skipped even though its stage could not follow"
        );
    }
    assert_eq!(stage_status(&db, graph.stage_id).await, "pending");

    assert!(
        pipeline_ops::skip_embedded_stage(&db, graph.pipeline_id, graph.stage_id)
            .await
            .expect("the same call goes through once the database recovers")
    );
    for job_id in [first, second] {
        assert_eq!(job_status(&db, job_id).await, "skipped");
    }
    assert_eq!(stage_status(&db, graph.stage_id).await, "skipped");
    assert_eq!(
        pipeline_status(&db, graph.pipeline_id).await,
        "success",
        "skipping the last stage is what finishes the run"
    );
}

/// A gate is what the UI and the release routes read. A stage advertising one
/// under a pipeline that still looks busy is a gate on a run nobody is waiting
/// for — and the runner's next pass cannot repair it, because it would find a
/// stage it has already moved.
#[tokio::test]
async fn a_gate_is_never_advertised_on_the_stage_alone() {
    let (_temp, db, graph) = setup("pause").await;
    let job_id = add_job(&db, graph.stage_id, "gated").await;
    start_everything(&db, &graph, &[job_id]).await;

    fail_updates_on(&db, "pipelines").await;
    let error = pipeline_ops::pause_embedded_stage_at_gate(
        &db,
        graph.pipeline_id,
        graph.stage_id,
        "manual",
    )
    .await
    .expect_err("a pipeline write the database refuses must escape as an error");
    assert_injected(&error, "pipelines");
    clear_fault(&db, "pipelines").await;

    assert_eq!(
        stage_status(&db, graph.stage_id).await,
        "running",
        "the stage advertised a gate its pipeline never learned about"
    );
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "running");

    assert!(pipeline_ops::pause_embedded_stage_at_gate(
        &db,
        graph.pipeline_id,
        graph.stage_id,
        "manual",
    )
    .await
    .expect("the same call goes through once the database recovers"));
    assert_eq!(stage_status(&db, graph.stage_id).await, "manual");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "manual");
}

/// The runner's own stage verdict — the only write for a stage whose jobs were
/// all terminal before it started — carries the pipeline roll-up with it.
#[tokio::test]
async fn a_stage_verdict_that_cannot_finish_its_pipeline_publishes_nothing() {
    let (_temp, db, graph) = setup("stagesettle").await;
    let job_id = add_job(&db, graph.stage_id, "already-done").await;
    start_everything(&db, &graph, &[job_id]).await;
    assert!(
        pipeline_ops::settle_job_if_active(&db, job_id, "success", Some(0), None, None)
            .await
            .expect("settle the job outside the transition under test")
    );

    fail_updates_on(&db, "pipelines").await;
    let error = pipeline_ops::settle_embedded_stage(
        &db,
        graph.pipeline_id,
        graph.stage_id,
        "success",
        Some(chrono::Utc::now().naive_utc()),
    )
    .await
    .expect_err("a pipeline roll-up the database refuses must escape as an error");
    assert_injected(&error, "pipelines");
    clear_fault(&db, "pipelines").await;

    assert_eq!(
        stage_status(&db, graph.stage_id).await,
        "running",
        "the stage settled under a pipeline that never finished"
    );
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "running");

    assert!(pipeline_ops::settle_embedded_stage(
        &db,
        graph.pipeline_id,
        graph.stage_id,
        "success",
        Some(chrono::Utc::now().naive_utc()),
    )
    .await
    .expect("the same call goes through once the database recovers"));
    assert_eq!(stage_status(&db, graph.stage_id).await, "success");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "success");
}

/// Cancellation fencing survives the move into a transaction: a runner coming
/// back with a result for a pipeline the server has already answered `canceled`
/// still writes nothing at all.
#[tokio::test]
async fn a_canceled_graph_refuses_the_result_the_runner_brings_back() {
    let (_temp, db, graph) = setup("canceled").await;
    let job_id = add_job(&db, graph.stage_id, "doomed").await;
    start_everything(&db, &graph, &[job_id]).await;
    assert!(pipeline_ops::cancel_pipeline_chain(&db, graph.pipeline_id)
        .await
        .expect("cancel the pipeline the runner is executing"));

    let transition = pipeline_ops::finish_embedded_job(
        &db,
        graph.pipeline_id,
        graph.stage_id,
        job_id,
        "success",
        Some(0),
        Some("build log"),
        None,
    )
    .await
    .expect("a refused result is an answer, not an error");
    assert!(
        !transition.job_settled,
        "the runner's late result overwrote a cancellation"
    );
    assert_eq!(transition.stage_status, None);
    assert_eq!(transition.pipeline_status, None);
    for (level, status) in [
        ("job", job_status(&db, job_id).await),
        ("stage", stage_status(&db, graph.stage_id).await),
        ("pipeline", pipeline_status(&db, graph.pipeline_id).await),
    ] {
        assert_eq!(status, "canceled", "the {level} left the cancellation");
    }
}

/// A job that does not belong to the stage the caller named is a bug in the
/// caller, not a graph to settle: the transition refuses it rather than
/// rolling up a stage over somebody else's work.
#[tokio::test]
async fn a_job_from_another_stage_is_refused() {
    let (_temp, db, graph) = setup("foreign").await;
    let other_stage = pipeline_ops::create_stage(&db, graph.pipeline_id, "other", 1)
        .await
        .expect("create the second stage");
    let job_id = add_job(&db, other_stage.id, "elsewhere").await;
    start_everything(&db, &graph, &[job_id]).await;

    let error = pipeline_ops::finish_embedded_job(
        &db,
        graph.pipeline_id,
        graph.stage_id,
        job_id,
        "success",
        Some(0),
        None,
        None,
    )
    .await
    .expect_err("a job outside the named stage must be refused");
    assert!(
        format!("{error:#}").contains("does not belong to stage"),
        "the refusal does not name the mismatch: {error:#}"
    );
    assert_eq!(job_status(&db, job_id).await, "running");
}

/// card_e29c8d4274af: a pipeline whose workspace never got laid down settles
/// whole.
///
/// The runner's `prepare_workspace` failure used to write one row — the
/// pipeline — and return. Every stage and job under it stayed `pending`
/// forever: the scheduler will not take them (`job_is_schedulable` refuses a
/// job whose pipeline left `pending`/`running`), so nothing ever moved them,
/// and the API kept advertising work as waiting under a run that failed
/// minutes ago. A single-row write reddens this test on the second and third
/// assertion group.
#[tokio::test]
async fn a_graph_whose_work_never_started_settles_at_every_level() {
    let (_temp, db, graph) = setup("neverstarted").await;
    let first = add_job(&db, graph.stage_id, "build").await;
    let second = add_job(&db, graph.stage_id, "lint").await;
    let later_stage = add_stage(&db, graph.pipeline_id, "deploy", 1).await;
    let third = add_job(&db, later_stage, "ship").await;

    const REASON: &str = "runner could not prepare the workspace: no such repository";
    assert!(
        pipeline_ops::fail_pipeline_chain(&db, graph.pipeline_id, REASON)
            .await
            .expect("settle a graph that never started"),
        "the graph was active, so the cascade had work to do"
    );

    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "failed");
    assert_eq!(stage_status(&db, graph.stage_id).await, "failed");
    assert_eq!(stage_status(&db, later_stage).await, "failed");
    for job_id in [first, second, third] {
        assert_eq!(
            job_status(&db, job_id).await,
            "failed",
            "job {job_id} was left waiting under a pipeline that had already failed"
        );
        assert_eq!(
            job_log(&db, job_id).await.as_deref(),
            Some(REASON),
            "job {job_id} settled without saying why"
        );
    }
}

/// The same call twice does not overwrite the verdict of whoever got there
/// first.
#[tokio::test]
async fn a_settled_graph_refuses_a_second_failure_cascade() {
    let (_temp, db, graph) = setup("replayed").await;
    let job_id = add_job(&db, graph.stage_id, "only").await;
    start_everything(&db, &graph, &[job_id]).await;
    pipeline_ops::finish_embedded_job(
        &db,
        graph.pipeline_id,
        graph.stage_id,
        job_id,
        "success",
        Some(0),
        Some("build log"),
        None,
    )
    .await
    .expect("finish the one job the pipeline had");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "success");

    assert!(
        !pipeline_ops::fail_pipeline_chain(&db, graph.pipeline_id, "a late workspace error")
            .await
            .expect("a cascade over a finished graph is not an error"),
        "a graph that already settled must report that there was nothing to do"
    );
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "success");
    assert_eq!(stage_status(&db, graph.stage_id).await, "success");
    assert_eq!(job_status(&db, job_id).await, "success");
    assert_eq!(job_log(&db, job_id).await.as_deref(), Some("build log"));
}

/// The cascade is one write or none: a database that refuses the pipeline row
/// must leave the jobs and stages it had already touched exactly as they were.
#[tokio::test]
async fn a_failure_cascade_that_cannot_settle_its_pipeline_publishes_nothing() {
    let (_temp, db, graph) = setup("cascadefault").await;
    let job_id = add_job(&db, graph.stage_id, "only").await;

    fail_updates_on(&db, "pipelines").await;
    let error = pipeline_ops::fail_pipeline_chain(&db, graph.pipeline_id, "workspace error")
        .await
        .expect_err("a pipeline row the database refuses must escape as an error");
    assert_injected(&error, "pipelines");
    clear_fault(&db, "pipelines").await;

    assert_eq!(
        job_status(&db, job_id).await,
        "pending",
        "the job settled even though the pipeline above it could not follow"
    );
    assert_eq!(job_log(&db, job_id).await, None);
    assert_eq!(stage_status(&db, graph.stage_id).await, "pending");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "pending");

    assert!(
        pipeline_ops::fail_pipeline_chain(&db, graph.pipeline_id, "workspace error")
            .await
            .expect("the same call goes through once the database recovers")
    );
    assert_eq!(job_status(&db, job_id).await, "failed");
    assert_eq!(stage_status(&db, graph.stage_id).await, "failed");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "failed");
}
