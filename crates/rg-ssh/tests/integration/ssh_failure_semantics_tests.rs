//! Git-over-SSH failure semantics at the wire boundary.
//!
//! The SSH transport has no HTTP status code or `AppError` response. Its
//! equivalent is an accepted exec request followed by sanitized stderr and a
//! non-zero exit status. These tests keep a real russh connection open while
//! breaking the database, so a failed lookup cannot be mistaken for a failed
//! authentication handshake.

use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg};
use russh::{Channel, ChannelMsg};
use sea_orm::{ConnectionTrait, Set};

use crate::common::{self, AcceptAnyServer, TestSshServer};

async fn register_key(
    db: &rg_db::DatabaseConnection,
    user_id: i64,
    title: &str,
) -> Arc<PrivateKey> {
    let key = Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap());
    let openssh = key.public_key().to_openssh().unwrap();
    let fingerprint = rg_core::auth::ssh_key::fingerprint_from_openssh(&openssh).unwrap();
    rg_db::ops::ssh_key_ops::create(
        db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(user_id),
            title: Set(title.to_string()),
            public_key: Set(openssh),
            fingerprint: Set(fingerprint),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();
    key
}

async fn authenticated_session(
    server: &TestSshServer,
    key: Arc<PrivateKey>,
) -> russh::client::Handle<AcceptAnyServer> {
    let mut session = server.connect().await;
    assert!(
        session
            .authenticate_publickey("git", PrivateKeyWithHashAlg::new(key, None))
            .await
            .expect("public-key authentication exchange")
            .success(),
        "registered key did not authenticate"
    );
    session
}

struct ExecFailure {
    stderr: String,
    exit_status: u32,
}

async fn exec_failure(
    session: &mut russh::client::Handle<AcceptAnyServer>,
    command: &str,
) -> ExecFailure {
    let mut channel = session
        .channel_open_session()
        .await
        .expect("open SSH session channel");
    channel
        .exec(true, command)
        .await
        .expect("send SSH exec request");
    read_refusal(&mut channel).await
}

/// Read a channel to its close, expecting the request on it to have been
/// accepted and then answered as a command that failed.
async fn read_refusal(channel: &mut Channel<russh::client::Msg>) -> ExecFailure {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut accepted = false;
    let mut stderr = Vec::new();
    let mut exit_status = None;
    loop {
        let message = tokio::time::timeout_at(deadline, channel.wait())
            .await
            .expect("SSH exec response timed out");
        let Some(message) = message else {
            break;
        };
        match message {
            ChannelMsg::Success => accepted = true,
            ChannelMsg::Failure => panic!("server rejected the exec request without stderr"),
            ChannelMsg::ExtendedData { ext: 1, data } => {
                stderr.extend_from_slice(data.as_ref());
            }
            ChannelMsg::ExitStatus {
                exit_status: status,
            } => exit_status = Some(status),
            ChannelMsg::Close => break,
            _ => {}
        }
    }

    assert!(
        accepted,
        "failure must be represented as an accepted command that exits non-zero"
    );
    ExecFailure {
        stderr: String::from_utf8(stderr).expect("server stderr must be UTF-8"),
        exit_status: exit_status.expect("server did not send an exit status"),
    }
}

fn assert_failure(reply: &ExecFailure, expected: &str) {
    assert_eq!(reply.exit_status, 1);
    assert!(
        reply.stderr.contains(expected),
        "stderr did not contain {expected:?}: {:?}",
        reply.stderr
    );
    assert!(
        !reply.stderr.contains("db:") && !reply.stderr.contains("SELECT "),
        "internal database context leaked to the SSH client: {:?}",
        reply.stderr
    );
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: rg_db::DatabaseConnection,
    repo_root: std::path::PathBuf,
    owner_key: Arc<PrivateKey>,
    outsider_key: Arc<PrivateKey>,
    server: TestSshServer,
}

/// `ssh-failure-owner` with one private repository on disk, an outsider, and a
/// server over the same database.
async fn fixture() -> Fixture {
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

    let owner = rg_db::ops::user_ops::create_user(
        &db,
        "ssh-failure-owner",
        "ssh-failure-owner@example.com",
        "",
        "SSH Failure Owner",
    )
    .await
    .unwrap();
    let outsider = rg_db::ops::user_ops::create_user(
        &db,
        "ssh-failure-outsider",
        "ssh-failure-outsider@example.com",
        "",
        "SSH Failure Outsider",
    )
    .await
    .unwrap();
    let owner_key = register_key(&db, owner.id, "failure-semantics owner").await;
    let outsider_key = register_key(&db, outsider.id, "failure-semantics outsider").await;

    let repo_root = dir.path().join("repos");
    rg_core::repo::service::create_repo(
        &db,
        owner.id,
        "private-repo",
        None,
        true,
        &repo_root,
        None,
    )
    .await
    .unwrap();

    let server_config = rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root: repo_root.clone(),
        db: db.clone(),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        lfs: None,
        shutdown: None,
        shutdown_grace_secs: 5,
    };
    let server = common::spawn_ssh_server(server_config).await;

    Fixture {
        _dir: dir,
        db,
        repo_root,
        owner_key,
        outsider_key,
        server,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_git_failures_distinguish_outage_not_found_and_denial() {
    let Fixture {
        _dir,
        db,
        owner_key,
        outsider_key,
        server,
        ..
    } = fixture().await;

    // Authenticate the outage probe while the database is still healthy. The
    // exec below happens only after the shared pool has been closed.
    let mut outage_session = authenticated_session(&server, owner_key.clone()).await;

    let mut missing_session = authenticated_session(&server, owner_key.clone()).await;
    let missing = exec_failure(
        &mut missing_session,
        "git-upload-pack '/ssh-failure-owner/missing.git'",
    )
    .await;
    assert_failure(&missing, "repository not found");
    assert!(!missing.stderr.contains("access denied"));
    assert!(!missing.stderr.contains("temporarily unavailable"));

    let mut denied_session = authenticated_session(&server, outsider_key).await;
    let denied = exec_failure(
        &mut denied_session,
        "git-upload-pack '/ssh-failure-owner/private-repo.git'",
    )
    .await;
    assert_failure(&denied, "repository access denied");
    assert!(!denied.stderr.contains("repository not found"));
    assert!(!denied.stderr.contains("temporarily unavailable"));

    // The authorization query succeeds, then receive-pack's policy lookup
    // fails. This is the neighbouring pre-git-process path that previously
    // escaped through `HandlerError` and tore down the session without a reason.
    let mut receive_context_session = authenticated_session(&server, owner_key).await;
    db.execute_unprepared("DROP TABLE protected_branches")
        .await
        .expect("break receive-pack policy lookup");
    let receive_context_failure = exec_failure(
        &mut receive_context_session,
        "git-receive-pack '/ssh-failure-owner/private-repo.git'",
    )
    .await;
    assert_failure(&receive_context_failure, "server temporarily unavailable");
    assert!(!receive_context_failure.stderr.contains("access denied"));

    db.close().await.expect("close shared database pool");
    let outage = exec_failure(
        &mut outage_session,
        "git-upload-pack '/ssh-failure-owner/private-repo.git'",
    )
    .await;
    assert_failure(&outage, "server temporarily unavailable");
    assert!(!outage.stderr.contains("access denied"));
    assert!(!outage.stderr.contains("repository not found"));

    server.abort();
}

/// The upload-pack command for the fixture's repository.
const UPLOAD_PACK: &str = "git-upload-pack '/ssh-failure-owner/private-repo.git'";

/// Run upload-pack on `channel` to completion: the ref advertisement arrives on
/// THIS channel, a flush ends the negotiation, and the command exits 0.
async fn upload_pack_succeeds_on(channel: &mut Channel<russh::client::Msg>) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut advertised = false;
    let mut exit_status = None;
    loop {
        let message = tokio::time::timeout_at(deadline, channel.wait())
            .await
            .expect("upload-pack response timed out on its own channel");
        let Some(message) = message else {
            break;
        };
        match message {
            ChannelMsg::Data { .. } if !advertised => {
                advertised = true;
                channel.data(&b"0000"[..]).await.expect("send flush");
                channel.eof().await.expect("send eof");
            }
            ChannelMsg::Failure => panic!("upload-pack exec was refused"),
            ChannelMsg::ExitStatus {
                exit_status: status,
            } => exit_status = Some(status),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    assert!(
        advertised,
        "the ref advertisement never reached this channel"
    );
    assert_eq!(exit_status, Some(0));
}

/// `ssh -T git@host` is how people check that a key works, and plain `ssh`,
/// `sftp` and `scp` are what they try next. russh answers none of those
/// requests by itself, so each used to leave the client waiting on a reply that
/// never came. Each is now a refusal that says what the port is for — on a
/// connection that keeps serving git.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_this_port_does_not_serve_are_refused_on_a_connection_that_keeps_working() {
    let f = fixture().await;
    let mut session = authenticated_session(&f.server, f.owner_key.clone()).await;

    // `ssh -T`: a shell, no terminal.
    let mut channel = session.channel_open_session().await.unwrap();
    channel.request_shell(true).await.unwrap();
    let shell = read_refusal(&mut channel).await;
    assert_failure(&shell, "there is no shell");
    assert!(shell.stderr.contains("authenticated"), "{:?}", shell.stderr);

    // Plain `ssh`: a terminal first. It is declined on its own, and the shell
    // after it still gets its own answer, not the one meant for the terminal.
    let mut channel = session.channel_open_session().await.unwrap();
    channel
        .request_pty(true, "xterm", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    let declined = tokio::time::timeout(std::time::Duration::from_secs(10), channel.wait())
        .await
        .expect("the terminal request was never answered");
    assert!(
        matches!(declined, Some(ChannelMsg::Failure)),
        "a terminal must be declined, got {declined:?}"
    );
    // `ssh -X` asks for a display too; declined the same way.
    channel
        .request_x11(true, false, "MIT-MAGIC-COOKIE-1", "00", 0)
        .await
        .unwrap();
    let declined = tokio::time::timeout(std::time::Duration::from_secs(10), channel.wait())
        .await
        .expect("the display request was never answered");
    assert!(
        matches!(declined, Some(ChannelMsg::Failure)),
        "a display must be declined, got {declined:?}"
    );
    channel.request_shell(true).await.unwrap();
    assert_failure(&read_refusal(&mut channel).await, "there is no shell");

    // `sftp`, and `scp` in its sftp mode.
    let mut channel = session.channel_open_session().await.unwrap();
    channel.request_subsystem(true, "sftp").await.unwrap();
    assert_failure(&read_refusal(&mut channel).await, "serves only Git");

    // A path that escapes the namespace is a refusal, not a dropped session.
    let traversal = exec_failure(
        &mut session,
        "git-upload-pack '/ssh-failure-owner/../private-repo.git'",
    )
    .await;
    assert_failure(&traversal, "repository not found");

    // Same connection, after four refusals.
    let mut channel = session.channel_open_session().await.unwrap();
    channel.exec(true, UPLOAD_PACK).await.unwrap();
    upload_pack_succeeds_on(&mut channel).await;

    f.server.abort();
}

/// The gate found the repository's row, the directory under the repository
/// root is gone: the server's storage is out of step with its database. That
/// used to be a handler error — a dropped connection and a `Broken pipe` that
/// blamed the network. It is the server's failure, said as one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registered_repository_missing_from_disk_is_a_server_failure_not_a_dropped_connection() {
    let f = fixture().await;
    let mut session = authenticated_session(&f.server, f.owner_key.clone()).await;

    std::fs::remove_dir_all(f.repo_root.join("ssh-failure-owner/private-repo.git"))
        .expect("remove the repository directory");
    let missing = exec_failure(&mut session, UPLOAD_PACK).await;
    assert_failure(&missing, "server temporarily unavailable");
    assert!(
        !missing.stderr.contains(&f.repo_root.display().to_string()),
        "the server's storage path leaked to the client: {:?}",
        missing.stderr
    );

    // The connection is still there to answer the next request.
    assert_failure(
        &exec_failure(
            &mut session,
            "git-upload-archive '/ssh-failure-owner/private-repo.git'",
        )
        .await,
        "serves only",
    );

    f.server.abort();
}

/// One connection may carry several session channels — `ssh` multiplexing
/// opens them side by side. Each exec has to stream over its own channel: with
/// a single slot for "the" channel, an exec took the one opened last and wrote
/// its git session into someone else's channel. A second exec on a channel
/// that is already running one is refused without touching that session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_channel_streams_its_own_exec() {
    let f = fixture().await;
    let session = authenticated_session(&f.server, f.owner_key.clone()).await;

    let mut first = session.channel_open_session().await.unwrap();
    let mut second = session.channel_open_session().await.unwrap();

    first.exec(true, UPLOAD_PACK).await.unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut accepted = false;
    loop {
        match tokio::time::timeout_at(deadline, first.wait())
            .await
            .expect("the ref advertisement never reached the channel that asked for it")
        {
            Some(ChannelMsg::Success) => accepted = true,
            Some(ChannelMsg::Data { .. }) => break,
            Some(ChannelMsg::Failure) => panic!("the first exec was refused"),
            Some(_) => {}
            None => panic!("the first channel closed before its advertisement"),
        }
    }
    assert!(accepted, "the first exec must be accepted");

    // The rest of the advertisement may still be in flight; past it git waits
    // on the client, so the next thing that is not data answers the second
    // exec.
    first.exec(true, UPLOAD_PACK).await.unwrap();
    loop {
        match tokio::time::timeout_at(deadline, first.wait())
            .await
            .expect("the second exec on one channel was never answered")
        {
            Some(ChannelMsg::Data { .. }) => {}
            Some(ChannelMsg::Failure) => break,
            other => panic!("a second exec on a running channel must be refused, got {other:?}"),
        }
    }
    first.request_shell(true).await.unwrap();
    let refused = tokio::time::timeout_at(deadline, first.wait())
        .await
        .expect("a shell on a running channel was never answered");
    assert!(
        matches!(refused, Some(ChannelMsg::Failure)),
        "a shell on a running channel must be refused, got {refused:?}"
    );

    first.data(&b"0000"[..]).await.unwrap();
    first.eof().await.unwrap();
    let mut exit_status = None;
    loop {
        match tokio::time::timeout_at(deadline, first.wait())
            .await
            .expect("the first session never finished")
        {
            Some(ChannelMsg::ExitStatus {
                exit_status: status,
            }) => exit_status = Some(status),
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    assert_eq!(
        exit_status,
        Some(0),
        "the refused exec must leave the running session intact"
    );

    second.exec(true, UPLOAD_PACK).await.unwrap();
    upload_pack_succeeds_on(&mut second).await;

    f.server.abort();
}
