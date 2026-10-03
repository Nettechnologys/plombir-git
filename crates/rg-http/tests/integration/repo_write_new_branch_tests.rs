//! A file written to a branch that does not exist yet creates that branch —
//! and the branch has to be the default branch plus that file
//! (card_25220b69c295).
//!
//! The service reached a missing branch through `git clone --no-checkout` and
//! `git checkout -b`. That clone writes no index, so the new branch started at
//! the default branch's commit with an empty index, and the commit the edit
//! made held the one file it wrote: every other file of the repository was
//! deleted in it. A pull request from such a branch removes the whole tree.
//! Nothing noticed because every caller wrote to a branch that already existed
//! — until the MCP `write_file` tool made "start a branch with its first
//! commit" the way an agent works.

use std::path::Path;

use crate::common::{create_repo, register_full, spawn_test_app_with_repo_root};

const OWNER: &str = "branch-owner";
const REPO: &str = "branch-repo";

fn contents_url(base: &str, path: &str) -> String {
    format!("{base}/api/v1/repos/{OWNER}/{REPO}/contents/{path}")
}

async fn write(
    base: &str,
    token: &str,
    path: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let response = reqwest::Client::new()
        .post(contents_url(base, path))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("request");
    let status = response.status();
    (status, response.json().await.unwrap_or_default())
}

fn git(bare: &Path, args: &[&str]) -> String {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway");
    let output = gateway.run(args, Some(bare)).expect("git runs");
    assert!(output.success(), "git {args:?}: {}", output.stderr_str());
    output.stdout_str().trim().to_string()
}

/// A repository whose default branch `main` holds `README.md` and `docs/guide.md`.
async fn seeded(base: &str) -> String {
    let (token, _) = register_full(base, OWNER, "branch-owner@example.com").await;
    create_repo(base, &token, REPO).await;
    for (path, content) in [("README.md", "readme\n"), ("docs/guide.md", "guide\n")] {
        let (status, body) = write(
            base,
            &token,
            path,
            serde_json::json!({ "content": content, "message": format!("add {path}") }),
        )
        .await;
        assert_eq!(status, 200, "seed {path}: {body}");
    }
    token
}

#[tokio::test]
async fn a_new_branch_is_the_default_branch_plus_the_written_file() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let token = seeded(&base).await;
    let bare = repo_root.join(format!("{OWNER}/{REPO}.git"));
    let main = git(&bare, &["rev-parse", "refs/heads/main"]);

    let (status, body) = write(
        &base,
        &token,
        "src/new.rs",
        serde_json::json!({
            "branch": "topic/new", "content": "new\n", "message": "start a branch",
        }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let commit = body["commit_sha"].as_str().expect("commit sha");

    assert_eq!(
        git(&bare, &["rev-parse", &format!("{commit}^")]),
        main,
        "the branch starts at the default branch"
    );
    assert_eq!(
        git(&bare, &["ls-tree", "-r", "--name-only", commit]),
        "README.md\ndocs/guide.md\nsrc/new.rs",
        "the commit adds one file and deletes nothing"
    );
    assert_eq!(
        git(&bare, &["diff", "--name-status", &main, commit]),
        "A\tsrc/new.rs"
    );
}

/// The new branch is the default branch's tree, so that is what "already
/// exists" and "the current blob" are judged against.
#[tokio::test]
async fn a_new_branch_judges_the_file_against_the_default_branch() {
    let (base, _repo_root) = spawn_test_app_with_repo_root().await;
    let token = seeded(&base).await;

    let (status, body) = write(
        &base,
        &token,
        "README.md",
        serde_json::json!({ "branch": "topic/a", "content": "clobbered\n", "message": "m" }),
    )
    .await;
    assert_eq!(
        status, 409,
        "creating a file the new branch already carries is an overwrite: {body}"
    );

    let readme: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{OWNER}/{REPO}/blob/README.md"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    let (status, body) = write(
        &base,
        &token,
        "README.md",
        serde_json::json!({
            "branch": "topic/b", "content": "updated\n", "message": "m", "sha": readme["sha"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the default branch's blob sha updates the file on a new branch: {body}"
    );
}

/// With branches present but `HEAD` naming none of them, there is no default
/// branch to start from — and a branch with no history would carry only the
/// written file.
///
/// The repository is seeded by a push straight into its storage: a push
/// through the server would run the post-push hook, whose `adopt_unborn_head`
/// points a dangling `HEAD` at the branch the push created — detached, so it
/// would also race any `symbolic-ref` the test made afterwards.
#[tokio::test]
async fn a_new_branch_without_a_default_branch_to_start_from_is_refused() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, OWNER, "branch-owner@example.com").await;
    create_repo(&base, &token, REPO).await;
    let bare = repo_root.join(format!("{OWNER}/{REPO}.git"));
    let worktree = tempfile::tempdir().expect("tempdir");
    let work = worktree.path();
    let bare_arg = bare.to_string_lossy();
    git(work, &["init", "--initial-branch=trunk"]);
    git(work, &["config", "user.name", "Seed"]);
    git(work, &["config", "user.email", "seed@example.test"]);
    std::fs::write(work.join("README.md"), "readme\n").expect("write");
    git(work, &["add", "."]);
    git(work, &["commit", "-m", "seed"]);
    git(work, &["push", &bare_arg, "trunk"]);
    assert_eq!(git(&bare, &["symbolic-ref", "HEAD"]), "refs/heads/main");

    let (status, body) = write(
        &base,
        &token,
        "src/new.rs",
        serde_json::json!({ "branch": "topic/orphan", "content": "new\n", "message": "m" }),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway");
    let probe = gateway
        .run(
            &["rev-parse", "--verify", "refs/heads/topic/orphan"],
            Some(&bare),
        )
        .expect("git runs");
    assert!(!probe.success(), "no branch was created");
}
