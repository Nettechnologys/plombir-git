//! Regression coverage for card_aa048c2956b1: a *failed git layer* must not be
//! reported as an *absent file* — and must not put the server's storage path in
//! the response body.
//!
//! `get_blob` and `get_commit_signature` had a single error arm,
//! `AppError::not_found(e)`, so every outcome of `get_blob_content` /
//! `verify_commit_signature` became a `404`. The first thing both helpers do is
//! `gix::open(repo_path).with_context(|| format!("failed to open repository:
//! {repo_path:?}"))`, so an unopenable repository answered `404` with the
//! absolute on-disk path of the repository in the body — and unlike the 5xx
//! variants, a `404` is *not* sanitized by `IntoResponse`, so that leak reached
//! the client verbatim (H-05).
//!
//! The break has to happen to the repository *on disk*: closing the database
//! pool fails the authentication lookup first, so the request would never reach
//! the git layer these tests are about.

use std::path::Path;

use crate::common::{create_repo, register_full, spawn_test_app_with_repo_root};

/// Make the bare repository unopenable while leaving its directory in place —
/// the handlers guard on `repo_path.exists()`, so a removed directory would
/// exercise the already-correct "repository not found" arm instead.
fn break_repository(bare: &Path) {
    std::fs::remove_dir_all(bare.join("objects")).expect("bare repo must have an objects dir");
    assert!(
        bare.exists(),
        "the repository directory must survive so the handler still reaches the git layer"
    );
    assert!(
        gix::open(bare).is_err(),
        "fixture must produce a repository that gix refuses to open, otherwise \
         the test exercises a different arm"
    );
}

/// `GET /repos/{owner}/{name}/blob/{path}`.
#[tokio::test]
async fn broken_repository_on_the_blob_endpoint_is_not_a_missing_file() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "blobfail-owner", "blobfail@example.com").await;
    create_repo(&base, &token, "blobfail-repo").await;

    let url = format!("{base}/api/v1/repos/blobfail-owner/blobfail-repo/blob/README.md");
    let client = reqwest::Client::new();

    // Baseline on a healthy repository: the file really is absent → 404 with a
    // fixed message. Without it the assertion below cannot tell "we fixed the
    // status" from "this endpoint 500s on everything".
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent file is still a 404");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "file not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    break_repository(&repo_root.join("blobfail-owner/blobfail-repo.git"));

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a repository that cannot be opened must be a 5xx, not {status} — a 404 \
         tells the client the file was deleted and puts nothing in the alerts \
         (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// `GET /repos/{owner}/{name}/commits/{sha}/signature` — same helper shape, same
/// single error arm.
#[tokio::test]
async fn broken_repository_on_the_signature_endpoint_is_not_a_missing_commit() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "sigfail-owner", "sigfail@example.com").await;
    create_repo(&base, &token, "sigfail-repo").await;

    // Well-formed but absent: the SHA-format guard runs before the git layer, so
    // a malformed SHA would never reach the arm under test.
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let url = format!("{base}/api/v1/repos/sigfail-owner/sigfail-repo/commits/{sha}/signature");
    let client = reqwest::Client::new();

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent commit is still a 404");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "commit not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    break_repository(&repo_root.join("sigfail-owner/sigfail-repo.git"));

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a repository that cannot be opened must be a 5xx, not {status} (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// H-05: whatever the status, the body must not carry the storage path or the
/// git library's own wording. This is the half of the bug a status-only fix
/// would have left in place.
fn assert_no_internal_detail(body: &serde_json::Value, repo_root: &Path) {
    let message = body["error"]["message"].as_str().unwrap_or_default();
    let root = repo_root.to_string_lossy();
    assert!(
        !message.contains(root.as_ref()),
        "the response body must not carry the server's storage path, got: {message}"
    );
    for leak in ["failed to open repository", "gix", ".git", "objects"] {
        assert!(
            !message.contains(leak),
            "the response body must not carry internal error detail ({leak}), got: {message}"
        );
    }
}
