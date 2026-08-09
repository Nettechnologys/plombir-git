//! What the server answers when a write it already committed half of fails.
//!
//! The silent-failure sweep changed behaviour that only exists on the failure
//! branch: a `let _ =` on a database write turns "the row is gone" into
//! `201 Created`, and no happy-path test can tell the two apart. These drive
//! the real HTTP stack with one specific write broken — see
//! [`crate::common::fault`] for the two seams.

use std::time::Duration;

use crate::common::fault::{
    fail_db_writes, spawn_test_app_with_faults, spawn_test_app_with_first_put_gate,
    spawn_test_app_with_two_put_gate, DbWrite,
};
use crate::common::{create_issue, create_repo, register_full, spawn_test_app_with_db};
use reqwest::multipart::{Form, Part};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, PaginatorTrait, QueryFilter, Statement,
};
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
    crate::common::assert_blob_push_created(finish, payload.len()).await;
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

/// A mount owns only the destination bytes it actually copied.
///
/// Losing the first target row must roll its new copy back. Losing a later
/// idempotent insert must not delete the already-recorded layer the first
/// successful mount owns.
#[tokio::test]
async fn a_mount_row_failure_rolls_back_only_its_own_copy() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) =
        register_full(&app.base, "mount_rollback", "mount_rollback@example.com").await;
    create_repo(&app.base, &token, "source-image").await;
    create_repo(&app.base, &token, "target-image").await;

    let payload = b"forgekeep-mounted-layer";
    let digest = push_blob(&app.base, &token, "mount_rollback", "source-image", payload).await;
    let target = oci_blob_path(&app.repo_root, "mount_rollback", "target-image", &digest);
    let mount = || {
        client
            .post(format!(
                "{}/v2/mount_rollback/target-image/blobs/uploads/",
                app.base
            ))
            .query(&[
                ("mount", digest.as_str()),
                ("from", "mount_rollback/source-image"),
            ])
            .bearer_auth(&token)
            .send()
    };

    let fault = fail_db_writes(&app.db, "oci_blob", DbWrite::Insert).await;
    let failed = mount().await.unwrap();
    assert_eq!(
        failed.status(),
        500,
        "a mount whose target row was lost must not answer 201"
    );
    assert!(
        !target.exists(),
        "the first failed mount left unowned bytes at {}",
        target.display()
    );

    fault.clear().await;
    let healthy = mount().await.unwrap();
    assert_eq!(healthy.status(), 201, "the healthy mount must succeed");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        payload,
        "the successful mount stored different bytes"
    );

    let fault = fail_db_writes(&app.db, "oci_blob", DbWrite::Insert).await;
    let failed_retry = mount().await.unwrap();
    assert_eq!(
        failed_retry.status(),
        500,
        "the injected database failure must still reach the retrying client"
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        payload,
        "a failed repeated mount deleted the layer an earlier mount recorded"
    );

    fault.clear().await;
    assert_eq!(
        mount().await.unwrap().status(),
        201,
        "the repeated mount must be idempotent once the database is healthy"
    );
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

/// Where a content-addressed OCI manifest lands in the shared blob store.
fn oci_manifest_path(
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
        .join("manifests")
        .join("sha256")
        .join(hash)
}

/// Re-uploading content the repository already owns is a successful no-op.
///
/// Docker may skip the preliminary HEAD, and retries routinely send the same
/// layer again. The distribution contract is 201 plus a retrievable, unchanged
/// blob — never a database constraint diagnostic.
#[tokio::test]
async fn a_second_push_of_the_same_blob_is_idempotent() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&app.base, "oci_retry", "oci_retry@example.com").await;
    create_repo(&app.base, &token, "same-layer").await;

    let payload = b"forgekeep-idempotent-layer";
    let digest = push_blob(&app.base, &token, "oci_retry", "same-layer", payload).await;
    let blob = oci_blob_path(&app.repo_root, "oci_retry", "same-layer", &digest);
    let original = std::fs::read(&blob).expect("the first push must store the layer");

    let location = start_upload(&app.base, &token, "oci_retry", "same-layer", payload).await;
    let finish = client
        .put(format!("{app_base}{location}", app_base = app.base))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let status = finish.status();
    let returned_digest = finish
        .headers()
        .get("docker-content-digest")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = finish.text().await.unwrap();

    assert_eq!(status, 201, "the repeated push failed: {body}");
    assert_eq!(returned_digest.as_deref(), Some(digest.as_str()));
    assert!(
        !body.contains("UNIQUE constraint"),
        "a database constraint escaped through the OCI response: {body}"
    );
    assert_eq!(
        std::fs::read(&blob).expect("the repeated push must keep the layer"),
        original,
        "the no-op push changed the bytes already stored under the digest"
    );

    let head = client
        .head(format!(
            "{app_base}/v2/oci_retry/same-layer/blobs/{digest}",
            app_base = app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        head.status(),
        200,
        "the repeated layer must remain readable"
    );
}

/// Two clients can both miss the preliminary HEAD and finalize the same layer.
/// The unique index is the serialization point; neither winner is an error.
#[tokio::test]
async fn concurrent_pushes_of_the_same_blob_both_succeed() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&app.base, "oci_race", "oci_race@example.com").await;
    create_repo(&app.base, &token, "same-layer").await;

    let payload = b"forgekeep-concurrent-layer";
    let digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(payload))
    );
    let first = start_upload(&app.base, &token, "oci_race", "same-layer", payload).await;
    let second = start_upload(&app.base, &token, "oci_race", "same-layer", payload).await;

    let finish = |location: String| {
        client
            .put(format!("{app_base}{location}", app_base = app.base))
            .query(&[("digest", digest.as_str())])
            .bearer_auth(&token)
            .send()
    };
    let (first, second) = tokio::join!(finish(first), finish(second));

    for response in [first.unwrap(), second.unwrap()] {
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(status, 201, "one concurrent finalize lost the race: {body}");
        assert!(
            !body.contains("UNIQUE constraint"),
            "a database constraint escaped through the OCI response: {body}"
        );
    }

    let fetched = client
        .get(format!(
            "{app_base}/v2/oci_race/same-layer/blobs/{digest}",
            app_base = app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(fetched.status(), 200);
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), payload);
}

/// The first two uploads for a ForgeKeep repository can both observe that its
/// OCI row is absent. The unique namespace index serializes creation, but the
/// losing request must reuse the winner's row rather than surface the conflict.
#[tokio::test]
async fn concurrent_first_uploads_share_one_oci_repository() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) =
        register_full(&app.base, "oci_repo_race", "oci_repo_race@example.com").await;
    let repo_id = create_repo(&app.base, &token, "first-upload").await;

    let start = || {
        client
            .post(format!(
                "{}/v2/oci_repo_race/first-upload/blobs/uploads/",
                app.base
            ))
            .bearer_auth(&token)
            .send()
    };
    let (first, second) = tokio::join!(start(), start());

    let mut upload_uuids = Vec::new();
    for response in [first.unwrap(), second.unwrap()] {
        let status = response.status();
        let upload_uuid = response
            .headers()
            .get("docker-upload-uuid")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.text().await.unwrap();
        assert_eq!(
            status, 202,
            "one concurrent first upload lost repository creation: {body}"
        );
        assert!(
            !body.contains("UNIQUE constraint") && !body.contains("duplicate key"),
            "a database constraint escaped through the OCI response: {body}"
        );
        upload_uuids.push(upload_uuid.expect("start upload must return Docker-Upload-UUID"));
    }
    assert_ne!(
        upload_uuids[0], upload_uuids[1],
        "each successful request must own a distinct upload session"
    );

    let rows = rg_db::entities::oci_repository::Entity::find()
        .filter(rg_db::entities::oci_repository::Column::RepoId.eq(repo_id))
        .filter(rg_db::entities::oci_repository::Column::Namespace.eq("oci_repo_race/first-upload"))
        .count(&app.db)
        .await
        .unwrap();
    assert_eq!(rows, 1, "the namespace must have exactly one OCI row");
}

/// Conflict handling is deliberately scoped to the unique namespace race. A
/// real insert failure must still fail the request instead of being mistaken
/// for a concurrently-created row.
#[tokio::test]
async fn an_oci_repository_insert_failure_still_returns_5xx() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(
        &app.base,
        "oci_repo_insert_fault",
        "oci_repo_insert_fault@example.com",
    )
    .await;
    create_repo(&app.base, &token, "broken-first-upload").await;
    let fault = fail_db_writes(&app.db, "oci_repository", DbWrite::Insert).await;

    let response = client
        .post(format!(
            "{}/v2/oci_repo_insert_fault/broken-first-upload/blobs/uploads/",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert!(
        status.is_server_error(),
        "a real OCI repository insert failure must stay 5xx, got {status}: {body}"
    );

    fault.clear().await;
}

/// Retrying a digest-addressed manifest PUT is a successful no-op.
///
/// Only the request that creates the manifest row owns its blob-reference
/// increments. A retry must neither surface the unique index nor count the
/// same manifest a second time.
#[tokio::test]
async fn a_second_put_of_the_same_manifest_digest_is_idempotent() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) =
        register_full(&app.base, "manifest_retry", "manifest_retry@example.com").await;
    create_repo(&app.base, &token, "same-manifest").await;

    let config = b"{\"architecture\":\"amd64\",\"os\":\"linux\"}";
    let config_digest =
        push_blob(&app.base, &token, "manifest_retry", "same-manifest", config).await;
    let manifest = manifest_json(&config_digest, config.len());
    let manifest_digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(manifest.as_bytes()))
    );
    let url = format!(
        "{}/v2/manifest_retry/same-manifest/manifests/{manifest_digest}",
        app.base
    );

    for attempt in 0..2 {
        let response = client
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
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(
            status, 201,
            "manifest PUT attempt {attempt} failed instead of being idempotent: {body}"
        );
        let lower = body.to_ascii_lowercase();
        assert!(
            !lower.contains("unique constraint") && !lower.contains("duplicate key"),
            "a database constraint escaped through the OCI response: {body}"
        );
    }

    let forgekeep_repo =
        rg_core::repo::service::find_repo_by_owner_name(&app.db, "manifest_retry", "same-manifest")
            .await
            .unwrap()
            .unwrap();
    let oci_repo = rg_db::ops::oci_ops::find_repo_by_id(&app.db, forgekeep_repo.id)
        .await
        .unwrap()
        .unwrap();
    let manifest_rows = rg_db::entities::oci_manifest::Entity::find()
        .filter(rg_db::entities::oci_manifest::Column::OciRepositoryId.eq(oci_repo.id))
        .filter(rg_db::entities::oci_manifest::Column::Digest.eq(&manifest_digest))
        .count(&app.db)
        .await
        .unwrap();
    assert_eq!(manifest_rows, 1, "a retry created a second manifest row");

    let blobs = rg_db::entities::oci_blob::Entity::find()
        .filter(rg_db::entities::oci_blob::Column::OciRepositoryId.eq(oci_repo.id))
        .filter(rg_db::entities::oci_blob::Column::Digest.eq(&config_digest))
        .count(&app.db)
        .await
        .unwrap();
    assert_eq!(
        blobs, 1,
        "the retry created a second row for the same layer"
    );
}

/// Two concurrent PUTs of one manifest digest both succeed, and the second one
/// never enters the storage write while the first owns the key.
///
/// It used to rendezvous both writes inside the blob store and assert only the
/// outcome. That interleaving is the hazard, not the contract: with both
/// requests holding the same content-addressed key, `published` stops meaning
/// "these bytes are mine" and the loser's rollback deletes the winner's
/// manifest. The publication lease keeps the second request outside the storage
/// layer entirely, so the invariant to state is that it stays there — the
/// arrival count is what proves it, and the outcome assertions below are the
/// ones the old test already made.
#[tokio::test]
async fn concurrent_puts_of_the_same_manifest_digest_both_succeed() {
    let config = b"{\"architecture\":\"arm64\",\"os\":\"linux\"}";
    let config_digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(config))
    );
    let manifest = manifest_json(&config_digest, config.len());
    let manifest_digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(manifest.as_bytes()))
    );
    let hash = manifest_digest.strip_prefix("sha256:").unwrap();
    let needle = format!("manifests/sha256/{hash}");
    let (base, db, gate) = spawn_test_app_with_first_put_gate(&needle).await;
    let client = reqwest::Client::new();
    let (token, _user_id) =
        register_full(&base, "manifest_race", "manifest_race@example.com").await;
    create_repo(&base, &token, "same-manifest").await;
    let pushed_digest = push_blob(&base, &token, "manifest_race", "same-manifest", config).await;
    assert_eq!(pushed_digest, config_digest);

    let url = format!("{base}/v2/manifest_race/same-manifest/manifests/{manifest_digest}");
    let publish = || {
        let client = client.clone();
        let token = token.clone();
        let url = url.clone();
        let manifest = manifest.clone();
        tokio::spawn(async move {
            client
                .put(url)
                .bearer_auth(token)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/vnd.docker.distribution.manifest.v2+json",
                )
                .body(manifest)
                .send()
                .await
                .unwrap()
        })
    };

    let left = publish();
    assert!(
        gate.await_first().await,
        "the first manifest PUT never reached the storage write"
    );

    // Only now does the second request start, with the first one holding the
    // lease and its object already under the shared key.
    let right = publish();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        gate.arrivals(),
        1,
        "the second manifest PUT reached the shared key while the first still held the lease"
    );

    gate.release_first();
    let (first, second) = tokio::time::timeout(Duration::from_secs(10), async {
        (left.await.unwrap(), right.await.unwrap())
    })
    .await
    .expect("both manifest PUTs completed once the lease was released");

    for response in [first, second] {
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(status, 201, "one concurrent manifest PUT failed: {body}");
        let lower = body.to_ascii_lowercase();
        assert!(
            !lower.contains("unique constraint") && !lower.contains("duplicate key"),
            "a database constraint escaped through the OCI response: {body}"
        );
    }

    let forgekeep_repo =
        rg_core::repo::service::find_repo_by_owner_name(&db, "manifest_race", "same-manifest")
            .await
            .unwrap()
            .unwrap();
    let oci_repo = rg_db::ops::oci_ops::find_repo_by_id(&db, forgekeep_repo.id)
        .await
        .unwrap()
        .unwrap();
    let manifest_rows = rg_db::entities::oci_manifest::Entity::find()
        .filter(rg_db::entities::oci_manifest::Column::OciRepositoryId.eq(oci_repo.id))
        .filter(rg_db::entities::oci_manifest::Column::Digest.eq(&manifest_digest))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(manifest_rows, 1);
    let blobs = rg_db::entities::oci_blob::Entity::find()
        .filter(rg_db::entities::oci_blob::Column::OciRepositoryId.eq(oci_repo.id))
        .filter(rg_db::entities::oci_blob::Column::Digest.eq(&config_digest))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(
        blobs, 1,
        "the two concurrent requests left two rows for one layer"
    );
}

/// A manifest row failure rolls back only an object this request published.
/// Existing content-addressed bytes belong to the earlier successful push and
/// remain readable even when a later tag INSERT genuinely fails.
#[tokio::test]
async fn a_manifest_row_failure_rolls_back_only_its_own_object() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(
        &app.base,
        "manifest_rollback",
        "manifest_rollback@example.com",
    )
    .await;
    create_repo(&app.base, &token, "rollback-manifest").await;

    let config = b"{\"architecture\":\"amd64\",\"os\":\"linux\"}";
    let config_digest = push_blob(
        &app.base,
        &token,
        "manifest_rollback",
        "rollback-manifest",
        config,
    )
    .await;
    let manifest = manifest_json(&config_digest, config.len());
    let manifest_digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(manifest.as_bytes()))
    );
    let digest_url = format!(
        "{}/v2/manifest_rollback/rollback-manifest/manifests/{manifest_digest}",
        app.base
    );
    let stored = oci_manifest_path(
        &app.repo_root,
        "manifest_rollback",
        "rollback-manifest",
        &manifest_digest,
    );
    let put = |url: &str| {
        client
            .put(url)
            .bearer_auth(&token)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/vnd.docker.distribution.manifest.v2+json",
            )
            .body(manifest.clone())
            .send()
    };

    let fault = fail_db_writes(&app.db, "oci_manifest", DbWrite::Insert).await;
    let failed = put(&digest_url).await.unwrap();
    assert_eq!(
        failed.status(),
        500,
        "a real manifest INSERT failure must reach the client"
    );
    fault.clear().await;
    assert!(
        !stored.exists(),
        "the failed first publish left an unowned manifest at {}",
        stored.display()
    );

    let healthy = put(&digest_url).await.unwrap();
    assert_eq!(
        healthy.status(),
        201,
        "the healthy manifest PUT must succeed"
    );
    assert!(
        stored.exists(),
        "the successful manifest PUT stored no object"
    );

    let fault = fail_db_writes(&app.db, "oci_manifest", DbWrite::Insert).await;
    let tag_url = format!(
        "{}/v2/manifest_rollback/rollback-manifest/manifests/another-tag",
        app.base
    );
    let failed_retry = put(&tag_url).await.unwrap();
    assert_eq!(
        failed_retry.status(),
        500,
        "the injected tag INSERT failure must not be mistaken for a retry"
    );
    fault.clear().await;

    assert!(
        stored.exists(),
        "the failed tag PUT deleted a manifest an earlier push recorded"
    );
    let pulled = client
        .get(&digest_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pulled.status(), 200);
    assert_eq!(pulled.text().await.unwrap(), manifest);
}

/// A failed tag lookup is a server failure, not evidence that the tag is free.
///
/// The view fails only reads of `tag`; its INSERT trigger leaves a durable probe
/// row if the handler ever reaches the old "lookup Err -> INSERT" fallback.
/// This keeps the failure at the actual branch instead of merely breaking the
/// table, where both the lookup and a later INSERT would naturally fail.
#[tokio::test]
async fn a_failed_manifest_tag_lookup_never_falls_through_to_insert() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "tag_lookup", "tag_lookup@example.com").await;
    create_repo(&base, &token, "lookup-failure").await;

    let config = b"{\"architecture\":\"amd64\",\"os\":\"linux\"}";
    let config_digest = push_blob(&base, &token, "tag_lookup", "lookup-failure", config).await;
    let manifest = manifest_json(&config_digest, config.len());

    // SQLite has no SELECT trigger. Swap only the table name the SeaORM entity
    // uses for a view whose `tag` expression errors, then use an INSTEAD OF
    // INSERT trigger as the proof that no write followed that failed lookup.
    db.execute_unprepared("ALTER TABLE oci_manifest RENAME TO oci_manifest_backing")
        .await
        .unwrap();
    db.execute_unprepared(
        "CREATE VIEW oci_manifest AS \
         SELECT id, oci_repository_id, digest, json_extract('not JSON', '$') AS tag, \
                media_type, size, manifest_json, schema_version, push_by, created_at, updated_at \
         FROM oci_manifest_backing",
    )
    .await
    .unwrap();
    db.execute_unprepared("CREATE TABLE manifest_tag_lookup_probe (hit INTEGER NOT NULL)")
        .await
        .unwrap();
    db.execute_unprepared(
        "CREATE TRIGGER manifest_tag_lookup_probe_insert \
         INSTEAD OF INSERT ON oci_manifest \
         BEGIN \
           INSERT INTO manifest_tag_lookup_probe (hit) VALUES (1); \
           SELECT RAISE(FAIL, 'unexpected insert after failed tag lookup'); \
         END",
    )
    .await
    .unwrap();

    let failed = client
        .put(format!(
            "{base}/v2/tag_lookup/lookup-failure/manifests/latest"
        ))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/vnd.docker.distribution.manifest.v2+json",
        )
        .body(manifest)
        .send()
        .await
        .unwrap();
    assert_eq!(
        failed.status(),
        500,
        "a failed tag lookup must not be reported as a successful manifest PUT"
    );
    let body = failed.text().await.unwrap();
    assert!(
        !body.contains("not JSON") && !body.contains("unexpected insert after failed tag lookup"),
        "database internals escaped through the OCI response: {body}"
    );

    let probe = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS count FROM manifest_tag_lookup_probe".to_owned(),
        ))
        .await
        .unwrap()
        .expect("probe query must return its aggregate row")
        .try_get::<i64>("", "count")
        .unwrap();
    assert_eq!(
        probe, 0,
        "a failed tag lookup reached INSERT instead of stopping before the write"
    );
}

/// Moving a tag is all-or-nothing: an UPDATE failure must retain the old row.
#[tokio::test]
async fn a_failed_manifest_tag_move_keeps_the_old_tag_live() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "tag_move", "tag_move@example.com").await;
    create_repo(&base, &token, "atomic-move").await;

    let old_config = b"{\"architecture\":\"amd64\",\"os\":\"linux\"}";
    let old_config_digest = push_blob(&base, &token, "tag_move", "atomic-move", old_config).await;
    let old_manifest = manifest_json(&old_config_digest, old_config.len());
    let new_config = b"{\"architecture\":\"arm64\",\"os\":\"linux\"}";
    let new_config_digest = push_blob(&base, &token, "tag_move", "atomic-move", new_config).await;
    let new_manifest = manifest_json(&new_config_digest, new_config.len());
    let tag_url = format!("{base}/v2/tag_move/atomic-move/manifests/latest");
    let put_tag = |body: String| {
        client
            .put(&tag_url)
            .bearer_auth(&token)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/vnd.docker.distribution.manifest.v2+json",
            )
            .body(body)
            .send()
    };

    let initial = put_tag(old_manifest.clone()).await.unwrap();
    assert_eq!(
        initial.status(),
        201,
        "baseline tag push failed: {}",
        initial.text().await.unwrap()
    );

    let fault = fail_db_writes(&db, "oci_manifest", DbWrite::Update).await;
    let failed = put_tag(new_manifest.clone()).await.unwrap();
    assert_eq!(
        failed.status(),
        500,
        "a failed tag replacement must be visible to the client"
    );
    let failed_body = failed.text().await.unwrap();
    assert!(
        !failed_body.contains("injected failure"),
        "the OCI response leaked backend-specific database text: {failed_body}"
    );
    fault.clear().await;

    let forgekeep_repo =
        rg_core::repo::service::find_repo_by_owner_name(&db, "tag_move", "atomic-move")
            .await
            .unwrap()
            .unwrap();
    let oci_repo = rg_db::ops::oci_ops::find_repo_by_id(&db, forgekeep_repo.id)
        .await
        .unwrap()
        .unwrap();
    let retained = rg_db::ops::oci_ops::find_manifest_by_tag(&db, oci_repo.id, "latest")
        .await
        .unwrap()
        .expect("the failed move must retain the old tag row");
    assert_eq!(retained.manifest_json, old_manifest);

    let pulled = client
        .get(&tag_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pulled.status(), 200);
    assert_eq!(pulled.text().await.unwrap(), old_manifest);

    let healthy = put_tag(new_manifest.clone()).await.unwrap();
    assert_eq!(
        healthy.status(),
        201,
        "the tag must still be movable after the injected failure: {}",
        healthy.text().await.unwrap()
    );
    let moved = rg_db::ops::oci_ops::find_manifest_by_tag(&db, oci_repo.id, "latest")
        .await
        .unwrap()
        .expect("the healthy move must retain a tag row");
    assert_eq!(moved.manifest_json, new_manifest);
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

    // A second push of the identical layer takes the storage dedup branch. The
    // row write then fails for a real database reason, not because the row is a
    // harmless duplicate; that conflict is now an idempotent success.
    let location = start_upload(&app.base, &token, "oci_dedup", "shared-layer", payload).await;
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
        "a real INSERT failure must not be mistaken for an idempotent duplicate"
    );
    fault.clear().await;

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

/// A manifest naming a blob this repository does not have must fail the push.
///
/// A manifest is only as good as its parts: accept one whose layer row is
/// missing and the registry hands out an image that 404s on a pull, long after
/// the client that could still have retried the push has gone. The claim runs
/// inside the manifest transaction precisely so the failure lands on the push.
///
/// Both reference forms are driven, and that is the point rather than
/// thoroughness for its own sake: the check used to live in the tagged writer
/// only, so a digest-addressed push of the very same broken manifest was
/// accepted (card_e9b4da7bf8ca). One claim now serves both writers.
#[tokio::test]
async fn a_manifest_naming_an_absent_blob_fails_the_push() {
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

    // Re-pushing the tag takes the transactional replacement path and claims
    // its blob references again — against a row that is now gone. Removing the
    // row is the honest way to state the fault: it is the state a future blob
    // collector would produce, and unlike a write trigger it does not depend on
    // the claim happening to be a write.
    rg_db::entities::oci_blob::Entity::delete_many()
        .filter(rg_db::entities::oci_blob::Column::Digest.eq(&digest))
        .exec(&db)
        .await
        .expect("remove the blob row the manifest names");

    let broken = client
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
    let status = broken.status();
    let body: Value = broken.json().await.unwrap();
    assert_eq!(
        status, 500,
        "a manifest naming a blob this repository does not have must fail the push: {body}"
    );
    let message = body["errors"][0]["message"].as_str().unwrap_or_default();
    assert_eq!(
        message, "failed to record manifest",
        "the OCI response must be useful without disclosing backend internals: {message}"
    );

    // The digest-addressed writer is the other half, and it is driven directly
    // rather than over HTTP on purpose. `put_manifest` refuses an unknown layer
    // at the edge with `MANIFEST_BLOB_UNKNOWN`, so no request can reach this
    // writer with a missing row — except by losing the race between that check
    // and the transaction, which is precisely the state the tagged half above
    // simulates and which this writer used to commit without noticing.
    let forgekeep_repo =
        rg_core::repo::service::find_repo_by_owner_name(&db, "fault_ref", "lost-ref")
            .await
            .unwrap()
            .unwrap();
    let oci_repo = rg_db::ops::oci_ops::find_repo_by_id(&db, forgekeep_repo.id)
        .await
        .unwrap()
        .unwrap();
    let claimed = rg_db::ops::oci_ops::insert_digest_manifest(
        &db,
        oci_repo.id,
        "sha256:0000000000000000000000000000000000000000000000000000000000000001",
        "application/vnd.docker.distribution.manifest.v2+json",
        manifest.len() as i64,
        &manifest,
        2,
        None,
        std::slice::from_ref(&digest),
    )
    .await;
    assert!(
        claimed.is_err(),
        "the digest-addressed writer recorded a manifest whose layer row is gone: {claimed:?}"
    );
    assert!(
        rg_db::ops::oci_ops::find_manifest_by_digest(
            &db,
            oci_repo.id,
            "sha256:0000000000000000000000000000000000000000000000000000000000000001",
        )
        .await
        .unwrap()
        .is_none(),
        "the refused manifest still left its row behind — the claim ran outside the transaction"
    );
}

// ── Package registry ─────────────────────────────────────────

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

    let healthy = rg_core::package_registry::PackageStorage::from_backend(root);
    assert!(
        !healthy
            .has_files("pkg_part", "half-published", "maven", "widget", "1.0.0")
            .await
            .expect("the healthy store must be able to answer"),
        "the file stored before the failure outlived the publish that would have claimed it"
    );

    // Control: the same publish, with the store healthy, does write that file —
    // so the assertion above is about the rollback and not about a path that is
    // never written in the first place.
    rg_core::package_registry::service::publish(&app.db, &healthy, info())
        .await
        .expect("the publish must succeed once the blob store accepts every file");
    assert!(
        healthy
            .has_files("pkg_part", "half-published", "maven", "widget", "1.0.0")
            .await
            .expect("the healthy store must be able to answer"),
        "a successful publish must leave its files in storage"
    );
}

/// Two retries can both observe an absent version, but only the request that
/// wins the UNIQUE claim owns a published version. The loser must neither
/// overwrite nor delete the winner's bytes while compensating its own work.
#[tokio::test]
async fn concurrent_new_version_publish_keeps_the_winners_file() {
    let (base, _db, gate) = spawn_test_app_with_two_put_gate("race.bin").await;
    let (token, _) = register_full(&base, "pkg_race", "pkg_race@example.com").await;
    create_repo(&base, &token, "racing-publishes").await;
    let client = reqwest::Client::new();

    // Seed only the package/registry rows. Both requests below still create a
    // brand-new version, so their version-existence reads are the race under
    // test rather than the unrelated first-package get-or-create path.
    let seeded = client
        .post(format!(
            "{base}/api/v1/repos/pkg_race/racing-publishes/packages/generic/publish?name=widget&version=0.9.0"
        ))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"seed.bin\"",
        )
        .body("seed")
        .send()
        .await
        .unwrap();
    assert_eq!(seeded.status(), reqwest::StatusCode::CREATED);

    let publish = |body: &'static str| {
        let client = client.clone();
        let token = token.clone();
        let base = base.clone();
        tokio::spawn(async move {
            let response = client
                .post(format!(
                    "{base}/api/v1/repos/pkg_race/racing-publishes/packages/generic/publish?name=widget&version=1.0.0"
                ))
                .bearer_auth(token)
                .header(
                    reqwest::header::CONTENT_DISPOSITION,
                    "attachment; filename=\"race.bin\"",
                )
                .body(body)
                .send()
                .await
                .unwrap();
            (response, body.as_bytes().to_vec())
        })
    };

    let mut left = publish("left request bytes");
    let mut right = publish("right request bytes");
    let ((winner, winner_bytes), (loser, _loser_bytes)) =
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                left_result = &mut left => {
                    let left_result = left_result.unwrap();
                    gate.release_second();
                    let right_result = right.await.unwrap();
                    (left_result, right_result)
                }
                right_result = &mut right => {
                    let right_result = right_result.unwrap();
                    gate.release_second();
                    let left_result = left.await.unwrap();
                    (right_result, left_result)
                }
            }
        })
        .await
        .expect("both publishes reached the gated storage write and completed");

    assert_eq!(winner.status(), reqwest::StatusCode::CREATED);
    assert_eq!(loser.status(), reqwest::StatusCode::CONFLICT);

    let downloaded = client
        .get(format!(
            "{base}/api/v1/repos/pkg_race/racing-publishes/packages/generic/widget/1.0.0/race.bin"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), reqwest::StatusCode::OK);
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), winner_bytes);
}

/// Two additions to an already-visible version can both pass the filename
/// precheck. The package-file UNIQUE claim must leave exactly one row and one
/// size increment, and compensation must not touch the winner's private blob.
#[tokio::test]
async fn concurrent_existing_version_publish_keeps_one_filename_and_one_size_increment() {
    let (base, db, gate) = spawn_test_app_with_two_put_gate("same.bin").await;
    let (token, _) = register_full(&base, "pkg_file_race", "pkg_file_race@example.com").await;
    create_repo(&base, &token, "racing-files").await;
    let client = reqwest::Client::new();
    let publish_url = format!(
        "{base}/api/v1/repos/pkg_file_race/racing-files/packages/generic/publish?name=widget&version=1.0.0"
    );

    let seed = b"seed file";
    let seeded = client
        .post(&publish_url)
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"seed.bin\"",
        )
        .body(seed.as_slice())
        .send()
        .await
        .unwrap();
    assert_eq!(seeded.status(), reqwest::StatusCode::CREATED);

    let publish = |body: &'static str| {
        let client = client.clone();
        let token = token.clone();
        let publish_url = publish_url.clone();
        tokio::spawn(async move {
            let response = client
                .post(publish_url)
                .bearer_auth(token)
                .header(
                    reqwest::header::CONTENT_DISPOSITION,
                    "attachment; filename=\"same.bin\"",
                )
                .body(body)
                .send()
                .await
                .unwrap();
            (response, body.as_bytes().to_vec())
        })
    };

    let mut left = publish("left existing-version bytes");
    let mut right = publish("right existing-version bytes");
    let ((winner, winner_bytes), (loser, _)) =
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                left_result = &mut left => {
                    let left_result = left_result.unwrap();
                    gate.release_second();
                    (left_result, right.await.unwrap())
                }
                right_result = &mut right => {
                    let right_result = right_result.unwrap();
                    gate.release_second();
                    (right_result, left.await.unwrap())
                }
            }
        })
        .await
        .expect("both additions reached the gated storage write and completed");

    assert_eq!(winner.status(), reqwest::StatusCode::OK);
    assert_eq!(loser.status(), reqwest::StatusCode::CONFLICT);

    let repo =
        rg_core::repo::service::find_repo_by_owner_name(&db, "pkg_file_race", "racing-files")
            .await
            .unwrap()
            .unwrap();
    let registry = rg_db::ops::package_registry_ops::find_by_repo_and_type(&db, repo.id, "generic")
        .await
        .unwrap()
        .unwrap();
    let package = rg_db::ops::package_ops::find_by_registry_and_name(&db, registry.id, "widget")
        .await
        .unwrap()
        .unwrap();
    let version =
        rg_db::ops::package_version_ops::find_by_package_and_version(&db, package.id, "1.0.0")
            .await
            .unwrap()
            .unwrap();
    let files = rg_db::ops::package_file_ops::list_by_version(&db, version.id)
        .await
        .unwrap();
    assert_eq!(files.len(), 2, "seed plus one winning same.bin row");
    assert_eq!(
        files
            .iter()
            .filter(|file| file.filename == "same.bin")
            .count(),
        1,
        "the losing publish left a duplicate package_file row"
    );
    assert_eq!(
        version.size,
        (seed.len() + winner_bytes.len()) as i64,
        "the losing publish changed the version size"
    );

    let downloaded = client
        .get(format!(
            "{base}/api/v1/repos/pkg_file_race/racing-files/packages/generic/widget/1.0.0/same.bin"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), reqwest::StatusCode::OK);
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), winner_bytes);
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

/// A retry does not own the blob an earlier successful upload published.
///
/// The object route accepts a repeated `PUT` (a signed LFS action URL can be
/// retried for six hours). If the metadata update then fails, its compensation
/// must not delete the stable content-addressed key that the first request
/// already made live.
#[tokio::test]
async fn a_failed_lfs_retry_keeps_the_object_an_earlier_upload_published() {
    let app = crate::common::fault::spawn_test_app_for_fault_sweep().await;
    let client = reqwest::Client::new();
    let (token, _user_id) =
        register_full(&app.base, "lfs_retry_owner", "lfs_retry_owner@example.com").await;
    create_repo(&app.base, &token, "retry-lfs").await;

    let payload = b"forgekeep-lfs-retry-keeps-live-bytes";
    let (first_status, oid) =
        upload_lfs_object(&app.base, &token, "lfs_retry_owner", "retry-lfs", payload).await;
    assert_eq!(first_status, 200, "baseline upload must publish the object");

    let blob = lfs_blob_path(&app.repo_root, "lfs_retry_owner", "retry-lfs", &oid);
    let bytes_before_retry = std::fs::read(&blob).unwrap();

    let fault = fail_db_writes(&app.db, "lfs_objects", DbWrite::Update).await;
    let (retry_status, retry_oid) =
        upload_lfs_object(&app.base, &token, "lfs_retry_owner", "retry-lfs", payload).await;
    assert_eq!(retry_oid, oid);
    assert_eq!(retry_status, 500, "the injected metadata failure must land");
    fault.clear().await;

    assert_eq!(
        std::fs::read(&blob).unwrap(),
        bytes_before_retry,
        "the failed retry changed or deleted bytes owned by the first upload"
    );

    let downloaded = client
        .get(format!(
            "{}/api/v1/repos/lfs_retry_owner/retry-lfs/lfs/objects/{oid}",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        downloaded.status(),
        200,
        "the first upload must remain downloadable after the failed retry"
    );
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), payload);
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
    let (runner, runner_token) =
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
        .bearer_auth(&runner_token)
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
        .bearer_auth(&runner_token)
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
