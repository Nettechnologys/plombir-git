//! A pull request's head before and after it exists.
//!
//! * `GET /compare?base=&head=` previews what a pull request would bring — the
//!   commits and the diff — for a branch of the repository and for a branch of
//!   a fork (card_87f9b1c97489).
//! * A fork is a repository of its own. Reading its parent is no licence to see
//!   a private fork: the fork list, the comparison and the creation of a pull
//!   request all ask the fork's own read gate (card_bb2ef2307588).
//! * "Delete head branch after merge" removes the merged branch, and keeps one
//!   another open pull request still reads (card_2060696224ff).

use std::path::{Path, PathBuf};

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

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

async fn api(
    method: reqwest::Method,
    url: String,
    token: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut request = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token);
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

async fn create_initialised_repo(base: &str, session: &str, name: &str, private: bool) {
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        session,
        Some(serde_json::json!({
            "name": name, "is_private": private, "auto_init": true, "readme": "default"
        })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
}

fn git_ok(cwd: &Path, who: &str, args: &[&str]) -> String {
    let email = format!("{who}@example.com");
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run_with_env(
            args,
            Some(cwd),
            &[
                ("GIT_AUTHOR_NAME", who),
                ("GIT_AUTHOR_EMAIL", &email),
                ("GIT_COMMITTER_NAME", who),
                ("GIT_COMMITTER_EMAIL", &email),
            ],
        )
        .expect("run git");
    assert!(
        output.success(),
        "{who} ran git {args:?}: {}{}",
        output.stdout_str(),
        output.stderr_str()
    );
    output.stdout_str().trim().to_string()
}

/// Clone `owner/repo` as `who`, commit `files` on a new `branch` and push it.
#[allow(clippy::too_many_arguments)]
async fn push_branch(
    root: PathBuf,
    base: String,
    who: &'static str,
    pat: String,
    owner: &'static str,
    repo: &'static str,
    branch: &'static str,
    files: &'static [&'static str],
) {
    tokio::task::spawn_blocking(move || {
        let address = base.trim_start_matches("http://");
        let url = format!("http://{who}:{pat}@{address}/git/{owner}/{repo}");
        let dir = format!("{who}-{owner}-{repo}-{}", branch.replace('/', "-"));
        git_ok(&root, who, &["clone", "-q", &url, &dir]);
        let work = root.join(dir);
        git_ok(&work, who, &["checkout", "-q", "-b", branch]);
        for file in files {
            std::fs::write(work.join(file), format!("{file}\n")).unwrap();
            git_ok(&work, who, &["add", file]);
            git_ok(&work, who, &["commit", "-q", "-m", file]);
        }
        git_ok(&work, who, &["push", "-q", "origin", branch]);
    })
    .await
    .expect("git task");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compare_previews_a_branch_and_a_fork_branch() {
    let (base, _db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (alice, _) = register_full(&base, "cmp_alice", "cmp_alice@example.com").await;
    let (bob, _) = register_full(&base, "cmp_bob", "cmp_bob@example.com").await;
    let alice_pat = pat_for(&base, &alice).await;
    let bob_pat = pat_for(&base, &bob).await;
    create_initialised_repo(&base, &alice, "proj", false).await;
    let root = tempfile::tempdir().unwrap();
    push_branch(
        root.path().to_path_buf(),
        base.clone(),
        "cmp_alice",
        alice_pat,
        "cmp_alice",
        "proj",
        "feature",
        &["a.txt", "b.txt"],
    )
    .await;

    let compare =
        |head: &str| format!("{base}/api/v1/repos/cmp_alice/proj/compare?base=main&head={head}");
    let (status, body) = api(reqwest::Method::GET, compare("feature"), &bob, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total_commits"], 2, "{body}");
    assert_eq!(
        body["commits"][0]["message"], "a.txt",
        "oldest first: {body}"
    );
    assert_eq!(body["stats"]["files_changed"], 2, "{body}");
    assert!(body["merge_base_sha"]
        .as_str()
        .is_some_and(|sha| sha.len() == 40));

    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/cmp_alice/proj/fork"),
        &bob,
        Some(serde_json::json!({})),
    )
    .await;
    assert!(status == 201 || status == 200, "{status}: {body}");
    push_branch(
        root.path().to_path_buf(),
        base.clone(),
        "cmp_bob",
        bob_pat,
        "cmp_bob",
        "proj",
        "fork-feature",
        &["c.txt"],
    )
    .await;
    let (status, body) = api(
        reqwest::Method::GET,
        compare("cmp_bob:fork-feature"),
        &alice,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total_commits"], 1, "{body}");
    assert_eq!(body["files_changed"][0]["path"], "c.txt", "{body}");

    for head in ["no-such-branch", "cmp_bob:no-such-branch", "nobody:feature"] {
        let (status, body) = api(reqwest::Method::GET, compare(head), &alice, None).await;
        assert_eq!(status, 400, "{head}: {body}");
    }
}

/// card_bb2ef2307588: a collaborator of a private parent is not a reader of
/// somebody else's private fork of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_private_fork_stays_private_to_readers_of_its_parent() {
    let (base, _db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (carol, _) = register_full(&base, "pf_carol", "pf_carol@example.com").await;
    let (dave, _) = register_full(&base, "pf_dave", "pf_dave@example.com").await;
    let (erin, _) = register_full(&base, "pf_erin", "pf_erin@example.com").await;
    let erin_pat = pat_for(&base, &erin).await;
    create_initialised_repo(&base, &carol, "secret", true).await;
    for (who, permission) in [("pf_dave", "read"), ("pf_erin", "write")] {
        let (status, body) = api(
            reqwest::Method::POST,
            format!("{base}/api/v1/repos/pf_carol/secret/collaborators"),
            &carol,
            Some(serde_json::json!({ "username": who, "permission": permission })),
        )
        .await;
        assert!(status == 201 || status == 200, "{who}: {status} {body}");
    }
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/pf_carol/secret/fork"),
        &erin,
        Some(serde_json::json!({})),
    )
    .await;
    assert!(status == 201 || status == 200, "{status}: {body}");
    let root = tempfile::tempdir().unwrap();
    push_branch(
        root.path().to_path_buf(),
        base.clone(),
        "pf_erin",
        erin_pat,
        "pf_erin",
        "secret",
        "hidden",
        &["private.txt"],
    )
    .await;

    // The fork's owner sees it; the parent's reader does not.
    let forks = format!("{base}/api/v1/repos/pf_carol/secret/forks");
    let (status, body) = api(reqwest::Method::GET, forks.clone(), &erin, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["pagination"]["total"], 1, "{body}");
    let (status, body) = api(reqwest::Method::GET, forks, &dave, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["pagination"]["total"], 0,
        "the parent's reader was shown a private fork: {body}"
    );

    let (status, body) = api(
        reqwest::Method::GET,
        format!("{base}/api/v1/repos/pf_carol/secret/compare?base=main&head=pf_erin:hidden"),
        &dave,
        None,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(!body.to_string().contains("private.txt"), "{body}");

    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/pf_carol/secret/pulls"),
        &dave,
        Some(serde_json::json!({ "title": "t", "head": "pf_erin:hidden", "base": "main" })),
    )
    .await;
    assert_eq!(
        status, 400,
        "a pull request was opened off an unreadable fork: {body}"
    );

    // The fork's owner may still compare and open the pull request.
    let (status, body) = api(
        reqwest::Method::GET,
        format!("{base}/api/v1/repos/pf_carol/secret/compare?base=main&head=pf_erin:hidden"),
        &erin,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

/// card_2060696224ff: the merge form's "delete head branch" removes the merged
/// branch — and keeps one another open pull request still reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_merge_deletes_its_head_branch_only_when_nothing_else_reads_it() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, "dh_owner", "dh_owner@example.com").await;
    let pat = pat_for(&base, &owner).await;
    create_initialised_repo(&base, &owner, "proj", false).await;
    let root = tempfile::tempdir().unwrap();
    push_branch(
        root.path().to_path_buf(),
        base.clone(),
        "dh_owner",
        pat.clone(),
        "dh_owner",
        "proj",
        "solo",
        &["solo.txt"],
    )
    .await;
    push_branch(
        root.path().to_path_buf(),
        base.clone(),
        "dh_owner",
        pat,
        "dh_owner",
        "proj",
        "shared",
        &["shared.txt"],
    )
    .await;
    let pulls = format!("{base}/api/v1/repos/dh_owner/proj/pulls");
    for (head, base_branch) in [("solo", "main"), ("shared", "main"), ("shared", "solo")] {
        let (status, body) = api(
            reqwest::Method::POST,
            pulls.clone(),
            &owner,
            Some(serde_json::json!({ "title": head, "head": head, "base": base_branch })),
        )
        .await;
        assert_eq!(status, 201, "{body}");
    }
    let bare = repo_root.join("dh_owner/proj.git");
    let exists = |branch: &str| {
        rg_git::cli_gateway::global_gateway()
            .as_ref()
            .unwrap()
            .run(
                &[
                    "rev-parse",
                    "--verify",
                    "-q",
                    &format!("refs/heads/{branch}"),
                ],
                Some(&bare),
            )
            .unwrap()
            .success()
    };

    let (status, body) = api(
        reqwest::Method::POST,
        format!("{pulls}/1/merge"),
        &owner,
        Some(serde_json::json!({ "strategy": "merge", "delete_head_branch": false })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.get("head_branch_deleted").is_none(), "{body}");
    assert!(exists("solo"), "the branch went without being asked");

    let (status, body) = api(
        reqwest::Method::POST,
        format!("{pulls}/2/merge"),
        &owner,
        Some(serde_json::json!({ "strategy": "merge", "delete_head_branch": true })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["head_branch_deleted"], false, "{body}");
    assert!(
        body["head_branch_kept"]
            .as_str()
            .is_some_and(|reason| reason.contains("other open pull request")),
        "{body}"
    );
    assert!(exists("shared"));

    let (status, body) = api(
        reqwest::Method::POST,
        format!("{pulls}/3/merge"),
        &owner,
        Some(serde_json::json!({ "strategy": "merge", "delete_head_branch": true })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["head_branch_deleted"], true, "{body}");
    assert!(!exists("shared"), "the merged head branch is still there");
}
