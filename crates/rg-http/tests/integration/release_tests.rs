//! Integration tests for Releases endpoint.
//!
//! Guards:
//!   POST   /repos/:o/:r/releases        — create release
//!   GET    /repos/:o/:r/releases        — list releases (paginated)
//!   GET    /repos/:o/:r/releases/:id    — get release
//!   PATCH  /repos/:o/:r/releases/:id   — update release
//!   DELETE /repos/:o/:r/releases/:id   — delete release
//!   GET    /repos/:o/:r/releases/assets/:asset_id — get asset metadata

use crate::common::{
    create_repo, register_user, setup_test_db, spawn_test_app, spawn_test_app_over_db_with,
    StateOverrides,
};
use sea_orm::{ConnectionTrait, Statement};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PW: &str = "Qz7$wRtm";

async fn setup(suffix: &str) -> (String, String, String, String) {
    let base = spawn_test_app().await;
    let owner = format!("reluser{suffix}");
    let token = register_user(&base, &owner, &format!("reluser{suffix}@example.com"), PW).await;
    let repo = format!("relrepo{suffix}");
    create_repo(&base, &token, &repo).await;
    (base, token, owner, repo)
}

async fn create_release(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    tag: &str,
    title: &str,
) -> serde_json::Value {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos/{}/{}/releases", base, owner, repo))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "tag_name": tag,
            "title": title,
            "body": "Release notes here",
            "is_draft": false,
            "is_prerelease": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "create release '{}' failed: {}",
        tag,
        resp.status()
    );
    resp.json().await.unwrap()
}

async fn headers_only_asset_upload_status(
    base: &str,
    path: &str,
    token: Option<&str>,
    content_length: usize,
) -> u16 {
    let authority = base.strip_prefix("http://").expect("HTTP test base URL");
    let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();
    let authorization = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {authority}\r\n\
         {authorization}\
         Content-Type: application/octet-stream\r\n\
         Content-Disposition: attachment; filename=boundary.bin\r\n\
         Content-Length: {content_length}\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    let mut response = [0_u8; 256];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut response))
        .await
        .expect("the upload gate waited for a body it should not read")
        .unwrap();
    let status_line = std::str::from_utf8(&response[..read])
        .unwrap()
        .lines()
        .next()
        .expect("HTTP status line");
    status_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn test_create_and_get_release() {
    let (base, token, owner, repo) = setup("1").await;

    let release = create_release(&base, &token, &owner, &repo, "v1.0.0", "First Release").await;
    assert_eq!(release["tag_name"], "v1.0.0");
    assert_eq!(release["title"], "First Release");
    assert_eq!(release["is_draft"], false);

    let id = release["id"].as_i64().unwrap();
    let client = reqwest::Client::new();
    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/releases/{}",
            base, owner, repo, id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let got: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(got["id"], id);
    assert_eq!(got["tag_name"], "v1.0.0");
}

#[tokio::test]
async fn test_list_releases() {
    let (base, token, owner, repo) = setup("2").await;

    for (tag, title) in &[
        ("v0.1.0", "Alpha"),
        ("v0.2.0", "Beta"),
        ("v1.0.0", "Stable"),
    ] {
        create_release(&base, &token, &owner, &repo, tag, title).await;
    }

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/v1/repos/{}/{}/releases", base, owner, repo))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let releases = body["data"].as_array().unwrap();
    assert_eq!(releases.len(), 3);
}

#[tokio::test]
async fn test_update_release() {
    let (base, token, owner, repo) = setup("3").await;
    let release = create_release(
        &base,
        &token,
        &owner,
        &repo,
        "v2.0.0-draft",
        "Draft Release",
    )
    .await;
    let id = release["id"].as_i64().unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/releases/{}",
            base, owner, repo, id
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "v2.0.0 Final",
            "is_draft": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "update failed: {}", resp.status());
    let updated: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(updated["title"], "v2.0.0 Final");
    assert_eq!(updated["is_draft"], false);
}

#[tokio::test]
async fn test_delete_release() {
    let (base, token, owner, repo) = setup("4").await;
    let release = create_release(&base, &token, &owner, &repo, "v9.9.9", "Delete Me").await;
    let id = release["id"].as_i64().unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/releases/{}",
            base, owner, repo, id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "delete failed: {}",
        resp.status()
    );

    // List should be empty
    let body: serde_json::Value = client
        .get(format!("{}/api/v1/repos/{}/{}/releases", base, owner, repo))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_create_release_requires_auth() {
    let (base, _token, owner, repo) = setup("5").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/repos/{}/{}/releases", base, owner, repo))
        .json(&serde_json::json!({"tag_name": "v0.0.1", "title": "Unauthorized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "unauthenticated create_release should return 401"
    );
}

#[tokio::test]
async fn release_asset_round_trip_uses_blob_storage() {
    let (base, token, owner, repo) = setup("asset").await;
    let release = create_release(&base, &token, &owner, &repo, "v1.2.3", "Assets").await;
    let release_id = release["id"].as_i64().unwrap();
    let client = reqwest::Client::new();

    let uploaded = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/{release_id}/assets"
        ))
        .bearer_auth(&token)
        .header("content-type", "text/plain")
        .header("content-disposition", "attachment; filename=notes.txt")
        .body("release asset")
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), 201);
    let asset: serde_json::Value = uploaded.json().await.unwrap();
    let asset_id = asset["id"].as_i64().unwrap();

    // Upload records a SHA-256 digest of the bytes.
    let sha256 = asset["sha256"].as_str().expect("asset carries sha256");
    assert_eq!(sha256.len(), 64, "sha256 is 64 hex chars");
    assert!(sha256.bytes().all(|b| b.is_ascii_hexdigit()));
    // "release asset" → known SHA-256.
    assert_eq!(
        sha256,
        "e6abe9df7db8513616674b02b5edb26c37bf3b2f81daeec1e3c6fc8c9a802850",
    );

    let downloaded = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/download"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), 200);
    // Download echoes the digest so clients can verify the payload.
    assert_eq!(
        downloaded
            .headers()
            .get("x-checksum-sha256")
            .and_then(|v| v.to_str().ok()),
        Some(sha256),
    );
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), b"release asset");

    let deleted = client
        .delete(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(deleted.status().is_success());
}

#[tokio::test]
async fn release_asset_metadata_is_routed_and_scoped_to_its_repository() {
    let (base, token, owner, repo) = setup("assetmetadata").await;
    let release = create_release(&base, &token, &owner, &repo, "v1.2.4", "Asset metadata").await;
    let release_id = release["id"].as_i64().unwrap();
    let client = reqwest::Client::new();

    let uploaded = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/{release_id}/assets"
        ))
        .bearer_auth(&token)
        .header("content-type", "text/plain")
        .header("content-disposition", "attachment; filename=metadata.txt")
        .body("release asset metadata")
        .send()
        .await
        .expect("upload the metadata fixture");
    assert_eq!(uploaded.status(), 201, "uploading the asset failed");
    let created: serde_json::Value = uploaded.json().await.unwrap();
    let asset_id = created["id"].as_i64().unwrap();

    let response = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read release asset metadata through the Axum router");
    assert_eq!(response.status(), 200, "asset metadata is not routed");
    let metadata: serde_json::Value = response.json().await.unwrap();
    for field in [
        "id",
        "release_id",
        "filename",
        "size",
        "content_type",
        "download_count",
        "uploader_id",
        "sha256",
    ] {
        assert_eq!(
            metadata[field], created[field],
            "GET asset metadata changed {field}"
        );
    }

    let other_repo = format!("{repo}other");
    create_repo(&base, &token, &other_repo).await;
    let foreign = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{other_repo}/releases/assets/{asset_id}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read the asset through a different repository");
    assert_eq!(
        foreign.status(),
        404,
        "an asset id must not escape the repository named in the route"
    );

    let missing = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{}",
            asset_id + 1_000_000
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read missing release asset metadata");
    assert_eq!(missing.status(), 404, "a missing asset must stay a 404");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn release_asset_downloads_keep_parallel_counts_and_type_delete_races() {
    let (db, dir) = setup_test_db().await;
    let base = spawn_test_app_over_db_with(
        db.clone(),
        dir.path().join("repos"),
        StateOverrides::default(),
    )
    .await;
    let owner = "reldownloadrace".to_string();
    let token = register_user(&base, &owner, "reldownloadrace@example.com", PW).await;
    let repo = "reldownloadracerepo".to_string();
    create_repo(&base, &token, &repo).await;
    let release = create_release(
        &base,
        &token,
        &owner,
        &repo,
        "v1.0.0-download-race",
        "Concurrent downloads",
    )
    .await;
    let release_id = release["id"].as_i64().unwrap();
    let client = reqwest::Client::new();
    let uploaded = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/{release_id}/assets"
        ))
        .bearer_auth(&token)
        .header("content-type", "text/plain")
        .header("content-disposition", "attachment; filename=parallel.txt")
        .body("parallel release asset")
        .send()
        .await
        .expect("upload the concurrent-download fixture");
    assert_eq!(uploaded.status(), 201);
    let asset_id = uploaded.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let url = format!("{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/download");

    let (first, second) = tokio::join!(
        client.get(&url).bearer_auth(&token).send(),
        client.get(&url).bearer_auth(&token).send(),
    );
    let first = first.expect("first concurrent download");
    let second = second.expect("second concurrent download");
    assert_eq!(first.status(), 200);
    assert_eq!(second.status(), 200);
    assert_eq!(
        first.bytes().await.unwrap().as_ref(),
        b"parallel release asset"
    );
    assert_eq!(
        second.bytes().await.unwrap().as_ref(),
        b"parallel release asset"
    );

    let asset = rg_db::ops::release_ops::find_asset_by_id(&db, asset_id)
        .await
        .expect("read the asset after concurrent downloads")
        .expect("the downloaded asset still exists");
    assert_eq!(
        asset.download_count, 2,
        "both downloads must be represented in the persisted counter"
    );

    // As with attestation signing, run the competing DELETE inside the actual
    // counter UPDATE, after both repository scoping and the service read. This
    // proves the download path reports absence instead of RecordNotUpdated.
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_release_asset_before_download_increment \
             BEFORE UPDATE OF download_count ON release_assets WHEN OLD.id = {asset_id} \
             BEGIN DELETE FROM release_assets WHERE id = OLD.id; END"
        ),
    ))
    .await
    .expect("install the competing release asset delete");
    let raced = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("race asset download against deletion");
    assert_eq!(
        raced.status(),
        404,
        "a DELETE after the scoped read must stay a typed missing asset"
    );
    assert!(
        rg_db::ops::release_ops::find_asset_by_id(&db, asset_id)
            .await
            .expect("look for the release asset after the download race")
            .is_none(),
        "the losing download must not recreate the deleted asset"
    );
}

#[tokio::test]
async fn release_asset_upload_crosses_axum_default_and_gates_before_reading() {
    let (base, token, owner, repo) = setup("assetboundary").await;
    let release = create_release(&base, &token, &owner, &repo, "v2.0.0", "Asset boundary").await;
    let release_id = release["id"].as_i64().unwrap();
    let path = format!("/api/v1/repos/{owner}/{repo}/releases/{release_id}/assets");
    let client = reqwest::Client::new();
    let payload = vec![b'x'; 2 * 1024 * 1024 + 1];

    let uploaded = client
        .post(format!("{base}{path}"))
        .bearer_auth(&token)
        .header("content-type", "application/octet-stream")
        .header("content-disposition", "attachment; filename=large.bin")
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        uploaded.status(),
        201,
        "the declared release ceiling must replace Axum's hidden 2 MiB default"
    );
    let asset: serde_json::Value = uploaded.json().await.unwrap();
    let downloaded = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{}/download",
            asset["id"].as_i64().unwrap()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), 200);
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), payload);

    assert_eq!(
        headers_only_asset_upload_status(&base, &path, Some(&token), 512 * 1024 * 1024 + 1).await,
        413,
        "an impossible Content-Length must be rejected before upload staging"
    );
    assert_eq!(
        headers_only_asset_upload_status(&base, &path, None, 1).await,
        401,
        "RepoWrite must reject an unauthenticated request without waiting for its body"
    );
    let missing_release_path = format!(
        "/api/v1/repos/{owner}/{repo}/releases/{}/assets",
        release_id + 1_000_000
    );
    assert_eq!(
        headers_only_asset_upload_status(&base, &missing_release_path, Some(&token), 1).await,
        404,
        "release scope must be checked without waiting for the upload body"
    );
}
