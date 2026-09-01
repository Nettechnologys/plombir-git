//! Branch-protection regressions for server-side contents commits.
//!
//! The contents endpoints authorize repository writes through `RepoWrite`, but
//! then clone and push over a local `file://` remote. That transport does not
//! enter HTTP or SSH receive-pack, so the core writer itself must apply the
//! shared push-policy decision before it moves the branch.

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};
use sea_orm::{ActiveValue::NotSet, Set};

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_push_allow_list_still_applies_to_server_side_contents_commits() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db, repo_root));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (owner_token, _) = register_full(&base, "allow-owner", "allow-owner@example.com").await;
    let (writer_token, _) = register_full(&base, "allow-writer", "allow-writer@example.com").await;
    crate::common::create_repo(&base, &owner_token, "allow-repo").await;
    add_write_collaborator(
        &base,
        &owner_token,
        "allow-owner",
        "allow-repo",
        "allow-writer",
    )
    .await;

    let client = reqwest::Client::new();
    let protection = client
        .post(format!(
            "{base}/api/v1/repos/allow-owner/allow-repo/branches/protection"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "require_pr": true,
            "allow_force_push": true,
            "allowed_push_users": ["allow-writer"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(protection.status(), 201, "seed allow-listed protection");

    let allowed = client
        .post(format!(
            "{base}/api/v1/repos/allow-owner/allow-repo/contents/allowed.txt"
        ))
        .bearer_auth(&writer_token)
        .json(&serde_json::json!({
            "content": "allowed direct commit\n",
            "message": "allowed server-side push",
        }))
        .send()
        .await
        .unwrap();
    let status = allowed.status();
    let body = allowed.text().await.unwrap();
    assert_eq!(status, 200, "allow-listed writer was refused: {body}");

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_commit_policy_rejects_unsigned_contents_commits() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db, repo_root));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let (token, _) = register_full(&base, "signed-owner", "signed-owner@example.com").await;
    crate::common::create_repo(&base, &token, "signed-repo").await;
    let client = reqwest::Client::new();
    let seeded = client
        .post(format!(
            "{base}/api/v1/repos/signed-owner/signed-repo/contents/kept.txt"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "content": "must survive\n",
            "message": "unsigned baseline before protection",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(seeded.status(), 200, "unprotected baseline must succeed");

    let kept = client
        .get(format!(
            "{base}/api/v1/repos/signed-owner/signed-repo/blob/kept.txt"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(kept.status(), 200);
    let kept: serde_json::Value = kept.json().await.unwrap();
    let kept_sha = kept["sha"].as_str().unwrap();

    let protection = client
        .post(format!(
            "{base}/api/v1/repos/signed-owner/signed-repo/branches/protection"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "allow_force_push": true,
            "require_signed_commits": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(protection.status(), 201, "seed signed-commit protection");

    let create = client
        .post(format!(
            "{base}/api/v1/repos/signed-owner/signed-repo/contents/blocked.txt"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "content": "unsigned\n",
            "message": "unsigned contents create",
        }))
        .send()
        .await
        .unwrap();
    let create_status = create.status();
    let create_body = create.text().await.unwrap();
    assert_eq!(
        create_status, 403,
        "unsigned contents create landed: {create_body}"
    );
    assert!(
        create_body.contains("cryptographically valid signature"),
        "{create_body}"
    );

    let delete = client
        .delete(format!(
            "{base}/api/v1/repos/signed-owner/signed-repo/contents/kept.txt?message=unsigned+delete&sha={kept_sha}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let delete_status = delete.status();
    let delete_body = delete.text().await.unwrap();
    assert_eq!(
        delete_status, 403,
        "unsigned contents delete landed: {delete_body}"
    );
    assert!(
        delete_body.contains("cryptographically valid signature"),
        "{delete_body}"
    );

    server.abort();
}

async fn suggestion_policy_refusal(
    prefix: &str,
    protection: serde_json::Value,
    expected_reason: &str,
) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db.clone(), repo_root.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");
    let username = format!("{prefix}-owner");
    let email = format!("{prefix}-owner@example.com");
    let repo = format!("{prefix}-repo");
    let (token, user_id) = register_full(&base, &username, &email).await;
    let repo_id = crate::common::create_repo(&base, &token, &repo).await;
    let client = reqwest::Client::new();

    let seeded = client
        .post(format!(
            "{base}/api/v1/repos/{username}/{repo}/contents/notes.md"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "content": "first line\n",
            "message": "seed suggestion source",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(seeded.status(), 200, "seed suggestion source");
    let seeded: serde_json::Value = seeded.json().await.unwrap();
    let head_sha = seeded["commit_sha"].as_str().unwrap().to_string();

    let now = chrono::Utc::now();
    rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("server-side policy".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(user_id),
            reviewer_id: Set(None),
            head_branch: Set("main".to_string()),
            base_branch: Set("release".to_string()),
            head_sha: Set(Some(head_sha.clone())),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .unwrap();

    let create_comment = |commit: &str, replacement: &str| {
        client
            .post(format!(
                "{base}/api/v1/repos/{username}/{repo}/pulls/1/comments"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "path": "notes.md",
                "line": 1,
                "side": "RIGHT",
                "body": "use the suggestion",
                "suggestion": replacement,
                "commit_id": commit,
            }))
            .send()
    };
    let baseline_comment = create_comment(&head_sha, "baseline applied").await.unwrap();
    assert_eq!(baseline_comment.status(), 201);
    let baseline_comment: serde_json::Value = baseline_comment.json().await.unwrap();
    let baseline_id = baseline_comment["id"].as_i64().unwrap();
    let baseline = client
        .post(format!(
            "{base}/api/v1/repos/{username}/{repo}/pulls/1/comments/{baseline_id}/suggestion/apply"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let baseline_status = baseline.status();
    let baseline_body = baseline.text().await.unwrap();
    assert_eq!(
        baseline_status, 200,
        "live unprotected baseline failed: {baseline_body}"
    );
    let baseline: serde_json::Value = serde_json::from_str(&baseline_body).unwrap();
    let allowed_sha = baseline["commit_sha"].as_str().unwrap().to_string();

    let refused_comment = create_comment(&allowed_sha, "must not land").await.unwrap();
    assert_eq!(refused_comment.status(), 201);
    let refused_comment: serde_json::Value = refused_comment.json().await.unwrap();
    let refused_id = refused_comment["id"].as_i64().unwrap();

    let protected = client
        .post(format!(
            "{base}/api/v1/repos/{username}/{repo}/branches/protection"
        ))
        .bearer_auth(&token)
        .json(&protection)
        .send()
        .await
        .unwrap();
    assert_eq!(protected.status(), 201, "seed suggestion branch policy");

    let refused = client
        .post(format!(
            "{base}/api/v1/repos/{username}/{repo}/pulls/1/comments/{refused_id}/suggestion/apply"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let refused_status = refused.status();
    let refused_body = refused.text().await.unwrap();
    assert_eq!(
        refused_status, 403,
        "server-side suggestion bypassed policy: {refused_body}"
    );
    assert!(refused_body.contains(expected_reason), "{refused_body}");

    let repo_path = repo_root.join(format!("{username}/{repo}.git"));
    assert_eq!(
        rg_core::repo::service::try_get_branch_sha(&repo_path, "main")
            .unwrap()
            .as_deref(),
        Some(allowed_sha.as_str()),
        "refused suggestion changed the branch"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn require_pr_rejects_review_suggestions_after_a_live_baseline() {
    suggestion_policy_refusal(
        "suggestion-pr-policy",
        serde_json::json!({
            "branch_name": "main",
            "require_pr": true,
            "allow_force_push": true,
        }),
        "open a pull request instead",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_commit_policy_rejects_unsigned_review_suggestions() {
    suggestion_policy_refusal(
        "suggestion-sign-policy",
        serde_json::json!({
            "branch_name": "main",
            "allow_force_push": true,
            "require_signed_commits": true,
        }),
        "cryptographically valid signature",
    )
    .await;
}
