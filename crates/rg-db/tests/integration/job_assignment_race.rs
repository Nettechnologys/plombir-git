//! card_ce65e08d78aa: handing a CI job to a runner was "find, then write".
//!
//! `poll_job` picks a candidate with `find_pending_job_matching_labels`
//! (`status = 'pending' AND runner_id IS NULL`) and calls `assign_job`
//! afterwards. `assign_job` used to accept any *active* row, and `assigned` is
//! itself an active status — so a second runner polling in the same instant
//! overwrote `runner_id` on a job the first one had already been answered `200`
//! for. The loser was told to run a job that stopped being its own a
//! millisecond later, and learnt about it only when its `/start` and `/finish`
//! came back `404 job not found`, because `assigned_job` matches on the runner
//! the row now names.
//!
//! What these tests guard:
//!
//! * **One claim wins.** Eight runners racing one pending job produce exactly
//!   one `true`, and the row names that runner.
//! * **The refusal is a `false`, not an error.** A poller that lost has to be
//!   able to look for the next candidate.
//! * **A canceled job is still not claimable** — the property the old, wider
//!   filter did cover, kept.
//! * **The claim is not vacuous**: an ordinary first assignment still lands.

use rg_db::entities::repository;
use rg_db::sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, NotSet, Set, Statement,
};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-job-claim-race-{label}-{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        Self { path }
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

/// A migrated database with more than one pooled connection, so the racing
/// tasks really do run their statements against separate connections.
async fn setup(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

/// One pending job, plus the runners that will fight over it. `label` keeps the
/// unique keys (account, repository, runner names) apart when one test builds
/// more than one fixture on the same database.
async fn fixture(db: &DatabaseConnection, label: &str, runners: usize) -> (i64, Vec<i64>) {
    let user = rg_db::ops::user_ops::create_user(
        db,
        &format!("dana-{label}"),
        &format!("dana-{label}@example.com"),
        "",
        "Dana",
    )
    .await
    .expect("create the account the rows hang off");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set(format!("forge-{label}")),
            description: Set(None),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            origin_repo_id: Set(None),
        },
    )
    .await
    .expect("create the repository the pipeline hangs off");
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo.id,
        "1111111111111111111111111111111111111111",
        "refs/heads/main",
        "push",
        Some(user.id),
    )
    .await
    .expect("create pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "unit", "echo ok", None, None, None, None, None, None, false, None, None,
        None,
    )
    .await
    .expect("create job");

    let mut runner_ids = Vec::with_capacity(runners);
    for i in 0..runners {
        let (runner, _runner_token) = rg_db::ops::runner_ops::register_runner(
            db,
            repo.id,
            &format!("runner-{label}-{i}"),
            r#"["linux"]"#,
            None,
            None,
            None,
        )
        .await
        .expect("register runner");
        runner_ids.push(runner.id);
    }
    (job.id, runner_ids)
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await
        .expect("query")
        .expect("one row")
        .try_get::<i64>("", "n")
        .expect("count column")
}

/// How many runners poll at the same instant. Enough that several of them read
/// the same candidate before any of them has written to it.
const RUNNERS: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_claims_on_one_pending_job_leave_exactly_one_winner() {
    let (db, _temp) = setup("claim").await;
    let (job_id, runner_ids) = fixture(&db, "race", RUNNERS).await;

    let attempts = runner_ids.iter().copied().map(|runner_id| {
        let db = db.clone();
        async move {
            rg_db::ops::pipeline_ops::assign_job(&db, job_id, runner_id)
                .await
                .map(|claimed| (runner_id, claimed))
        }
    });

    let results = futures_join_all(attempts).await;

    // A refused claim is an ordinary outcome, not a failure: the poller that
    // lost has to be able to go looking for the next candidate.
    for (i, result) in results.iter().enumerate() {
        assert!(
            result.is_ok(),
            "runner {i} lost the race and got an error instead of a refusal: {:?}",
            result.as_ref().err()
        );
    }

    let winners: Vec<i64> = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .filter(|(_, claimed)| *claimed)
        .map(|(runner_id, _)| *runner_id)
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "one job may be claimed by exactly one runner, got {winners:?}"
    );

    // The row has to agree with the answer the winner was given — that identity
    // is what `/start` and `/finish` are later matched against.
    let job = rg_db::ops::pipeline_ops::get_job(&db, job_id)
        .await
        .expect("reload the claimed job")
        .expect("the job still exists");
    assert_eq!(job.status, "assigned");
    assert_eq!(
        job.runner_id,
        Some(winners[0]),
        "the row must name the runner whose claim returned true",
    );
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT COUNT(*) AS n FROM pipeline_jobs WHERE id = {job_id} AND status = 'assigned'"),
        )
        .await,
        1,
    );
}

/// Non-vacuity, and the property the previous, wider filter already had: a job
/// the server has disowned is not claimable either, so the narrowing did not
/// merely swap one hole for another.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_claim_lands_and_a_canceled_job_is_never_claimed() {
    let (db, _temp) = setup("cancel").await;
    let (job_id, runner_ids) = fixture(&db, "first", 2).await;

    assert!(
        rg_db::ops::pipeline_ops::assign_job(&db, job_id, runner_ids[0])
            .await
            .expect("claim an untouched pending job"),
        "an uncontested claim on a pending job must land",
    );
    assert!(
        !rg_db::ops::pipeline_ops::assign_job(&db, job_id, runner_ids[1])
            .await
            .expect("a second claim is a refusal, not an error"),
        "a job already assigned must not be re-claimed by another runner",
    );

    // A fresh pending job, canceled before anyone claims it.
    let (canceled_id, canceled_runners) = fixture(&db, "canceled", 1).await;
    assert!(rg_db::ops::pipeline_ops::settle_job_if_active(
        &db,
        canceled_id,
        "canceled",
        None,
        None,
        None
    )
    .await
    .expect("cancel the job"),);
    assert!(
        !rg_db::ops::pipeline_ops::assign_job(&db, canceled_id, canceled_runners[0])
            .await
            .expect("claiming a canceled job is a refusal, not an error"),
        "work the server has already answered `canceled` for must not be handed out",
    );
    let canceled = rg_db::ops::pipeline_ops::get_job(&db, canceled_id)
        .await
        .expect("reload the canceled job")
        .expect("the job still exists");
    assert_eq!(canceled.status, "canceled");
    assert_eq!(canceled.runner_id, None);
}

/// `futures::future::join_all` without taking a dependency on `futures` for one
/// call: poll the futures together by handing them to the runtime as tasks.
async fn futures_join_all<F>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futures.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await.expect("task panicked"));
    }
    out
}
