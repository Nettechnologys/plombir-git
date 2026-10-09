//! Security audit finding #1: a release asset's name is a path twice over —
//! the blob key, and for a row the blob store cannot answer for, the
//! pre-migration `<repo_root>/<owner>/<repo>.releases/assets/<id>/<name>`
//! layout that `Path::join`s it in. The upload handler checked nothing but
//! non-emptiness, so `x-asset-filename: /data/encryption_key` (or the same
//! thing percent-encoded in `filename*`) was stored as given, and a download
//! that caught the row before its blob was written resolved to that file.
//!
//! These tests pin the door: every path-shaped name, in every header form a
//! client can carry it, is a `400` that leaves no row and no bytes behind —
//! and the names legitimate clients send keep working.

use crate::common::{create_initialised_repo, register_full, spawn_test_app_with_repo_root};

async fn create_release(base: &str, token: &str, owner: &str, repo: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/releases"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "tag_name": "v1.0.0", "title": "v1.0.0" }))
        .send()
        .await
        .expect("create release");
    assert_eq!(response.status().as_u16(), 201, "create release failed");
    response.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("release id")
}

/// Every way a client can spell a name that is a path: the plain header, the
/// plain `filename=`, and the RFC 5987 form whose percent-decoding is what
/// let `%2F` through as `/`.
const HOSTILE_NAMES: &[(&str, &str)] = &[
    ("x-asset-filename", "/etc/passwd"),
    ("x-asset-filename", "/data/encryption_key"),
    ("x-asset-filename", "../../x"),
    ("x-asset-filename", ".."),
    ("x-asset-filename", "..\\..\\x"),
    (
        "content-disposition",
        "attachment; filename=\"/etc/passwd\"",
    ),
    (
        "content-disposition",
        "attachment; filename*=UTF-8''..%2F..%2Fx",
    ),
    (
        "content-disposition",
        "attachment; filename*=UTF-8''%2Fdata%2Fencryption_key",
    ),
];

#[tokio::test]
async fn a_release_asset_name_that_is_a_path_is_refused_and_leaves_nothing_behind() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "path-name", "path-name@example.com").await;
    create_initialised_repo(&base, &token, "shipments").await;
    let release_id = create_release(&base, &token, "path-name", "shipments").await;
    let client = reqwest::Client::new();
    let assets_url =
        format!("{base}/api/v1/repos/path-name/shipments/releases/{release_id}/assets");

    for (header, value) in HOSTILE_NAMES {
        let response = client
            .post(&assets_url)
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .header(*header, *value)
            .body("release bytes")
            .send()
            .await
            .expect("upload asset");
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert_eq!(
            status.as_u16(),
            400,
            "`{header}: {value}` is the client's mistake and must say so: {status} {body}"
        );
        assert!(
            body.contains("filename"),
            "the refusal must name the rule it enforces: {body}"
        );
        assert!(
            !body.contains("passwd") && !body.contains("encryption_key"),
            "the refusal must not echo the hostile name back: {body}"
        );
    }

    // Nothing of any of them may exist: no row the listing walks, no blob
    // under the store, no directory in the layout the fallback reads.
    let listed: serde_json::Value = client
        .get(&assets_url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("list assets")
        .json()
        .await
        .expect("json");
    assert_eq!(
        listed.as_array().map(Vec::len),
        Some(0),
        "a refused upload must leave no metadata row behind: {listed}"
    );
    let blobs = repo_root.join("releases");
    assert!(
        !blobs.exists(),
        "a refused upload must write no blob: {} exists",
        blobs.display()
    );
    let legacy = repo_root.join("path-name").join("shipments.releases");
    assert!(
        !legacy.exists(),
        "a refused upload must create no legacy directory: {} exists",
        legacy.display()
    );
}

/// The other half of the rule: the check refuses paths, not names. Non-ASCII,
/// spaces and parentheses — through both header forms — are stored and
/// served exactly as before.
#[tokio::test]
async fn a_release_asset_name_with_unicode_and_spaces_is_still_stored_and_served() {
    let (base, _repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "plain-name", "plain-name@example.com").await;
    create_initialised_repo(&base, &token, "shipments").await;
    let release_id = create_release(&base, &token, "plain-name", "shipments").await;
    let client = reqwest::Client::new();
    let assets_url =
        format!("{base}/api/v1/repos/plain-name/shipments/releases/{release_id}/assets");

    // `мой файл (final).tar.gz`, as RFC 5987 spells it.
    const UNICODE: &str = "мой файл (final).tar.gz";
    const UNICODE_ENCODED: &str =
        "%D0%BC%D0%BE%D0%B9%20%D1%84%D0%B0%D0%B9%D0%BB%20%28final%29.tar.gz";

    for (header, value, expected) in [
        (
            "content-disposition",
            format!("attachment; filename*=UTF-8''{UNICODE_ENCODED}"),
            UNICODE,
        ),
        (
            "x-asset-filename",
            "my file (final).bin".to_string(),
            "my file (final).bin",
        ),
    ] {
        let response = client
            .post(&assets_url)
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .header(header, value)
            .body("release bytes")
            .send()
            .await
            .expect("upload asset");
        let status = response.status();
        let asset = response.json::<serde_json::Value>().await.expect("json");
        assert_eq!(
            status.as_u16(),
            201,
            "a plain name must be accepted: {asset}"
        );
        assert_eq!(asset["filename"].as_str(), Some(expected), "{asset}");
        let asset_id = asset["id"].as_i64().expect("asset id");

        let response = client
            .get(format!(
                "{base}/api/v1/repos/plain-name/shipments/releases/assets/{asset_id}/download"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .expect("download asset");
        assert_eq!(response.status().as_u16(), 200, "{expected}");
        assert_eq!(response.text().await.unwrap_or_default(), "release bytes");
    }
}
