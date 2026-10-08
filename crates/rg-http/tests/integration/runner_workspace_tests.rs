use crate::common::{register_full, spawn_test_app_with_db};
use sea_orm::{ActiveModelTrait, Set};

#[tokio::test]
async fn assigned_runner_downloads_exact_commit_workspace_and_other_runner_is_denied() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "workspace-owner", "workspace@example.com").await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name":"workspace","auto_init":true,"readme":"default"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let log = client
        .get(format!("{base}/api/v1/repos/workspace-owner/workspace/log"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(log.status(), 200);
    let sha = log.json::<serde_json::Value>().await.unwrap()["commits"][0]["sha"]
        .as_str()
        .unwrap()
        .to_owned();

    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "workspace-runner",
        "[]",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let (other, other_token) = rg_db::ops::runner_ops::register_runner(
        &db,
        repo_id,
        "other-runner",
        "[]",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        &sha,
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        &db,
        stage.id,
        "workspace",
        "test -f README.md",
        None,
        None,
        None,
        Some("build-main"),
        Some(r#"["target"]"#),
        None,
        false,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    rg_db::ops::pipeline_ops::assign_job(&db, job.id, runner.id)
        .await
        .unwrap();

    let denied = client
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/{}/workspace",
            other.id, job.id
        ))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    // 404, not 403: another runner's job must not be tellable apart from a job
    // that does not exist — see
    // `runner_auth_tests::another_runners_job_id_is_indistinguishable_from_an_unused_one`.
    assert_eq!(denied.status(), 404);
    let archive = client
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/{}/workspace",
            runner.id, job.id
        ))
        .bearer_auth(&runner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(archive.status(), 200);
    let bytes = archive.bytes().await.unwrap();
    let names = tar::Archive::new(std::io::Cursor::new(bytes))
        .entries()
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    assert!(names.iter().any(|name| name == "README.md"), "{names:?}");

    let uploaded = client
        .put(format!(
            "{base}/api/v1/runners/{}/jobs/{}/cache",
            runner.id, job.id
        ))
        .bearer_auth(&runner_token)
        .header("x-cache-key", "build-main")
        .body("cache-bytes")
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), 204);
    let downloaded = client
        .get(format!(
            "{base}/api/v1/runners/{}/jobs/{}/cache",
            runner.id, job.id
        ))
        .bearer_auth(&runner_token)
        .header("x-cache-key", "build-main")
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), 200);
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), b"cache-bytes");
    // The row key is scoped to the job's ref (card_b7a25458b98b).
    let key_hash = rg_core::ci_cache::CacheScope::for_pipeline("refs/heads/main", "main")
        .save_hash("build-main");
    let entry = rg_db::ops::ci_retention_ops::find_cache_entry(&db, repo_id, &key_hash)
        .await
        .unwrap()
        .unwrap();
    assert!(entry.expires_at > chrono::Utc::now());
    let mut expired: rg_db::entities::ci_cache_entry::ActiveModel = entry.into();
    expired.expires_at = Set(chrono::Utc::now() - chrono::Duration::minutes(1));
    expired.update(&db).await.unwrap();
    let cleanup = client
        .delete(format!(
            "{base}/api/v1/repos/workspace-owner/workspace/actions/retention/expired"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(cleanup.status(), 200);
    assert_eq!(
        cleanup.json::<serde_json::Value>().await.unwrap()["caches_deleted"],
        1
    );
    assert!(
        rg_db::ops::ci_retention_ops::find_cache_entry(&db, repo_id, &key_hash)
            .await
            .unwrap()
            .is_none()
    );
}
