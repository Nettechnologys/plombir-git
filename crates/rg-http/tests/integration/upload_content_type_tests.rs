//! card_36b620ab3467: a file a user uploaded is never served as something a
//! browser would run from this origin.
//!
//! The release asset carried the uploader's `Content-Type` and the issue
//! attachment the multipart part's. `attachment` and `nosniff` do not stop
//! `<script src>` from running a file served as `text/javascript`, and
//! `script-src 'self'` lets it in.

use crate::common::{
    create_initialised_repo, create_issue, create_repo, register_user, spawn_test_app,
};

const PASSWORD: &str = "Qz7$wRtm";

fn assert_served_passively(response: &reqwest::Response, what: &str) {
    assert_eq!(response.status(), reqwest::StatusCode::OK, "{what}");
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/octet-stream"),
        "{what}: served with the uploader's script type"
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
