//! Regression coverage for card_589844957f80: viewing a file stored in Git LFS
//! must show the file, not its pointer.
//!
//! The blob API answered an LFS file with the three lines of pointer text
//! `git lfs` commits in its place, and there was no route that served a file's
//! bytes at all — so the web view printed
//! `version https://git-lfs.github.com/spec/v1` where an image should be. The
//! blob answer now says the file is a pointer and whether this repository has
//! the object, and the raw route serves the object itself; a pointer whose
//! object never arrived is an honest `404`, never the pointer text.

use sha2::{Digest, Sha256};

use crate::common::{register_full, spawn_test_app};

const OWNER: &str = "lfs_view_owner";
const OUTSIDER: &str = "lfs_view_outsider";

fn oid_of(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

fn pointer_text(payload: &[u8]) -> String {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        oid_of(payload),
        payload.len()
    )
}

async fn create_repo(base: &str, token: &str, name: &str, is_private: bool) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "is_private": is_private,
            "auto_init": true,
            "readme": "default",
        }))
        .send()
        .await
        .expect("create repository");
    assert_eq!(response.status(), 201, "creating {name} failed");
}

async fn commit_file(base: &str, token: &str, repo: &str, path: &str, content: &str) {
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{repo}/contents/{path}"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"content": content, "message": format!("add {path}")}))
        .send()
        .await
        .expect("commit file");
    assert_eq!(
        response.status(),
        200,
        "committing {path} failed: {}",
        response.text().await.unwrap_or_default()
    );
}

/// Store an object the way `git lfs push` does: announce it through the batch
/// API, then `PUT` the bytes to the signed href it hands back.
async fn upload_object(base: &str, token: &str, repo: &str, payload: &[u8]) {
    let client = reqwest::Client::new();
    let batch = client
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{repo}/lfs/objects/batch"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": oid_of(payload), "size": payload.len()}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .expect("upload batch");
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
        .expect("upload object");
    assert_eq!(stored.status(), 200, "LFS object upload failed");
}

async fn get(url: &str, token: Option<&str>) -> reqwest::Response {
    let request = reqwest::Client::new().get(url);
    match token {
        Some(token) => request.bearer_auth(token),
        None => request,
    }
    .send()
    .await
    .expect("request")
}

#[tokio::test]
async fn an_lfs_file_is_viewed_as_its_object_and_a_missing_object_is_a_404() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    let repo = "lfs-view";
    create_repo(&base, &token, repo, false).await;

    let image: Vec<u8> = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR stand-in image bytes"
        .iter()
        .copied()
        .chain((0..4096).map(|n| (n % 251) as u8))
        .collect();
    upload_object(&base, &token, repo, &image).await;
    commit_file(&base, &token, repo, "art/hero.png", &pointer_text(&image)).await;
    let never_pushed = b"an object nobody ran git lfs push for".to_vec();
    commit_file(
        &base,
        &token,
        repo,
        "missing.bin",
        &pointer_text(&never_pushed),
    )
    .await;
    commit_file(&base, &token, repo, "notes.txt", "ordinary text\n").await;

    let api = format!("{base}/api/v1/repos/{OWNER}/{repo}");

    // The blob answer names the object and says the repository has it.
    let blob = get(&format!("{api}/blob/art/hero.png"), Some(&token)).await;
    assert_eq!(blob.status(), 200);
    let blob = blob.json::<serde_json::Value>().await.unwrap();
    assert_eq!(blob["lfs"]["oid"], oid_of(&image));
    assert_eq!(blob["lfs"]["size"], image.len());
    assert_eq!(blob["lfs"]["available"], true);

    // The raw route serves the object, not the pointer, as an inline image
    // that cannot run as this origin.
    let raw = get(&format!("{api}/raw/art/hero.png"), Some(&token)).await;
    assert_eq!(raw.status(), 200);
    let headers = raw.headers().clone();
    assert_eq!(headers["content-type"], "image/png");
    assert_eq!(headers["content-disposition"], "inline");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(
        headers
            .get_all("content-security-policy")
            .iter()
            .any(|policy| policy.to_str().unwrap().contains("sandbox")),
        "the raw route's sandbox policy did not survive: {headers:?}"
    );
    assert_eq!(raw.bytes().await.unwrap().to_vec(), image);

    // A pointer whose object never arrived: said so in the blob answer, and a
    // 404 — not the pointer text — from the raw route.
    let blob = get(&format!("{api}/blob/missing.bin"), Some(&token)).await;
    let blob = blob.json::<serde_json::Value>().await.unwrap();
    assert_eq!(blob["lfs"]["oid"], oid_of(&never_pushed));
    assert_eq!(blob["lfs"]["available"], false);
    let raw = get(&format!("{api}/raw/missing.bin"), Some(&token)).await;
    assert_eq!(raw.status(), 404);
    let body = raw.text().await.unwrap();
    assert!(
        !body.contains("git-lfs.github.com/spec"),
        "the 404 carried the pointer text: {body}"
    );

    // An ordinary file: no `lfs` block, and its own bytes as an attachment.
    let blob = get(&format!("{api}/blob/notes.txt"), Some(&token)).await;
    let blob = blob.json::<serde_json::Value>().await.unwrap();
    assert!(
        blob.get("lfs").is_none(),
        "an ordinary file got an lfs block: {blob}"
    );
    let raw = get(&format!("{api}/raw/notes.txt"), Some(&token)).await;
    assert_eq!(raw.status(), 200);
    assert_eq!(raw.headers()["content-type"], "application/octet-stream");
    assert!(raw.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment"));
    assert_eq!(raw.bytes().await.unwrap().as_ref(), b"ordinary text\n");

    // A path that is a directory is not a file, on either route.
    assert_eq!(
        get(&format!("{api}/raw/art"), Some(&token)).await.status(),
        400
    );
}

/// A private repository's LFS file is refused to a stranger exactly as its
/// ordinary files are — the raw route is a read of the repository, not of a
/// storage key.
#[tokio::test]
async fn a_private_lfs_file_is_refused_like_any_other_file_of_its_repository() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    let (outsider, _) = register_full(&base, OUTSIDER, &format!("{OUTSIDER}@example.com")).await;
    let repo = "lfs-private";
    create_repo(&base, &token, repo, true).await;
    let payload = b"private LFS payload".to_vec();
    upload_object(&base, &token, repo, &payload).await;
    commit_file(&base, &token, repo, "secret.bin", &pointer_text(&payload)).await;
    let api = format!("{base}/api/v1/repos/{OWNER}/{repo}");

    let owner_raw = get(&format!("{api}/raw/secret.bin"), Some(&token)).await;
    assert_eq!(owner_raw.status(), 200);
    assert_eq!(owner_raw.bytes().await.unwrap().to_vec(), payload);

    for caller in [Some(outsider.as_str()), None] {
        let ordinary = get(&format!("{api}/blob/README.md"), caller).await.status();
        let raw = get(&format!("{api}/raw/secret.bin"), caller).await;
        assert!(
            ordinary.is_client_error(),
            "the fixture's gate let {caller:?} in"
        );
        assert_eq!(
            raw.status(),
            ordinary,
            "the raw LFS route refused {caller:?} differently from an ordinary file"
        );
        assert!(!raw.text().await.unwrap().contains("private LFS payload"));
    }
}
