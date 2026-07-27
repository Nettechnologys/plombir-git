//! JSON blob-API size-cap coverage.
//!
//! Guards `repo_content::get_blob`, which returns a committed file inline in a
//! single JSON frame. A file above `MAX_BLOB_API_BYTES` must be reported with
//! `too_large: true` and an empty body instead of being read into memory and
//! base64/UTF-8 inflated into that frame — the memory-amplification defense
//! (`card_c574d6640e21`). Files under the cap are still served byte-for-byte.

use std::path::Path;

use crate::common::{build_test_app_state, setup_test_db, wait_for_listener};

/// Run git through the sanctioned gateway (the `test_no_raw_git_command_in_crates`
/// regression guard forbids raw git process construction).
fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway must initialize");
    let output = gateway.run(args, cwd).expect("git invocation failed");
    assert!(
        output.success(),
        "git {args:?} failed: {}",
        output.stderr_str().trim()
    );
    output.stdout_str().trim().to_string()
}

/// A blob larger than the API cap is refused with `too_large` + an empty body,
/// while a small sibling blob in the same commit is returned in full.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_blob_is_reported_too_large_not_buffered() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let user = rg_db::ops::user_ops::create_user(
        &db,
        "blob-owner",
        "blob-owner@example.com",
        "",
        "Blob Owner",
    )
    .await
    .unwrap();
    // Public so an anonymous blob GET is authorized.
    rg_core::repo::service::create_repo(&db, user.id, "blob-repo", None, false, &repo_root, None)
        .await
        .unwrap();
    let bare_path = repo_root.join("blob-owner/blob-repo.git");

    // ── Seed a commit with one file above the cap and one small text file. ──
    let worktree = tempfile::tempdir().unwrap();
    git(&["init", "--initial-branch=main"], Some(worktree.path()));
    git(
        &["config", "user.name", "Blob Integration"],
        Some(worktree.path()),
    );
    git(
        &["config", "user.email", "blob-integration@example.com"],
        Some(worktree.path()),
    );

    // 6 MiB > MAX_BLOB_API_BYTES (5 MiB). Only the object size matters here.
    let big_len = 6 * 1024 * 1024usize;
    std::fs::write(worktree.path().join("big.bin"), vec![b'a'; big_len]).unwrap();
    std::fs::write(worktree.path().join("small.txt"), "hello blob\n").unwrap();
    git(&["add", "."], Some(worktree.path()));
    git(&["commit", "-m", "seed blobs"], Some(worktree.path()));
    let bare_str = bare_path.to_string_lossy().to_string();
    git(&["push", &bare_str, "main"], Some(worktree.path()));
    git(
        &[
            "--git-dir",
            &bare_str,
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
        None,
    );

    // ── Spawn the live HTTP app. ──
    let state = build_test_app_state(db.clone(), repo_root.clone());
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;

    let base = format!("http://{addr}/api/v1/repos/blob-owner/blob-repo/blob");
    let client = reqwest::Client::new();

    // Oversized blob: refused with too_large, no body buffered/encoded.
    let big: serde_json::Value = client
        .get(format!("{base}/big.bin"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        big["too_large"],
        serde_json::json!(true),
        "big.bin must be flagged too_large"
    );
    assert_eq!(
        big["content"],
        serde_json::json!(""),
        "oversized body must not be buffered"
    );
    assert_eq!(big["encoding"], serde_json::json!("none"));
    assert_eq!(
        big["size"],
        serde_json::json!(big_len as i64),
        "size metadata is still reported"
    );

    // Small blob: served in full, unchanged.
    let small: serde_json::Value = client
        .get(format!("{base}/small.txt"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        small["too_large"],
        serde_json::json!(false),
        "small.txt must not be flagged"
    );
    assert_eq!(small["encoding"], serde_json::json!("utf-8"));
    assert_eq!(
        small["content"],
        serde_json::json!("hello blob\n"),
        "small blob served byte-for-byte"
    );

    server.abort();
}
