//! Regression coverage for `card_4d1d8b9fba56`.
//!
//! Retiring an unreachable runner is two writes that are only correct together:
//! its jobs go back to the queue, and it goes `offline`. The watchdog made them
//! separately, status first, and `find_offline_runners` selects on
//! `status IN ('online', 'busy')` — so a reset that failed after the status
//! landed left the jobs pinned to a runner that branch would never look at
//! again. The loop had written the row out of its own selection, and the rows
//! were left to the ten-minute stuck-job sweep instead of to the sixty-second
//! retry that was supposed to cover them.
//!
//! What these tests hold onto is convergence, not the ordering that happens to
//! produce it: after a failed retirement the runner must still be *selected* by
//! the query that drives the branch.

use rg_db::entities::repository;
use rg_db::sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, NotSet, Set, Statement,
};

struct TempDb(std::path::PathBuf);

impl TempDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "forgekeep-runner-retirement-{}.db",
            uuid::Uuid::new_v4().simple()
        )))
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.0.display())
    }
}

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "test cleanup must not hide the assertion that failed"
    )]
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

async fn setup() -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new();
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

/// A runner holding one `assigned` job, with a heartbeat old enough that the
/// watchdog's own query considers it unreachable.
async fn unreachable_runner_holding_a_job(db: &DatabaseConnection, label: &str) -> (i64, i64) {
    let user = rg_db::ops::user_ops::create_user(
        db,
        &format!("retire-owner-{label}"),
        &format!("retire-owner-{label}@example.invalid"),
        "",
        "Retire Owner",
    )
    .await
    .expect("create fixture user");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set(format!("retire-{label}")),
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
    .expect("create fixture repository");
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo.id,
        "2222222222222222222222222222222222222222",
        "refs/heads/main",
        "push",
        Some(user.id),
    )
    .await
    .expect("create fixture pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create fixture stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "unit", "echo ok", None, None, None, None, None, None, false, None, None,
        None,
    )
    .await
    .expect("create fixture job");
    let (runner, _) = rg_db::ops::runner_ops::register_runner(
        db,
        repo.id,
        &format!("runner-{label}"),
        r#"["linux"]"#,
        None,
        None,
        None,
    )
    .await
    .expect("register fixture runner");

    assert!(
        rg_db::ops::pipeline_ops::assign_job(db, job.id, runner.id)
            .await
            .expect("assign the fixture job"),
        "the fixture job was not assigned to its runner"
    );
    // A runner is registered `offline` and only goes `online` when it polls, so
    // the fixture has to put it in the state the watchdog actually finds: an
    // online runner whose heartbeat has stopped.
    rg_db::ops::runner_ops::update_status(db, runner.id, "online")
        .await
        .expect("bring the fixture runner online");
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "UPDATE runners SET last_seen_at = ? WHERE id = ?",
        ["2024-01-01 00:00:00".into(), runner.id.into()],
    ))
    .await
    .expect("age the runner heartbeat");

    (job.id, runner.id)
}

async fn offline_candidates(db: &DatabaseConnection) -> Vec<i64> {
    rg_db::ops::pipeline_ops::find_offline_runners(db, 90)
        .await
        .expect("find offline runners")
        .into_iter()
        .map(|runner| runner.id)
        .collect()
}

async fn job_status(db: &DatabaseConnection, job_id: i64) -> String {
    rg_db::ops::pipeline_ops::get_job(db, job_id)
        .await
        .expect("read the job back")
        .expect("the job is still there")
        .status
}

async fn runner_status(db: &DatabaseConnection, runner_id: i64) -> String {
    rg_db::ops::runner_ops::find_by_id(db, runner_id)
        .await
        .expect("read the runner back")
        .expect("the runner is still there")
        .status
}

/// The healthy path, so the convergence test below cannot pass by the
/// retirement quietly doing nothing at all.
#[tokio::test]
async fn a_runner_that_stopped_answering_loses_its_jobs_and_its_status() {
    let (db, _temp) = setup().await;
    let (job_id, runner_id) = unreachable_runner_holding_a_job(&db, "healthy").await;

    assert!(
        offline_candidates(&db).await.contains(&runner_id),
        "the fixture runner is not a candidate, so nothing below is being tested"
    );

    let released = rg_db::ops::runner_ops::retire_unreachable_runner(&db, runner_id)
        .await
        .expect("retire the unreachable runner");

    assert_eq!(released, 1, "the runner's job was not handed back");
    assert_eq!(job_status(&db, job_id).await, "pending");
    assert_eq!(runner_status(&db, runner_id).await, "offline");
    assert!(
        !offline_candidates(&db).await.contains(&runner_id),
        "a retired runner must leave the branch's selection"
    );
}

/// The defect itself: the job reset is refused, and the next tick still has to
/// be able to finish the work — by its *own* query, without the ten-minute
/// stuck-job sweep being involved.
#[tokio::test]
async fn a_refused_job_reset_leaves_the_runner_where_the_next_tick_finds_it() {
    let (db, _temp) = setup().await;
    let (job_id, runner_id) = unreachable_runner_holding_a_job(&db, "refused").await;

    // Take the jobs table away, which is the one failure that can be produced
    // from outside without a fault-injection seam. What it stands in for is any
    // reason the reset does not land: a lock, an outage, a constraint.
    db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        "ALTER TABLE pipeline_jobs RENAME TO pipeline_jobs_hidden".to_string(),
    ))
    .await
    .expect("hide the jobs table");

    let refused = rg_db::ops::runner_ops::retire_unreachable_runner(&db, runner_id).await;
    assert!(
        refused.is_err(),
        "the reset was supposed to fail; the rest of this test proves nothing"
    );

    assert_eq!(
        runner_status(&db, runner_id).await,
        "online",
        "the status write landed even though the job reset did not — the runner is now invisible \
         to the branch that was supposed to retry it"
    );
    assert!(
        offline_candidates(&db).await.contains(&runner_id),
        "the next watchdog tick will not select this runner, so its jobs wait on the ten-minute \
         stuck-job sweep instead of on the sixty-second retry"
    );

    // The next tick, with whatever made the reset fail now over.
    db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        "ALTER TABLE pipeline_jobs_hidden RENAME TO pipeline_jobs".to_string(),
    ))
    .await
    .expect("restore the jobs table");

    let released = rg_db::ops::runner_ops::retire_unreachable_runner(&db, runner_id)
        .await
        .expect("the retry retires the runner");
    assert_eq!(released, 1);
    assert_eq!(job_status(&db, job_id).await, "pending");
    assert_eq!(runner_status(&db, runner_id).await, "offline");
}
