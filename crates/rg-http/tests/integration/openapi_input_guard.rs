//! The source contract check compares every published handler signature with
//! its `#[utoipa::path]` declaration. This test owns the other boundary: the
//! declarations added by that check must survive macro expansion and reach the
//! document the production router serves.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::common::{register_user, spawn_test_app_with_routes};

fn operation<'a>(doc: &'a Value, method: &str, path: &str) -> &'a Value {
    doc.get("paths")
        .and_then(|paths| paths.get(path))
        .and_then(|item| item.get(method))
        .unwrap_or_else(|| panic!("published OpenAPI has no {method} {path}"))
}

fn query_parameter_names(operation: &Value) -> BTreeSet<&str> {
    operation
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|parameter| parameter.get("in").and_then(Value::as_str) == Some("query"))
        .filter_map(|parameter| parameter.get("name").and_then(Value::as_str))
        .collect()
}

#[tokio::test]
async fn handler_inputs_reach_the_published_openapi_document() {
    let (base, _) = spawn_test_app_with_routes().await;
    let jwt = register_user(&base, "specinput", "specinput@example.com", "Qz7$wRtm").await;
    let doc: Value = reqwest::Client::new()
        .get(format!("{base}/api-docs/openapi.json"))
        .bearer_auth(jwt)
        .send()
        .await
        .expect("fetch the published OpenAPI document")
        .json()
        .await
        .expect("published OpenAPI is JSON");

    let multipart_uploads = [
        "/repos/{owner}/{name}/issues/{number}/assets",
        "/repos/{owner}/{name}/pulls/{number}/assets",
        "/repos/{owner}/{name}/issues/comments/{comment_id}/assets",
        "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets",
    ];
    for path in multipart_uploads {
        let operation = operation(&doc, "post", path);
        assert!(
            operation
                .pointer("/requestBody/content/multipart~1form-data")
                .is_some(),
            "POST {path} consumes Multipart but the published operation has no \
             multipart/form-data request body: {operation}"
        );
        assert_eq!(
            query_parameter_names(operation),
            BTreeSet::from(["name"]),
            "POST {path} consumes Query<UploadQuery>"
        );
    }

    let release_upload = operation(
        &doc,
        "post",
        "/repos/{owner}/{name}/releases/{release_id}/assets",
    );
    assert!(
        release_upload
            .pointer("/requestBody/content/application~1octet-stream")
            .is_some(),
        "release asset upload consumes raw Body but the published operation has no \
         application/octet-stream request body: {release_upload}"
    );

    for (method, path, expected) in [
        ("get", "/admin/users", &["page", "per_page"][..]),
        ("get", "/admin/orgs", &["page", "per_page"][..]),
        (
            "get",
            "/notifications",
            &["page", "per_page", "unread_only"][..],
        ),
        ("get", "/search", &["page", "per_page", "q", "type"][..]),
        (
            "get",
            "/repos/{owner}/{name}/pipelines",
            &["page", "per_page"][..],
        ),
        (
            "get",
            "/repos/{owner}/{name}/issues",
            // `label` and `labels` are the two spellings of the same filter, and
            // both have to stay published: `labels` is the legacy comma-split
            // string, `label` the repeated key that is the only way to name a
            // label whose own name contains a comma (card_84d081b18275).
            &["label", "labels", "page", "per_page", "state"][..],
        ),
        ("get", "/repos/{owner}/{name}/milestones", &["state"][..]),
        // `session` and `pat` are the two spellings of the credential half of a
        // signed URL — a session generation or an `access_tokens.id` — and
        // exactly one of them is ever present on a minted URL (card_e4e177acd095).
        // Both are published, because a client redeeming a URL sends whichever
        // one it was handed.
        (
            "put",
            "/repos/{owner}/{name}/lfs/objects/{oid}",
            &["actor", "expires", "pat", "session", "signature"][..],
        ),
        (
            "get",
            "/repos/{owner}/{name}/lfs/objects/{oid}",
            &["actor", "expires", "pat", "session", "signature"][..],
        ),
        (
            "post",
            "/repos/{owner}/{name}/packages/{pkg_type}/publish",
            &[
                "description",
                "homepage",
                "name",
                "repository_url",
                "semver",
                "version",
            ][..],
        ),
        (
            "post",
            "/repos/{owner}/{name}/packages/npm/publish",
            &[
                "description",
                "homepage",
                "name",
                "repository_url",
                "semver",
                "version",
            ][..],
        ),
        (
            "get",
            "/repos/{owner}/{name}/pulls",
            &["page", "per_page", "state"][..],
        ),
        (
            "get",
            "/repos/{owner}/{name}/releases",
            &["page", "per_page"][..],
        ),
        (
            "delete",
            "/repos/{owner}/{name}/contents/{*path}",
            &["branch", "message", "sha"][..],
        ),
        ("get", "/repos/{owner}/{name}/blob/{*path}", &["ref"][..]),
        ("get", "/repos/{owner}/{name}/tree", &["path", "ref"][..]),
        (
            "get",
            "/repos/{owner}/{name}/stargazers",
            &["page", "per_page"][..],
        ),
        (
            "get",
            "/repos/{owner}/{name}/forks",
            &["page", "per_page"][..],
        ),
        (
            "get",
            "/repos/{owner}/{name}/issues/{number}/time",
            &["page", "per_page"][..],
        ),
    ] {
        assert_eq!(
            query_parameter_names(operation(&doc, method, path)),
            expected.iter().copied().collect(),
            "{} {path} publishes the wrong query surface",
            method.to_uppercase()
        );
    }

    let mark_read = operation(&doc, "post", "/notifications/{id}/read");
    assert!(
        mark_read.get("requestBody").is_none(),
        "mark_read takes no body, but the published operation still advertises one: {mark_read}"
    );
}
