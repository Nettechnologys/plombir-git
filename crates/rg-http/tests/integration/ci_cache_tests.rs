use crate::common::fault::{
    fail_db_writes, spawn_test_app_for_fault_sweep, DbWrite, FaultSweepApp,
};
use crate::common::{register_full, spawn_test_app_with_db};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, Set,
};
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

#[tokio::test]
async fn repository_cascade_during_retention_policy_update_is_typed_not_found() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) =
        register_full(&base, "policy-race-owner", "policy-race-owner@example.com").await;
    let repo_id = create_private_repo(&base, &token, "policy-race").await;
    let endpoint = format!("{base}/api/v1/repos/policy-race-owner/policy-race/actions/retention");
    let client = reqwest::Client::new();

    let created = client
        .put(&endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "artifact_retention_days": 30,
            "cache_retention_days": 7
        }))
        .send()
        .await
        .expect("create the policy the raced request observes");
    assert_eq!(created.status(), 200);

    db.execute(sea_orm::Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_policy_repo_before_http_update \
             BEFORE UPDATE ON ci_retention_policies WHEN OLD.repo_id = {repo_id} \
             BEGIN DELETE FROM repositories WHERE id = OLD.repo_id; END"
        ),
    ))
    .await
    .expect("install the competing repository delete");

    let raced = client
        .put(&endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "artifact_retention_days": 90,
            "cache_retention_days": 14
        }))
        .send()
        .await
        .expect("race policy update against repository deletion");
    assert_eq!(
        raced.status(),
        404,
        "repository deletion after the policy read must stay typed absence"
    );
    let body = raced.text().await.expect("read the typed error body");
    assert!(body.contains("repository not found"), "{body}");
    assert!(!body.contains("RecordNotUpdated"), "{body}");
    assert!(rg_db::ops::repo_ops::find_by_id(&db, repo_id)
        .await
        .expect("look for the deleted repository")
        .is_none());
    assert!(
        rg_db::entities::ci_retention_policy::Entity::find_by_id(repo_id)
            .one(&db)
            .await
            .expect("look for a resurrected policy")
            .is_none(),
        "the losing request recreated a policy after repository deletion"
    );
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

fn key_hash(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

#[tokio::test]
async fn runner_gate_precedes_large_cache_body_extraction() {
    let (base, _db) = spawn_test_app_with_db().await;
    let response = reqwest::Client::new()
        .put(format!("{base}/api/v1/runners/1/jobs/1/cache"))
        .header("x-cache-key", "unauthenticated")
        .body(vec![b'x'; 2 * 1024 * 1024 + 1])
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the runner credential gate must answer before a large body is read at all"
    );
}

#[tokio::test]
async fn ci_cache_round_trip_records_and_verifies_content_digest() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "cache_owner", "cache_owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "private-cache").await;
    let (runner, runner_token) =
        rg_db::ops::runner_ops::register_runner(&db, repo_id, "cache-runner", "", None, None, None)
            .await
            .unwrap();
    let cache_key = "deps-v1";
    let job_id = create_cached_job(&db, repo_id, runner.id, cache_key).await;

    // Cross Axum's hidden 2 MiB buffered-extractor default. The route declares
    // 1 GiB, so this must reach the handler and survive the full round trip.
    let archive = vec![b'c'; 2 * 1024 * 1024 + 1];
    let expected_sha = hex::encode(Sha256::digest(&archive));
    assert_eq!(expected_sha.len(), 64);

    // Upload the cache archive (PUT with the cache key header).
    let upload = client
        .put(format!(
            "{}/api/v1/runners/{}/jobs/{}/cache",
            base, runner.id, job_id
        ))
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .body(archive.clone())
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
    assert_eq!(download.bytes().await.unwrap().as_ref(), archive.as_slice());

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
        rg_db::ops::runner_ops::register_runner(db, repo_id, "cache-runner", "", None, None, None)
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

/// An empty archive is refused — and the spool that received it goes with the
/// refusal.
///
/// The emptiness used to be decided on a `Bytes` the extractor had already
/// collected, so nothing had touched the disk when the 400 was written. The
/// streaming handler learns the length only after it has received the body,
/// which means an empty upload now creates a file first; if the refusal did not
/// take it along, every runner publishing an empty cache would leave one behind
/// where retention — which walks rows — will never look.
#[tokio::test]
async fn an_empty_cache_upload_is_refused_and_leaves_no_spool() {
    let app = spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let cache_key = "deps-v1";
    let (repo_id, runner_id, job_id, runner_token) = cache_upload_fixture(
        &app.base,
        &app.db,
        "cache_empty_upload",
        "empty-upload",
        cache_key,
    )
    .await;

    let upload = client
        .put(format!(
            "{}/api/v1/runners/{}/jobs/{}/cache",
            app.base, runner_id, job_id
        ))
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .body(Vec::new())
        .send()
        .await
        .unwrap();
    assert_eq!(
        upload.status(),
        400,
        "an empty cache archive is the runner's"
    );

    assert!(
        rg_db::ops::ci_retention_ops::find_cache_entry(&app.db, repo_id, &key_hash(cache_key))
            .await
            .unwrap()
            .is_none(),
        "a refused upload must not publish a cache entry"
    );
    let leftovers = leftover_cache_files(&cache_dir(&app.repo_root, repo_id));
    assert!(
        leftovers.is_empty(),
        "the refused upload left files behind that no row points at: {leftovers:?}"
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

/// Retention can remove the conflicting row from inside the upsert's UPDATE.
/// That is ordinary cache eviction, not a backend failure: the request-private
/// archive is already durable and must become the one publication left behind.
#[tokio::test]
async fn upload_converges_when_retention_evicts_the_existing_cache_row() {
    let app = spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let cache_key = "deps-evicted-during-upsert";
    let (repo_id, runner_id, job_id, runner_token) = cache_upload_fixture(
        &app.base,
        &app.db,
        "cache_eviction_race",
        "eviction-race",
        cache_key,
    )
    .await;
    let url = format!(
        "{}/api/v1/runners/{runner_id}/jobs/{job_id}/cache",
        app.base
    );
    let first = b"the expired publication".to_vec();
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(&runner_token)
            .header("x-cache-key", cache_key)
            .body(first)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    expire_cache(&app.db, repo_id, cache_key).await;
    let old = cache_entry(&app.db, repo_id, cache_key)
        .await
        .expect("the expired publication exists");
    let old_path = std::path::PathBuf::from(&old.file_path);

    app.db
        .execute_unprepared(&format!(
            "CREATE TRIGGER evict_cache_inside_upload BEFORE UPDATE ON ci_cache_entries \
             WHEN OLD.id = {} \
             BEGIN DELETE FROM ci_cache_entries WHERE id = OLD.id; END;",
            old.id
        ))
        .await
        .expect("install the deterministic eviction");

    let fresh_bytes = b"the fresh publication".to_vec();
    let retry = client
        .put(&url)
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .body(fresh_bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        retry.status(),
        204,
        "retention eviction surfaced as an upload failure"
    );

    let fresh = cache_entry(&app.db, repo_id, cache_key)
        .await
        .expect("the fresh publication owns a row");
    assert_ne!(fresh.file_path, old.file_path);
    assert_eq!(fresh.size, fresh_bytes.len() as i64);
    assert_eq!(
        fresh.sha256.as_deref(),
        Some(hex::encode(Sha256::digest(&fresh_bytes)).as_str())
    );
    assert!(fresh.expires_at > chrono::Utc::now());
    assert!(
        !old_path.exists(),
        "the superseded archive survived successful convergence"
    );
    assert!(
        std::path::Path::new(&fresh.file_path).exists(),
        "the row does not point at the fresh request-private archive"
    );
    assert_eq!(
        rg_db::entities::ci_cache_entry::Entity::find()
            .filter(rg_db::entities::ci_cache_entry::Column::RepoId.eq(repo_id))
            .filter(rg_db::entities::ci_cache_entry::Column::KeyHash.eq(key_hash(cache_key)))
            .count(&app.db)
            .await
            .unwrap(),
        1,
        "eviction convergence must leave exactly one cache row"
    );
    assert_eq!(
        leftover_cache_files(&cache_dir(&app.repo_root, repo_id)).len(),
        1,
        "eviction convergence leaked a request-private archive"
    );
}

/// Repository deletion is authoritative, unlike cache eviction. If the parent
/// disappears inside the conflict UPDATE, the retry must stay a foreign-key
/// failure and upload compensation must remove only this request's new archive.
#[tokio::test]
async fn repository_cascade_during_cache_upsert_does_not_resurrect_or_leak_the_retry() {
    let app = spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let cache_key = "deps-repo-cascade";
    let (repo_id, runner_id, job_id, runner_token) = cache_upload_fixture(
        &app.base,
        &app.db,
        "cache_repo_cascade",
        "repo-cascade",
        cache_key,
    )
    .await;
    let url = format!(
        "{}/api/v1/runners/{runner_id}/jobs/{job_id}/cache",
        app.base
    );
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(&runner_token)
            .header("x-cache-key", cache_key)
            .body(b"the publication before repository deletion".to_vec())
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let old = cache_entry(&app.db, repo_id, cache_key)
        .await
        .expect("the old publication exists");
    let old_name = std::path::Path::new(&old.file_path)
        .file_name()
        .expect("cache archive has a file name")
        .to_string_lossy()
        .into_owned();

    app.db
        .execute_unprepared(&format!(
            "CREATE TRIGGER delete_repo_inside_cache_upsert BEFORE UPDATE ON ci_cache_entries \
             WHEN OLD.id = {} \
             BEGIN DELETE FROM repositories WHERE id = OLD.repo_id; END;",
            old.id
        ))
        .await
        .expect("install the deterministic repository cascade");

    let retry = client
        .put(&url)
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .body(b"must never become an ownerless publication".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        retry.status(),
        500,
        "a deleted repository was silently recreated by cache upsert"
    );
    assert!(
        cache_entry(&app.db, repo_id, cache_key).await.is_none(),
        "repository cascade left or recreated a cache row"
    );
    assert_eq!(
        leftover_cache_files(&cache_dir(&app.repo_root, repo_id)),
        vec![old_name],
        "the failed retry leaked its request-private archive"
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

/// Publish one cache archive through the real upload route.
///
/// Hands back the owner's token (the retention route is repo-admin only), the
/// repository the archive belongs to, and the exact path the row now names.
async fn published_cache(
    app: &FaultSweepApp,
    login: &str,
    repo_name: &str,
    cache_key: &str,
) -> (String, i64, std::path::PathBuf) {
    let (owner_token, _owner_id) =
        register_full(&app.base, login, &format!("{login}@example.com")).await;
    let repo_id = create_private_repo(&app.base, &owner_token, repo_name).await;
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &app.db,
        repo_id,
        &format!("cache-runner-{cache_key}"),
        "",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let job_id = create_cached_job(&app.db, repo_id, runner.id, cache_key).await;

    let upload = reqwest::Client::new()
        .put(format!(
            "{}/api/v1/runners/{}/jobs/{}/cache",
            app.base, runner.id, job_id
        ))
        .bearer_auth(&runner_token)
        .header("x-cache-key", cache_key)
        .body(format!("archive of {cache_key}").into_bytes())
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), 204, "cache upload failed");

    let entry = cache_entry(&app.db, repo_id, cache_key)
        .await
        .expect("uploaded cache entry");
    let archive = std::path::PathBuf::from(&entry.file_path);
    assert!(archive.exists(), "upload did not write the cache archive");
    (owner_token, repo_id, archive)
}

async fn cache_entry(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    cache_key: &str,
) -> Option<rg_db::entities::ci_cache_entry::Model> {
    rg_db::ops::ci_retention_ops::find_cache_entry(db, repo_id, &key_hash(cache_key))
        .await
        .unwrap()
}

/// Age a cache entry out of its retention window without waiting for one.
async fn expire_cache(db: &rg_db::DatabaseConnection, repo_id: i64, cache_key: &str) -> i64 {
    let entry = cache_entry(db, repo_id, cache_key)
        .await
        .expect("cache entry to expire");
    let id = entry.id;
    let mut active: rg_db::entities::ci_cache_entry::ActiveModel = entry.into();
    active.expires_at = Set(chrono::Utc::now() - chrono::Duration::days(1));
    active.update(db).await.expect("expire cache entry");
    id
}

async fn cleanup_expired(
    app: &FaultSweepApp,
    token: &str,
    owner: &str,
    repo: &str,
) -> serde_json::Value {
    let response = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/{owner}/{repo}/actions/retention/expired",
            app.base
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("run retention cleanup");
    assert_eq!(
        response.status(),
        200,
        "one uncleanable cache entry brought the whole sweep down"
    );
    response.json().await.unwrap()
}

/// A reader can refresh the exact publication after retention listed it but
/// before the conditional delete. The staged archive still belongs to the live
/// row in that case and must be restored, not retired as expired bytes.
#[tokio::test]
async fn expired_cache_cleanup_restores_an_archive_refreshed_during_eviction() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "cache_retention_refresh";
    let repo = "retention-refresh";
    let cache_key = "deps-refreshed";
    let (token, repo_id, archive) = published_cache(&app, owner, repo, cache_key).await;
    let cache_id = expire_cache(&app.db, repo_id, cache_key).await;
    let refreshed_until = (chrono::Utc::now() + chrono::Duration::days(7)).to_rfc3339();

    app.db
        .execute_unprepared(&format!(
            "CREATE TRIGGER refresh_cache_during_cleanup BEFORE DELETE ON ci_cache_entries \
             WHEN OLD.id = {cache_id} \
             BEGIN \
                 UPDATE ci_cache_entries SET expires_at = '{refreshed_until}' WHERE id = OLD.id; \
                 SELECT RAISE(IGNORE); \
             END;"
        ))
        .await
        .expect("arm the deterministic cache refresh");

    let summary = cleanup_expired(&app, &token, owner, repo).await;
    assert_eq!(summary["caches_deleted"], 0);
    assert_eq!(summary["failures"], 0);
    assert!(archive.exists(), "cleanup retired the refreshed archive");
    let current = cache_entry(&app.db, repo_id, cache_key)
        .await
        .expect("the refreshed cache entry survives");
    assert_eq!(current.id, cache_id);
    assert_eq!(current.file_path, archive.to_string_lossy());
    assert!(current.expires_at > chrono::Utc::now());
    assert_eq!(
        leftover_cache_files(&cache_dir(&app.repo_root, repo_id)),
        vec![archive
            .file_name()
            .expect("live cache archive name")
            .to_string_lossy()
            .into_owned()],
        "restoring the live archive left a retention tombstone or lost the publication"
    );
}

/// A newer upload can replace the publication while the old archive is staged.
/// Cleanup may retire the superseded bytes, but must never put them back over
/// the archive path now owned by the successful replacement.
#[tokio::test]
async fn expired_cache_cleanup_does_not_restore_over_a_new_publication() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "cache_retention_replace";
    let repo = "retention-replace";
    let cache_key = "deps-replaced";
    let (token, repo_id, old_archive) = published_cache(&app, owner, repo, cache_key).await;
    let cache_id = expire_cache(&app.db, repo_id, cache_key).await;

    let replacement_archive =
        old_archive.with_file_name(format!("replacement-{}.tar", uuid::Uuid::new_v4().simple()));
    let replacement_bytes = b"new publication wins";
    std::fs::write(&replacement_archive, replacement_bytes)
        .expect("write the replacement cache archive");
    let replacement_path = replacement_archive.to_string_lossy().replace('\'', "''");
    let replacement_sha = hex::encode(Sha256::digest(replacement_bytes));
    let replacement_until = (chrono::Utc::now() + chrono::Duration::days(7)).to_rfc3339();

    app.db
        .execute_unprepared(&format!(
            "CREATE TRIGGER replace_cache_during_cleanup BEFORE DELETE ON ci_cache_entries \
             WHEN OLD.id = {cache_id} \
             BEGIN \
                 UPDATE ci_cache_entries \
                    SET file_path = '{replacement_path}', \
                        size = {}, \
                        sha256 = '{replacement_sha}', \
                        expires_at = '{replacement_until}' \
                  WHERE id = OLD.id; \
                 SELECT RAISE(IGNORE); \
             END;",
            replacement_bytes.len()
        ))
        .await
        .expect("arm the deterministic cache replacement");

    let summary = cleanup_expired(&app, &token, owner, repo).await;
    assert_eq!(summary["caches_deleted"], 0);
    assert_eq!(summary["failures"], 0);
    assert!(
        !old_archive.exists(),
        "cleanup restored the superseded archive"
    );
    assert_eq!(
        std::fs::read(&replacement_archive).expect("read the replacement cache archive"),
        replacement_bytes,
        "cleanup changed the replacement archive"
    );
    let current = cache_entry(&app.db, repo_id, cache_key)
        .await
        .expect("the replacement cache entry survives");
    assert_eq!(current.id, cache_id);
    assert_eq!(current.file_path, replacement_archive.to_string_lossy());
    assert_eq!(current.size, replacement_bytes.len() as i64);
    assert_eq!(current.sha256.as_deref(), Some(replacement_sha.as_str()));
    assert_eq!(
        leftover_cache_files(&cache_dir(&app.repo_root, repo_id)),
        vec![replacement_archive
            .file_name()
            .expect("replacement cache archive name")
            .to_string_lossy()
            .into_owned()],
        "retiring the superseded archive left a tombstone or disturbed the replacement"
    );
}

/// card_789fa4252b8c: the retention sweep unlinked the archive and only then
/// deleted the row it belonged to, which is not compensable — a metadata
/// failure left a live entry naming bytes the sweep had already destroyed, and
/// every `download_cache` on that key failed until the next pass, an hour
/// later, came back for the row.
#[tokio::test]
async fn expired_cache_cleanup_restores_the_archive_when_the_entry_delete_fails() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "cache_retention_fault";
    let repo = "retention-fault";
    let cache_key = "deps-expired";
    let (token, repo_id, archive) = published_cache(&app, owner, repo, cache_key).await;
    expire_cache(&app.db, repo_id, cache_key).await;

    let db_fault = fail_db_writes(&app.db, "ci_cache_entries", DbWrite::Delete).await;
    let summary = cleanup_expired(&app, &token, owner, repo).await;
    assert_eq!(
        summary["caches_deleted"], 0,
        "a failed entry delete was counted as a cleaned cache"
    );
    assert_eq!(summary["failures"], 1, "the failure was not reported");
    db_fault.clear().await;

    assert!(
        archive.exists(),
        "the staged cache archive was not put back"
    );
    assert!(
        cache_entry(&app.db, repo_id, cache_key).await.is_some(),
        "the cache entry did not survive its failed cleanup"
    );

    // The same sweep, once the database is healthy again, is what actually
    // frees the space — and only then is the cache counted.
    let summary = cleanup_expired(&app, &token, owner, repo).await;
    assert_eq!(summary["caches_deleted"], 1, "retry did not clean up");
    assert_eq!(summary["failures"], 0);
    assert!(!archive.exists(), "cleanup left the cache archive");
    assert!(
        cache_entry(&app.db, repo_id, cache_key).await.is_none(),
        "cleanup left the cache entry"
    );
    assert!(
        leftover_cache_files(&cache_dir(&app.repo_root, repo_id)).is_empty(),
        "cleanup counted a cache whose staged archive was still parked: {:?}",
        leftover_cache_files(&cache_dir(&app.repo_root, repo_id))
    );
}

/// One entry that cannot be cleaned is a `failures` line, not a reason to
/// abandon every entry behind it: the sweep used to propagate the failed delete
/// with `?`, which dropped the remaining expired caches *and* the counts the
/// artifact half had already earned.
#[tokio::test]
async fn one_uncleanable_cache_entry_does_not_abandon_the_rest_of_the_sweep() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "cache_retention_partial";
    let repo = "retention-partial";
    let (token, repo_id, stuck_archive) = published_cache(&app, owner, repo, "deps-stuck").await;
    let stuck_id = expire_cache(&app.db, repo_id, "deps-stuck").await;

    // A second archive of the same repository, published through the same
    // route, so the sweep has something behind the failing entry to reach.
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &app.db,
        repo_id,
        "cache-runner-follower",
        "",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let job_id = create_cached_job(&app.db, repo_id, runner.id, "deps-follower").await;
    let upload = reqwest::Client::new()
        .put(format!(
            "{}/api/v1/runners/{}/jobs/{}/cache",
            app.base, runner.id, job_id
        ))
        .bearer_auth(&runner_token)
        .header("x-cache-key", "deps-follower")
        .body(b"archive of deps-follower".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), 204);
    let follower_archive = std::path::PathBuf::from(
        &cache_entry(&app.db, repo_id, "deps-follower")
            .await
            .expect("follower cache entry")
            .file_path,
    );
    expire_cache(&app.db, repo_id, "deps-follower").await;

    // Only the first entry's delete fails, so the sweep has to survive it and
    // still reach the second.
    app.db
        .execute_unprepared(&format!(
            "CREATE TRIGGER fk_fault_one_cache_delete BEFORE DELETE ON ci_cache_entries \
             WHEN OLD.id = {stuck_id} \
             BEGIN SELECT RAISE(ABORT, 'injected failure: DELETE on ci_cache_entries'); END;"
        ))
        .await
        .expect("arm the single-entry delete fault");

    let summary = cleanup_expired(&app, &token, owner, repo).await;
    assert_eq!(summary["failures"], 1, "the failure was not reported");
    assert_eq!(
        summary["caches_deleted"], 1,
        "the entry behind the failing one was never reached"
    );
    assert!(
        stuck_archive.exists(),
        "the staged archive of the failing entry was not put back"
    );
    assert!(
        cache_entry(&app.db, repo_id, "deps-stuck").await.is_some(),
        "the failing entry's row did not survive"
    );
    assert!(
        !follower_archive.exists(),
        "the reachable entry's archive was left on disk"
    );
    assert!(
        cache_entry(&app.db, repo_id, "deps-follower")
            .await
            .is_none(),
        "the reachable entry was never cleaned"
    );
}

/// After the entry commits there is no rollback left — the live name is already
/// free — but retained bytes still make `caches_deleted` a lie. The sweep counts
/// the entry on the failure side and leaves the tombstone where an operator can
/// find it.
#[tokio::test]
async fn expired_cache_cleanup_counts_a_retained_tombstone_as_a_failure() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "cache_retention_debt";
    let repo = "retention-debt";
    let cache_key = "deps-debt";
    let (token, repo_id, archive) = published_cache(&app, owner, repo, cache_key).await;
    expire_cache(&app.db, repo_id, cache_key).await;

    // A directory under the archive's name stages fine (a rename moves it) but
    // cannot be unlinked afterwards, which is exactly the post-commit cleanup
    // failure this asserts on.
    std::fs::remove_file(&archive).expect("remove the published archive");
    std::fs::create_dir(&archive).expect("park a directory under the archive name");

    let summary = cleanup_expired(&app, &token, owner, repo).await;
    assert_eq!(
        summary["caches_deleted"], 0,
        "a cache whose bytes are still staged was counted as cleaned"
    );
    assert_eq!(summary["failures"], 1, "the cleanup debt was not reported");
    assert!(
        cache_entry(&app.db, repo_id, cache_key).await.is_none(),
        "the cache entry did not commit before retirement"
    );
    assert!(
        !archive.exists(),
        "the live archive name outlived a committed delete"
    );
    assert!(
        !leftover_cache_files(&cache_dir(&app.repo_root, repo_id)).is_empty(),
        "failed cleanup did not leave a discoverable tombstone"
    );
}
