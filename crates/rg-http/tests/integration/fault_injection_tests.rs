//! What the server answers when a write it already committed half of fails.
//!
//! The silent-failure sweep changed behaviour that only exists on the failure
//! branch: a `let _ =` on a database write turns "the row is gone" into
//! `201 Created`, and no happy-path test can tell the two apart. These drive
//! the real HTTP stack with one specific write broken — see
//! [`crate::common::fault`] for the two seams.

use std::time::Duration;

use crate::common::fault::{fail_db_writes, spawn_test_app_with_faults, DbWrite};
use crate::common::{create_issue, create_repo, register_full, spawn_test_app_with_db};
use reqwest::multipart::{Form, Part};
use serde_json::Value;

// ── OCI registry ─────────────────────────────────────────────

/// Run the blob-upload sequence a `docker push` performs, returning the digest.
async fn push_blob(base: &str, token: &str, owner: &str, repo: &str, payload: &[u8]) -> String {
    let client = reqwest::Client::new();
    let digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(payload))
    );
    let location = start_upload(base, token, owner, repo, payload).await;
    let finish = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(finish.status(), 201, "blob push failed");
    digest
}

/// Open an upload session and stage `payload` in it, returning the `Location`.
async fn start_upload(base: &str, token: &str, owner: &str, repo: &str, payload: &[u8]) -> String {
    let client = reqwest::Client::new();
    let start = client
        .post(format!("{base}/v2/{owner}/{repo}/blobs/uploads/"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202, "start upload failed");
    let location = start
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("start upload must return a Location")
        .to_string();
    let chunk = client
        .patch(format!("{base}{location}"))
        .bearer_auth(token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 202, "chunk upload failed");
    location
}

/// An image manifest whose only referenced blob is `digest`.
///
/// Docker V2 Schema 2, in the camelCase wire format a real `docker push` sends
/// — no `layers` / `manifests` padding, exactly what the client puts on the
/// wire (card_83a755704a2c).
fn manifest_json(digest: &str, size: usize) -> String {
    serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {
            "mediaType": "application/vnd.docker.container.image.v1+json",
            "size": size,
            "digest": digest,
        },
        "layers": [],
    })
    .to_string()
}

/// A blob whose locating row was lost must not be reported as `201 Created`.
///
/// The bytes reach blob storage one step before the row that points at them. A
/// push that answers 201 with the row missing is a push the client will never
/// retry, into an image whose very next layer pull 404s.
#[tokio::test]
async fn a_blob_row_that_was_not_written_fails_the_push() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "fault_push", "fault_push@example.com").await;
    create_repo(&base, &token, "lost-row").await;

    let payload = b"forgekeep-fault-injected-blob";
    let digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(payload))
    );
    let location = start_upload(&base, &token, "fault_push", "lost-row", payload).await;

    // Everything up to here is the ordinary push; only the blob row fails.
    let fault = fail_db_writes(&db, "oci_blob", DbWrite::Insert).await;
    let finish = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let status = finish.status();
    let body: Value = finish.json().await.unwrap();
    assert_eq!(
        status, 500,
        "a lost blob row must fail the push, not answer 201: {body}"
    );
    assert_eq!(body["errors"][0]["code"], "UNKNOWN");
    let message = body["errors"][0]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&digest),
        "the 500 must name the blob it failed to record: {message}"
    );

    // Control: the identical sequence answers 201 once the row can be written,
    // so the fault is the only difference between the two answers. It needs a
    // fresh session — finalizing consumed the staging file of the first one,
    // which is why the failed push cannot simply be re-`PUT`.
    fault.clear().await;
    push_blob(&base, &token, "fault_push", "lost-row", payload).await;
    let head = client
        .head(format!("{base}/v2/fault_push/lost-row/blobs/{digest}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200, "the successful push must be readable");
}

/// Where a finalized OCI layer lands once the blob store accepts it.
fn oci_blob_path(
    repo_root: &std::path::Path,
    owner: &str,
    repo: &str,
    digest: &str,
) -> std::path::PathBuf {
    let hash = digest.strip_prefix("sha256:").expect("sha256 digest");
    repo_root
        .join("oci")
        .join(owner)
        .join(repo)
        .join("blobs")
        .join("sha256")
        .join(&hash[..2])
        .join(hash)
}

/// A push whose blob row was never written must not keep the bytes.
///
/// Every route to an OCI blob — reclamation included — goes through its
/// `oci_blobs` row, so bytes left without one are unreachable. Every failed push
/// then costs one layer of disk forever, with nothing in the tree tying the file
/// back to the request that made it.
#[tokio::test]
async fn a_push_whose_blob_row_was_never_written_leaves_no_blob() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&app.base, "oci_orphan", "oci_orphan@example.com").await;
    create_repo(&app.base, &token, "orphan-blob").await;

    let payload = b"forgekeep-orphaned-layer";
    let digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(payload))
    );
    let location = start_upload(&app.base, &token, "oci_orphan", "orphan-blob", payload).await;

    let fault = fail_db_writes(&app.db, "oci_blob", DbWrite::Insert).await;
    let finish = client
        .put(format!("{app_base}{location}", app_base = app.base))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        finish.status(),
        500,
        "a lost blob row must fail the push, not answer 201"
    );
    fault.clear().await;

    let blob = oci_blob_path(&app.repo_root, "oci_orphan", "orphan-blob", &digest);
    assert!(
        !blob.exists(),
        "the published layer outlived the row that would have claimed it: {}",
        blob.display()
    );

    // Control: the same sequence, with the row writable, does store the bytes —
    // so the assertion above is about the rollback and not about a path that is
    // simply never written.
    push_blob(&app.base, &token, "oci_orphan", "orphan-blob", payload).await;
    assert!(
        blob.exists(),
        "a successful push must leave the layer in storage: {}",
        blob.display()
    );
}

/// A re-push whose row insert fails must NOT take the stored layer with it.
///
/// Finalizing deduplicates: a key that already holds these bytes is reused
/// untouched, and the push that first stored them has a row pointing at it.
/// Compensating there would answer a failed push by deleting a live layer —
/// the earlier image stops pulling. The rollback is only for the caller that
/// actually published.
#[tokio::test]
async fn a_failed_repush_does_not_delete_the_layer_an_earlier_push_stored() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&app.base, "oci_dedup", "oci_dedup@example.com").await;
    create_repo(&app.base, &token, "shared-layer").await;

    let payload = b"forgekeep-shared-layer";
    let digest = push_blob(&app.base, &token, "oci_dedup", "shared-layer", payload).await;
    let blob = oci_blob_path(&app.repo_root, "oci_dedup", "shared-layer", &digest);
    assert!(blob.exists(), "the first push must store the layer");

    // A second push of the identical layer takes the dedup branch and then
    // fails to record a row (here because the row already exists — the same
    // shape a concurrent push produces).
    let location = start_upload(&app.base, &token, "oci_dedup", "shared-layer", payload).await;
    let finish = client
        .put(format!("{app_base}{location}", app_base = app.base))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(
        finish.status().is_client_error() || finish.status().is_server_error(),
        "this scenario is only meaningful while the duplicate row is refused (card_eff9561d6171)"
    );

    assert!(
        blob.exists(),
        "the failed re-push deleted the layer the first push stored: {}",
        blob.display()
    );
    let head = client
        .head(format!(
            "{app_base}/v2/oci_dedup/shared-layer/blobs/{digest}",
            app_base = app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        head.status(),
        200,
        "the image the first push produced must still pull"
    );
}

/// A manifest whose blob ref counts were not incremented must fail the push.
///
/// An under-counted blob is one the GC may delete while this manifest still
/// points at it: the image breaks later, far from the push that caused it.
#[tokio::test]
async fn a_ref_count_that_was_not_incremented_fails_the_manifest_push() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "fault_ref", "fault_ref@example.com").await;
    create_repo(&base, &token, "lost-ref").await;

    let config = b"{\"architecture\":\"amd64\",\"os\":\"linux\"}";
    let digest = push_blob(&base, &token, "fault_ref", "lost-ref", config).await;
    let manifest = manifest_json(&digest, config.len());
    let url = format!("{base}/v2/fault_ref/lost-ref/manifests/v1");

    // Control: the same manifest, same tag, with nothing broken.
    let ok = client
        .put(&url)
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/vnd.docker.distribution.manifest.v2+json",
        )
        .body(manifest.clone())
        .send()
        .await
        .unwrap();
    let ok_status = ok.status();
    assert_eq!(
        ok_status,
        201,
        "control manifest push failed: {}",
        ok.text().await.unwrap()
    );

    // Re-pushing the tag takes the update path, and increments the ref counts
    // again — which is the write this fault rejects.
    let _fault = fail_db_writes(&db, "oci_blob", DbWrite::Update).await;
    let broken = client
        .put(&url)
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/vnd.docker.distribution.manifest.v2+json",
        )
        .body(manifest)
        .send()
        .await
        .unwrap();
    let status = broken.status();
    let body: Value = broken.json().await.unwrap();
    assert_eq!(
        status, 500,
        "a lost ref-count increment must fail the manifest push: {body}"
    );
    let message = body["errors"][0]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&digest),
        "the 500 must name the blob whose ref count it could not increment: {message}"
    );
}

// ── Package registry ─────────────────────────────────────────

/// Where a published package file lands once the blob store accepts it.
fn package_file_path(
    repo_root: &std::path::Path,
    owner: &str,
    repo: &str,
    package_type: &str,
    name: &str,
    version: &str,
    filename: &str,
) -> std::path::PathBuf {
    repo_root
        .join("packages")
        .join(owner)
        .join(repo)
        .join(package_type)
        .join(name)
        .join(version)
        .join(filename)
}

/// A publish that failed on its second file must not keep the first one.
///
/// This one calls the service instead of driving the route, and deliberately:
/// the HTTP endpoint sends one file per request (`packages.rs`), so the loop
/// that stores several of them is only reachable through
/// `package_registry::service::publish` itself. The branch is still the one
/// that runs in production — a second file goes through `add_files_to_version`,
/// which rolls back, while the *first* request of a version takes this path —
/// and leaving it uncompensated means any future caller that publishes a
/// multi-file version leaks every file stored before the failure: the version
/// row is written after the loop, so nothing points at them and retention,
/// which walks rows, never comes back for them.
#[tokio::test]
async fn a_publish_that_failed_part_way_keeps_none_of_its_files() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let (token, user_id) = register_full(&app.base, "pkg_part", "pkg_part@example.com").await;
    create_repo(&app.base, &token, "half-published").await;

    let info = || rg_core::package_registry::PublishInfo {
        owner: "pkg_part".to_string(),
        repo: "half-published".to_string(),
        package_type: "maven".to_string(),
        name: "widget".to_string(),
        version: "1.0.0".to_string(),
        semver: None,
        metadata: None,
        description: None,
        homepage: None,
        repository_url: None,
        author_id: user_id,
        files: vec![
            ("widget-1.0.0.pom".to_string(), b"<project/>".to_vec()),
            ("widget-1.0.0.jar".to_string(), b"jar bytes".to_vec()),
        ],
    };

    let root = std::sync::Arc::new(rg_core::blob_storage::LocalBlobStorage::new(&app.repo_root));
    let broken = rg_core::package_registry::PackageStorage::from_backend(
        crate::common::fault::RejectOneKey::wrap(root.clone(), "widget-1.0.0.jar"),
    );
    let failed = rg_core::package_registry::service::publish(&app.db, &broken, info()).await;
    assert!(
        failed.is_err(),
        "a file the blob store refused must fail the publish"
    );

    let pom = package_file_path(
        &app.repo_root,
        "pkg_part",
        "half-published",
        "maven",
        "widget",
        "1.0.0",
        "widget-1.0.0.pom",
    );
    assert!(
        !pom.exists(),
        "the file stored before the failure outlived the publish that would have claimed it: {}",
        pom.display()
    );

    // Control: the same publish, with the store healthy, does write that file —
    // so the assertion above is about the rollback and not about a path that is
    // never written in the first place.
    let healthy = rg_core::package_registry::PackageStorage::from_backend(root);
    rg_core::package_registry::service::publish(&app.db, &healthy, info())
        .await
        .expect("the publish must succeed once the blob store accepts every file");
    assert!(
        pom.exists(),
        "a successful publish must leave its files in storage: {}",
        pom.display()
    );
}

// ── Attachments ──────────────────────────────────────────────

/// Upload one attachment, returning `(status, body)`.
async fn upload_attachment(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
) -> (u16, String) {
    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{number}/assets"
        ))
        .bearer_auth(token)
        .timeout(Duration::from_secs(30))
        .multipart(
            Form::new().part(
                "attachment",
                Part::bytes(b"fault-injected attachment".to_vec())
                    .file_name("evidence.txt")
                    .mime_str("text/plain")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}

async fn list_attachments(base: &str, owner: &str, repo: &str, number: i64) -> Vec<Value> {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{number}/assets"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// A blob store that refused the bytes must not produce a `201 Created`.
#[tokio::test]
async fn an_attachment_whose_bytes_were_refused_is_not_reported_as_created() {
    let (base, _db, faults) = spawn_test_app_with_faults().await;
    let (token, _user_id) = register_full(&base, "fault_blob", "fault_blob@example.com").await;
    create_repo(&base, &token, "refused-bytes").await;
    let (_issue_id, number) = create_issue(
        &base,
        &token,
        "fault_blob",
        "refused-bytes",
        "attachment target",
    )
    .await;

    faults.fail_put_file();
    let (status, body) =
        upload_attachment(&base, &token, "fault_blob", "refused-bytes", number).await;
    assert_eq!(
        status, 500,
        "a blob store that refused the upload is the server's failure: {body}"
    );

    faults.heal();
    assert!(
        list_attachments(&base, "fault_blob", "refused-bytes", number)
            .await
            .is_empty(),
        "a refused upload must leave no attachment behind"
    );
}

/// A rollback that fails too must not swallow the failure that caused it.
///
/// The metadata insert fails after the bytes are already stored, so the handler
/// compensates by deleting the blob — and here that delete fails as well. The
/// compensation is best-effort by design: it may warn, it may not clean up, but
/// it may never turn the original database failure into a success the client
/// believes.
#[tokio::test]
async fn a_failed_rollback_does_not_mask_the_failure_that_triggered_it() {
    let (base, db, faults) = spawn_test_app_with_faults().await;
    let (token, _user_id) = register_full(&base, "fault_comp", "fault_comp@example.com").await;
    create_repo(&base, &token, "failed-rollback").await;
    let (_issue_id, number) = create_issue(
        &base,
        &token,
        "fault_comp",
        "failed-rollback",
        "attachment target",
    )
    .await;

    let fault = fail_db_writes(&db, "attachments", DbWrite::Insert).await;
    faults.fail_delete();
    let (status, body) =
        upload_attachment(&base, &token, "fault_comp", "failed-rollback", number).await;
    assert_eq!(
        status, 500,
        "the metadata failure must reach the client even when its rollback failed too: {body}"
    );

    fault.clear().await;
    faults.heal();
    assert!(
        list_attachments(&base, "fault_comp", "failed-rollback", number)
            .await
            .is_empty(),
        "no attachment row can exist after the insert that failed"
    );
}

// ── Repository directory rollback ────────────────────────────

/// A fork whose row was lost must not leave the clone behind.
///
/// The bare clone lands at the forker's *canonical* path one step before the
/// row that names it. Left there, it is invisible to the duplicate check (which
/// reads rows) and fatal to `git clone` (which sees the directory), so the
/// retry dies on "destination path already exists" for as long as the
/// deployment lives. The retry is the assertion: a fork that can be repeated is
/// a fork that left nothing behind.
#[tokio::test]
async fn a_fork_whose_row_was_lost_can_be_retried() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (source_token, _source_id) =
        register_full(&app.base, "fork_source", "fork_source@example.com").await;
    create_repo(&app.base, &source_token, "forkable").await;
    let (forker_token, _forker_id) =
        register_full(&app.base, "fork_taker", "fork_taker@example.com").await;

    let fault = fail_db_writes(&app.db, "repositories", DbWrite::Insert).await;
    let failed = client
        .post(format!(
            "{}/api/v1/repos/fork_source/forkable/fork",
            app.base
        ))
        .bearer_auth(&forker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        failed.status(),
        500,
        "a lost fork row must fail the fork, not answer 201"
    );

    let clone_path = app.repo_root.join("fork_taker").join("forkable.git");
    assert!(
        !clone_path.exists(),
        "the failed fork left its clone at {}",
        clone_path.display()
    );

    fault.clear().await;
    let retried = client
        .post(format!(
            "{}/api/v1/repos/fork_source/forkable/fork",
            app.base
        ))
        .bearer_auth(&forker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        retried.status(),
        201,
        "the fork must be repeatable once the database is healthy again"
    );
    assert!(clone_path.exists(), "the successful fork wrote no clone");
}

/// A transfer whose row was lost must leave the tree where the row still says
/// it is.
///
/// The directory moves before the ownership update. If the update fails and the
/// move stands, the old owner holds a row whose tree is gone and the new owner
/// a tree no row names — a repository that is broken for both and that nothing
/// repairs on its own.
///
/// The destination is an organization the owner belongs to rather than another
/// account, because since card_934b6037bcda a transfer only reaches the service
/// when the caller may put a repository in the destination namespace at all —
/// handing one to a stranger is `403` at the route. The rollback under test is
/// the same either way.
#[tokio::test]
async fn a_transfer_whose_row_was_lost_leaves_the_tree_with_its_owner() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (org_owner_token, _) = register_full(&app.base, "xfer_org", "xfer_org@example.com").await;
    let (owner_token, owner_id) =
        register_full(&app.base, "xfer_from", "xfer_from@example.com").await;
    create_repo(&app.base, &owner_token, "movable").await;

    let created = client
        .post(format!("{}/api/v1/orgs", app.base))
        .bearer_auth(&org_owner_token)
        .json(&serde_json::json!({ "name": "xfercorp", "visibility": "public" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        created.status(),
        201,
        "baseline: the destination org exists"
    );
    let joined = client
        .post(format!("{}/api/v1/orgs/xfercorp/members", app.base))
        .bearer_auth(&org_owner_token)
        .json(&serde_json::json!({ "user_id": owner_id, "role": "member" }))
        .send()
        .await
        .unwrap();
    assert_eq!(joined.status(), 201, "baseline: the owner may transfer in");

    let fault = fail_db_writes(&app.db, "repositories", DbWrite::Update).await;
    let failed = client
        .post(format!(
            "{}/api/v1/repos/xfer_from/movable/transfer",
            app.base
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "new_owner": "xfercorp" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        failed.status(),
        500,
        "a lost ownership update must fail the transfer, not answer 200"
    );
    fault.clear().await;

    let stayed = app.repo_root.join("xfer_from").join("movable.git");
    let moved = app.repo_root.join("xfercorp").join("movable.git");
    assert!(
        stayed.exists(),
        "the tree must be back where its row says it is: {}",
        stayed.display()
    );
    assert!(
        !moved.exists(),
        "the tree stayed at the destination no row names: {}",
        moved.display()
    );

    // The row still points at a repository the server can serve.
    let readable = client
        .get(format!("{}/api/v1/repos/xfer_from/movable", app.base))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        readable.status(),
        200,
        "the repository must still work for its owner after the failed transfer"
    );
}

// ── LFS object upload ────────────────────────────────────────

/// `PUT` an LFS object the way `git lfs push` does, returning `(status, oid)`.
async fn upload_lfs_object(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    payload: &[u8],
) -> (u16, String) {
    let oid = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(payload));
    let status = reqwest::Client::new()
        .put(format!(
            "{base}/api/v1/repos/{owner}/{repo}/lfs/objects/{oid}"
        ))
        .bearer_auth(token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    (status, oid)
}

/// Everything the upload staged in the repository's LFS root, sorted.
///
/// The staging files carry a UUID in their name, so the assertion cannot name
/// them — it can only insist the directory is empty. That is the right shape
/// anyway: a leak the test does not know how to name is still a leak.
fn lfs_staging_leftovers(repo_root: &std::path::Path, owner: &str, repo: &str) -> Vec<String> {
    let root = repo_root.join(format!("{owner}.lfs")).join(repo);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

/// Where the compressed object lands once the blob store accepts it.
fn lfs_blob_path(
    repo_root: &std::path::Path,
    owner: &str,
    repo: &str,
    oid: &str,
) -> std::path::PathBuf {
    repo_root
        .join("lfs")
        .join(owner)
        .join(repo)
        .join(&oid[..2])
        .join(format!("{oid}.zst"))
}

/// An upload that failed before storing anything must not keep the body.
///
/// The handler streams the request body to `.tmp_<oid>` — the full size of the
/// object — before the service ever looks at the database. Every exit between
/// that write and the blob store used to keep it: no row points at the file, so
/// retention never comes back for it and the LFS root grows by one full upload
/// per failed push, with nothing in the log tying the two together.
#[tokio::test]
async fn an_lfs_upload_whose_row_was_never_written_leaves_no_staging_file() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let (token, _user_id) = register_full(&app.base, "lfs_row", "lfs_row@example.com").await;
    create_repo(&app.base, &token, "lost-lfs-row").await;

    let fault = fail_db_writes(&app.db, "lfs_objects", DbWrite::Insert).await;
    let (status, _oid) = upload_lfs_object(
        &app.base,
        &token,
        "lfs_row",
        "lost-lfs-row",
        b"forgekeep-lfs-payload",
    )
    .await;
    assert_eq!(
        status, 500,
        "a lost LFS row must fail the upload, not answer 200"
    );
    fault.clear().await;

    assert_eq!(
        lfs_staging_leftovers(&app.repo_root, "lfs_row", "lost-lfs-row"),
        Vec::<String>::new(),
        "the failed upload kept its staging files"
    );
}

/// A blob store that refused the object must not keep either staging file.
#[tokio::test]
async fn an_lfs_object_the_blob_store_refused_leaves_no_staging_file() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let (token, _user_id) = register_full(&app.base, "lfs_put", "lfs_put@example.com").await;
    create_repo(&app.base, &token, "refused-lfs").await;

    app.blob_faults.fail_put_file();
    let (status, _oid) = upload_lfs_object(
        &app.base,
        &token,
        "lfs_put",
        "refused-lfs",
        b"forgekeep-lfs-refused",
    )
    .await;
    assert_eq!(
        status, 500,
        "a blob store that refused the object is the server's failure"
    );
    app.blob_faults.heal();

    assert_eq!(
        lfs_staging_leftovers(&app.repo_root, "lfs_put", "refused-lfs"),
        Vec::<String>::new(),
        "the refused upload kept its staging files"
    );
}

/// An object whose row was never marked uploaded must not keep the blob.
///
/// The bytes reach the blob store one step before the row that claims them. Left
/// behind, they are invisible to retention (which walks rows) and useless to
/// downloads (which refuse a row reading `uploaded = false`) — storage that
/// nothing will ever free and nothing will ever serve.
#[tokio::test]
async fn an_lfs_object_that_was_never_marked_uploaded_leaves_no_blob() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let (token, _user_id) = register_full(&app.base, "lfs_mark", "lfs_mark@example.com").await;
    create_repo(&app.base, &token, "unmarked-lfs").await;

    let fault = fail_db_writes(&app.db, "lfs_objects", DbWrite::Update).await;
    let (status, oid) = upload_lfs_object(
        &app.base,
        &token,
        "lfs_mark",
        "unmarked-lfs",
        b"forgekeep-lfs-unmarked",
    )
    .await;
    assert_eq!(
        status, 500,
        "an object that was never marked uploaded must fail the upload"
    );
    fault.clear().await;

    let blob = lfs_blob_path(&app.repo_root, "lfs_mark", "unmarked-lfs", &oid);
    assert!(
        !blob.exists(),
        "the stored blob outlived the row that would have claimed it: {}",
        blob.display()
    );
    assert_eq!(
        lfs_staging_leftovers(&app.repo_root, "lfs_mark", "unmarked-lfs"),
        Vec::<String>::new(),
        "the failed upload kept its staging files"
    );
}

// ── Release assets ───────────────────────────────────────────

/// Create a release and return its id.
async fn create_release(base: &str, token: &str, owner: &str, repo: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/releases"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "tag_name": "v1.0.0", "title": "release" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "create release failed");
    response.json::<Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// A release asset whose bytes were refused must not leave its row behind.
///
/// This one runs the other way round from the uploads above: `upload_asset`
/// inserts the metadata row *first*, to derive the blob key from the id it gets
/// back, and only then writes the bytes. So the leak is a row rather than a
/// file — and it is the worse of the two, because a row is what every listing
/// walks. The release keeps advertising an asset, and each download of it dies
/// on a blob that was never written; nothing on the happy path can tell,
/// because the row looks exactly like a healthy one.
///
/// `upload_failure_status_tests` already pins the *status* of this failure to
/// 500. What it does not check is that the row is gone afterwards, which is the
/// half `warn_orphan_asset_row` exists for.
#[tokio::test]
async fn a_release_asset_whose_bytes_were_refused_leaves_no_row_behind() {
    let (base, db, faults) = spawn_test_app_with_faults().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "asset_orphan", "asset_orphan@example.com").await;
    create_repo(&base, &token, "orphan-asset").await;
    let release_id = create_release(&base, &token, "asset_orphan", "orphan-asset").await;
    let assets_url =
        format!("{base}/api/v1/repos/asset_orphan/orphan-asset/releases/{release_id}/assets");

    faults.fail_put();
    let refused = client
        .post(&assets_url)
        .bearer_auth(&token)
        .header("x-asset-filename", "payload.bin")
        .body(b"forgekeep-release-asset".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        500,
        "a blob store that refused the asset is the server's failure"
    );

    faults.heal();
    let orphans = rg_db::ops::release_ops::list_assets(&db, release_id)
        .await
        .expect("listing the release assets must work once the store is healthy");
    assert!(
        orphans.is_empty(),
        "the metadata row outlived the bytes it describes: {:?}",
        orphans.iter().map(|a| &a.filename).collect::<Vec<_>>()
    );

    // Control: the identical request succeeds once the store accepts the bytes,
    // so the assertion above is about the rollback and not about an upload that
    // never got as far as inserting a row.
    let accepted = client
        .post(&assets_url)
        .bearer_auth(&token)
        .header("x-asset-filename", "payload.bin")
        .body(b"forgekeep-release-asset".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        accepted.status(),
        201,
        "the upload must succeed once the blob store accepts the bytes"
    );
    assert_eq!(
        rg_db::ops::release_ops::list_assets(&db, release_id)
            .await
            .unwrap()
            .len(),
        1,
        "a successful upload must leave exactly its own row"
    );
}

// ── CI artifacts ─────────────────────────────────────────────

/// A pipeline with one job, already assigned to `runner_id`.
///
/// The artifact route is runner-authenticated and refuses a job that is not
/// assigned to the caller, so there is no shortcut to reaching the handler.
async fn create_assigned_job(db: &rg_db::DatabaseConnection, repo_id: i64, runner_id: i64) -> i64 {
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
        db, stage.id, "unit", "echo ok", None, None, None, None, None, false, None, None, None,
    )
    .await
    .unwrap();
    rg_db::ops::pipeline_ops::assign_job(db, job.id, runner_id)
        .await
        .unwrap();
    job.id
}

/// What a job's artifacts are called on disk, whatever their blob names are.
///
/// The key carries a UUID (`artifact_key`), so the test cannot name the file it
/// is looking for — it asks whether the job's directory holds anything at all.
fn artifact_leftovers(repo_root: &std::path::Path, job_id: i64) -> Vec<String> {
    let dir = repo_root
        .join("artifacts")
        .join("jobs")
        .join(job_id.to_string());
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

/// A CI artifact whose row was never written must not leave its blob behind.
///
/// The bytes land in blob storage before the row that names them, and the row
/// is the only thing that can ever find them again: the download route resolves
/// an artifact by id, and CI retention expires artifacts by walking rows. A blob
/// that outlives its failed insert is therefore not merely unreferenced, it is
/// unreachable *and* immune to the cleanup that would otherwise bound the disk
/// a runner can fill — every retry of the upload adds another copy, because the
/// key carries a fresh UUID each time.
#[tokio::test]
async fn a_ci_artifact_whose_row_was_never_written_leaves_no_blob() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) =
        register_full(&app.base, "artifact_orphan", "artifact_orphan@example.com").await;
    let repo_id = create_repo(&app.base, &token, "orphan-artifact").await;
    let runner =
        rg_db::ops::runner_ops::register_runner(&app.db, "orphan-runner", "", None, None, None)
            .await
            .unwrap();
    let job_id = create_assigned_job(&app.db, repo_id, runner.id).await;
    let artifacts_url = format!(
        "{}/api/v1/runners/{}/jobs/{}/artifacts",
        app.base, runner.id, job_id
    );

    let fault = fail_db_writes(&app.db, "artifacts", DbWrite::Insert).await;
    let failed = client
        .post(&artifacts_url)
        .bearer_auth(&runner.token)
        .header("x-artifact-name", "report.txt")
        .body(b"forgekeep-artifact-bytes".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        failed.status(),
        500,
        "a lost artifact row must fail the upload, not answer 201"
    );
    assert_eq!(
        artifact_leftovers(&app.repo_root, job_id),
        Vec::<String>::new(),
        "the stored blob outlived the row that would have claimed it"
    );

    // Control: the same upload writes exactly one blob once the row can be
    // recorded, so the emptiness above is the rollback and not a path that
    // never stored anything to begin with.
    fault.clear().await;
    let accepted = client
        .post(&artifacts_url)
        .bearer_auth(&runner.token)
        .header("x-artifact-name", "report.txt")
        .body(b"forgekeep-artifact-bytes".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        accepted.status(),
        201,
        "the upload must succeed once the database accepts the row"
    );
    assert_eq!(
        artifact_leftovers(&app.repo_root, job_id).len(),
        1,
        "a successful upload must leave exactly its own blob"
    );
}
