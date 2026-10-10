//! Streaming log storage and range reads over a large number of appends.

use rg_db::sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

async fn job() -> (rg_db::DatabaseConnection, tempfile::TempDir, i64) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("log.db").display());
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .unwrap();
    rg_db::run_migrations(&db).await.unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        1,
        "deadbeef",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "build", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        &db,
        stage.id,
        "compile",
        "cargo build",
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
    .unwrap();
    (db, dir, job.id)
}

#[tokio::test]
async fn ten_thousand_chunks_do_not_rewrite_the_log_column() {
    let (db, _dir, id) = job().await;
    let chunk = "x".repeat(1024);
    for _ in 0..10_000 {
        rg_db::ops::pipeline_ops::append_job_log(&db, id, &chunk)
            .await
            .unwrap();
    }
    let row = rg_db::ops::pipeline_ops::get_job(&db, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.log, None, "streaming appends must not rewrite the base");
    assert_eq!(row.log_length, 10_000 * 1024 + 9_999);
    assert_eq!(
        rg_db::entities::job_log_chunk::Entity::find()
            .filter(rg_db::entities::job_log_chunk::Column::JobId.eq(id))
            .count(&db)
            .await
            .unwrap(),
        10_000
    );
    let tail = rg_db::ops::pipeline_ops::get_job_log_slice(&db, id, row.log_length - 1025, 1025)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tail.content.as_deref(), Some(format!("\n{chunk}").as_str()));

    // Full-log retries replace both the old base and all queued segments.
    rg_db::ops::pipeline_ops::update_job_log(&db, id, "done")
        .await
        .unwrap();
    let replaced = rg_db::ops::pipeline_ops::get_job_log_slice(&db, id, 0, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replaced.content.as_deref(), Some("done"));
    assert_eq!(replaced.total_length, Some(4));
    assert_eq!(
        rg_db::entities::job_log_chunk::Entity::find()
            .filter(rg_db::entities::job_log_chunk::Column::JobId.eq(id))
            .count(&db)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn chunk_ranges_count_unicode_characters_and_split_large_uploads() {
    let (db, _dir, id) = job().await;
    let long = "é".repeat(40_000);
    rg_db::ops::pipeline_ops::append_job_log(&db, id, &long)
        .await
        .unwrap();
    rg_db::ops::pipeline_ops::append_job_log(&db, id, "終")
        .await
        .unwrap();
    let slice = rg_db::ops::pipeline_ops::get_job_log_slice(&db, id, 39_997, 5)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(slice.content.as_deref(), Some("ééé\n終"));
    assert_eq!(slice.total_length, Some(40_002));
}
