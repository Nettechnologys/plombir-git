//! Branch-protection regressions for server-side contents commits.
//!
//! The contents endpoints authorize repository writes through `RepoWrite`, but
//! then clone and push over a local `file://` remote. That transport does not
//! enter HTTP or SSH receive-pack, so the core writer itself must apply the
//! shared push-policy decision before it moves the branch.

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

async fn add_write_collaborator(
    base: &str,
    owner_token: &str,
    owner: &str,
    repo: &str,
    username: &str,
) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/collaborators"))
        .bearer_auth(owner_token)
        .json(&serde_json::json!({
            "username": username,
            "permission": "write",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "seed write collaborator");
}

/// A contents edit moves the same branch ref as receive-pack and therefore
/// owes the same `require_pr` refusal. `RepoWrite` alone is intentionally not
/// enough: the collaborator may author a PR, but may not bypass it by asking
/// the server-side editor to push over a local `file://` remote.
///
/// Both write shapes are pinned because they call separate core entrypoints.
/// Removing either shared-policy call must make this regression red.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn require_pr_rejects_contents_writes_from_a_write_collaborator() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let state = build_test_app_state(db, repo_root);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (owner_token, _) =
        register_full(&base, "protected-owner", "protected-owner@example.com").await;
    let (writer_token, _) =
        register_full(&base, "protected-writer", "protected-writer@example.com").await;
    crate::common::create_repo(&base, &owner_token, "protected-repo").await;
    add_write_collaborator(
        &base,
        &owner_token,
        "protected-owner",
        "protected-repo",
        "protected-writer",
    )
    .await;

    let client = reqwest::Client::new();
    let seeded = client
        .post(format!(
            "{base}/api/v1/repos/protected-owner/protected-repo/contents/kept.txt"
        ))
        .bearer_auth(&writer_token)
        .json(&serde_json::json!({
            "content": "must survive the refused delete\n",
            "message": "seed before protection",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        seeded.status(),
        200,
        "the fixture must prove the collaborator really has RepoWrite"
    );

    let kept_url = format!("{base}/api/v1/repos/protected-owner/protected-repo/blob/kept.txt");
    let kept = client
        .get(&kept_url)
        .bearer_auth(&writer_token)
        .send()
        .await
        .unwrap();
    assert_eq!(kept.status(), 200, "seeded file is readable");
    let kept: serde_json::Value = kept.json().await.unwrap();
    let kept_blob_sha = kept["sha"].as_str().expect("blob response carries sha");

    let protection = client
        .post(format!(
            "{base}/api/v1/repos/protected-owner/protected-repo/branches/protection"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "require_pr": true,
            // Isolate the require_pr decision: force-push policy must not be
            // the reason this write is refused.
            "allow_force_push": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(protection.status(), 201, "seed require_pr protection");

    let create = client
        .post(format!(
            "{base}/api/v1/repos/protected-owner/protected-repo/contents/blocked.txt"
        ))
        .bearer_auth(&writer_token)
        .json(&serde_json::json!({
            "content": "must never land\n",
            "message": "bypass require_pr",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), 403, "contents create bypassed require_pr");
    let create_body = create.text().await.unwrap();
    assert!(
        create_body.contains("open a pull request instead"),
        "the refusal must come from the shared push rule: {create_body}"
    );

    let delete = client
        .delete(format!(
            "{base}/api/v1/repos/protected-owner/protected-repo/contents/kept.txt?message=delete+bypass&sha={kept_blob_sha}"
        ))
        .bearer_auth(&writer_token)
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status(), 403, "contents delete bypassed require_pr");
    let delete_body = delete.text().await.unwrap();
    assert!(
        delete_body.contains("open a pull request instead"),
        "the delete refusal must come from the shared push rule: {delete_body}"
    );

    let blocked = client
        .get(format!(
            "{base}/api/v1/repos/protected-owner/protected-repo/blob/blocked.txt"
        ))
        .bearer_auth(&writer_token)
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), 404, "refused create changed the branch");

    let kept = client
        .get(&kept_url)
        .bearer_auth(&writer_token)
        .send()
        .await
        .unwrap();
    assert_eq!(kept.status(), 200, "refused delete changed the branch");

    server.abort();
}
