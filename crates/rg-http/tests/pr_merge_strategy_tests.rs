//! End-to-end coverage for ordinary PR merge strategies and conflict recovery.

mod common;

use std::path::Path;

use common::{build_test_app_state, register_full, setup_test_db};

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

fn configure_worktree(path: &Path) {
    git(&["config", "user.name", "Merge Test"], Some(path));
    git(
        &["config", "user.email", "merge-test@example.com"],
        Some(path),
    );
}

fn seed_diverged_repository(bare_path: &Path) -> (tempfile::TempDir, String) {
    let worktree = tempfile::tempdir().unwrap();
    let path = worktree.path();
    let path_arg = path.to_string_lossy();
    git(&["init", "--initial-branch=main", &path_arg], None);
    configure_worktree(path);

    std::fs::write(path.join("README.md"), "base\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "base"], Some(path));
    let bare_arg = bare_path.to_string_lossy();
    git(&["remote", "add", "origin", &bare_arg], Some(path));
    git(&["push", "origin", "main"], Some(path));

    git(&["checkout", "-b", "feature"], Some(path));
    std::fs::write(path.join("feature-a.txt"), "feature a\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "feature a"], Some(path));
    std::fs::write(path.join("feature-b.txt"), "feature b\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "feature b"], Some(path));
    git(&["push", "origin", "feature"], Some(path));

    git(&["checkout", "main"], Some(path));
    std::fs::write(path.join("base-only.txt"), "advanced base\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "advance base"], Some(path));
    git(&["push", "origin", "main"], Some(path));
    let base_sha = git(&["rev-parse", "refs/heads/main"], Some(bare_path));

    (worktree, base_sha)
}

fn seed_conflicting_repository(bare_path: &Path) -> (tempfile::TempDir, String) {
    let worktree = tempfile::tempdir().unwrap();
    let path = worktree.path();
    let path_arg = path.to_string_lossy();
    git(&["init", "--initial-branch=main", &path_arg], None);
    configure_worktree(path);

    std::fs::write(path.join("conflict.txt"), "base\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "base"], Some(path));
    let bare_arg = bare_path.to_string_lossy();
    git(&["remote", "add", "origin", &bare_arg], Some(path));
    git(&["push", "origin", "main"], Some(path));

    git(&["checkout", "-b", "feature"], Some(path));
    std::fs::write(path.join("conflict.txt"), "feature\n").unwrap();
    git(&["commit", "-am", "feature conflict"], Some(path));
    git(&["push", "origin", "feature"], Some(path));

    git(&["checkout", "main"], Some(path));
    std::fs::write(path.join("conflict.txt"), "main\n").unwrap();
    git(&["commit", "-am", "base conflict"], Some(path));
    git(&["push", "origin", "main"], Some(path));
    let base_sha = git(&["rev-parse", "refs/heads/main"], Some(bare_path));

    (worktree, base_sha)
}

async fn create_repo_and_pr(base: &str, token: &str, repo: &str) {
    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": repo, "is_private": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "{}", created.text().await.unwrap());
}

async fn open_pr(base: &str, token: &str, repo: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/merge-owner/{repo}/pulls"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "title": "Merge the feature",
            "head": "feature",
            "base": "main"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
}

#[tokio::test]
async fn merge_squash_and_rebase_update_refs_and_pr_state() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db, repo_root.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let (token, _) = register_full(&base, "merge-owner", "merge-owner@example.com").await;
    let client = reqwest::Client::new();

    for (strategy, expected_new_commits, expected_parent_fields) in
        [("merge", 3, 3), ("squash", 1, 2), ("rebase", 2, 2)]
    {
        let repo = format!("{strategy}-repo");
        create_repo_and_pr(&base, &token, &repo).await;
        let bare_path = repo_root.join(format!("merge-owner/{repo}.git"));
        let (_worktree, base_sha) = seed_diverged_repository(&bare_path);
        open_pr(&base, &token, &repo).await;

        let merged = client
            .post(format!(
                "{base}/api/v1/repos/merge-owner/{repo}/pulls/1/merge"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"strategy": strategy}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            merged.status(),
            200,
            "{strategy} failed: {}",
            merged.text().await.unwrap()
        );
        let merged = merged.json::<serde_json::Value>().await.unwrap();
        assert_eq!(merged["strategy"], strategy);

        let main_sha = git(&["rev-parse", "refs/heads/main"], Some(&bare_path));
        assert_eq!(merged["merge_commit_sha"], main_sha);
        let parents = git(
            &["rev-list", "--parents", "-n", "1", "refs/heads/main"],
            Some(&bare_path),
        );
        assert_eq!(parents.split_whitespace().count(), expected_parent_fields);
        let new_commit_count = git(
            &[
                "rev-list",
                "--count",
                &format!("{base_sha}..refs/heads/main"),
            ],
            Some(&bare_path),
        );
        assert_eq!(
            new_commit_count.parse::<usize>().unwrap(),
            expected_new_commits
        );
        assert_eq!(
            git(&["show", "refs/heads/main:feature-a.txt"], Some(&bare_path)),
            "feature a"
        );
        assert_eq!(
            git(&["show", "refs/heads/main:feature-b.txt"], Some(&bare_path)),
            "feature b"
        );

        let pr = client
            .get(format!("{base}/api/v1/repos/merge-owner/{repo}/pulls/1"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(pr["state"], "merged");
        assert_eq!(pr["merge_strategy"], strategy);
        assert_eq!(pr["merge_commit_sha"], main_sha);
    }

    server.abort();
}

#[tokio::test]
async fn merge_conflict_keeps_base_ref_and_restores_open_pr_state() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db, repo_root.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let (token, _) = register_full(&base, "merge-owner", "merge-owner@example.com").await;
    create_repo_and_pr(&base, &token, "conflict-repo").await;
    let bare_path = repo_root.join("merge-owner/conflict-repo.git");
    let (_worktree, base_sha) = seed_conflicting_repository(&bare_path);
    open_pr(&base, &token, "conflict-repo").await;

    let client = reqwest::Client::new();
    let failed = client
        .post(format!(
            "{base}/api/v1/repos/merge-owner/conflict-repo/pulls/1/merge"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), 400);
    assert!(failed
        .text()
        .await
        .unwrap()
        .to_lowercase()
        .contains("conflict"));
    assert_eq!(
        git(&["rev-parse", "refs/heads/main"], Some(&bare_path)),
        base_sha
    );

    let pr = client
        .get(format!(
            "{base}/api/v1/repos/merge-owner/conflict-repo/pulls/1"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(pr["state"], "open");
    assert!(pr["merge_strategy"].is_null());
    assert!(pr["merge_commit_sha"].is_null());

    server.abort();
}
