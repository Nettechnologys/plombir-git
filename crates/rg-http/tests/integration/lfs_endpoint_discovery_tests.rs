//! Git LFS at the endpoint a stock `git lfs` derives from the remote
//! (card_0af262f2c4e8).
//!
//! The client never asks `/api/v1/repos/{owner}/{name}/lfs`: it appends
//! `.git/info/lfs` to the remote URL. Those paths used to fall through to the
//! GET-only SPA fallback, so every `git lfs push` answered `405` unless the
//! user configured `lfs.url` by hand. They are rewritten onto the REST route
//! now, and these tests hold the alias to the canonical path's guarantees —
//! a PAT over Basic auth, repository confinement and maintenance mode are all
//! decided by layers in front of the REST table, which is exactly what a
//! separately mounted route would have skipped.

use crate::common::{register_full, spawn_test_app_with_db};
use sha2::{Digest, Sha256};

async fn create_repo(base: &str, token: &str, name: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
}

async fn create_pat(base: &str, token: &str, body: serde_json::Value) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The two endpoints `git lfs` derives: from the clone URL the UI shows
/// (`/git/{owner}/{repo}`, `.git` appended by the client) and from a root or
/// SSH remote (`/{owner}/{repo}.git`).
fn derived_endpoints(base: &str, owner: &str, repo: &str) -> [String; 2] {
    [
        format!("{base}/git/{owner}/{repo}.git/info/lfs"),
        format!("{base}/{owner}/{repo}.git/info/lfs"),
    ]
}

/// A batch request as `git lfs` 3.x sends it: Basic auth carrying a PAT, the
/// protocol's own media type, and its transfer adapters in its own order —
/// the local-file one first, which an HTTP server must not echo back.
async fn batch(
    endpoint: &str,
    user: &str,
    pat: Option<&str>,
    operation: &str,
    content: &[u8],
) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(format!("{endpoint}/objects/batch"))
        .header("Accept", "application/vnd.git-lfs+json")
        .header("Content-Type", "application/vnd.git-lfs+json")
        .body(
            serde_json::json!({
                "operation": operation,
                "transfers": ["lfs-standalone-file", "basic", "ssh"],
                "objects": [{ "oid": hex::encode(Sha256::digest(content)), "size": content.len() }],
            })
            .to_string(),
        );
    if let Some(pat) = pat {
        request = request.basic_auth(user, Some(pat));
    }
    request.send().await.unwrap()
}

#[tokio::test]
async fn a_pat_over_basic_auth_pushes_and_pulls_at_the_derived_endpoints() {
    let (base, _) = spawn_test_app_with_db().await;
    let (session, _) = register_full(&base, "lfs_disc", "lfs_disc@example.com").await;
    create_repo(&base, &session, "media").await;
    let pat = create_pat(
        &base,
        &session,
        serde_json::json!({ "name": "git-lfs", "scopes": "repo" }),
    )
    .await;

    for (index, endpoint) in derived_endpoints(&base, "lfs_disc", "media")
        .iter()
        .enumerate()
    {
        let content = format!("large file pushed through endpoint {index}").into_bytes();

        let upload = batch(endpoint, "lfs_disc", Some(&pat), "upload", &content).await;
        assert_eq!(upload.status(), 200, "{endpoint}");
        let upload: serde_json::Value = upload.json().await.unwrap();
        assert_eq!(upload["transfer"], "basic", "{endpoint}: {upload}");
        let href = upload["objects"][0]["actions"]["upload"]["href"]
            .as_str()
            .unwrap_or_else(|| panic!("no upload action from {endpoint}: {upload}"));
        assert!(
            href.contains("/api/v1/repos/lfs_disc/media/lfs/objects/"),
            "transfers stay on the signed REST URL: {href}"
        );
        let stored = reqwest::Client::new()
            .put(href)
            .body(content.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(stored.status(), 200, "{endpoint}");

        let download = batch(endpoint, "lfs_disc", Some(&pat), "download", &content).await;
        assert_eq!(download.status(), 200, "{endpoint}");
        let download: serde_json::Value = download.json().await.unwrap();
        let href = download["objects"][0]["actions"]["download"]["href"]
            .as_str()
            .unwrap_or_else(|| panic!("no download action from {endpoint}: {download}"));
        let fetched = reqwest::get(href).await.unwrap();
        assert_eq!(fetched.status(), 200);
        assert_eq!(fetched.bytes().await.unwrap().as_ref(), content.as_slice());

        // The repository is private: without the PAT the alias is as closed as
        // the canonical path.
        let anonymous = batch(endpoint, "lfs_disc", None, "download", &content).await;
        assert_eq!(anonymous.status(), 401, "{endpoint}");
    }
}

#[tokio::test]
async fn repository_confinement_holds_at_the_derived_endpoints() {
    let (base, _) = spawn_test_app_with_db().await;
    let (session, _) = register_full(&base, "lfs_conf", "lfs_conf@example.com").await;
    create_repo(&base, &session, "allowed").await;
    create_repo(&base, &session, "other").await;
    let pat = create_pat(
        &base,
        &session,
        serde_json::json!({
            "name": "confined", "scopes": "repo", "repositories": ["lfs_conf/allowed"],
        }),
    )
    .await;

    for repo in ["allowed", "other"] {
        for endpoint in derived_endpoints(&base, "lfs_conf", repo) {
            let response = batch(&endpoint, "lfs_conf", Some(&pat), "download", b"x").await;
            let expected = if repo == "allowed" { 200 } else { 403 };
            assert_eq!(response.status(), expected, "{endpoint}");
        }
    }
}

#[tokio::test]
async fn maintenance_mode_judges_the_derived_endpoint_by_operation() {
    let (base, db) = spawn_test_app_with_db().await;
    let (admin, admin_id) = register_full(&base, "lfs_maint_admin", "lfs_ma@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap()
        .expect("registered user must exist");
    create_repo(&base, &admin, "media").await;
    let pat = create_pat(
        &base,
        &admin,
        serde_json::json!({ "name": "git-lfs", "scopes": "repo" }),
    )
    .await;
    let switched = reqwest::Client::new()
        .patch(format!("{base}/api/v1/admin/settings"))
        .bearer_auth(&admin)
        .json(&serde_json::json!({ "maintenance_mode": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(switched.status(), 200);

    for endpoint in derived_endpoints(&base, "lfs_maint_admin", "media") {
        let download = batch(&endpoint, "lfs_maint_admin", Some(&pat), "download", b"x").await;
        assert_eq!(download.status(), 200, "a download is a read: {endpoint}");
        let upload = batch(&endpoint, "lfs_maint_admin", Some(&pat), "upload", b"x").await;
        assert_eq!(upload.status(), 503, "an upload is a write: {endpoint}");
        let body: serde_json::Value = upload.json().await.unwrap();
        assert_eq!(body["error"]["code"], "MAINTENANCE_MODE");
    }
}

/// `git lfs push` asks `locks/verify` before uploading. This server has no
/// locking API, and the client reads `404` as "not supported" and carries on —
/// the GET-only page fallback answered `405`, which reads as a failure.
#[tokio::test]
async fn an_lfs_endpoint_this_server_lacks_is_a_404_not_a_page() {
    let (base, _) = spawn_test_app_with_db().await;
    let (session, _) = register_full(&base, "lfs_locks", "lfs_locks@example.com").await;
    create_repo(&base, &session, "media").await;
    let pat = create_pat(
        &base,
        &session,
        serde_json::json!({ "name": "git-lfs", "scopes": "repo" }),
    )
    .await;

    for endpoint in derived_endpoints(&base, "lfs_locks", "media") {
        let response = reqwest::Client::new()
            .post(format!("{endpoint}/locks/verify"))
            .basic_auth("lfs_locks", Some(&pat))
            .header("Content-Type", "application/vnd.git-lfs+json")
            .body(r#"{"ref":{"name":"refs/heads/main"}}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{endpoint}");
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            content_type.starts_with("application/json"),
            "{endpoint}: {content_type}"
        );
    }
}
