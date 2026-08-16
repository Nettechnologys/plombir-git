//! Git-over-SSH failure semantics at the wire boundary.
//!
//! The SSH transport has no HTTP status code or `AppError` response. Its
//! equivalent is an accepted exec request followed by sanitized stderr and a
//! non-zero exit status. These tests keep a real russh connection open while
//! breaking the database, so a failed lookup cannot be mistaken for a failed
//! authentication handshake.

use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg};
use russh::ChannelMsg;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_git_failures_distinguish_outage_not_found_and_denial() {
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
        repo_root,
        db: db.clone(),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
    };
    let server = common::spawn_ssh_server(server_config).await;

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
