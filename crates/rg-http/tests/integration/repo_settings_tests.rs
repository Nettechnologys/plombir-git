//! card_3625a7b89abb: a repository's settings can be edited after it is
//! created — description, visibility, default branch and name — through
//! `PATCH /repos/{owner}/{name}`.
//!
//! * The default branch is what `git clone` checks out, not only what the UI
//!   reads.
//! * Making a repository private closes anonymous reads on the next request.
//! * A rename keeps everything the repository holds: its issues, its release
//!   assets (blob storage keyed by `<owner>/<name>`), its Git history.

use std::path::Path;

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

async fn api(
    method: reqwest::Method,
    url: String,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut request = reqwest::Client::new().request(method, url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::json!(text)),
    )
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .unwrap()
        .run_with_env(
            args,
            Some(cwd),
            &[
                ("GIT_AUTHOR_NAME", "settings"),
                ("GIT_AUTHOR_EMAIL", "settings@example.com"),
                ("GIT_COMMITTER_NAME", "settings"),
                ("GIT_COMMITTER_EMAIL", "settings@example.com"),
            ],
        )
        .unwrap();
    assert!(
        output.success(),
        "git {args:?}: {}{}",
        output.stdout_str(),
        output.stderr_str()
    );
    output.stdout_str().trim().to_string()
}

async fn pat_for(base: &str, session: &str) -> String {
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/users/tokens"),
        Some(session),
        Some(serde_json::json!({ "name": "git", "scopes": "repo" })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    body["token"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repository_settings_are_editable_and_take_effect_where_they_are_read() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, "rs_owner", "rs_owner@example.com").await;
    let (writer, _) = register_full(&base, "rs_writer", "rs_writer@example.com").await;
    let pat = pat_for(&base, &owner).await;
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        Some(&owner),
        Some(serde_json::json!({
            "name": "proj", "auto_init": true, "readme": "default", "description": "first"
        })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let repo_api = |name: &str| format!("{base}/api/v1/repos/rs_owner/{name}");
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{}/collaborators", repo_api("proj")),
        Some(&owner),
        Some(serde_json::json!({ "username": "rs_writer", "permission": "write" })),
    )
    .await;
    assert!(status == 200 || status == 201, "{status} {body}");

    // A branch to make the default, pushed over HTTP.
    let root = tempfile::tempdir().unwrap();
    let address = base.trim_start_matches("http://").to_string();
    let root_path = root.path().to_path_buf();
    let pat_for_git = pat.clone();
    tokio::task::spawn_blocking(move || {
        let url = format!("http://rs_owner:{pat_for_git}@{address}/git/rs_owner/proj");
        git(&root_path, &["clone", "-q", &url, "work"]);
        let work = root_path.join("work");
        git(&work, &["checkout", "-q", "-b", "dev"]);
        std::fs::write(work.join("dev.txt"), "dev\n").unwrap();
        git(&work, &["add", "dev.txt"]);
        git(&work, &["commit", "-q", "-m", "dev"]);
        git(&work, &["push", "-q", "origin", "dev"]);
    })
    .await
    .unwrap();

    // What each caller may do is told to the client that draws the page.
    for (token, expected) in [
        (Some(owner.as_str()), serde_json::json!("admin")),
        (Some(writer.as_str()), serde_json::json!("write")),
        (None, serde_json::Value::Null),
    ] {
        let (status, body) = api(reqwest::Method::GET, repo_api("proj"), token, None).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["viewer_permission"], expected, "{body}");
    }

    // Only an administrator edits settings.
    let (status, body) = api(
        reqwest::Method::PATCH,
        repo_api("proj"),
        Some(&writer),
        Some(serde_json::json!({ "description": "mine now" })),
    )
    .await;
    assert_eq!(status, 403, "{body}");

    // Description: set, then cleared.
    let (status, body) = api(
        reqwest::Method::PATCH,
        repo_api("proj"),
        Some(&owner),
        Some(serde_json::json!({ "description": "second" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["description"], "second");
    let (status, body) = api(
        reqwest::Method::PATCH,
        repo_api("proj"),
        Some(&owner),
        Some(serde_json::json!({ "description": null })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body["description"].is_null(), "{body}");

    // Default branch: an unknown one is refused; an existing one moves HEAD.
    let (status, body) = api(
        reqwest::Method::PATCH,
        repo_api("proj"),
        Some(&owner),
        Some(serde_json::json!({ "default_branch": "no-such-branch" })),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = api(
        reqwest::Method::PATCH,
        repo_api("proj"),
        Some(&owner),
        Some(serde_json::json!({ "default_branch": "dev" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["default_branch"], "dev");
    let bare = repo_root.join("rs_owner/proj.git");
    assert_eq!(git(&bare, &["symbolic-ref", "HEAD"]), "refs/heads/dev");
    let root_path = root.path().to_path_buf();
    let address = base.trim_start_matches("http://").to_string();
    let pat_for_git = pat.clone();
    let checked_out = tokio::task::spawn_blocking(move || {
        let url = format!("http://rs_owner:{pat_for_git}@{address}/git/rs_owner/proj");
        git(&root_path, &["clone", "-q", &url, "fresh"]);
        git(
            &root_path.join("fresh"),
            &["rev-parse", "--abbrev-ref", "HEAD"],
        )
    })
    .await
    .unwrap();
    assert_eq!(
        checked_out, "dev",
        "a clone checks out the new default branch"
    );

    // Visibility: anonymous readers are shut out on the very next request.
    let (status, _) = api(reqwest::Method::GET, repo_api("proj"), None, None).await;
    assert_eq!(status, 200);
    let (status, body) = api(
        reqwest::Method::PATCH,
        repo_api("proj"),
        Some(&owner),
        Some(serde_json::json!({ "is_private": true })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, _) = api(reqwest::Method::GET, repo_api("proj"), None, None).await;
    assert!(
        status == 404 || status == 401,
        "a private repository answered {status}"
    );

    // Rename: the issue, the release asset and the history all come along.
    let (status, issue) = api(
        reqwest::Method::POST,
        format!("{}/issues", repo_api("proj")),
        Some(&owner),
        Some(serde_json::json!({ "title": "stays with the repository" })),
    )
    .await;
    assert_eq!(status, 201, "{issue}");
    let (status, release) = api(
        reqwest::Method::POST,
        format!("{}/releases", repo_api("proj")),
        Some(&owner),
        Some(serde_json::json!({ "tag_name": "keep-1", "title": "Keep" })),
    )
    .await;
    assert_eq!(status, 201, "{release}");
    let uploaded = reqwest::Client::new()
        .post(format!(
            "{}/releases/{}/assets",
            repo_api("proj"),
            release["id"]
        ))
        .bearer_auth(&owner)
        .header("content-type", "text/plain")
        .header("content-disposition", "attachment; filename=notes.txt")
        .body("release asset")
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), 201);
    let asset: serde_json::Value = uploaded.json().await.unwrap();

    let (status, body) = api(
        reqwest::Method::PATCH,
        repo_api("proj"),
        Some(&owner),
        Some(serde_json::json!({ "name": "bad name!" })),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    let (status, renamed) = api(
        reqwest::Method::PATCH,
        repo_api("proj"),
        Some(&owner),
        Some(serde_json::json!({ "name": "renamed" })),
    )
    .await;
    assert_eq!(status, 200, "{renamed}");
    assert_eq!(renamed["name"], "renamed");

    // The old name leads to the renamed repository (card_e83bf21a5e5b;
    // `repo_redirect_tests` pins the redirect itself).
    let (status, followed) = api(reqwest::Method::GET, repo_api("proj"), Some(&owner), None).await;
    assert_eq!(status, 200, "{followed}");
    assert_eq!(followed["name"], "renamed");
    let (status, body) = api(
        reqwest::Method::GET,
        format!("{}/issues/{}", repo_api("renamed"), issue["number"]),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["title"], "stays with the repository");
    let downloaded = reqwest::Client::new()
        .get(format!(
            "{}/releases/assets/{}/download",
            repo_api("renamed"),
            asset["id"]
        ))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), 200);
    assert_eq!(downloaded.text().await.unwrap(), "release asset");
    assert!(!repo_root.join("rs_owner/proj.git").exists());
    assert_eq!(
        git(
            &repo_root.join("rs_owner/renamed.git"),
            &["symbolic-ref", "HEAD"]
        ),
        "refs/heads/dev"
    );

    // A name that is taken in the namespace is refused, and nothing moves.
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        Some(&owner),
        Some(serde_json::json!({ "name": "taken" })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = api(
        reqwest::Method::PATCH,
        repo_api("renamed"),
        Some(&owner),
        Some(serde_json::json!({ "name": "taken" })),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert!(repo_root.join("rs_owner/renamed.git").exists());
}
