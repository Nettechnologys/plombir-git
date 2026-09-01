//! Regression coverage for `card_3043059000b0`.
//!
//! The watchdog treats `pipeline_jobs.updated_at` as a liveness signal. These
//! tests prove that every production producer refreshes it, while a genuinely
//! silent assigned job still remains reclaimable.

use rg_db::entities::repository;
use rg_db::sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, NotSet, Set, Statement,
};

struct TempDb(std::path::PathBuf);

impl TempDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "forgekeep-job-liveness-{}.db",
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

async fn fixture(db: &DatabaseConnection, label: &str) -> (i64, i64) {
    let user = rg_db::ops::user_ops::create_user(
        db,
        &format!("runner-owner-{label}"),
        &format!("runner-owner-{label}@example.com"),
        "",
        "Runner Owner",
    )
    .await
    .expect("create fixture user");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set(format!("liveness-{label}")),
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
        "1111111111111111111111111111111111111111",
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
    .expect("create fixture runner");
    (job.id, runner.id)
}

async fn age_job(db: &DatabaseConnection, job_id: i64) {
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "UPDATE pipeline_jobs SET updated_at = ? WHERE id = ?",
        ["2024-01-01 00:00:00".into(), job_id.into()],
    ))
    .await
    .expect("age job liveness timestamp");
}

async fn stuck_ids(db: &DatabaseConnection) -> Vec<i64> {
    rg_db::ops::pipeline_ops::find_stuck_jobs(db, 600)
        .await
        .expect("find stuck jobs")
        .into_iter()
        .map(|job| job.id)
        .collect()
}

#[tokio::test]
async fn a_recent_log_refreshes_liveness_but_a_silent_assignment_is_reclaimed() {
    let (db, _temp) = setup().await;
    let (live_job, live_runner) = fixture(&db, "log").await;
    assert!(
        rg_db::ops::pipeline_ops::assign_job(&db, live_job, live_runner)
            .await
            .expect("assign live job")
    );
    age_job(&db, live_job).await;
    assert!(stuck_ids(&db).await.contains(&live_job));

    rg_db::ops::pipeline_ops::update_job_log(&db, live_job, "still building")
        .await
        .expect("persist runner log and liveness");
    assert!(
        !stuck_ids(&db).await.contains(&live_job),
        "a log just accepted from the owning runner is proof of liveness"
    );

    let (silent_job, silent_runner) = fixture(&db, "silent").await;
    assert!(
        rg_db::ops::pipeline_ops::assign_job(&db, silent_job, silent_runner)
            .await
            .expect("assign silent job")
    );
    age_job(&db, silent_job).await;
    assert!(
        stuck_ids(&db).await.contains(&silent_job),
        "an assigned job with no liveness for ten minutes must remain reclaimable"
    );
}

#[tokio::test]
async fn running_jobs_are_refreshed_by_external_and_embedded_heartbeats() {
    let (db, _temp) = setup().await;
    let (external_job, runner_id) = fixture(&db, "external").await;
    assert!(
        rg_db::ops::pipeline_ops::assign_job(&db, external_job, runner_id)
            .await
            .expect("assign external job")
    );
    assert!(
        rg_db::ops::pipeline_ops::start_job_if_active(&db, external_job, None)
            .await
            .expect("start external job")
    );
    age_job(&db, external_job).await;
    rg_db::ops::runner_ops::update_heartbeat(&db, runner_id)
        .await
        .expect("refresh runner and its job atomically");
    assert!(!stuck_ids(&db).await.contains(&external_job));

    let (embedded_job, embedded_runner) = fixture(&db, "embedded").await;
    assert!(
        rg_db::ops::pipeline_ops::assign_job(&db, embedded_job, embedded_runner)
            .await
            .expect("assign embedded fixture job")
    );
    assert!(
        rg_db::ops::pipeline_ops::start_job_if_active(&db, embedded_job, None)
            .await
            .expect("start embedded fixture job")
    );
    age_job(&db, embedded_job).await;
    assert!(
        rg_db::ops::pipeline_ops::touch_running_job(&db, embedded_job)
            .await
            .expect("refresh embedded job heartbeat")
    );
    assert!(!stuck_ids(&db).await.contains(&embedded_job));
}

#[tokio::test]
async fn result_updates_also_advance_the_watchdog_timestamp() {
    let (db, _temp) = setup().await;
    let (job_id, runner_id) = fixture(&db, "result").await;
    assert!(rg_db::ops::pipeline_ops::assign_job(&db, job_id, runner_id)
        .await
        .expect("assign result fixture"));
    age_job(&db, job_id).await;
    rg_db::ops::pipeline_ops::update_job_result(
        &db,
        job_id,
        "running",
        None,
        None,
        Some(chrono::Utc::now().naive_utc()),
        None,
    )
    .await
    .expect("update job result and liveness");
    assert!(!stuck_ids(&db).await.contains(&job_id));
}
