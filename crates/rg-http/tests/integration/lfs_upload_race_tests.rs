//! Two uploads of one LFS object at the same time (card_f71734f9274b).
//!
//! The digest an upload is checked against is computed over that request's own
//! body stream, while what gets compressed and published is a file on disk.
//! The file used to be `.tmp_<oid>`, shared by every upload of the object and
//! opened with truncation, so a second upload arriving mid-way through the
//! first truncated its file — and, when the second one failed its digest
//! check, deleted it. The first upload's digest still matched, and it then
//! published whatever the shared file held, or found no file and failed.

use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

const OWNER: &str = "lfs_race_owner";
const REPO: &str = "assets";

fn oid_of(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

/// The signed upload URL the batch API gives for `payload`.
async fn upload_href(base: &str, token: &str, payload: &[u8]) -> String {
    let batch = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/lfs/objects/batch"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": oid_of(payload), "size": payload.len()}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch.status(), 200);
    let batch = batch.json::<serde_json::Value>().await.unwrap();
    batch["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .expect("upload href")
        .to_string()
}

/// Every upload spool in the repository's LFS directory, with its size.
fn spools(lfs_dir: &std::path::Path) -> Vec<(String, u64)> {
    let Ok(entries) = std::fs::read_dir(lfs_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with(".tmp_")
                .then(|| (name, entry.metadata().map(|meta| meta.len()).unwrap_or(0)))
        })
        .collect()
}

#[tokio::test]
async fn a_failing_concurrent_upload_cannot_corrupt_or_delete_the_good_one() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, OWNER, "lfs_race_owner@example.com").await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({"name": REPO, "is_private": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);

    let good: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();
    let bad: Vec<u8> = vec![0x5a; good.len()];
    let href = upload_href(&base, &owner, &good).await;
    let url = reqwest::Url::parse(&href).unwrap();
    let path_and_query = format!("{}?{}", url.path(), url.query().unwrap());
    let lfs_dir = repo_root.join(format!("{OWNER}.lfs")).join(REPO);

    // A: the good upload, sent by hand so it can stop half-way.
    let address = base.trim_start_matches("http://").to_string();
    let mut first = tokio::net::TcpStream::connect(&address).await.unwrap();
    first
        .write_all(
            format!(
                "PUT {path_and_query} HTTP/1.1\r\nHost: {address}\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                good.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let (head, tail) = good.split_at(good.len() / 2);
    first.write_all(head).await.unwrap();
    first.flush().await.unwrap();

    // Until A's first half is on disk.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !spools(&lfs_dir).iter().any(|(_, size)| *size > 0) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first upload never reached its spool"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // B: a complete upload of the same object with the wrong content.
    let refused = reqwest::Client::new()
        .put(&href)
        .body(bad.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        400,
        "a body that does not hash to the oid is refused"
    );

    // A finishes.
    first.write_all(tail).await.unwrap();
    first.flush().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(60), first.read_to_end(&mut response))
        .await
        .expect("the first upload never answered")
        .unwrap();
    let response = String::from_utf8_lossy(&response);
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "the good upload must succeed whatever a concurrent bad one did: {response}"
    );

    // What is stored under the oid is the good upload, byte for byte.
    let batch = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/lfs/objects/batch"
        ))
        .bearer_auth(&owner)
        .json(&serde_json::json!({
            "operation": "download",
            "objects": [{"oid": oid_of(&good), "size": good.len()}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let download = batch["objects"][0]["actions"]["download"]["href"]
        .as_str()
        .expect("the object is downloadable");
    let stored = reqwest::Client::new()
        .get(download)
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(stored.status(), 200);
    assert!(
        stored.bytes().await.unwrap().as_ref() == good.as_slice(),
        "the stored object is not the upload whose digest was checked"
    );
    assert!(
        spools(&lfs_dir).is_empty(),
        "both uploads retire their own spools: {:?}",
        spools(&lfs_dir)
    );
}
