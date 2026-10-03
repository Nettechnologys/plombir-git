//! Regression coverage for card_a02122e1b29b: `GET .../lfs/objects/{oid}` must
//! not report a blob store it cannot reach as an object the repository does not
//! have.
//!
//! The handler used to answer `404` to every `Err` out of
//! `rg_core::lfs::service::read_object_source`, with the error text as the
//! body. That function fails for two unrelated reasons — the storage backend
//! did not answer, and the object is genuinely absent — so an unreadable LFS
//! root told `git lfs pull` the objects had been deleted. A `404` is also the
//! one verdict a client never retries, and (unlike a `5xx`, which
//! `IntoResponse` sanitizes) its body is passed through verbatim, so the
//! storage path went out with it.
//!
//! This is the read side of the same split `upload_failure_status_tests` drives
//! for releases, attachments and OCI blobs. The requests below are the same
//! request shape over and over: the object is there, the object is not there,
//! and the server cannot tell — and only the middle one may be a `404`.
//!
//! "Cannot tell" is driven twice, because the lookup has two storeys and each
//! could collapse on its own: the blob backend, and the legacy on-disk layout
//! it falls back to. The fallback used to ask `Path::exists`, which answers
//! `false` to an unreadable directory just as it does to an absent file.

use sha2::{Digest, Sha256};

use crate::common::{create_repo, register_full, spawn_test_app_with_db_and_repo_root};

const OWNER: &str = "lfs_blame_owner";
const REPO: &str = "blamed-lfs";

/// `count` payloads whose object ids land in *different* shard directories.
///
/// Each broken-storage request works by replacing one shard directory with a
/// plain file, which is only a fault injection if no other object of the test
/// lives in that shard. sha256 is deterministic, so this picks the same
/// payloads on every run.
fn payloads_in_distinct_shards(count: usize) -> Vec<Vec<u8>> {
    let mut chosen: Vec<Vec<u8>> = Vec::new();
    let mut shards: Vec<String> = Vec::new();
    for n in 0..10_000u32 {
        let payload = format!("plombir-git lfs blame payload {n}").into_bytes();
        let oid = hex::encode(Sha256::digest(&payload));
        let shard = oid[..2].to_string();
        if shards.contains(&shard) {
            continue;
        }
        shards.push(shard);
        chosen.push(payload);
        if chosen.len() == count {
            return chosen;
        }
    }
    panic!("no {count} distinct LFS shards among the candidate payloads");
}

fn oid_of(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

/// Store an object the way `git lfs push` does: announce it through the batch
/// API, then `PUT` the bytes to the signed href it hands back.
async fn upload_object(base: &str, token: &str, payload: &[u8]) -> String {
    let oid = oid_of(payload);
    let client = reqwest::Client::new();
    let batch = client
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/lfs/objects/batch"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": oid, "size": payload.len()}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch.status(), 200, "LFS upload batch failed");
    let batch = batch.json::<serde_json::Value>().await.unwrap();
    let href = batch["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .expect("upload href")
        .to_string();

    let stored = client
        .put(href)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(stored.status(), 200, "LFS object upload failed");
    oid
}

async fn download(base: &str, token: &str, oid: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/lfs/objects/{oid}"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn lfs_download_separates_a_missing_object_from_a_storage_it_cannot_read() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, _user_id) = register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    create_repo(&base, &token, REPO).await;

    let payloads = payloads_in_distinct_shards(4);
    let stored_payload = &payloads[0];
    let absent_oid = oid_of(&payloads[1]);
    let unreadable_oid = oid_of(&payloads[2]);
    let unreadable_legacy_oid = oid_of(&payloads[3]);

    // 1. Baseline: an object that is there comes back whole. Without this the
    //    two failures below would prove nothing — a route that never worked
    //    answers everything the same way.
    let stored_oid = upload_object(&base, &token, stored_payload).await;
    let healthy = download(&base, &token, &stored_oid).await;
    assert_eq!(healthy.status(), 200, "a stored LFS object must download");
    assert_eq!(
        healthy.bytes().await.unwrap().as_ref(),
        stored_payload.as_slice(),
        "the object must come back byte for byte"
    );

    // 2. An object this repository genuinely does not have is still a 404 —
    //    the fix must not turn every miss into a 500.
    let absent = download(&base, &token, &absent_oid).await;
    assert_eq!(
        absent.status(),
        404,
        "an object that was never uploaded is absent, and absence is a 404"
    );

    // 3. Same request shape, but the server cannot answer the question. A plain
    //    file where the object's shard directory belongs makes `metadata` fail
    //    with ENOTDIR — the shape a bind-mount owned by another uid has — so
    //    the backend returns an error instead of "not there".
    let shard = repo_root
        .join("lfs")
        .join(OWNER)
        .join(REPO)
        .join(&unreadable_oid[..2]);
    std::fs::create_dir_all(shard.parent().unwrap()).unwrap();
    std::fs::write(&shard, b"not a directory").unwrap();

    let unreadable = download(&base, &token, &unreadable_oid).await;
    assert_eq!(
        unreadable.status(),
        500,
        "a blob store that cannot be read is the server's failure, not a deleted object"
    );

    // The 404 body used to be the raw error text, which for a storage failure
    // carries the absolute path the server assembled from `repo_root`. A 5xx is
    // sanitized, so the path stays in the operator log.
    let body = unreadable.text().await.unwrap();
    assert!(
        !body.contains(&repo_root.display().to_string()),
        "the storage path must not reach the client: {body}"
    );

    // 4. The other storey of the same lookup. This oid is absent from the blob
    //    backend, so the read falls through to the legacy on-disk layout — and
    //    that is where the plain file sits this time. The fallback used to ask
    //    `Path::exists`, which cannot distinguish this from a missing file, so
    //    the whole storey collapsed back into a 404 no matter what the handler
    //    above it did.
    let legacy_shard = repo_root
        .join(format!("{OWNER}.lfs"))
        .join(REPO)
        .join(&unreadable_legacy_oid[..2]);
    std::fs::create_dir_all(legacy_shard.parent().unwrap()).unwrap();
    std::fs::write(&legacy_shard, b"not a directory").unwrap();

    let unreadable_legacy = download(&base, &token, &unreadable_legacy_oid).await;
    assert_eq!(
        unreadable_legacy.status(),
        500,
        "an unreadable legacy LFS root is the server's failure, not a deleted object"
    );

    // 5. The healthy object is still served: the fault injection took away two
    //    shards, not the endpoint.
    let still_healthy = download(&base, &token, &stored_oid).await;
    assert_eq!(
        still_healthy.status(),
        200,
        "breaking two shards must not break the objects in the others"
    );
}
