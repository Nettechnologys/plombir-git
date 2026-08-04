use crate::common::fault::{fail_db_writes, spawn_test_app_for_fault_sweep, DbWrite};
use crate::common::{register_full, spawn_test_app_with_db};
use sea_orm::{ActiveModelTrait, ConnectionTrait, Set};
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
    let (runner, runner_token) =
        rg_db::ops::runner_ops::register_runner(&db, "cache-runner", "", None, None, None)
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
        .bearer_auth(&runner_token)
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
        .bearer_auth(&runner_token)
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
        .bearer_auth(&runner_token)
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

/// Where an uploaded archive and its staging file live, so a test can assert
/// that a failed upload left neither behind.
fn cache_dir(repo_root: &std::path::Path, repo_id: i64) -> std::path::PathBuf {
    repo_root.join("_ci_cache").join(repo_id.to_string())
}

/// The files a failed upload left in the cache directory, sorted for a stable
/// assertion message.
fn leftover_cache_files(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // No directory at all is the strongest form of "nothing was left".
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.expect("read cache dir entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Set up a repo + runner + cache-declaring job, and return what an upload needs.
async fn cache_upload_fixture(
    base: &str,
    db: &rg_db::DatabaseConnection,
    login: &str,
    repo_name: &str,
    cache_key: &str,
) -> (i64, i64, i64, String) {
    let (owner_token, _owner_id) =
        register_full(base, login, &format!("{login}@example.com")).await;
    let repo_id = create_private_repo(base, &owner_token, repo_name).await;
    let (runner, runner_token) =
        rg_db::ops::runner_ops::register_runner(db, "cache-runner", "", None, None, None)
            .await
            .unwrap();
    let job_id = create_cached_job(db, repo_id, runner.id, cache_key).await;
    (repo_id, runner.id, job_id, runner_token)
}

/// A retention-policy read that fails must not leave the archive on disk.
///
/// The archive is renamed to its *final* path before the policy is read, and
/// retention only ever walks the rows of `ci_cache_entries`. An upload that
/// returns 500 with the file still there has leaked a cache-sized archive that
/// nothing will ever come back for — and `download_cache`, which also resolves
/// through the database, cannot even see it.
#[tokio::test]
async fn a_failed_policy_read_takes_the_uploaded_archive_with_it() {
    let app = spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let cache_key = "deps-v1";
    let (repo_id, runner_id, job_id, runner_token) = cache_upload_fixture(
        &app.base,
        &app.db,
        "cache_policy_outage",
        "policy-outage",
        cache_key,
    )
    .await;

    // The one dependency this upload has past the rename. Dropped rather than
    // trigger-faulted: `get_policy` reads, and SQLite has no `BEFORE SELECT`.
    app.db
        .execute_unprepared(
            "PRAGMA foreign_keys = OFF;\nDROP TABLE IF EXISTS ci_retention_policies;",
        )
        .await
        .expect("failed to take the retention policy table away");

    let upload = client
        .put(format!(
            "{}/api/v1/runners/{}/jobs/{}/cache",
            app.base, runner_id, job_id
        ))
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .body(b"cache archive bytes".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        upload.status(),
        500,
        "a failed policy read is the server's fault, not the runner's"
    );

    let leftovers = leftover_cache_files(&cache_dir(&app.repo_root, repo_id));
    assert!(
        leftovers.is_empty(),
        "the failed upload left files behind that no row points at: {leftovers:?}"
    );
}

/// A retry of a key that already has an archive must not let its own failure
/// destroy the cache the live row still names.
///
/// Every upload used to be renamed onto one stable `<key_hash>.tar`, so a retry
/// overwrote the previous publication's bytes before anything had confirmed the
/// new ones — and the compensation then deleted that same path as if the failed
/// request owned it. One failed retry was enough to turn a working cache hit
/// into a row pointing at an archive that no longer exists.
#[tokio::test]
async fn a_failed_retry_keeps_the_previous_cache_archive_downloadable() {
    let app = spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let cache_key = "deps-v3";
    let (repo_id, runner_id, job_id, runner_token) = cache_upload_fixture(
        &app.base,
        &app.db,
        "cache_retry_outage",
        "retry-outage",
        cache_key,
    )
    .await;
    let url = format!(
        "{}/api/v1/runners/{}/jobs/{}/cache",
        app.base, runner_id, job_id
    );

    let published = b"the cache that already works".to_vec();
    let first = client
        .put(url.clone())
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .body(published.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 204, "the first upload must succeed");

    // The row exists now, so the retry's `upsert_cache_entry` updates it — the
    // last step before the new archive would become the live one.
    let fault = fail_db_writes(&app.db, "ci_cache_entries", DbWrite::Update).await;
    let retry = client
        .put(url.clone())
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .body(b"the cache that never landed".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        retry.status(),
        500,
        "a lost cache row must fail the retry, not answer 204"
    );
    fault.clear().await;

    let download = client
        .get(url)
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .send()
        .await
        .unwrap();
    assert_eq!(
        download.status(),
        200,
        "the failed retry took the previously published cache with it"
    );
    assert_eq!(
        download
            .headers()
            .get("x-checksum-sha256")
            .and_then(|value| value.to_str().ok()),
        Some(hex::encode(Sha256::digest(&published)).as_str()),
        "the archive on disk is no longer the one the row vouches for",
    );
    assert_eq!(download.bytes().await.unwrap().as_ref(), &published[..]);

    assert_eq!(
        leftover_cache_files(&cache_dir(&app.repo_root, repo_id)).len(),
        1,
        "the failed retry kept an archive no row points at"
    );
}

/// Two publications of one key racing each other: whichever the row ends up
/// naming must still be downloadable, byte for byte.
///
/// The loser may leave its archive behind — waste, and the deliberate side of
/// the trade — but it must never delete the winner's. Under one stable path the
/// two renames and the two row updates could interleave into a row vouching for
/// a digest that belongs to the other upload's bytes, which the download's
/// integrity check reports as a corrupted cache.
#[tokio::test]
async fn concurrent_uploads_of_one_cache_key_keep_the_winner_downloadable() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let cache_key = "deps-v4";
    let (_repo_id, runner_id, job_id, runner_token) =
        cache_upload_fixture(&base, &db, "cache_race", "race", cache_key).await;
    let url = format!("{base}/api/v1/runners/{runner_id}/jobs/{job_id}/cache");

    let one = vec![b'a'; 4096];
    let two = vec![b'b'; 8192];
    let upload = |body: Vec<u8>| {
        client
            .put(url.clone())
            .bearer_auth(&runner_token)
            .header("x-cache-key", cache_key)
            .body(body)
            .send()
    };
    let (first, second) = tokio::join!(upload(one.clone()), upload(two.clone()));
    assert_eq!(first.unwrap().status(), 204);
    assert_eq!(second.unwrap().status(), 204);

    let download = client
        .get(url)
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .send()
        .await
        .unwrap();
    // A 500 here is the integrity check: the row and the file it names disagree.
    assert_eq!(
        download.status(),
        200,
        "the entry no longer matches the archive it names"
    );
    let served = download.bytes().await.unwrap().to_vec();
    assert!(
        served == one || served == two,
        "the download served neither upload's bytes"
    );
}

/// The same for the cache row itself: the compensation must run on every branch
/// past the write, not only on the one that was noticed first.
#[tokio::test]
async fn a_failed_cache_row_takes_the_uploaded_archive_with_it() {
    let app = spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let cache_key = "deps-v2";
    let (repo_id, runner_id, job_id, runner_token) = cache_upload_fixture(
        &app.base,
        &app.db,
        "cache_row_outage",
        "row-outage",
        cache_key,
    )
    .await;

    let _fault = fail_db_writes(&app.db, "ci_cache_entries", DbWrite::Insert).await;
    let upload = client
        .put(format!(
            "{}/api/v1/runners/{}/jobs/{}/cache",
            app.base, runner_id, job_id
        ))
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .body(b"cache archive bytes".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        upload.status(),
        500,
        "a lost cache row must fail the upload, not answer 204"
    );

    let leftovers = leftover_cache_files(&cache_dir(&app.repo_root, repo_id));
    assert!(
        leftovers.is_empty(),
        "the failed upload left files behind that no row points at: {leftovers:?}"
    );
}
