mod common;

use common::{register_full, spawn_test_app_with_db};

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
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/packages/generic/publish?name=sample&version=1.0.0",
            base, owner, repo
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
