//! The SSH transport is the second consumer of the process shutdown signal
//! (card_5317e172fd25).
//!
//! `forgekeep serve` fans one `watch` channel out to the HTTP server and every
//! background worker, and the comment above it says so. This transport was the
//! consumer it never reached: `SshServerConfig` carried no receiver, `rg-ssh`
//! contained no `ctrl_c` or `SignalKind` at all, and `run_serve` bound the SSH
//! task to `_ssh_handle` and never awaited it. So a `SIGTERM` during a
//! `git-receive-pack` over SSH cut the stream mid-objects — while the identical
//! push over HTTP was drained inside the grace window — and `drop(server_db)`
//! released the database lease under a transport that was still using it, with
//! a comment claiming the opposite.
//!
//! What that comment claims is now a wait this process performs, and the two
//! halves are tested where each is observable. The drain itself is unit-tested
//! beside `drain_git_sessions` in `rg-ssh/src/lib.rs`, where a session's
//! progress can be watched directly instead of raced against a real push. The
//! ordering in `run_serve` is held by `serve_tests` in `rg-cli`, which is the
//! only place the sequence exists. This file holds the part neither can see:
//! that a running server actually observes the signal and returns.

use tokio::sync::watch;

use crate::common;

async fn server_with_shutdown() -> (
    tempfile::TempDir,
    watch::Sender<bool>,
    common::TestSshServer,
) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", db_path.display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        2,
    )
    .await
    .unwrap();
    rg_db::run_migrations(&db).await.unwrap();

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = common::spawn_ssh_server(rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root: dir.path().join("repos"),
        db,
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        shutdown: Some(shutdown_rx),
        shutdown_grace_secs: 5,
    })
    .await;

    (dir, shutdown_tx, server)
}

/// The signal reaches the transport and the transport stops.
///
/// Before the fix the server had no arm for it at all, so this is the assertion
/// that fails outright rather than flakily: the task simply never finished.
/// Nothing is in flight here on purpose — a server with no sessions must stop
/// at once rather than sit out the grace window on every ordinary restart, so
/// the deadline is well under the five seconds configured above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_ssh_server_stops_when_the_process_is_asked_to() {
    let (_dir, shutdown_tx, server) = server_with_shutdown().await;

    // A real handshake first: a server that never came up would "stop"
    // trivially and prove nothing.
    let _client = server.connect().await;

    shutdown_tx.send(true).expect("the server is listening");
    server
        .stopped_within(std::time::Duration::from_secs(3))
        .await;
}

/// A server whose embedder fans out no signal keeps running, which is what
/// every test above this one and every standalone start depend on.
///
/// The other direction of the same wiring: an arm that resolved on `None` would
/// shut the server down the moment it started, and every SSH test in this crate
/// would fail for a reason that looks like anything but this.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_with_no_shutdown_channel_keeps_serving() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", db_path.display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        2,
    )
    .await
    .unwrap();
    rg_db::run_migrations(&db).await.unwrap();

    let server = common::spawn_ssh_server(rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root: dir.path().join("repos"),
        db,
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        shutdown: None,
        shutdown_grace_secs: 5,
    })
    .await;

    let _client = server.connect().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let _client = server.connect().await;
}
