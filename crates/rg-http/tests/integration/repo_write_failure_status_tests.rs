//! Regression coverage for card_b72433569b50: the file-write endpoints picked
//! their status by matching the *text* of the service error.
//!
//! ```ignore
//! if e.to_string().contains("SHA mismatch") {
//!     AppError::conflict(e.to_string())
//! } else {
//!     AppError::bad_request(e.to_string())
//! }
//! ```
//!
//! Two defects in four lines. The `409` was hostage to the exact wording, so
//! rephrasing the message in `rg_core` would silently downgrade it to `400`;
//! and everything that was *not* a SHA mismatch — an unopenable repository, a
//! failed clone, a rejected push — was declared the client's fault, so the
//! caller never retried and the alerts stayed empty. The raw `e.to_string()`
//! went into the body of that `400` unsanitized.
//!
//! The outcomes now travel as types (`rg_core::error::{NotFound, Conflict,
//! InvalidRequest}`) and the handlers are a bare `AppError::from(e)`, so these
//! tests assert on status, never on wording.

use std::path::Path;

use crate::common::{create_repo, register_full, spawn_test_app_with_repo_root};

const ABSENT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn contents_url(base: &str, owner: &str, repo: &str, path: &str) -> String {
    format!("{base}/api/v1/repos/{owner}/{repo}/contents/{path}")
}

/// Create a file through the API so the repository has a commit to race with.
async fn put_file(base: &str, token: &str, owner: &str, repo: &str, path: &str, content: &str) {
    let resp = reqwest::Client::new()
        .post(contents_url(base, owner, repo, path))
        .bearer_auth(token)
        .json(&serde_json::json!({"content": content, "message": format!("add {path}")}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "fixture setup failed: {}",
        resp.text().await.unwrap_or_default()
    );
}

/// Blob SHA of a file, read back through the API the client would use.
async fn blob_sha(base: &str, token: &str, owner: &str, repo: &str, path: &str) -> String {
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{owner}/{repo}/blob/{path}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");
    body["sha"]
        .as_str()
        .unwrap_or_else(|| panic!("blob response carries no sha: {body}"))
        .to_string()
}

/// Leave the bare repository openable while deleting the file object the client
/// asks to update or delete. This is the storage fault that used to be collapsed
/// into `None` by `get_file_sha`, and is more precise than removing `objects/`.
fn remove_loose_object(bare: &Path, object_id: &str) {
    let object_path = bare
        .join("objects")
        .join(&object_id[..2])
        .join(&object_id[2..]);
    assert!(
        object_path.exists(),
        "fixture must keep {object_id} as a loose object"
    );
    std::fs::remove_file(&object_path).expect("remove blob object");
    assert!(
        gix::open(bare).is_ok(),
        "fixture must leave repository opening intact so the file probe reads the missing object"
    );
}

/// H-05: a 4xx body reaches the client verbatim, so it must not carry the
/// server's storage path or an internal error chain.
fn assert_no_internal_detail(body: &serde_json::Value, repo_root: &Path) {
    let message = body["error"]["message"].as_str().unwrap_or_default();
    let root = repo_root.to_string_lossy();
    assert!(
        !message.contains(root.as_ref()),
        "the response body must not carry the server's storage path, got: {message}"
    );
    for leak in ["db:", "failed to open repository", ".git", "/tmp", "gix"] {
        assert!(
            !message.contains(leak),
            "the response body must not carry internal detail ({leak}), got: {message}"
        );
    }
}

/// The behaviour the string match was there to produce, now pinned to a status
/// instead of to the wording of the message that produced it.
#[tokio::test]
async fn a_lost_write_race_is_still_a_conflict() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "racewrite-owner", "racewrite@example.com").await;
    create_repo(&base, &token, "racewrite-repo").await;
    put_file(
        &base,
        &token,
        "racewrite-owner",
        "racewrite-repo",
        "README.md",
        "first",
    )
    .await;

    // A stale (but well-formed) blob SHA is exactly what a client holds after
    // someone else pushed: the request is correct, the state moved.
    let resp = reqwest::Client::new()
        .post(contents_url(
            &base,
            "racewrite-owner",
            "racewrite-repo",
            "README.md",
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "content": "second",
            "message": "update",
            "sha": ABSENT_SHA,
        }))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 409,
        "a stale blob SHA is a conflict, not a bad request (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

#[tokio::test]
async fn contents_write_rejects_revspecs_and_qualified_branch_names() {
    let (base, _) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "badref-owner", "badref@example.com").await;
    create_repo(&base, &token, "badref-repo").await;

    for branch in ["main^", "@{-1}", "refs/heads/-x", "a..b"] {
        let resp = reqwest::Client::new()
            .post(contents_url(
                &base,
                "badref-owner",
                "badref-repo",
                "README.md",
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "branch": branch,
                "content": "body",
                "message": "hostile ref must not reach git",
            }))
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(status, 400, "branch {branch:?} was not rejected: {body}");
        assert_eq!(body["error"]["code"], "BAD_REQUEST", "{body}");
    }
}

#[tokio::test]
async fn contents_write_over_the_blob_api_ceiling_is_413() {
    let (base, _) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "largeedit-owner", "largeedit@example.com").await;
    create_repo(&base, &token, "largeedit-repo").await;
    let content = "x".repeat(rg_http::api::repo_content::MAX_BLOB_API_BYTES as usize + 1);

    let resp = reqwest::Client::new()
        .post(contents_url(
            &base,
            "largeedit-owner",
            "largeedit-repo",
            "oversized.txt",
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "content": content,
            "message": "must stay bounded",
        }))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");

    assert_eq!(status, 413, "{body}");
    assert_eq!(body["error"]["code"], "PAYLOAD_TOO_LARGE", "{body}");
}

/// Same race on the delete endpoint, which carried its own copy of the match.
#[tokio::test]
async fn a_lost_delete_race_is_still_a_conflict() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "racedel-owner", "racedel@example.com").await;
    create_repo(&base, &token, "racedel-repo").await;
    put_file(
        &base,
        &token,
        "racedel-owner",
        "racedel-repo",
        "README.md",
        "first",
    )
    .await;

    let url = contents_url(&base, "racedel-owner", "racedel-repo", "README.md");
    let resp = reqwest::Client::new()
        .delete(format!("{url}?message=drop&sha={ABSENT_SHA}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 409,
        "a stale blob SHA is a conflict, not a bad request (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// Deleting a file that is not there is the caller reading stale state, not a
/// malformed request — and the `404` body must stay the fixed message.
#[tokio::test]
async fn deleting_an_absent_file_is_a_not_found() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "delmiss-owner", "delmiss@example.com").await;
    create_repo(&base, &token, "delmiss-repo").await;
    put_file(
        &base,
        &token,
        "delmiss-owner",
        "delmiss-repo",
        "README.md",
        "first",
    )
    .await;

    let url = contents_url(&base, "delmiss-owner", "delmiss-repo", "NOPE.md");
    let resp = reqwest::Client::new()
        .delete(format!("{url}?message=drop&sha={ABSENT_SHA}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status, 404, "an absent file is a 404 (body: {body})");
    assert_eq!(
        body["error"]["message"], "file not found",
        "the 404 body must be the fixed message, got: {body}"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// A dangling blob entry is a server failure, not an absent client file.
#[tokio::test]
async fn a_missing_file_object_on_the_write_endpoint_is_not_the_client_s_fault() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "writefail-owner", "writefail@example.com").await;
    create_repo(&base, &token, "writefail-repo").await;
    put_file(
        &base,
        &token,
        "writefail-owner",
        "writefail-repo",
        "README.md",
        "first",
    )
    .await;
    let sha = blob_sha(
        &base,
        &token,
        "writefail-owner",
        "writefail-repo",
        "README.md",
    )
    .await;

    remove_loose_object(&repo_root.join("writefail-owner/writefail-repo.git"), &sha);

    let resp = reqwest::Client::new()
        .post(contents_url(
            &base,
            "writefail-owner",
            "writefail-repo",
            "README.md",
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "content": "second",
            "message": "update",
            "sha": sha,
        }))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a missing stored blob must be a 5xx, not {status} — a 400 \
         tells the client to fix a request that was never wrong (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// Same object-store failure on the delete endpoint.
#[tokio::test]
async fn a_missing_file_object_on_the_delete_endpoint_is_not_the_client_s_fault() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "delfail-owner", "delfail@example.com").await;
    create_repo(&base, &token, "delfail-repo").await;
    put_file(
        &base,
        &token,
        "delfail-owner",
        "delfail-repo",
        "README.md",
        "first",
    )
    .await;
    let sha = blob_sha(&base, &token, "delfail-owner", "delfail-repo", "README.md").await;

    remove_loose_object(&repo_root.join("delfail-owner/delfail-repo.git"), &sha);

    let url = contents_url(&base, "delfail-owner", "delfail-repo", "README.md");
    let resp = reqwest::Client::new()
        .delete(format!("{url}?message=drop&sha={sha}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a missing stored blob must be a 5xx, not {status} (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// Found while typing the outcomes of these two services (H-02): the write
/// endpoints never validated the file path. `update_files_in_commit` rejects a
/// non-`Normal` component, but `create_or_update_file` joined the client's path
/// straight onto the temp clone — and `PathBuf::join` leaves the tree for an
/// absolute path, so an authenticated user with write access to any repository
/// could write a file anywhere the server process could.
#[tokio::test]
async fn an_escaping_path_is_rejected_before_anything_is_written() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "escape-owner", "escape@example.com").await;
    create_repo(&base, &token, "escape-repo").await;

    // Inside the temp dir the service itself writes into, so the target is
    // writable — the test must fail because the path was rejected, not because
    // the filesystem said no.
    let target = std::env::temp_dir().join(format!("forgekeep-escape-{}", uuid::Uuid::new_v4()));
    assert!(!target.exists(), "the fixture target must start absent");
    let encoded = target.to_string_lossy().replace('/', "%2F");

    let resp = reqwest::Client::new()
        .post(contents_url(&base, "escape-owner", "escape-repo", &encoded))
        .bearer_auth(&token)
        .json(&serde_json::json!({"content": "pwned", "message": "escape"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");

    let escaped = target.exists();
    if escaped {
        std::fs::remove_file(&target).expect("clean up the escaped write");
    }
    assert!(
        !escaped,
        "the write escaped the repository working tree and landed at {} \
         (status {status})",
        target.display()
    );
    assert_eq!(
        status, 400,
        "an out-of-tree path is a malformed request (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}

/// The relative form of the same escape, which no `is_absolute()` check alone
/// would catch.
#[tokio::test]
async fn a_traversing_path_is_rejected_before_anything_is_written() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "traverse-owner", "traverse@example.com").await;
    create_repo(&base, &token, "traverse-repo").await;

    // The service builds its working tree as `<temp_dir>/forgekeep-file-<uuid>`,
    // so one `..` lands back in the temp dir under a name we can look for.
    let name = format!("forgekeep-traverse-{}", uuid::Uuid::new_v4());
    let target = std::env::temp_dir().join(&name);
    assert!(!target.exists(), "the fixture target must start absent");

    let resp = reqwest::Client::new()
        .post(contents_url(
            &base,
            "traverse-owner",
            "traverse-repo",
            &format!("..%2F{name}"),
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"content": "pwned", "message": "escape"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");

    let escaped = target.exists();
    if escaped {
        std::fs::remove_file(&target).expect("clean up the escaped write");
    }
    assert!(
        !escaped,
        "the write escaped the repository working tree and landed at {} \
         (status {status})",
        target.display()
    );
    assert_eq!(
        status, 400,
        "a traversing path is a malformed request (body: {body})"
    );
    assert_no_internal_detail(&body, &repo_root);
}
