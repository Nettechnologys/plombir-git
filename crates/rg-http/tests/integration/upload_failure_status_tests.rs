//! Regression coverage for card_eafc0c814d1e and card_0e926677f021: a failure of
//! the *server's* blob storage must never be reported as the client's mistake.
//!
//! The two cards are the same defect pointed in opposite directions. On the
//! write side (`POST .../releases/{id}/assets`, attachment upload) every failure
//! of the service — including an unwritable `repo_root` — was flattened into
//! `AppError::bad_request`, so a broken volume answered `400` and no client ever
//! retried. On the read side (`GET /v2/{o}/{r}/blobs/{digest}`, cross-repo blob
//! mount) the error was discarded entirely and answered `404 BLOB_UNKNOWN`, so
//! `docker pull` reported a perfectly good image as referencing a layer that
//! does not exist.
//!
//! Each test drives both halves of the split against the same request shape:
//! the client-caused failure must still be a `4xx`, and the storage-caused one
//! must be a `5xx`.

use reqwest::multipart::{Form, Part};

use crate::common::{create_issue, create_repo, register_full, spawn_test_app_with_oci_root};

async fn create_release(base: &str, token: &str, owner: &str, repo: &str) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/releases"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "tag_name": "v1.0.0", "title": "release" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create release failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// Uploading a release asset fails for a client reason and for a server reason,
/// and the status has to tell them apart.
#[tokio::test]
async fn release_asset_upload_separates_a_bad_request_from_a_broken_blob_store() {
    let (base, repo_root, _oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "asset_blame", "asset_blame@example.com").await;
    create_repo(&base, &token, "blamed-release").await;
    let release_id = create_release(&base, &token, "asset_blame", "blamed-release").await;
    let assets_url =
        format!("{base}/api/v1/repos/asset_blame/blamed-release/releases/{release_id}/assets");

    // 1. No filename in either accepted header — the request itself is unusable.
    let nameless = client
        .post(&assets_url)
        .bearer_auth(&token)
        .body("payload".as_bytes())
        .send()
        .await
        .unwrap();
    assert_eq!(
        nameless.status(),
        400,
        "a missing asset filename is the client's fault"
    );

    // 2. Same request, complete this time — but the blob store cannot host the
    //    asset. `LocalBlobStorage` writes release assets under
    //    `<repo_root>/releases/...`; a plain file there makes every parent
    //    `create_dir_all` fail the way a bind-mount owned by another uid does.
    let occupied = repo_root.join("releases");
    std::fs::write(&occupied, b"not a directory").unwrap();

    let ours = client
        .post(&assets_url)
        .bearer_auth(&token)
        .header("x-asset-filename", "payload.bin")
        .body("payload".as_bytes())
        .send()
        .await
        .unwrap();
    assert_eq!(
        ours.status(),
        500,
        "a blob store that cannot take the asset is the server's fault, not a bad request"
    );

    // The metadata row is inserted before the blob is written, so a failed
    // publish has to roll it back — otherwise the listing grows an asset whose
    // bytes were never stored.
    std::fs::remove_file(&occupied).unwrap();
    let listed: serde_json::Value = client
        .get(&assets_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        listed.as_array().map(Vec::len),
        Some(0),
        "a failed asset upload must not leave a metadata row behind: {listed}"
    );
}

/// The attachment upload makes the same split: the staging half of the handler
/// already reported its filesystem failures as 500, while the publish half
/// reported everything — including the blob store — as 400.
#[tokio::test]
async fn attachment_upload_separates_a_rejected_file_from_a_broken_blob_store() {
    let (base, repo_root, _oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "attach_blame", "attach_blame@example.com").await;
    create_repo(&base, &token, "blamed-issues").await;
    let (_issue_id, number) = create_issue(
        &base,
        &token,
        "attach_blame",
        "blamed-issues",
        "attachment target",
    )
    .await;
    let url = format!("{base}/api/v1/repos/attach_blame/blamed-issues/issues/{number}/assets");

    // 1. A file type the instance does not accept — the client's to fix.
    let rejected = client
        .post(&url)
        .bearer_auth(&token)
        .multipart(Form::new().part(
            "attachment",
            Part::bytes(b"payload".to_vec()).file_name("payload.exe"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        rejected.status(),
        400,
        "a disallowed attachment type is the client's fault"
    );

    // 2. An accepted file, staged fine, that the blob store then refuses.
    //    Attachments live under `<repo_root>/attachments/...`; staging happens
    //    under `<repo_root>/.tmp/`, so this breaks only the publish.
    let occupied = repo_root.join("attachments");
    std::fs::write(&occupied, b"not a directory").unwrap();

    let ours = client
        .post(&url)
        .bearer_auth(&token)
        .multipart(Form::new().part(
            "attachment",
            Part::bytes(b"payload".to_vec()).file_name("report.txt"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        ours.status(),
        500,
        "a blob store that cannot take the attachment is the server's fault, not a bad request"
    );
}

/// Pulling a layer: "the registry does not have this blob" and "the registry
/// cannot reach its blob store" are different answers to `docker pull`.
#[tokio::test]
async fn oci_blob_read_separates_a_missing_blob_from_an_unreachable_store() {
    let (base, repo_root, _oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "pull_blame", "pull_blame@example.com").await;
    create_repo(&base, &token, "blamed-image").await;
    let digest = format!("sha256:{}", "ab".repeat(32));
    let url = format!("{base}/v2/pull_blame/blamed-image/blobs/{digest}");

    // 1. Nothing was ever pushed under that digest — genuinely unknown.
    let missing = client.get(&url).bearer_auth(&token).send().await.unwrap();
    assert_eq!(
        missing.status(),
        404,
        "an absent blob is still BLOB_UNKNOWN"
    );
    let body: serde_json::Value = missing.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "BLOB_UNKNOWN");

    // 2. Same request, but the tree the blobs live in is not a directory. The
    //    blob is no more absent than before — the store is what cannot answer.
    let occupied = repo_root.join("oci");
    std::fs::write(&occupied, b"not a directory").unwrap();

    let broken = client.get(&url).bearer_auth(&token).send().await.unwrap();
    assert_eq!(
        broken.status(),
        500,
        "an unreachable blob store must not be reported as a missing layer"
    );
    let body: serde_json::Value = broken.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "UNKNOWN");
    let message = body["errors"][0]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&occupied.display().to_string()),
        "the 500 must name the path it could not read: {message}"
    );
}

/// The same split on the cross-repo mount, where the existence check used to be
/// `unwrap_or(false)` — an error and an honest "no" collapsed into one answer.
#[tokio::test]
async fn blob_mount_separates_a_missing_source_from_an_unreachable_store() {
    let (base, repo_root, _oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "mount_blame", "mount_blame@example.com").await;
    create_repo(&base, &token, "mount-source").await;
    create_repo(&base, &token, "mount-target").await;
    let digest = format!("sha256:{}", "cd".repeat(32));
    let url = format!(
        "{base}/v2/mount_blame/mount-target/blobs/uploads/\
         ?mount={digest}&from=mount_blame/mount-source"
    );

    // 1. The source repo really does not have that blob.
    let missing = client.post(&url).bearer_auth(&token).send().await.unwrap();
    assert_eq!(
        missing.status(),
        404,
        "a mount source that does not have the blob is still BLOB_UNKNOWN"
    );

    // 2. The store cannot say whether it has it.
    let occupied = repo_root.join("oci");
    std::fs::write(&occupied, b"not a directory").unwrap();

    let broken = client.post(&url).bearer_auth(&token).send().await.unwrap();
    assert_eq!(
        broken.status(),
        500,
        "a blob store that cannot answer the existence check is not a missing source blob"
    );
}

/// A staging file that disappeared under an open session is not a bad digest.
///
/// Finalizing hashes whatever the staging file holds and compares that to the
/// digest the client named. Those are two different claims — "the bytes you
/// sent do not hash to this" and "the bytes you sent are not the bytes I
/// hashed" — and the second one is ours. The registry could not tell them
/// apart: the finalize handler opens the staging file with `create(true)`, so a
/// file that had gone missing came back as an empty one, hashed to the digest
/// of nothing, and the client was told `400 digest invalid` for a layer it had
/// uploaded correctly. `docker push` does not retry a `4xx`.
///
/// The session row is the witness. It records how many bytes each `PATCH`
/// delivered, so a staging file holding anything other than that — nothing at
/// all here, a doubled body elsewhere — is a divergence the client had no part
/// in (card_c03bd9e96a66).
#[tokio::test]
async fn a_finalize_over_a_vanished_staging_file_is_not_the_client_s_digest() {
    let (base, _repo_root, oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "stage_blame", "stage_blame@example.com").await;
    create_repo(&base, &token, "blamed-layer").await;

    let payload = b"plombir-git-staged-layer";
    let digest = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(payload))
    );

    let start = client
        .post(format!("{base}/v2/stage_blame/blamed-layer/blobs/uploads/"))
        .bearer_auth(&token)
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
    let uuid = location.rsplit('/').next().expect("uuid in Location");

    let chunk = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 202, "chunk upload failed");

    // The failure this reproduces, made deterministic: whatever removes the
    // staged bytes in the wild — an operator, a stray cleanup, a volume that
    // came back empty — the session row still says they arrived.
    //
    // The *file* goes and the directory stays, which is the whole point. With
    // the directory gone too, `create(true)` fails on the missing parent and
    // the registry already answered honestly; it is the file alone that
    // `create(true)` silently invents, turning a lost upload into an empty one
    // and an empty one into the client's bad digest.
    let staged = oci_root
        .join("oci-uploads")
        .join("stage_blame")
        .join("blamed-layer")
        .join(uuid)
        .join("data");
    assert!(
        staged.is_file(),
        "the fixture did not find the staging file at {}",
        staged.display()
    );
    std::fs::remove_file(&staged).expect("remove the staged bytes");

    let finish = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let status = finish.status();
    let body = finish.text().await.unwrap_or_default();
    assert_eq!(
        status, 500,
        "a staging file the registry lost is the registry's failure, not a bad digest: {body}"
    );
    assert!(
        body.contains(&payload.len().to_string()),
        "the failure must name what the session recorded, or the next occurrence is \
         unreadable again: {body}"
    );
}

/// The client's own bad digest still answers `400`, and now says how much it
/// hashed.
///
/// The other half of the split above: strengthening the server-side branch is
/// only worth anything if the client-side one still fires. A push whose bytes
/// really do not match the digest it named is the one case where `400 digest
/// invalid` is the honest answer — and the staged byte count in the message is
/// what lets an operator tell a corrupt layer from a truncated one without
/// reproducing anything.
#[tokio::test]
async fn a_digest_the_client_got_wrong_is_still_the_client_s() {
    let (base, _repo_root, _oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _user_id) = register_full(&base, "wrong_digest", "wrong_digest@example.com").await;
    create_repo(&base, &token, "mismatched-layer").await;

    let payload = b"plombir-git-honest-layer";
    // A well-formed digest of something else entirely.
    let claimed = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"not what was sent"))
    );

    let start = client
        .post(format!(
            "{base}/v2/wrong_digest/mismatched-layer/blobs/uploads/"
        ))
        .bearer_auth(&token)
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

    let chunk = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 202, "chunk upload failed");

    let finish = client
        .put(format!("{base}{location}"))
        .query(&[("digest", claimed.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let status = finish.status();
    let body = finish.text().await.unwrap_or_default();
    assert_eq!(
        status, 400,
        "a digest that does not match the bytes sent is the client's: {body}"
    );
    assert!(
        body.contains("DIGEST_INVALID"),
        "the client's own bad digest keeps its OCI code: {body}"
    );
    assert!(
        body.contains(&format!("{} staged byte", payload.len())),
        "the mismatch must say how many bytes it hashed: {body}"
    );
}
