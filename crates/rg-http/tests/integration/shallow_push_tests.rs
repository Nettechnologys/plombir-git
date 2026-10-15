//! A push from a shallow clone, with a stock `git`, over smart HTTP
//! (card_3741851bba0b).
//!
//! CI runners and agents clone shallow (`actions/checkout` fetches one commit
//! by default). Such a client writes `shallow <oid>` lines ahead of its
//! commands, and the server used to read its capabilities off that first line
//! instead of the first command: it concluded the client wanted no
//! report-status, moved the ref and closed the stream, and git answered
//! `unexpected disconnect while reading sideband packet`, exit 128 — a push
//! that landed, reported as one that failed.

use std::path::Path;

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

const WHO: &str = "shallow_pusher";

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

async fn create_initialised_repo(base: &str, session: &str, name: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(session)
        .json(&serde_json::json!({
            "name": name, "is_private": false, "auto_init": true, "readme": "default"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
}

/// `git` through the sanctioned gateway, with an identity: success, stdout
/// and stderr, without asserting either way.
fn git(cwd: &Path, args: &[&str]) -> (bool, String, String) {
    let email = format!("{WHO}@example.com");
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run_with_env(
            args,
            Some(cwd),
            &[
                ("GIT_AUTHOR_NAME", WHO),
                ("GIT_AUTHOR_EMAIL", &email),
                ("GIT_COMMITTER_NAME", WHO),
                ("GIT_COMMITTER_EMAIL", &email),
            ],
        )
        .expect("run git");
    (
        output.success(),
        output.stdout_str().trim().to_string(),
        output.stderr_str().to_string(),
    )
}

fn git_ok(cwd: &Path, args: &[&str]) -> String {
    let (ok, stdout, stderr) = git(cwd, args);
    assert!(ok, "git {args:?}: {stdout}{stderr}");
    stdout
}

fn commit(work: &Path, name: &str) {
    std::fs::write(work.join(name), format!("{name}\n")).unwrap();
    git_ok(work, &["add", name]);
    git_ok(work, &["commit", "-q", "-m", name]);
}

/// A push git reports as delivered: exit 0, and the ref line it can only
/// print from the server's report-status.
fn assert_reported(outcome: &(bool, String, String), ref_line: &str, what: &str) {
    let (ok, stdout, stderr) = outcome;
    assert!(ok, "{what} failed: {stdout}{stderr}");
    assert!(
        !stderr.contains("unexpected disconnect") && !stderr.contains("hung up"),
        "{what}: {stderr}"
    );
    assert!(
        stderr.contains(ref_line),
        "{what}: no `{ref_line}` in {stderr}"
    );
}

/// A push the server refused, told as a refusal: git's `! [remote rejected]`
/// with the server's reason, never a dropped connection.
fn assert_rejected(outcome: &(bool, String, String), reason: &str, what: &str) {
    let (ok, stdout, stderr) = outcome;
    assert!(!ok, "{what} was accepted: {stdout}{stderr}");
    assert!(
        !stderr.contains("unexpected disconnect") && !stderr.contains("hung up"),
        "{what} ended in a dropped connection instead of a report: {stderr}"
    );
    assert!(stderr.contains("! [remote rejected]"), "{what}: {stderr}");
    assert!(stderr.contains(reason), "{what}: no `{reason}` in {stderr}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shallow_clone_pushes_with_a_report_and_is_refused_with_one() {
    let (base, _db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (session, _) = register_full(&base, WHO, &format!("{WHO}@example.com")).await;
    let pat = pat_for(&base, &session).await;
    create_initialised_repo(&base, &session, "deep").await;
    create_initialised_repo(&base, &session, "other").await;
    let address = base.trim_start_matches("http://").to_string();
    let url = move |repo: &str| format!("http://{WHO}:{pat}@{address}/git/{WHO}/{repo}");

    tokio::task::spawn_blocking(move || {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();

        // History deep enough that a depth-1 clone is cut short.
        for repo in ["deep", "other"] {
            git_ok(root, &["clone", "-q", &url(repo), &format!("full-{repo}")]);
            let full = root.join(format!("full-{repo}"));
            for index in 0..3 {
                commit(&full, &format!("{repo}-history-{index}.txt"));
            }
            git_ok(&full, &["push", "-q", "origin", "HEAD"]);
        }

        let shallow = root.join("shallow");
        git_ok(
            root,
            &[
                "clone",
                "-q",
                "--depth",
                "1",
                &url("deep"),
                &shallow.to_string_lossy(),
            ],
        );
        assert!(
            shallow.join(".git/shallow").is_file(),
            "the clone must be shallow, or the push sends no shallow lines"
        );
        commit(&shallow, "from-a-shallow-clone.txt");
        let pushed = git_ok(&shallow, &["rev-parse", "HEAD"]);

        // A new branch: the server already has the boundary's history.
        let outcome = git(&shallow, &["push", "origin", "HEAD:refs/heads/probe"]);
        assert_reported(&outcome, "[new branch]", "push of a new branch");
        let remote = git_ok(&shallow, &["ls-remote", "origin", "refs/heads/probe"]);
        assert!(remote.starts_with(&pushed), "{remote}");

        // A deletion sends shallow lines and no pack.
        let outcome = git(&shallow, &["push", "origin", "--delete", "probe"]);
        assert_reported(&outcome, "[deleted]", "deletion");

        // A ref the server refuses on its own grounds.
        let outcome = git(&shallow, &["push", "origin", "HEAD:refs/forks/x"]);
        assert_rejected(
            &outcome,
            rg_git::protocol::receive_pack::SERVER_OWNED_NAMESPACE,
            "push into a server-owned namespace",
        );

        // A shallow history the server has never seen: its boundary's parents
        // are nowhere, so the ref could never be cloned whole.
        let foreign = root.join("foreign");
        git_ok(
            root,
            &[
                "clone",
                "-q",
                "--depth",
                "1",
                &url("other"),
                &foreign.to_string_lossy(),
            ],
        );
        git_ok(&foreign, &["remote", "add", "deep", &url("deep")]);
        let outcome = git(&foreign, &["push", "deep", "HEAD:refs/heads/foreign"]);
        assert_rejected(
            &outcome,
            "shallow update not allowed",
            "push of an unknown shallow history",
        );
        let remote = git_ok(&shallow, &["ls-remote", "origin", "refs/heads/foreign"]);
        assert!(remote.is_empty(), "the refused ref landed: {remote}");
    })
    .await
    .expect("git task");
}
