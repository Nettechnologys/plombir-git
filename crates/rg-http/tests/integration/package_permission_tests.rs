use crate::common::{register_full, spawn_test_app_with_db};

async fn create_private_repo(base: &str, token: &str, name: &str) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "is_private": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create private repo failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn publish_generic_package(base: &str, token: &str, owner: &str, repo: &str) {
    publish_generic_version(base, token, owner, repo, "1.0.0").await;
}

async fn publish_generic_version(base: &str, token: &str, owner: &str, repo: &str, version: &str) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/packages/generic/publish?name=sample&version={}",
            base, owner, repo, version
        ))
        .bearer_auth(token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"sample.bin\"",
        )
        .body("package-bytes")
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap();
    assert_eq!(status, 201, "publish package failed: {body}");
}

#[tokio::test]
async fn private_package_list_requires_read_access() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) = register_full(&base, "pkg_owner", "pkg_owner@example.com").await;
    let (other_token, _other_id) = register_full(&base, "pkg_other", "pkg_other@example.com").await;
    create_private_repo(&base, &owner_token, "private-packages").await;
    publish_generic_package(&base, &owner_token, "pkg_owner", "private-packages").await;

    let anon_resp = client
        .get(format!(
            "{}/api/v1/repos/pkg_owner/private-packages/packages/generic/list",
            base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anon_resp.status(), 401);

    let other_resp = client
        .get(format!(
            "{}/api/v1/repos/pkg_owner/private-packages/packages/generic/list",
            base
        ))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(other_resp.status(), 403);

    let owner_resp = client
        .get(format!(
            "{}/api/v1/repos/pkg_owner/private-packages/packages/generic/list",
            base
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(owner_resp.status(), 200);
    let body: serde_json::Value = owner_resp.json().await.unwrap();
    assert_eq!(body["packages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn private_package_publish_requires_write_access() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "pkg_write_owner", "pkg_write_owner@example.com").await;
    let (other_token, _other_id) =
        register_full(&base, "pkg_write_other", "pkg_write_other@example.com").await;
    create_private_repo(&base, &owner_token, "private-write").await;

    let other_resp = client
        .post(format!(
            "{}/api/v1/repos/pkg_write_owner/private-write/packages/generic/publish?name=sample&version=1.0.0",
            base
        ))
        .bearer_auth(&other_token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"sample.bin\"",
        )
        .body("package-bytes")
        .send()
        .await
        .unwrap();
    assert_eq!(other_resp.status(), 403);

    let owner_resp = client
        .post(format!(
            "{}/api/v1/repos/pkg_write_owner/private-write/packages/generic/publish?name=sample&version=1.0.0",
            base
        ))
        .bearer_auth(&owner_token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"sample.bin\"",
        )
        .body("package-bytes")
        .send()
        .await
        .unwrap();
    let status = owner_resp.status();
    let body = owner_resp.text().await.unwrap();
    assert_eq!(status, 201, "owner publish failed: {body}");
}

/// The version list behind a package's page — the control that answers "which
/// releases of this exist, and may I see them".
///
/// It is a separate route from the package list next to it and carries the
/// releases themselves, so a repository turned private with the list still
/// readable would keep publishing its release history to anybody who kept the
/// URL. Nothing named this route until now.
#[tokio::test]
async fn private_package_versions_require_read_access() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "pkg_ver_owner", "pkg_ver_owner@example.com").await;
    let (other_token, _other_id) =
        register_full(&base, "pkg_ver_other", "pkg_ver_other@example.com").await;
    create_private_repo(&base, &owner_token, "private-versions").await;
    publish_generic_version(
        &base,
        &owner_token,
        "pkg_ver_owner",
        "private-versions",
        "1.0.0",
    )
    .await;
    publish_generic_version(
        &base,
        &owner_token,
        "pkg_ver_owner",
        "private-versions",
        "2.0.0",
    )
    .await;

    let anon_resp = client
        .get(format!(
            "{}/api/v1/repos/pkg_ver_owner/private-versions/packages/generic/sample/versions",
            base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anon_resp.status(), 401);

    let other_resp = client
        .get(format!(
            "{}/api/v1/repos/pkg_ver_owner/private-versions/packages/generic/sample/versions",
            base
        ))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap();
    assert_eq!(other_resp.status(), 403);

    let owner_resp = client
        .get(format!(
            "{}/api/v1/repos/pkg_ver_owner/private-versions/packages/generic/sample/versions",
            base
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(owner_resp.status(), 200);
    let body: serde_json::Value = owner_resp.json().await.unwrap();
    let mut versions: Vec<&str> = body["versions"]
        .as_array()
        .expect("the response carries a version array")
        .iter()
        .map(|v| v["version"].as_str().expect("each entry names its version"))
        .collect();
    versions.sort_unstable();
    assert_eq!(
        versions,
        ["1.0.0", "2.0.0"],
        "both published releases are listed"
    );

    // A package nobody published is the caller asking for something that does
    // not exist, not an empty release history.
    let unknown = client
        .get(format!(
            "{}/api/v1/repos/pkg_ver_owner/private-versions/packages/generic/absent/versions",
            base
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
}
