//! card_36b620ab3467 / security audit finding #8: a file a user uploaded is
//! never served as something a browser would run from this origin.
//!
//! The release asset carried the uploader's `Content-Type` and the issue
//! attachment the multipart part's. `attachment` and `nosniff` do not stop
//! `<script src>` from running a file served as `text/javascript`, and
//! `script-src 'self'` lets it in.
//!
//! The residual half of the finding: CI artifacts, package files and OCI
//! blobs and manifests were served as bytes but without the sandbox, and
//! every route hand-rolled its own header set. One helper now serves all of
//! them, and these tests pin that each route reaches it.

use crate::common::{
    create_initialised_repo, create_issue, create_repo, register_user, seed_artifact,
    spawn_test_app, spawn_test_app_with_db_and_repo_root,
};
use sha2::Digest as _;

const PASSWORD: &str = "Qz7$wRtm";
const OCI_MANIFEST_V1: &str = "application/vnd.oci.image.manifest.v1+json";

/// The header set every download of user content carries: the type it is
/// served as, `nosniff`, and the sandboxing policy.
fn assert_sandboxed(response: &reqwest::Response, content_type: &str, what: &str) {
    assert_eq!(response.status(), reqwest::StatusCode::OK, "{what}");
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some(content_type),
        "{what}: served under the wrong type"
    );
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::X_CONTENT_TYPE_OPTIONS)
            .and_then(|value| value.to_str().ok()),
        Some("nosniff"),
        "{what}: no nosniff"
    );
    let policies: Vec<_> = response
        .headers()
        .get_all(reqwest::header::CONTENT_SECURITY_POLICY)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    assert!(
        policies.contains(&"default-src 'none'; sandbox"),
        "{what}: no sandbox policy in {policies:?}"
    );
}

/// An uploader's script type is served as bytes, and as a named attachment.
fn assert_served_passively(response: &reqwest::Response, what: &str) {
    assert_sandboxed(response, "application/octet-stream", what);
    assert!(
        response
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("attachment;")),
        "{what}: not served as an attachment"
    );
}

#[tokio::test]
async fn a_js_release_asset_is_served_as_bytes() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let owner = "jsasset";
    let token = register_user(&base, owner, "jsasset@example.com", PASSWORD).await;
    create_initialised_repo(&base, &token, "repo").await;
    let release: serde_json::Value = client
        .post(format!("{base}/api/v1/repos/{owner}/repo/releases"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"tag_name": "v1.0.0", "title": "First"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let release_id = release["id"].as_i64().expect("release id");
    let asset: serde_json::Value = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/repo/releases/{release_id}/assets"
        ))
        .bearer_auth(&token)
        .header("x-asset-filename", "payload.js")
        .header("content-type", "text/javascript")
        .body(b"alert(document.domain)".to_vec())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let asset_id = asset["id"].as_i64().expect("asset id");
    // Normalised on the way in, not only on the way out: the row itself never
    // says `text/javascript`, so no other reader of the column can serve it.
    assert_eq!(
        asset["content_type"], "application/octet-stream",
        "the stored type must already be the passive one: {asset}"
    );

    let download = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/repo/releases/assets/{asset_id}/download"
        ))
        .send()
        .await
        .unwrap();
    assert_served_passively(&download, "release asset");
}

#[tokio::test]
async fn an_attachment_uploaded_as_script_is_served_as_bytes() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let owner = "jsattach";
    let token = register_user(&base, owner, "jsattach@example.com", PASSWORD).await;
    create_repo(&base, &token, "repo").await;
    let (_, issue_number) = create_issue(&base, &token, owner, "repo", "Attachment").await;

    let attachment: serde_json::Value = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/repo/issues/{issue_number}/assets"
        ))
        .bearer_auth(&token)
        .multipart(
            reqwest::multipart::Form::new().part(
                "attachment",
                reqwest::multipart::Part::bytes(b"alert(document.domain)".to_vec())
                    .file_name("notes.txt")
                    .mime_str("text/javascript")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let attachment_id = attachment["id"].as_i64().expect("attachment id");

    let download = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/repo/issues/{issue_number}/assets/{attachment_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_served_passively(&download, "issue attachment");
}

/// A CI artifact is whatever the job wrote; it was served as bytes with a
/// name, but without the sandbox the other downloads carry.
#[tokio::test]
async fn a_ci_artifact_download_is_sandboxed() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let client = reqwest::Client::new();
    let owner = "jsartifact";
    let token = register_user(&base, owner, "jsartifact@example.com", PASSWORD).await;
    let repo_id = create_repo(&base, &token, "repo").await;
    let artifact_id = seed_artifact(&base, &client, &db, &repo_root, repo_id, "runner").await;

    let download = client
        .get(format!("{base}/api/v1/artifacts/{artifact_id}/download"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_served_passively(&download, "CI artifact");
}

/// A package file is published under whatever name the client chose; a
/// `.js` one answered with bytes but no sandbox.
#[tokio::test]
async fn a_package_file_download_is_sandboxed() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let owner = "jspackage";
    let token = register_user(&base, owner, "jspackage@example.com", PASSWORD).await;
    create_repo(&base, &token, "repo").await;

    let published = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/repo/packages/generic/publish?name=gadget&version=1.0.0"
        ))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"gadget.js\"",
        )
        .header(reqwest::header::CONTENT_TYPE, "text/javascript")
        .body(b"alert(document.domain)".to_vec())
        .send()
        .await
        .unwrap();
    let status = published.status();
    let body = published.text().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::CREATED, "publish: {body}");

    let download = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/repo/packages/generic/gadget/1.0.0/gadget.js"
        ))
        .send()
        .await
        .unwrap();
    assert_served_passively(&download, "package file");
}

/// `/v2/` answers from the application's origin too. A config blob and a
/// manifest are JSON the pusher chose; both go out under the sandbox, and
/// the registry contract — the exact `Content-Type`, no
/// `Content-Disposition` — is untouched, because a registry client reads the
/// former and would be confused by the latter.
#[tokio::test]
async fn an_oci_blob_and_manifest_are_sandboxed_under_their_registry_types() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let owner = "jsoci";
    let token = register_user(&base, owner, "jsoci@example.com", PASSWORD).await;
    create_repo(&base, &token, "image").await;

    let config = br#"{"architecture":"amd64","os":"linux"}"#;
    let config_digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(config)));
    let start = client
        .post(format!("{base}/v2/{owner}/image/blobs/uploads/"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 202, "start upload");
    let location = start
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("upload location")
        .to_string();
    let chunk = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .body(config.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 202, "chunk upload");
    let finish = client
        .put(format!("{base}{location}"))
        .query(&[("digest", config_digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    crate::common::assert_blob_push_created(finish, config.len()).await;

    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST_V1,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "size": config.len(),
            "digest": config_digest,
        },
        "layers": [],
    })
    .to_string();
    let pushed = client
        .put(format!("{base}/v2/{owner}/image/manifests/v1"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, OCI_MANIFEST_V1)
        .body(manifest.clone())
        .send()
        .await
        .unwrap();
    let status = pushed.status();
    let body = pushed.text().await.unwrap();
    assert_eq!(status, 201, "manifest push: {body}");

    let blob = client
        .get(format!("{base}/v2/{owner}/image/blobs/{config_digest}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_sandboxed(&blob, "application/octet-stream", "OCI blob");
    assert!(
        blob.headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .is_none(),
        "an OCI blob must not grow a Content-Disposition"
    );
    assert_eq!(
        blob.headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok()),
        Some(config_digest.as_str())
    );
    assert_eq!(blob.bytes().await.unwrap().as_ref(), config);

    for (what, request) in [
        (
            "OCI manifest GET",
            client.get(format!("{base}/v2/{owner}/image/manifests/v1")),
        ),
        (
            "OCI manifest HEAD",
            client.head(format!("{base}/v2/{owner}/image/manifests/v1")),
        ),
    ] {
        let response = request.bearer_auth(&token).send().await.unwrap();
        assert_sandboxed(&response, OCI_MANIFEST_V1, what);
        assert!(
            response
                .headers()
                .get(reqwest::header::CONTENT_DISPOSITION)
                .is_none(),
            "{what}: must not grow a Content-Disposition"
        );
        if what == "OCI manifest GET" {
            assert_eq!(
                response.text().await.unwrap(),
                manifest,
                "the pulled manifest must be the pushed bytes"
            );
        }
    }
}
