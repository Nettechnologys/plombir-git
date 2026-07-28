//! Regression coverage for card_1bf4936f126b: `download_archive` declared every
//! `git archive` failure the client's fault.
//!
//! The handler's only error arm was `Err(_) => bad_request("Invalid ref or
//! SHA")`, so the error value was dropped rather than converted. A bare
//! repository that will not open, a missing object database, a full disk — all
//! of them answered `400 Invalid ref or SHA`, and the client went off to "fix" a
//! ref that was fine while nothing at all reached the operator log.
//!
//! Unlike its neighbours the endpoint really does take its tree-ish from the
//! caller and does not validate it beforehand, so "bad ref → 400" is an honest
//! outcome. What had to go was its *unconditionality*.
//!
//! The break has to happen to the repository *on disk*: closing the database
//! pool fails the authentication lookup first, so the request would never reach
//! the git layer these tests are about.

use std::path::Path;

use crate::common::{create_repo, register_full, spawn_test_app_with_repo_root};

/// Make the bare repository unopenable while leaving its directory in place.
/// The missing-directory case is covered by the differential failure sweep;
/// this fixture keeps exercising the sibling "directory exists but is not a
/// usable git repository" path.
///
/// Without `objects/` git no longer recognises the directory as a repository at
/// all (`fatal: not a git repository`), which is exactly the shape a Docker
/// bind-mount of a missing path leaves behind — the failure this phase started
/// from.
fn break_repository(bare: &Path) {
    std::fs::remove_dir_all(bare.join("objects")).expect("bare repo must have an objects dir");
    assert!(
        bare.exists(),
        "the repository directory must survive so the handler still reaches the git layer"
    );
}

/// Put a file through the contents API so the repository has a commit on `main`
/// — a freshly created repo has no ref to archive, so the 200 baseline below
/// would be indistinguishable from the bug.
async fn commit_a_file(client: &reqwest::Client, base: &str, token: &str, owner: &str, repo: &str) {
    let resp = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/contents/README.md"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "content": "# hello\n",
            "message": "add README.md",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "the fixture needs a commit: {}",
        resp.text().await.unwrap_or_default()
    );
}

/// A ref the client mistyped is still the client's miss — with a fixed message,
/// not git's, which echoes the ref straight back.
#[tokio::test]
async fn a_missing_ref_on_the_archive_endpoint_is_still_a_client_error() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "archref-owner", "archref@example.com").await;
    create_repo(&base, &token, "archref-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "archref-owner", "archref-repo").await;

    let url = format!("{base}/api/v1/repos/archref-owner/archref-repo/archive");

    // Baseline on a healthy repository: without it the assertion below cannot
    // tell "we fixed the status" from "this endpoint errors on everything".
    let resp = client
        .get(format!("{url}/main.zip"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "the default branch must still archive");
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/zip")
    );
    assert!(
        resp.bytes().await.expect("body").starts_with(b"PK"),
        "the 200 must carry a real zip"
    );

    let resp = client
        .get(format!("{url}/no-such-ref.zip"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 400,
        "a tree-ish the client named and that does not exist is its own miss (body: {body})"
    );
    assert_eq!(
        body["error"]["message"], "invalid ref or SHA",
        "the 400 body must be the fixed message, got: {body}"
    );
    assert_no_internal_detail(&body, &repo_root, "no-such-ref");
}

/// The half the old arm got wrong: a repository that will not open is ours, so
/// it must be a 5xx the client may retry and the operator can see — not a `400`
/// telling the caller its ref was bad.
#[tokio::test]
async fn a_broken_repository_on_the_archive_endpoint_is_not_a_bad_ref() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "archfail-owner", "archfail@example.com").await;
    create_repo(&base, &token, "archfail-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "archfail-owner", "archfail-repo").await;

    break_repository(&repo_root.join("archfail-owner/archfail-repo.git"));

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/archfail-owner/archfail-repo/archive/main.zip"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a repository that cannot be opened must be a 5xx, not {status} — a 400 \
         sends the client off to fix a ref that was fine and puts nothing in the \
         alerts (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root, "main");
}

/// The tree-ish reaches `git archive` as a bare positional argument, so one
/// beginning with `-` is parsed by git as an *option*. `--remote=…` would make
/// the server open an outbound git connection to a host of the caller's
/// choosing, routing around the SSRF guard (`rg_core::net::guard_git_url`) that
/// every other remote-facing path goes through. Such a tree-ish is refused as an
/// invalid ref — which is also exactly what it is: `git check-ref-format`
/// rejects a leading `-`, and no object id starts with one.
///
/// `--list` is the probe that makes "git parsed it as an option" observable from
/// the outside: `git archive --format=zip --list` exits **0** and prints the
/// format list, so without the guard this request answers `200` with
/// `tar\ntgz\ntar.gz\nzip` in the body, served as `application/zip`.
#[tokio::test]
async fn an_option_shaped_tree_ish_cannot_reach_git() {
    let (base, _repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "archopt-owner", "archopt@example.com").await;
    create_repo(&base, &token, "archopt-repo").await;
    let client = reqwest::Client::new();
    commit_a_file(&client, &base, &token, "archopt-owner", "archopt-repo").await;

    let url = format!("{base}/api/v1/repos/archopt-owner/archopt-repo/archive");
    for tree_ish in [
        "--list",
        "--remote=ssh:%2F%2F127.0.0.1:1%2Fx",
        "--output=%2Ftmp%2Fforgekeep-archive-pwn",
        "-o%2Ftmp%2Fforgekeep-archive-pwn",
    ] {
        let resp = client
            .get(format!("{url}/{tree_ish}.zip"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(
            status, 400,
            "an option-shaped tree-ish must be refused as an invalid ref, got {status} \
             for {tree_ish} (body: {body})"
        );
        assert_eq!(
            body["error"]["message"], "invalid ref or SHA",
            "the 400 body must be the fixed message, got: {body}"
        );
    }
    assert!(
        !Path::new("/tmp/forgekeep-archive-pwn").exists(),
        "`git archive --output=` must never have run"
    );
}

/// H-05: whatever the status, the body must not carry the storage path, git's
/// wording, or the tree-ish echoed back. A 4xx body is not sanitized by
/// `IntoResponse`, so this is the half a status-only fix would have left behind.
fn assert_no_internal_detail(body: &serde_json::Value, repo_root: &Path, tree_ish: &str) {
    let message = body["error"]["message"].as_str().unwrap_or_default();
    let root = repo_root.to_string_lossy();
    assert!(
        !message.contains(root.as_ref()),
        "the response body must not carry the server's storage path, got: {message}"
    );
    assert!(
        !message.contains(tree_ish),
        "the response body must not echo the tree-ish back, got: {message}"
    );
    for leak in ["fatal:", "git archive", ".git", "objects", "--format"] {
        assert!(
            !message.contains(leak),
            "the response body must not carry internal error detail ({leak}), got: {message}"
        );
    }
}
