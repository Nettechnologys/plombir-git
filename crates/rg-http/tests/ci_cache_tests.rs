mod common;

use common::{register_full, spawn_test_app_with_db};
use sea_orm::{ActiveModelTrait, Set};
use sha2::{Digest, Sha256};

async fn create_private_repo(base: &str, token: &str, name: &str) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create private repo failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// Create a pipeline job that declares a cache so the cache endpoints accept it.
async fn create_cached_job(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    runner_id: i64,
    cache_key: &str,
) -> i64 {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        db,
        stage.id,
        "unit",
        "echo ok",
        None,
        None,
        None,
        Some(cache_key),
        Some("[\"target\"]"),
        false,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    rg_db::ops::pipeline_ops::assign_job(db, job.id, runner_id)
        .await
        .unwrap();
    job.id
}

fn key_hash(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

#[tokio::test]
async fn ci_cache_round_trip_records_and_verifies_content_digest() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "cache_owner", "cache_owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "private-cache").await;
    let runner = rg_db::ops::runner_ops::register_runner(&db, "cache-runner", "", None, None, None)
        .await
        .unwrap();
    let cache_key = "deps-v1";
    let job_id = create_cached_job(&db, repo_id, runner.id, cache_key).await;

    let archive = b"cache archive bytes";
    let expected_sha = hex::encode(Sha256::digest(archive));
    assert_eq!(expected_sha.len(), 64);

    // Upload the cache archive (PUT with the cache key header).
    let upload = client
        .put(format!(
            "{}/api/v1/runners/{}/jobs/{}/cache",
            base, runner.id, job_id
        ))
        .bearer_auth(&runner.token)
        .header("x-cache-key", cache_key)
        .body(archive.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), 204, "cache upload should succeed");

    // The stored entry records the SHA-256 of the archive *contents*, distinct
    // from `key_hash` (which digests the cache key).
    let entry = rg_db::ops::ci_retention_ops::find_cache_entry(&db, repo_id, &key_hash(cache_key))
        .await
        .unwrap()
        .expect("cache entry persisted");
    assert_eq!(entry.sha256.as_deref(), Some(expected_sha.as_str()));
    assert_ne!(
        entry.sha256.as_deref(),
        Some(entry.key_hash.as_str()),
        "content digest must not equal the key hash"
    );

    // Download echoes the digest header and returns the exact bytes.
    let download = client
        .get(format!(
            "{}/api/v1/runners/{}/jobs/{}/cache",
            base, runner.id, job_id
        ))
        .bearer_auth(&runner.token)
        .header("x-cache-key", cache_key)
        .send()
        .await
        .unwrap();
    assert_eq!(download.status(), 200);
    assert_eq!(
        download
            .headers()
            .get("x-checksum-sha256")
            .and_then(|v| v.to_str().ok()),
        Some(expected_sha.as_str()),
    );
    assert_eq!(download.bytes().await.unwrap().as_ref(), archive);

    // Corrupt the recorded digest: the on-disk archive no longer matches, so the
    // server-side integrity check must reject the download instead of serving a
    // potentially poisoned cache.
    let mut active: rg_db::entities::ci_cache_entry::ActiveModel = entry.into();
    active.sha256 = Set(Some(
        "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
    ));
    active.update(&db).await.unwrap();

    let tampered = client
        .get(format!(
            "{}/api/v1/runners/{}/jobs/{}/cache",
            base, runner.id, job_id
        ))
        .bearer_auth(&runner.token)
        .header("x-cache-key", cache_key)
        .send()
        .await
        .unwrap();
    assert_eq!(
        tampered.status(),
        500,
        "digest mismatch must fail the cache download"
    );
}
