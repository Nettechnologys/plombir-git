//! Per-repository storage budgets (security audit finding #10): the read-out
//! route and the write paths that must consult the shared budget.
//!
//! The arithmetic itself is unit-tested in `rg_core::storage_quota`; these
//! tests drive the real routes, because the failure the audit found was a
//! request that never asked. Each case builds a state whose ceiling the request
//! can reach (`StateOverrides::storage_limits`) and asserts both the refusal
//! and that nothing was left behind — a 413 that still stored the bytes would
//! pass a status-only test.
//!
//! The one case a fixed ceiling cannot express is the post-commit recheck: the
//! request passes the pre-check and another writer consumes the room while the
//! bytes are on disk. `SeedUsageOnAttachmentWrite` turns that window into a
//! deterministic fixture by consuming the budget in the storage layer itself.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use serde_json::Value;

use rg_core::blob_storage::{BlobKey, BlobMetadata, BlobStorage};
use rg_core::storage_quota::StorageLimits;

use crate::common::{
    assert_blob_push_created, create_initialised_repo, create_issue, create_repo, register_full,
    setup_test_db, spawn_test_app_over_db_with, spawn_test_app_with_db,
    spawn_test_app_with_overrides, test_storage_limits, StateOverrides,
};

/// Test state whose `[limits]` differ from the harness defaults only where the
/// test says so.
fn overrides_with(mutate: impl FnOnce(&mut StorageLimits)) -> StateOverrides {
    let mut limits = test_storage_limits();
    mutate(&mut limits);
    StateOverrides {
        storage_limits: Some(limits),
        ..StateOverrides::default()
    }
}

/// A repository the storage route must ask a token for.
async fn create_private_repo(base: &str, token: &str, name: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create private repo failed");
    resp.json::<Value>().await.unwrap()["id"].as_i64().unwrap()
}

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

// ── The read-out route ───────────────────────────────────────

/// The read-out counts what the upload routes stored, store by store, and
/// reports the ceilings the server actually resolved.
#[tokio::test]
async fn storage_read_out_names_every_store_and_sums_them() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "readout-owner", "readout-owner@example.com").await;
    let repo = "storage-readout";
    create_initialised_repo(&base, &token, repo).await;
    let client = reqwest::Client::new();

    let attachment = b"storage read-out attachment";
    let (_, issue_number) = create_issue(&base, &token, "readout-owner", repo, "readout").await;
    let uploaded = client
        .post(format!(
            "{base}/api/v1/repos/readout-owner/{repo}/issues/{issue_number}/assets"
        ))
        .bearer_auth(&token)
        .multipart(reqwest::multipart::Form::new().part(
            "attachment",
            reqwest::multipart::Part::bytes(attachment.to_vec()).file_name("readout.txt"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), 201, "issue attachment upload");

    let lfs = b"lfs-bytes";
    let stored = put_lfs_object(&base, &token, "readout-owner", repo, lfs).await;
    assert_eq!(stored.status(), 200, "LFS upload");

    let release_id = create_release(&base, &token, "readout-owner", repo, "v1.0.0").await;
    let asset = b"release-asset-payload";
    let stored = upload_release_asset(
        &base,
        &token,
        "readout-owner",
        repo,
        release_id,
        "payload.bin",
        asset,
    )
    .await;
    assert_eq!(stored.status(), 201, "release asset upload");

    let readout = client
        .get(format!("{base}/api/v1/repos/readout-owner/{repo}/storage"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(readout.status(), 200);
    let body: Value = readout.json().await.unwrap();
    let usage = &body["usage"];

    for (store, expected) in [
        ("lfs", (lfs.len() as u64, 1_u64)),
        ("release", (asset.len() as u64, 1)),
        ("attachment", (attachment.len() as u64, 0)),
        ("ci_cache", (0, 0)),
        ("package", (0, 0)),
        ("oci", (0, 0)),
    ] {
        let bytes = usage[format!("{store}_bytes")]
            .as_u64()
            .unwrap_or_else(|| panic!("{store}_bytes missing: {body}"));
        assert_eq!(bytes, expected.0, "{store}_bytes");
    }
    assert_eq!(usage["lfs_objects"].as_u64(), Some(1));
    assert_eq!(usage["release_assets"].as_u64(), Some(1));
    assert_eq!(usage["ci_cache_entries"].as_u64(), Some(0));
    assert_eq!(
        usage["total_bytes"].as_u64(),
        Some((lfs.len() + attachment.len() + asset.len()) as u64),
        "the total must sum every store: {body}"
    );

    let limits = &body["limits"];
    let expected = test_storage_limits();
    assert_eq!(
        limits["repo_quota_bytes"].as_u64(),
        Some(expected.repo_quota_bytes)
    );
    assert_eq!(
        limits["oci_blob_max_bytes"].as_u64(),
        Some(expected.oci_blob_max_bytes)
    );
    assert_eq!(
        limits["ci_cache_max_entries_per_repo"].as_u64(),
        Some(expected.ci_cache_max_entries_per_repo)
    );
    assert_eq!(
        limits["release_assets_max_per_release"].as_u64(),
        Some(expected.release_assets_max_per_release)
    );

    // A private repository's accounting is not anonymously readable.
    create_private_repo(&base, &token, "readout-private").await;
    let anonymous = client
        .get(format!(
            "{base}/api/v1/repos/readout-owner/readout-private/storage"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
}

// ── Entry ceilings ───────────────────────────────────────────

#[tokio::test]
async fn ci_cache_entry_cap_refuses_a_new_key_but_still_updates_a_known_one() {
    let (base, db) = spawn_test_app_with_overrides(overrides_with(|limits| {
        limits.ci_cache_max_entries_per_repo = 2;
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
        limits.release_assets_max_per_release = 2;
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
        limits.repo_quota_bytes = 1000;
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
        limits.repo_quota_bytes = 1000;
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
        limits.repo_quota_bytes = 1024;
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
        limits.oci_blob_max_bytes = 8;
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
        limits.repo_quota_bytes = 1024;
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
        limits.repo_quota_bytes = 2048;
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

// ── The post-commit recheck ──────────────────────────────────

/// A [`BlobStorage`] that inserts a near-quota attachment row the first time an
/// attachment blob is written.
///
/// This is the deterministic form of the race the post-commit recheck exists
/// for: the request passed the pre-check, another writer consumed the budget
/// while the bytes were on disk, and the recheck must roll this request back.
/// Hooking the storage write is the only point between the two checks a test
/// controls, and it needs no seam in production code.
struct SeedUsageOnAttachmentWrite {
    inner: Arc<dyn BlobStorage>,
    db: rg_db::DatabaseConnection,
    uploader_id: AtomicI64,
    seeded: AtomicBool,
    rolled_back: AtomicBool,
}

impl SeedUsageOnAttachmentWrite {
    fn wrap(inner: Arc<dyn BlobStorage>, db: rg_db::DatabaseConnection) -> Arc<Self> {
        Arc::new(Self {
            inner,
            db,
            uploader_id: AtomicI64::new(0),
            seeded: AtomicBool::new(false),
            rolled_back: AtomicBool::new(false),
        })
    }

    async fn seed_if_attachment(&self, key: &BlobKey) {
        let Some(repo_id) = key
            .as_str()
            .strip_prefix("attachments/")
            .and_then(|rest| rest.split('/').next())
            .and_then(|segment| segment.parse::<i64>().ok())
        else {
            return;
        };
        if self.seeded.swap(true, Ordering::SeqCst) {
            return;
        }
        rg_db::ops::attachment_ops::create(
            &self.db,
            rg_db::entities::attachment::ActiveModel {
                id: sea_orm::NotSet,
                uuid: Set(uuid::Uuid::new_v4().to_string()),
                repo_id: Set(repo_id),
                uploader_id: Set(Some(self.uploader_id.load(Ordering::SeqCst))),
                issue_id: Set(None),
                pull_request_id: Set(None),
                issue_comment_id: Set(None),
                review_comment_id: Set(None),
                filename: Set("concurrent-writer.txt".to_string()),
                blob_key: Set(format!("attachments/{repo_id}/concurrent-writer.txt")),
                content_type: Set("text/plain".to_string()),
                size: Set(test_storage_limits().repo_quota_bytes as i64 - 1),
                download_count: Set(0),
                created_at: Set(chrono::Utc::now()),
                sha256: Set(None),
            },
        )
        .await
        .expect("insert the competing attachment that consumes the budget");
    }
}

impl BlobStorage for SeedUsageOnAttachmentWrite {
    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }

    fn put<'a>(
        &'a self,
        key: &'a BlobKey,
        data: &'a [u8],
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<BlobMetadata>> {
        Box::pin(async move {
            let written = self.inner.put(key, data).await?;
            self.seed_if_attachment(key).await;
            Ok(written)
        })
    }

    fn put_file<'a>(
        &'a self,
        key: &'a BlobKey,
        source: &'a std::path::Path,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<BlobMetadata>> {
        Box::pin(async move {
            let written = self.inner.put_file(key, source).await?;
            self.seed_if_attachment(key).await;
            Ok(written)
        })
    }

    fn get<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<Vec<u8>>> {
        self.inner.get(key)
    }

    fn metadata<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<BlobMetadata>> {
        self.inner.metadata(key)
    }

    fn exists<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<bool>> {
        self.inner.exists(key)
    }

    fn delete<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<bool>> {
        Box::pin(async move {
            let removed = self.inner.delete(key).await?;
            if key.as_str().starts_with("attachments/") {
                self.rolled_back.store(true, Ordering::SeqCst);
            }
            Ok(removed)
        })
    }

    fn list<'a>(
        &'a self,
        prefix: Option<&'a BlobKey>,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<Vec<BlobMetadata>>> {
        self.inner.list(prefix)
    }

    fn local_path(&self, key: &BlobKey) -> Option<std::path::PathBuf> {
        self.inner.local_path(key)
    }
}

#[tokio::test]
async fn an_attachment_that_loses_the_budget_race_is_rolled_back() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let storage = SeedUsageOnAttachmentWrite::wrap(
        Arc::new(rg_core::blob_storage::LocalBlobStorage::new(&repo_root)),
        db.clone(),
    );
    let base = spawn_test_app_over_db_with(
        db.clone(),
        repo_root,
        StateOverrides {
            blob_storage: Some(storage.clone() as Arc<dyn BlobStorage>),
            ..StateOverrides::default()
        },
    )
    .await;

    let (token, user_id) =
        register_full(&base, "rollback-owner", "rollback-owner@example.com").await;
    storage.uploader_id.store(user_id, Ordering::SeqCst);
    let repo_id = create_repo(&base, &token, "rollback").await;
    let (_, issue_number) = create_issue(&base, &token, "rollback-owner", "rollback", "race").await;

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/rollback-owner/rollback/issues/{issue_number}/assets"
        ))
        .bearer_auth(&token)
        .multipart(reqwest::multipart::Form::new().part(
            "attachment",
            reqwest::multipart::Part::bytes(b"xx".to_vec()).file_name("over-budget.txt"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        413,
        "the recheck must refuse an upload the pre-check admitted"
    );
    let body = response.text().await.unwrap();
    assert!(
        body.contains("repository storage quota exceeded"),
        "the refusal must name the budget: {body}"
    );

    let rows = rg_db::entities::attachment::Entity::find()
        .filter(rg_db::entities::attachment::Column::RepoId.eq(repo_id))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(rows, 1, "the refused upload must not leave its row behind");
    assert_eq!(
        rg_core::storage_quota::usage(&db, repo_id)
            .await
            .unwrap()
            .attachment_bytes,
        test_storage_limits().repo_quota_bytes - 1,
        "only the competing row remains"
    );
    assert!(
        storage.rolled_back.load(Ordering::SeqCst),
        "the blob written before the recheck must be deleted"
    );
}
