//! `GET /repos/{owner}/{name}/commits/{sha}/diff` — what one commit changed.
//!
//! The commit page showed a commit's message, author and checks and nothing of
//! its content: no route answered "which files did this commit touch", and
//! `compare` takes branch names only (card_8c46bad6d61e).

use std::path::Path;

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

async fn get(url: String, token: Option<&str>) -> (u16, serde_json::Value) {
    let mut request = reqwest::Client::new().get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::json!(text)),
    )
}

async fn pat_for(base: &str, session: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(session)
        .json(&serde_json::json!({ "name": "git", "scopes": "repo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn create_initialised_repo(base: &str, session: &str, name: &str, private: bool) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(session)
        .json(&serde_json::json!({
            "name": name, "is_private": private, "auto_init": true, "readme": "default"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
}

fn git_ok(cwd: &Path, args: &[&str]) -> String {
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run_with_env(
            args,
            Some(cwd),
            &[
                ("GIT_AUTHOR_NAME", "diff_alice"),
                ("GIT_AUTHOR_EMAIL", "diff_alice@example.com"),
                ("GIT_COMMITTER_NAME", "diff_alice"),
                ("GIT_COMMITTER_EMAIL", "diff_alice@example.com"),
            ],
        )
        .expect("run git");
    assert!(
        output.success(),
        "git {args:?}: {}{}",
        output.stdout_str(),
        output.stderr_str()
    );
    output.stdout_str().trim().to_string()
}

fn file<'a>(diff: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    diff["files_changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == path)
        .unwrap_or_else(|| panic!("{path} is not in {diff}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commit_lists_the_files_it_changed_against_its_first_parent() {
    let (base, _db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (alice, _) = register_full(&base, "diff_alice", "diff_alice@example.com").await;
    let pat = pat_for(&base, &alice).await;
    create_initialised_repo(&base, &alice, "assets", false).await;

    // One commit on top of the initial one: a text edit and a binary blob.
    let work_root = tempfile::tempdir().unwrap();
    let address = base.trim_start_matches("http://").to_string();
    let (root_sha, sha, tree_sha) = tokio::task::spawn_blocking(move || {
        let url = format!("http://diff_alice:{pat}@{address}/git/diff_alice/assets");
        git_ok(work_root.path(), &["clone", "-q", &url, "work"]);
        let work = work_root.path().join("work");
        let root_sha = git_ok(&work, &["rev-parse", "HEAD"]);
        let readme = std::fs::read_to_string(work.join("README.md")).unwrap();
        std::fs::write(work.join("README.md"), format!("{readme}one more line\n")).unwrap();
        std::fs::write(work.join("texture.bin"), [0u8, 1, 2, 255, 0, 7]).unwrap();
        git_ok(&work, &["add", "README.md", "texture.bin"]);
        git_ok(&work, &["commit", "-q", "-m", "textures"]);
        git_ok(&work, &["push", "-q", "origin", "HEAD"]);
        let sha = git_ok(&work, &["rev-parse", "HEAD"]);
        let tree_sha = git_ok(&work, &["rev-parse", "HEAD^{tree}"]);
        (root_sha, sha, tree_sha)
    })
    .await
    .unwrap();

    let diff_url = |sha: &str| format!("{base}/api/v1/repos/diff_alice/assets/commits/{sha}/diff");

    let (status, diff) = get(diff_url(&sha), Some(&alice)).await;
    assert_eq!(status, 200, "{diff}");
    assert_eq!(diff["sha"], sha);
    assert_eq!(diff["parent_sha"], root_sha);
    assert_eq!(diff["stats"]["files_changed"], 2, "{diff}");

    let readme = file(&diff, "README.md");
    assert_eq!(readme["status"], "modified");
    assert_eq!(readme["additions"], 1);
    assert_eq!(readme["deletions"], 0);
    let added: Vec<&str> = readme["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|line| line["kind"] == "addition")
        .map(|line| line["content"].as_str().unwrap())
        .collect();
    assert_eq!(added, ["one more line"]);

    // The browser marks a binary file by git's own `Binary files … differ`.
    let texture = file(&diff, "texture.bin");
    assert_eq!(texture["status"], "added");
    assert!(
        texture["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line["kind"] == "meta"
                && line["content"]
                    .as_str()
                    .unwrap()
                    .starts_with("Binary files ")),
        "{texture}"
    );

    // An abbreviated id names the same commit, and anonymous readers of a
    // public repository see it too.
    let (status, short) = get(diff_url(&sha[..10]), None).await;
    assert_eq!(status, 200, "{short}");
    assert_eq!(short["sha"], sha);

    // A root commit has no parent: everything it holds is added.
    let (status, root) = get(diff_url(&root_sha), Some(&alice)).await;
    assert_eq!(status, 200, "{root}");
    assert_eq!(root["parent_sha"], serde_json::Value::Null);
    assert_eq!(file(&root, "README.md")["status"], "added");
    assert!(file(&root, "README.md")["additions"].as_i64().unwrap() > 0);

    // Not a commit: a malformed id is the client's mistake, an unknown one and
    // a tree's id are missing commits.
    assert_eq!(get(diff_url("not-hex"), Some(&alice)).await.0, 400);
    assert_eq!(
        get(diff_url("deadbeefdeadbeefdeadbeef"), Some(&alice))
            .await
            .0,
        404
    );
    assert_eq!(get(diff_url(&tree_sha), Some(&alice)).await.0, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_private_commit_diff_is_not_served_to_an_outsider() {
    let (base, _db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (alice, _) = register_full(&base, "pdiff_alice", "pdiff_alice@example.com").await;
    let (mallory, _) = register_full(&base, "pdiff_mallory", "pdiff_mallory@example.com").await;
    create_initialised_repo(&base, &alice, "secret", true).await;

    let (status, log) = get(
        format!("{base}/api/v1/repos/pdiff_alice/secret/log"),
        Some(&alice),
    )
    .await;
    assert_eq!(status, 200, "{log}");
    let sha = log["commits"][0]["sha"].as_str().unwrap().to_string();
    let url = format!("{base}/api/v1/repos/pdiff_alice/secret/commits/{sha}/diff");

    assert_eq!(get(url.clone(), Some(&alice)).await.0, 200);
    // The same refusal the commit's signature and statuses give an outsider.
    let (status, body) = get(url.clone(), Some(&mallory)).await;
    assert_eq!(status, 403, "{body}");
    let (status, body) = get(url, None).await;
    assert_ne!(status, 200, "{body}");
}
