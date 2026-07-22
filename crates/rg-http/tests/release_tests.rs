//! Integration tests for Releases endpoint.
//!
//! Guards:
//!   POST   /repos/:o/:r/releases        — create release
//!   GET    /repos/:o/:r/releases        — list releases (paginated)
//!   GET    /repos/:o/:r/releases/:id    — get release
//!   PATCH  /repos/:o/:r/releases/:id   — update release
//!   DELETE /repos/:o/:r/releases/:id   — delete release

mod common;

use common::{create_repo, register_user, spawn_test_app};

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

    let downloaded = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/download"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), 200);
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
