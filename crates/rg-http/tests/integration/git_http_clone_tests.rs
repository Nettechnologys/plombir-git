//! Live HTTP smart-transport clone coverage.
//!
//! Guards the upload-pack **response** path, which streams the finished pack to
//! the client through a bounded, idle-guarded channel (`git_response_body_with_idle`)
//! instead of a single in-memory frame (the download-side slow-drip defense,
//! `card_751408c41e0c`). A real `git clone` against a live `axum::serve` proves
//! the streamed, chunked response is a byte-valid pack a stock git client
//! accepts — i.e. no clone regression from the streaming change.

use std::path::Path;

use crate::common::{build_test_app_state, setup_test_db, wait_for_listener};

/// Run git through the sanctioned gateway (the `test_no_raw_git_command_in_crates`
/// regression guard forbids raw git process construction). Returns trimmed stdout.
/// A `cwd` is passed as the gateway's repo path (`-C <cwd>`).
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

/// A public repo with a multi-frame-sized pack clones cleanly over HTTP: the
/// streamed upload-pack response reconstructs to the exact committed content.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_repo_clones_over_live_http() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let user = rg_db::ops::user_ops::create_user(
        &db,
        "http-owner",
        "http-owner@example.com",
        "",
        "HTTP Owner",
    )
    .await
    .unwrap();
    // Public (is_private = false) so an anonymous clone is authorized.
    rg_core::repo::service::create_repo(
        &db,
        user.id,
        "clone-repo",
        None,
        false,
        &repo_root,
        None,
    )
    .await
    .unwrap();
    let bare_path = repo_root.join("http-owner/clone-repo.git");

    // ── Seed the bare repo with a commit whose pack spans several 64 KiB
    //    stream frames. Incompressible bytes keep the pack from shrinking so the
    //    response genuinely exercises multi-chunk streaming + backpressure. ──
    let worktree = tempfile::tempdir().unwrap();
    git(&["init", "--initial-branch=main"], Some(worktree.path()));
    git(&["config", "user.name", "HTTP Integration"], Some(worktree.path()));
    git(
        &["config", "user.email", "http-integration@example.com"],
        Some(worktree.path()),
    );
    // ~1 MiB of pseudo-random, poorly-compressible content.
    let mut blob = Vec::with_capacity(1024 * 1024);
    let mut x: u32 = 0x9e3779b9;
    for _ in 0..(1024 * 1024) {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        blob.push((x & 0xff) as u8);
    }
    std::fs::write(worktree.path().join("big.bin"), &blob).unwrap();
    std::fs::write(worktree.path().join("README.md"), "cloned over HTTP\n").unwrap();
    git(&["add", "."], Some(worktree.path()));
    git(&["commit", "-m", "seed content"], Some(worktree.path()));
    // Push into the bare repo directly (filesystem), then point HEAD at main.
    let bare_str = bare_path.to_string_lossy().to_string();
    git(&["push", &bare_str, "main"], Some(worktree.path()));
    git(
        &["--git-dir", &bare_str, "symbolic-ref", "HEAD", "refs/heads/main"],
        None,
    );
    let expected_sha = git(&["rev-parse", "HEAD"], Some(worktree.path()));

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

    // ── Clone anonymously over HTTP — exercises the streamed upload-pack body. ──
    let dest = tempfile::tempdir().unwrap();
    let clone_path = dest.path().join("clone");
    let url = format!("http://{addr}/http-owner/clone-repo.git");
    git(&["clone", &url, &clone_path.to_string_lossy()], None);

    // The streamed pack must reconstruct the exact committed content + tip.
    let cloned_blob = std::fs::read(clone_path.join("big.bin")).expect("big.bin must be cloned");
    assert_eq!(cloned_blob, blob, "cloned blob must match byte-for-byte");
    let cloned_sha = git(&["rev-parse", "HEAD"], Some(clone_path.as_path()));
    assert_eq!(cloned_sha, expected_sha, "cloned HEAD must match origin tip");

    server.abort();
}
