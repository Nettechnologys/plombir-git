//! Per-repository storage ceilings (`[limits]`, security audit PR #13): the
//! write paths that consult the shared budget, and the default that consults
//! nothing.
//!
//! The arithmetic is unit-tested in `rg_core::storage_quota`; these drive the
//! real routes, because the failure the audit found was a request that never
//! asked. Each case sets the ceiling it needs and asserts both the refusal and
//! that nothing was left behind — a 413 that still stored the bytes would pass
//! a status-only test.

use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use serde_json::Value;

use rg_core::storage_quota::StorageLimits;

use crate::common::{
    assert_blob_push_created, create_initialised_repo, create_issue, create_repo, register_full,
    spawn_test_app_with_db, spawn_test_app_with_overrides, StateOverrides,
};

/// Test state with the `[limits]` the test sets, and none of the others —
/// which is what an operator who sets one knob gets.
fn overrides_with(mutate: impl FnOnce(&mut StorageLimits)) -> StateOverrides {
    let mut limits = StorageLimits::default();
    mutate(&mut limits);
    StateOverrides {
        storage_limits: Some(limits),
        ..StateOverrides::default()
    }
}

/// A repository the storage route must ask a token for.
fn oid_of(payload: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(payload))
}

fn sha256(payload: &[u8]) -> String {
    format!("sha256:{}", oid_of(payload))
}

/// Upload one LFS object through the batch + PUT handshake, returning the PUT's
/// response so the caller can assert the refusal.
async fn put_lfs_object(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    payload: &[u8],
) -> reqwest::Response {
    let href = lfs_upload_href(base, token, owner, repo, payload).await;
    reqwest::Client::new()
        .put(href)
        .body(payload.to_vec())
        .send()
        .await
        .expect("LFS PUT")
}

/// The upload href a batch hands out for `payload`, asserting the batch offered
/// an action at all.
async fn lfs_upload_href(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    payload: &[u8],
) -> String {
    let batch = lfs_batch(base, token, owner, repo, &oid_of(payload), payload.len()).await;
    assert_eq!(
        batch.status(),
        200,
        "LFS batch must answer 200 for a fit object"
    );
    let batch: Value = batch.json().await.unwrap();
    batch["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .expect("upload action for an object that fits")
        .to_string()
}

async fn lfs_batch(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    oid: &str,
    size: usize,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/lfs/objects/batch"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": oid, "size": size}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .expect("LFS batch request")
}

async fn create_release(base: &str, token: &str, owner: &str, repo: &str, tag: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/releases"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "tag_name": tag,
            "title": tag,
            "body": "",
            "is_draft": false,
            "is_prerelease": false
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 201, "create release failed: {body}");
    body["id"].as_i64().unwrap()
}

async fn upload_release_asset(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    release_id: i64,
    name: &str,
    payload: &[u8],
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/{release_id}/assets"
        ))
        .bearer_auth(token)
        .header("content-type", "application/octet-stream")
        .header(
            "content-disposition",
            format!("attachment; filename={name}"),
        )
        .body(payload.to_vec())
        .send()
        .await
        .expect("release asset upload")
}

/// The row key a `main` pipeline's cache is stored under.
fn cache_key_hash(key: &str) -> String {
    rg_core::ci_cache::CacheScope::for_pipeline("refs/heads/main", "main").save_hash(key)
}

/// A pipeline job that declares a cache so the cache endpoints accept it.
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
        None,
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

async fn upload_cache(
    base: &str,
    runner_id: i64,
    runner_token: &str,
    job_id: i64,
    key: &str,
    payload: &[u8],
) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/cache"
        ))
        .bearer_auth(runner_token)
        .header("x-cache-key", key)
        .body(payload.to_vec())
        .send()
        .await
        .expect("cache upload")
}

/// Publish one archive to the generic registry the way a client does.
async fn publish_generic(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    name: &str,
    version: &str,
    payload: Vec<u8>,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/packages/generic/publish?name={name}&version={version}"
        ))
        .bearer_auth(token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}-{version}.bin\""),
        )
        .body(payload)
        .send()
        .await
        .unwrap()
}

/// Start an OCI upload session and return `(location, upload uuid)`.
async fn start_oci_upload(base: &str, token: &str, owner: &str, repo: &str) -> (String, String) {
    let start = reqwest::Client::new()
        .post(format!("{base}/v2/{owner}/{repo}/blobs/uploads/"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202, "start upload failed");
    let location = start
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("start upload must return a Location")
        .to_string();
    let uuid = location
        .rsplit('/')
        .next()
        .expect("location names the upload")
        .to_string();
    (location, uuid)
}

// ── Entry ceilings ───────────────────────────────────────────

#[tokio::test]
async fn ci_cache_entry_cap_refuses_a_new_key_but_still_updates_a_known_one() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.ci_cache_max_entries_per_repo = Some(2);
    }))
    .await;
    let (token, _) = register_full(&base, "cache-cap-owner", "cache-cap-owner@example.com").await;
    let repo_id = create_repo(&base, &token, "cache-cap").await;
    let (runner, runner_token) =
        rg_db::ops::runner_ops::register_runner(&db, repo_id, "cap-runner", "", None, None, None)
            .await
            .unwrap();

    let job_a = create_cached_job(&db, repo_id, runner.id, "a").await;
    let job_b = create_cached_job(&db, repo_id, runner.id, "b").await;
    let job_c = create_cached_job(&db, repo_id, runner.id, "c").await;

    assert_eq!(
        upload_cache(&base, runner.id, &runner_token, job_a, "a", b"cache a")
            .await
            .status(),
        204,
        "the first entry fits the cap"
    );
    assert_eq!(
        upload_cache(&base, runner.id, &runner_token, job_b, "b", b"cache b")
            .await
            .status(),
        204,
        "the second entry fills the cap"
    );

    let refused = upload_cache(&base, runner.id, &runner_token, job_c, "c", b"cache c").await;
    assert_eq!(refused.status(), 413, "a third key is beyond the cap");
    let body = refused.text().await.unwrap();
    assert!(
        body.contains("maximum of 2 CI cache entries"),
        "the refusal must name the cap: {body}"
    );

    // Updating a key the repository already owns writes no new entry, so the
    // cap must not lock the repository's own caches out.
    assert_eq!(
        upload_cache(&base, runner.id, &runner_token, job_a, "a", b"cache a v2")
            .await
            .status(),
        204,
        "an existing key may be overwritten at the cap"
    );

    let entries = rg_db::entities::ci_cache_entry::Entity::find()
        .filter(rg_db::entities::ci_cache_entry::Column::RepoId.eq(repo_id))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(entries, 2, "the refused key must not leave an entry");
    let updated = rg_db::entities::ci_cache_entry::Entity::find()
        .filter(rg_db::entities::ci_cache_entry::Column::RepoId.eq(repo_id))
        .filter(rg_db::entities::ci_cache_entry::Column::KeyHash.eq(cache_key_hash("a")))
        .one(&db)
        .await
        .unwrap()
        .expect("the known key's entry survives the overwrite");
    assert_eq!(
        updated.size, 10,
        "the overwrite replaced the archive: \"cache a v2\""
    );
}

#[tokio::test]
async fn release_asset_cap_refuses_the_asset_after_the_last_slot() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.release_assets_max_per_release = Some(2);
    }))
    .await;
    let (token, _) = register_full(&base, "asset-cap-owner", "asset-cap-owner@example.com").await;
    let repo_id = create_initialised_repo(&base, &token, "asset-cap").await;
    let release_id = create_release(&base, &token, "asset-cap-owner", "asset-cap", "v1.0.0").await;

    for name in ["one.bin", "two.bin"] {
        let stored = upload_release_asset(
            &base,
            &token,
            "asset-cap-owner",
            "asset-cap",
            release_id,
            name,
            b"asset",
        )
        .await;
        assert_eq!(stored.status(), 201, "{name} fills a slot");
    }
    let refused = upload_release_asset(
        &base,
        &token,
        "asset-cap-owner",
        "asset-cap",
        release_id,
        "three.bin",
        b"asset",
    )
    .await;
    assert_eq!(refused.status(), 413);
    let body = refused.text().await.unwrap();
    assert!(
        body.contains("maximum of 2 asset(s)"),
        "the refusal must name the cap: {body}"
    );

    let (count, bytes) = rg_db::ops::release_ops::repo_asset_usage(&db, repo_id)
        .await
        .unwrap();
    assert_eq!(
        (count, bytes),
        (2, 10),
        "the refused asset must not be stored"
    );
}

#[tokio::test]
async fn release_asset_over_the_repository_budget_is_refused_with_the_numbers() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(1000);
    }))
    .await;
    let (token, _) =
        register_full(&base, "asset-quota-owner", "asset-quota-owner@example.com").await;
    let repo_id = create_initialised_repo(&base, &token, "asset-quota").await;
    let release_id =
        create_release(&base, &token, "asset-quota-owner", "asset-quota", "v1.0.0").await;

    let stored = upload_release_asset(
        &base,
        &token,
        "asset-quota-owner",
        "asset-quota",
        release_id,
        "six-hundred.bin",
        &vec![b'a'; 600],
    )
    .await;
    assert_eq!(stored.status(), 201);

    let refused = upload_release_asset(
        &base,
        &token,
        "asset-quota-owner",
        "asset-quota",
        release_id,
        "five-hundred.bin",
        &vec![b'b'; 500],
    )
    .await;
    assert_eq!(refused.status(), 413);
    let body = refused.text().await.unwrap();
    assert!(
        body.contains("repository storage quota exceeded"),
        "the refusal must name the budget: {body}"
    );
    for number in ["600", "500", "1000"] {
        assert!(body.contains(number), "expected {number} in {body}");
    }

    let (count, bytes) = rg_db::ops::release_ops::repo_asset_usage(&db, repo_id)
        .await
        .unwrap();
    assert_eq!((count, bytes), (1, 600), "only the fitting asset remains");
}

// ── LFS ──────────────────────────────────────────────────────

#[tokio::test]
async fn lfs_batch_refuses_an_object_that_does_not_fit_with_507() {
    let (base, _db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(1000);
    }))
    .await;
    let (token, _) = register_full(&base, "lfs-quota-owner", "lfs-quota-owner@example.com").await;
    create_repo(&base, &token, "lfs-quota").await;

    let oversized = vec![b'x'; 2000];
    let batch = lfs_batch(
        &base,
        &token,
        "lfs-quota-owner",
        "lfs-quota",
        &oid_of(&oversized),
        oversized.len(),
    )
    .await;
    assert_eq!(batch.status(), 200, "the batch itself is not the error");
    let body: Value = batch.json().await.unwrap();
    let object = &body["objects"][0];
    assert_eq!(
        object["error"]["code"].as_u64(),
        Some(507),
        "LFS spells an unfitting object 507, not 413: {body}"
    );
    assert!(
        object["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("repository storage quota exceeded"),
        "the per-object error must carry the budget: {body}"
    );
    assert!(
        object["actions"].is_null(),
        "no upload URL may be handed out for an object that cannot fit: {body}"
    );

    // A fitting object still gets its action.
    let fitting = vec![b'y'; 100];
    let href = lfs_upload_href(&base, &token, "lfs-quota-owner", "lfs-quota", &fitting).await;
    assert!(
        href.starts_with("http"),
        "a fit object gets an upload URL: {href}"
    );
}

#[tokio::test]
async fn lfs_put_rechecks_the_budget_after_the_batch_handed_out_a_url() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(1024);
    }))
    .await;
    let (token, owner_id) =
        register_full(&base, "lfs-race-owner", "lfs-race-owner@example.com").await;
    let repo_id = create_repo(&base, &token, "lfs-race").await;

    let payload = vec![b'z'; 100];
    let href = lfs_upload_href(&base, &token, "lfs-race-owner", "lfs-race", &payload).await;

    // Another request consumes the room between the batch and the PUT.
    rg_db::ops::attachment_ops::create(
        &db,
        rg_db::entities::attachment::ActiveModel {
            id: sea_orm::NotSet,
            uuid: Set(uuid::Uuid::new_v4().to_string()),
            repo_id: Set(repo_id),
            uploader_id: Set(Some(owner_id)),
            issue_id: Set(None),
            pull_request_id: Set(None),
            issue_comment_id: Set(None),
            review_comment_id: Set(None),
            filename: Set("competing.txt".to_string()),
            blob_key: Set(format!("attachments/{repo_id}/competing.txt")),
            content_type: Set("text/plain".to_string()),
            size: Set(1000),
            download_count: Set(0),
            created_at: Set(chrono::Utc::now()),
            sha256: Set(None),
        },
    )
    .await
    .unwrap();

    let refused = reqwest::Client::new()
        .put(href)
        .body(payload)
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        413,
        "the PUT must re-check the budget the batch saw as fitting"
    );
    let body = refused.text().await.unwrap();
    assert!(
        body.contains("repository storage quota exceeded"),
        "the refusal must name the budget: {body}"
    );

    // The spooled bytes were discarded: the repository holds only the
    // competing attachment.
    let usage = rg_core::storage_quota::usage(&db, repo_id).await.unwrap();
    assert_eq!(usage.lfs_bytes, 0, "a refused PUT publishes no LFS object");
    assert_eq!(usage.attachment_bytes, 1000);
}

// ── OCI ──────────────────────────────────────────────────────

#[tokio::test]
async fn oci_upload_session_stops_at_the_cumulative_cap() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.oci_blob_max_bytes = Some(8);
    }))
    .await;
    let (token, _) = register_full(&base, "oci-cap-owner", "oci-cap-owner@example.com").await;
    create_repo(&base, &token, "oci-cap").await;
    let client = reqwest::Client::new();

    let (location, uuid) = start_oci_upload(&base, &token, "oci-cap-owner", "oci-cap").await;
    let first = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .header("content-range", "0-4")
        .body(b"aaaaa".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 202, "the first chunk fits the session");

    let refused = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .header("content-range", "5-9")
        .body(b"bbbbb".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        413,
        "the second chunk crosses the session cap"
    );
    let body = refused.text().await.unwrap();
    assert!(
        body.contains("cumulative limit"),
        "the refusal must name the ceiling: {body}"
    );

    let upload = rg_db::ops::oci_ops::find_upload(&db, &uuid)
        .await
        .unwrap()
        .expect("the session survives the refused chunk");
    assert_eq!(
        upload.bytes_uploaded, 5,
        "a refused append must leave the session at the acknowledged offset"
    );

    // A session within the cap still completes.
    let (location, _) = start_oci_upload(&base, &token, "oci-cap-owner", "oci-cap").await;
    let payload = b"four";
    let patched = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), 202);
    let finished = client
        .put(format!("{base}{location}"))
        .query(&[("digest", sha256(payload).as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_blob_push_created(finished, payload.len()).await;
}

#[tokio::test]
async fn oci_completion_rechecks_the_repository_budget() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(1024);
    }))
    .await;
    let (token, owner_id) =
        register_full(&base, "oci-race-owner", "oci-race-owner@example.com").await;
    let repo_id = create_repo(&base, &token, "oci-race").await;
    let client = reqwest::Client::new();

    let (location, uuid) = start_oci_upload(&base, &token, "oci-race-owner", "oci-race").await;
    let payload = vec![b'l'; 200];
    let patched = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), 202, "the session fits before the race");

    // Another writer consumes the room between the append and completion.
    rg_db::ops::attachment_ops::create(
        &db,
        rg_db::entities::attachment::ActiveModel {
            id: sea_orm::NotSet,
            uuid: Set(uuid::Uuid::new_v4().to_string()),
            repo_id: Set(repo_id),
            uploader_id: Set(Some(owner_id)),
            issue_id: Set(None),
            pull_request_id: Set(None),
            issue_comment_id: Set(None),
            review_comment_id: Set(None),
            filename: Set("competing.txt".to_string()),
            blob_key: Set(format!("attachments/{repo_id}/competing.txt")),
            content_type: Set("text/plain".to_string()),
            size: Set(1000),
            download_count: Set(0),
            created_at: Set(chrono::Utc::now()),
            sha256: Set(None),
        },
    )
    .await
    .unwrap();

    let digest = sha256(&payload);
    let refused = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        413,
        "completion must re-check the repository budget"
    );
    let body = refused.text().await.unwrap();
    assert!(
        body.contains("repository storage quota exceeded"),
        "the refusal must name the budget: {body}"
    );

    let upload = rg_db::ops::oci_ops::find_upload(&db, &uuid)
        .await
        .unwrap()
        .expect("the session is not finalized");
    assert!(
        rg_db::ops::oci_ops::find_blob(&db, upload.oci_repository_id, &digest)
            .await
            .unwrap()
            .is_none(),
        "an over-budget blob must not become reachable"
    );
}

// ── Packages ─────────────────────────────────────────────────

#[tokio::test]
async fn package_publish_over_the_repository_budget_is_refused() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(2048);
    }))
    .await;
    let (token, _) = register_full(&base, "pkg-quota-owner", "pkg-quota-owner@example.com").await;
    let repo_id = create_repo(&base, &token, "pkg-quota").await;

    let stored = publish_generic(
        &base,
        &token,
        "pkg-quota-owner",
        "pkg-quota",
        "widget",
        "1.0.0",
        vec![b'a'; 1500],
    )
    .await;
    assert_eq!(stored.status(), 201, "the first version fits the budget");

    let refused = publish_generic(
        &base,
        &token,
        "pkg-quota-owner",
        "pkg-quota",
        "widget",
        "2.0.0",
        vec![b'b'; 1000],
    )
    .await;
    assert_eq!(refused.status(), 413);
    let body = refused.text().await.unwrap();
    assert!(
        body.contains("repository storage quota exceeded"),
        "the refusal must name the budget: {body}"
    );
    assert!(body.contains("2048"), "the refusal names the limit: {body}");

    let (files, bytes) = rg_db::ops::package_file_ops::repo_usage(&db, repo_id)
        .await
        .unwrap();
    assert_eq!(
        (files, bytes),
        (1, 1500),
        "the refused version stored nothing"
    );
}

// ── CI artifacts ─────────────────────────────────────────────

/// Stage `payload` as the job's artifact archive and publish it under `name`,
/// returning the publish response.
async fn stage_and_publish_artifact(
    base: &str,
    runner_id: i64,
    runner_token: &str,
    job_id: i64,
    name: &str,
    payload: &[u8],
) -> reqwest::Response {
    let client = reqwest::Client::new();
    let staged = client
        .put(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/artifacts/staging"
        ))
        .bearer_auth(runner_token)
        .header("content-type", "application/x-tar")
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        staged.status(),
        201,
        "staging is not where the budget bites"
    );
    let staged: Value = staged.json().await.unwrap();
    client
        .post(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/artifacts"
        ))
        .bearer_auth(runner_token)
        .json(&serde_json::json!({
            "name": name,
            "file_path": staged["file_path"].as_str().expect("staged path"),
        }))
        .send()
        .await
        .unwrap()
}

/// CI artifacts were neither counted nor checked: a repository at its budget
/// could still take a gigabyte per job. Both halves now hold — the publish is
/// refused, and an artifact already stored counts against the next write in
/// another store.
#[tokio::test]
async fn ci_artifacts_are_checked_and_counted_against_the_budget() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(64);
    }))
    .await;
    let (token, _) = register_full(&base, "artifact-quota", "artifact-quota@example.com").await;
    let repo_id = create_initialised_repo(&base, &token, "artifact-quota").await;
    let (runner, runner_token) =
        rg_db::ops::runner_ops::register_runner(&db, repo_id, "quota-runner", "", None, None, None)
            .await
            .unwrap();

    let job = create_cached_job(&db, repo_id, runner.id, "artifacts").await;
    let refused =
        stage_and_publish_artifact(&base, runner.id, &runner_token, job, "big", &[7_u8; 65]).await;
    assert_eq!(
        refused.status(),
        413,
        "an artifact past the budget is refused"
    );
    let body = refused.text().await.unwrap();
    assert!(
        body.contains("repo_quota_mb"),
        "the refusal names the knob: {body}"
    );
    assert_eq!(
        rg_db::ops::artifact_ops::repo_usage(&db, repo_id)
            .await
            .unwrap(),
        (0, 0),
        "a refused artifact leaves no row"
    );

    let stored =
        stage_and_publish_artifact(&base, runner.id, &runner_token, job, "small", &[7_u8; 40])
            .await;
    assert_eq!(stored.status(), 201, "an artifact that fits is stored");

    // 40 bytes held by the artifact leave 24: a 30-byte release asset that
    // would have fitted an empty repository no longer does.
    let release_id = create_release(&base, &token, "artifact-quota", "artifact-quota", "v1").await;
    let asset = upload_release_asset(
        &base,
        &token,
        "artifact-quota",
        "artifact-quota",
        release_id,
        "asset.bin",
        &[1_u8; 30],
    )
    .await;
    assert_eq!(
        asset.status(),
        413,
        "the stored artifact must count against the shared budget"
    );
}

// ── Merges off the request path ──────────────────────────────

/// The merge queue and auto-merge run off the request, through the CI wiring
/// and the post-push context rather than the handler's state. Both have to
/// carry `[limits]`, or a merge they run adopts a fork's LFS objects past the
/// budget a merge through the route is held to.
#[tokio::test]
async fn the_wiring_merges_run_through_carries_the_budget() {
    let (db, dir) = crate::common::setup_test_db().await;
    let limits = StorageLimits {
        repo_quota_bytes: Some(4096),
        ..StorageLimits::default()
    };
    let state = crate::common::build_test_app_state_with(
        db,
        dir.path().join("repos"),
        overrides_with(|configured| *configured = limits),
    );

    assert_eq!(state.pipeline_ci().storage_limits, limits);
    let context = state.post_push_context();
    assert_eq!(context.storage_limits, limits);
    assert_eq!(context.pipeline_ci().storage_limits, limits);
}

// ── The default asks nothing ─────────────────────────────────

/// A stored LFS object of `size` bytes, written straight into the table: the
/// cheapest way to make a repository look large.
async fn seed_lfs_object(db: &rg_db::DatabaseConnection, repo_id: i64, size: i64) {
    rg_db::ops::lfs_object_ops::create(
        db,
        rg_db::entities::lfs_object::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            oid: Set(oid_of(uuid::Uuid::new_v4().as_bytes())),
            size: Set(size),
            uploaded: Set(true),
            created_at: Set(chrono::Utc::now()),
            publisher_token: Set(None),
            publisher_since: Set(None),
            last_claimed_at: Set(None),
        },
    )
    .await
    .expect("seed an LFS object");
}

/// A competing attachment row of `size` bytes that consumes the budget.
async fn seed_attachment(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    owner_id: i64,
    size: i64,
) -> i64 {
    rg_db::ops::attachment_ops::create(
        db,
        rg_db::entities::attachment::ActiveModel {
            id: sea_orm::NotSet,
            uuid: Set(uuid::Uuid::new_v4().to_string()),
            repo_id: Set(repo_id),
            uploader_id: Set(Some(owner_id)),
            issue_id: Set(None),
            pull_request_id: Set(None),
            issue_comment_id: Set(None),
            review_comment_id: Set(None),
            filename: Set("competing.txt".to_string()),
            blob_key: Set(format!("attachments/{repo_id}/{}", uuid::Uuid::new_v4())),
            content_type: Set("text/plain".to_string()),
            size: Set(size),
            download_count: Set(0),
            created_at: Set(chrono::Utc::now()),
            sha256: Set(None),
        },
    )
    .await
    .expect("seed an attachment")
    .id
}

/// The decision behind the opt-in default: an instance that sets no
/// `[limits]` refuses nothing, however much a repository already holds. The
/// same repository under a 20 GiB budget is refused, so the seeded bytes are
/// the ones the budget reads.
#[tokio::test]
async fn without_limits_a_repository_past_20_gib_still_takes_uploads() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "big-repo-owner", "big-repo-owner@example.com").await;
    let repo_id = create_initialised_repo(&base, &token, "big-repo").await;
    seed_lfs_object(&db, repo_id, 30 * 1024 * 1024 * 1024).await;

    let href = lfs_upload_href(&base, &token, "big-repo-owner", "big-repo", b"one more").await;
    assert!(href.starts_with("http"), "{href}");
    let stored = put_lfs_object(&base, &token, "big-repo-owner", "big-repo", b"and another").await;
    assert_eq!(stored.status(), 200, "an LFS push past 20 GiB is accepted");

    let release_id = create_release(&base, &token, "big-repo-owner", "big-repo", "v1").await;
    let asset = upload_release_asset(
        &base,
        &token,
        "big-repo-owner",
        "big-repo",
        release_id,
        "asset.bin",
        b"asset",
    )
    .await;
    assert_eq!(asset.status(), 201);
    let package = publish_generic(
        &base,
        &token,
        "big-repo-owner",
        "big-repo",
        "widget",
        "1.0.0",
        b"package".to_vec(),
    )
    .await;
    assert_eq!(package.status(), 201);

    let (budgeted, _db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(20 * 1024 * 1024 * 1024);
    }))
    .await;
    let (token, _) = register_full(&budgeted, "big-repo-owner", "big-repo-owner@example.com").await;
    let repo_id = create_initialised_repo(&budgeted, &token, "big-repo").await;
    seed_lfs_object(&_db, repo_id, 30 * 1024 * 1024 * 1024).await;
    let batch = lfs_batch(
        &budgeted,
        &token,
        "big-repo-owner",
        "big-repo",
        &oid_of(b"one more"),
        8,
    )
    .await;
    let batch: Value = batch.json().await.unwrap();
    assert_eq!(
        batch["objects"][0]["error"]["code"].as_u64(),
        Some(507),
        "{batch}"
    );
}

// ── Attachments ──────────────────────────────────────────────

#[tokio::test]
async fn an_attachment_over_the_repository_budget_is_refused_and_stores_nothing() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(1000);
    }))
    .await;
    let (token, owner_id) =
        register_full(&base, "att-quota-owner", "att-quota-owner@example.com").await;
    let repo = "att-quota";
    let repo_id = create_initialised_repo(&base, &token, repo).await;
    seed_attachment(&db, repo_id, owner_id, 900).await;
    let (_, issue_number) = create_issue(&base, &token, "att-quota-owner", repo, "budget").await;

    let refused = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/att-quota-owner/{repo}/issues/{issue_number}/assets"
        ))
        .bearer_auth(&token)
        .multipart(reqwest::multipart::Form::new().part(
            "attachment",
            reqwest::multipart::Part::bytes(vec![b'a'; 200]).file_name("over.txt"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 413);
    let body = refused.text().await.unwrap();
    assert!(body.contains("repository storage quota exceeded"), "{body}");
    assert_eq!(
        rg_db::ops::attachment_ops::repo_size(&db, repo_id)
            .await
            .unwrap(),
        900,
        "the refused attachment left a row"
    );
}

// ── Not charged twice ────────────────────────────────────────

/// Replacing a cache key adds only the difference: a refresh at the limit
/// is not refused for room the old archive already holds.
#[tokio::test]
async fn a_cache_refresh_is_charged_only_the_difference() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(100);
    }))
    .await;
    let (token, _) = register_full(&base, "cache-delta-owner", "cache-delta@example.com").await;
    let repo_id = create_repo(&base, &token, "cache-delta").await;
    let (runner, runner_token) =
        rg_db::ops::runner_ops::register_runner(&db, repo_id, "delta-runner", "", None, None, None)
            .await
            .unwrap();
    let job_a = create_cached_job(&db, repo_id, runner.id, "a").await;
    let job_b = create_cached_job(&db, repo_id, runner.id, "b").await;

    let upload = |job, key: &'static str, size: usize| {
        let base = base.clone();
        let runner_token = runner_token.clone();
        async move { upload_cache(&base, runner.id, &runner_token, job, key, &vec![b'c'; size]).await }
    };
    assert_eq!(upload(job_a, "a", 60).await.status(), 204);
    assert_eq!(
        upload(job_a, "a", 70).await.status(),
        204,
        "60 held + 70 incoming is over 100, but the refresh adds only 10"
    );
    let refused = upload(job_b, "b", 50).await;
    assert_eq!(refused.status(), 413, "70 held + 50 new is over 100");
    let (entries, bytes) = rg_db::ops::ci_retention_ops::repo_cache_usage(&db, repo_id)
        .await
        .unwrap();
    assert_eq!((entries, bytes), (1, 70));
}

/// An LFS object the repository already stores adds nothing when it is
/// PUT again — a retried push at the limit must not be refused.
#[tokio::test]
async fn a_repeated_lfs_put_of_a_stored_object_is_not_charged_again() {
    let (base, _db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(150);
    }))
    .await;
    let (token, _) = register_full(&base, "lfs-again-owner", "lfs-again@example.com").await;
    create_repo(&base, &token, "lfs-again").await;
    let payload = vec![b'r'; 100];
    let stored = put_lfs_object(&base, &token, "lfs-again-owner", "lfs-again", &payload).await;
    assert_eq!(stored.status(), 200);

    let again = reqwest::Client::new()
        .put(format!(
            "{base}/api/v1/repos/lfs-again-owner/lfs-again/lfs/objects/{}",
            oid_of(&payload)
        ))
        .bearer_auth(&token)
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        again.status(),
        200,
        "100 held + 100 again is over 150, but the object is already stored"
    );
}

// ── OCI: a refused chunk leaves the session resumable ────────

/// A chunk that crosses the repository budget is refused, and staging is cut
/// back to the acknowledged offset: once room is freed the same chunk lands,
/// which it could not if the file still held the refused bytes.
#[tokio::test]
async fn an_oci_chunk_over_the_budget_is_refused_and_the_session_resumes() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.repo_quota_bytes = Some(100);
    }))
    .await;
    let (token, owner_id) =
        register_full(&base, "oci-resume-owner", "oci-resume-owner@example.com").await;
    let repo_id = create_repo(&base, &token, "oci-resume").await;
    let competing = seed_attachment(&db, repo_id, owner_id, 50).await;
    let client = reqwest::Client::new();

    let (location, uuid) = start_oci_upload(&base, &token, "oci-resume-owner", "oci-resume").await;
    let patch = |range: &'static str, fill: u8| {
        client
            .patch(format!("{base}{location}"))
            .bearer_auth(&token)
            .header("content-range", range)
            .body(vec![fill; 30])
            .send()
    };
    assert_eq!(
        patch("0-29", b'a').await.unwrap().status(),
        202,
        "50 + 30 fits"
    );
    let refused = patch("30-59", b'b').await.unwrap();
    assert_eq!(refused.status(), 413, "50 + 60 does not");
    assert!(refused
        .text()
        .await
        .unwrap()
        .contains("repository storage quota exceeded"));
    assert_eq!(
        rg_db::ops::oci_ops::find_upload(&db, &uuid)
            .await
            .unwrap()
            .expect("the session survives")
            .bytes_uploaded,
        30
    );

    rg_db::ops::attachment_ops::delete_by_id(&db, competing)
        .await
        .unwrap();
    assert_eq!(
        patch("30-59", b'b').await.unwrap().status(),
        202,
        "staging still matches the session, so the chunk resumes"
    );
    let mut payload = vec![b'a'; 30];
    payload.extend(vec![b'b'; 30]);
    let finished = client
        .put(format!("{base}{location}"))
        .query(&[("digest", sha256(&payload).as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_blob_push_created(finished, payload.len()).await;
}
