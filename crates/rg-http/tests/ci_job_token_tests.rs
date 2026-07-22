mod common;

use common::{register_full, spawn_test_app_with_db};

fn ci_token(repo_id: i64, scopes: &str) -> String {
    rg_core::auth::ci_token::generate_ci_job_token(repo_id, 100, 200, scopes, "test-secret-key")
        .expect("generate ci job token")
}

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
async fn ci_job_token_can_read_scoped_private_packages() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "ci_pkg_owner", "ci_pkg_owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "ci-private-packages").await;
    let other_repo_id = create_private_repo(&base, &owner_token, "ci-other-packages").await;
    publish_generic_package(&base, &owner_token, "ci_pkg_owner", "ci-private-packages").await;

    let matching_token = ci_token(repo_id, "packages:read");
    let matching_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_pkg_owner/ci-private-packages/packages/generic/list",
            base
        ))
        .bearer_auth(&matching_token)
        .send()
        .await
        .unwrap();
    assert_eq!(matching_resp.status(), 200);
    let body: serde_json::Value = matching_resp.json().await.unwrap();
    assert_eq!(body["packages"].as_array().unwrap().len(), 1);

    let wrong_repo_token = ci_token(other_repo_id, "packages:read");
    let wrong_repo_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_pkg_owner/ci-private-packages/packages/generic/list",
            base
        ))
        .bearer_auth(&wrong_repo_token)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_repo_resp.status(), 401);
}

#[tokio::test]
async fn ci_job_token_can_read_scoped_private_repo_content() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "ci_repo_owner", "ci_repo_owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "ci-private-content").await;
    let other_repo_id = create_private_repo(&base, &owner_token, "ci-other-content").await;

    let matching_token = ci_token(repo_id, "repo:read");
    let matching_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_repo_owner/ci-private-content/tree",
            base
        ))
        .bearer_auth(&matching_token)
        .send()
        .await
        .unwrap();
    assert_eq!(matching_resp.status(), 200);

    let wrong_repo_token = ci_token(other_repo_id, "repo:read");
    let wrong_repo_resp = client
        .get(format!(
            "{}/api/v1/repos/ci_repo_owner/ci-private-content/tree",
            base
        ))
        .bearer_auth(&wrong_repo_token)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_repo_resp.status(), 401);
}
