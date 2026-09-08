//! card_fff21eed27fe: the roll-up of a freshly published graph is one write.
//!
//! A workflow can publish work that is already finished — a matrix leg its
//! `if:` excluded arrives `skipped`, and a stage every job of which did is over
//! before a runner ever looks at it — so the graph needs a verdict no job
//! result will produce for it. `trigger_pipeline` used to hand that verdict out
//! one pool-level write at a time: each stage on its own connection, the
//! pipeline after all of them. Every gap between those writes was a state a
//! fault could stop in, and unlike a runner's own transitions this one has
//! nobody to come back for it — the request that published the graph has
//! already returned it, and no durable job re-runs the roll-up.
//!
//! Both faults below are aimed at one *later* write of the sequence, never at
//! the whole table: what has to be proved is that the writes which already
//! succeeded are undone with it, and that the same call goes through unchanged
//! once the database recovers.

use rg_db::ops::pipeline_ops;
use rg_db::sea_orm::{ConnectionTrait, DatabaseConnection};

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-ci-initial-graph-{label}-{}.db",
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

/// Two stages, each holding one job that is finished on arrival.
///
/// Two rather than one because the defect has two shapes: the pipeline write
/// failing after both stages settled, and the second stage's write failing
/// after the first one settled. A single-stage graph can only show the first.
struct Graph {
    pipeline_id: i64,
    first_stage_id: i64,
    second_stage_id: i64,
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
        "CI Initial Graph Owner",
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

    let mut stage_ids = Vec::new();
    for (order, name) in [(0, "build"), (1, "deploy")] {
        let stage = pipeline_ops::create_stage(&db, pipeline.id, name, order)
            .await
            .expect("create a stage");
        let job = pipeline_ops::create_job(
            &db,
            stage.id,
            &format!("{name}-excluded"),
            "true",
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
        .expect("create a job");
        // What a matrix leg excluded by `if:` looks like the moment the graph
        // is published: finished, unsuccessful in no way, and waiting for a
        // verdict on its stage that no runner will ever produce.
        pipeline_ops::update_job_result(
            &db,
            job.id,
            "skipped",
            None,
            None,
            None,
            Some(chrono::Utc::now().naive_utc()),
        )
        .await
        .expect("publish the job as skipped");
        stage_ids.push(stage.id);
    }

    (
        temp,
        db,
        Graph {
            pipeline_id: pipeline.id,
            first_stage_id: stage_ids[0],
            second_stage_id: stage_ids[1],
        },
    )
}

/// Abort every UPDATE on one table, and nothing else.
///
/// Narrower than dropping the table: the roll-up reads the stage list and every
/// job in it before it writes anything, and those reads have to keep working
/// for the rollback of the earlier write to be the thing under test.
async fn fail_updates_on(db: &DatabaseConnection, table: &str) {
    db.execute_unprepared(&format!(
        "CREATE TRIGGER fk_initial_fault_{table} BEFORE UPDATE ON {table} \
         BEGIN SELECT RAISE(ABORT, 'injected failure: UPDATE on {table}'); END;"
    ))
    .await
    .unwrap_or_else(|error| panic!("arm the UPDATE fault on {table}: {error}"));
}

/// Abort the UPDATE of exactly one stage row.
///
/// The whole-table form cannot show the multi-stage half of the defect: it
/// aborts the *first* stage write, so there is no earlier successful write left
/// to roll back. The `WHEN` is what moves the fault to the second one.
async fn fail_update_of_stage(db: &DatabaseConnection, stage_id: i64) {
    db.execute_unprepared(&format!(
        "CREATE TRIGGER fk_initial_fault_one_stage BEFORE UPDATE ON pipeline_stages \
         WHEN OLD.id = {stage_id} \
         BEGIN SELECT RAISE(ABORT, 'injected failure: UPDATE on pipeline_stages'); END;"
    ))
    .await
    .unwrap_or_else(|error| panic!("arm the UPDATE fault on stage {stage_id}: {error}"));
}

async fn clear_fault(db: &DatabaseConnection, trigger: &str) {
    db.execute_unprepared(&format!("DROP TRIGGER IF EXISTS {trigger}"))
        .await
        .unwrap_or_else(|error| panic!("disarm the fault {trigger}: {error}"));
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

fn assert_injected(error: &anyhow::Error) {
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("injected failure"),
        "the failure that surfaced is not the injected one: {rendered}"
    );
}

/// The stage verdicts and the pipeline verdict are one write or none.
#[tokio::test]
async fn an_initial_roll_up_that_cannot_finish_its_pipeline_publishes_no_stage() {
    let (_temp, db, graph) = setup("pipelinewrite").await;

    fail_updates_on(&db, "pipelines").await;
    let error = pipeline_ops::settle_initial_graph(&db, graph.pipeline_id)
        .await
        .expect_err("a pipeline roll-up the database refuses must escape as an error");
    assert_injected(&error);
    clear_fault(&db, "fk_initial_fault_pipelines").await;

    assert_eq!(
        stage_status(&db, graph.first_stage_id).await,
        "pending",
        "the first stage's verdict was published even though the pipeline could not follow"
    );
    assert_eq!(
        stage_status(&db, graph.second_stage_id).await,
        "pending",
        "the second stage's verdict was published even though the pipeline could not follow"
    );
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "pending");

    pipeline_ops::settle_initial_graph(&db, graph.pipeline_id)
        .await
        .expect("the same roll-up goes through once the database recovers");
    assert_eq!(stage_status(&db, graph.first_stage_id).await, "success");
    assert_eq!(stage_status(&db, graph.second_stage_id).await, "success");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "success");
}

/// The stage that settled first does not survive the one that could not.
#[tokio::test]
async fn an_initial_roll_up_that_cannot_settle_its_second_stage_publishes_neither() {
    let (_temp, db, graph) = setup("stagewrite").await;

    fail_update_of_stage(&db, graph.second_stage_id).await;
    let error = pipeline_ops::settle_initial_graph(&db, graph.pipeline_id)
        .await
        .expect_err("a stage roll-up the database refuses must escape as an error");
    assert_injected(&error);
    clear_fault(&db, "fk_initial_fault_one_stage").await;

    assert_eq!(
        stage_status(&db, graph.first_stage_id).await,
        "pending",
        "the first stage was published terminal while the second one could not follow"
    );
    assert_eq!(stage_status(&db, graph.second_stage_id).await, "pending");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "pending");

    pipeline_ops::settle_initial_graph(&db, graph.pipeline_id)
        .await
        .expect("the same roll-up goes through once the database recovers");
    assert_eq!(stage_status(&db, graph.first_stage_id).await, "success");
    assert_eq!(stage_status(&db, graph.second_stage_id).await, "success");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "success");
}

/// A graph with runnable work still on it is left exactly as it was.
///
/// The roll-up is not allowed to invent a verdict: a stage holding a `pending`
/// job has not finished, and neither has the pipeline above it. This is the
/// half a transaction that settled too eagerly would break while both fault
/// tests above stayed green.
#[tokio::test]
async fn an_initial_roll_up_leaves_a_runnable_graph_alone() {
    let (_temp, db, graph) = setup("runnable").await;
    let job = pipeline_ops::create_job(
        &db,
        graph.second_stage_id,
        "real-work",
        "true",
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
    .expect("create the job that keeps the second stage runnable");
    assert_eq!(job.status, "pending");

    pipeline_ops::settle_initial_graph(&db, graph.pipeline_id)
        .await
        .expect("a runnable graph is not an error");

    assert_eq!(
        stage_status(&db, graph.first_stage_id).await,
        "success",
        "the stage whose every job was skipped still gets its verdict"
    );
    assert_eq!(
        stage_status(&db, graph.second_stage_id).await,
        "pending",
        "a stage still holding a pending job must not be rolled up"
    );
    assert_eq!(
        pipeline_status(&db, graph.pipeline_id).await,
        "pending",
        "the pipeline must not settle while a stage below it is still runnable"
    );
}

/// The gate a stage advertises and the gate its pipeline advertises are one
/// write or none.
///
/// The external-runner branch of the same producer parks the first stage on its
/// manual gate, and it is the request's last word on the graph for exactly the
/// same reason: nothing behind it will run again. The pool-level spelling wrote
/// the stage's gate on one connection and the pipeline's on the next, so a
/// fault in between advertised a gate to the UI and the release routes on a run
/// still calling itself `pending`.
#[tokio::test]
async fn an_initial_manual_gate_that_cannot_reach_its_pipeline_publishes_nothing() {
    let (_temp, db, graph) = setup("manualgate").await;
    let job = pipeline_ops::create_job(
        &db,
        graph.first_stage_id,
        "hold-for-a-human",
        "true",
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
    .expect("create the job the gate is about");
    pipeline_ops::update_job_result(&db, job.id, "manual", None, None, None, None)
        .await
        .expect("park the job on its manual gate");

    fail_updates_on(&db, "pipelines").await;
    let error =
        pipeline_ops::pause_initial_stage_at_manual(&db, graph.pipeline_id, graph.first_stage_id)
            .await
            .expect_err("a gate the database refuses to publish must escape as an error");
    assert_injected(&error);
    clear_fault(&db, "fk_initial_fault_pipelines").await;

    assert_eq!(
        stage_status(&db, graph.first_stage_id).await,
        "pending",
        "the stage advertised a manual gate its pipeline never got"
    );
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "pending");

    assert!(
        pipeline_ops::pause_initial_stage_at_manual(&db, graph.pipeline_id, graph.first_stage_id)
            .await
            .expect("the same pause goes through once the database recovers"),
        "the gate is still there to publish"
    );
    assert_eq!(stage_status(&db, graph.first_stage_id).await, "manual");
    assert_eq!(pipeline_status(&db, graph.pipeline_id).await, "manual");
}
