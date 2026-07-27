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
/// The field names are the ones this registry's parser reads, which are *not*
/// the camelCase names a real client sends: `ParsedManifest::parse` answers
/// `400 missing field media_type` to every real `docker push` (card_83a755704a2c).
/// This fixture follows the parser so the test can reach the code it is
/// actually about; whoever fixes the parser will see it fail here and swap it
/// for a real manifest.
fn manifest_json(digest: &str, size: usize) -> String {
    serde_json::json!({
        "schema_version": 2,
        "media_type": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {
            "media_type": "application/vnd.docker.container.image.v1+json",
            "size": size,
            "digest": digest,
        },
        "layers": [],
        "manifests": [],
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
