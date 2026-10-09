//! card_b14a241b1e18: what one SSH client can make this server hold.
//!
//! One authenticated connection used to accept any number of session channels
//! and run a git process behind each; the git sessions it started counted
//! against nothing the HTTP transport knew about. The deadline before
//! authentication and the per-source connection bound are in-crate tests
//! (`rg-ssh/src/lib.rs`), where the limits can be shortened.

use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg};
use russh::ChannelMsg;
use sea_orm::Set;

use crate::common::{self, AcceptAnyServer, TestSshServer};

struct Fixture {
    _dir: tempfile::TempDir,
    key: Arc<PrivateKey>,
    server: TestSshServer,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        2,
    )
    .await
    .unwrap();
    rg_db::run_migrations(&db).await.unwrap();
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        "ssh-limits-owner",
        "ssh-limits-owner@example.com",
        "",
        "SSH Limits Owner",
    )
    .await
    .unwrap();
    let key = Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap());
    let openssh = key.public_key().to_openssh().unwrap();
    let fingerprint = rg_core::auth::ssh_key::fingerprint_from_openssh(&openssh).unwrap();
    rg_db::ops::ssh_key_ops::create(
        &db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(owner.id),
            title: Set("limits".to_string()),
            public_key: Set(openssh),
            fingerprint: Set(fingerprint),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();
    let repo_root = dir.path().join("repos");
    rg_core::repo::service::create_repo(&db, owner.id, "repo", None, true, &repo_root, None)
        .await
        .unwrap();
    let server = common::spawn_ssh_server(rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root,
        db_write: db.clone(),
        db,
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        lfs: None,
        shutdown: None,
        shutdown_grace_secs: 5,
    })
    .await;
    Fixture {
        _dir: dir,
        key,
        server,
    }
}

async fn authenticated(fixture: &Fixture) -> russh::client::Handle<AcceptAnyServer> {
    let mut session = fixture.server.connect().await;
    assert!(session
        .authenticate_publickey("git", PrivateKeyWithHashAlg::new(fixture.key.clone(), None))
        .await
        .unwrap()
        .success());
    session
}

/// The N+1-th channel on one connection is refused, and the connection
/// itself stays up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_connection_cannot_open_channels_without_end() {
    let fixture = fixture().await;
    let session = authenticated(&fixture).await;

    let mut open = Vec::new();
    let refused_at = loop {
        match session.channel_open_session().await {
            Ok(channel) => open.push(channel),
            Err(_) => break open.len(),
        }
        assert!(
            open.len() < 256,
            "256 channels on one connection were accepted: there is no bound"
        );
    };
    assert!(
        refused_at >= 4,
        "the bound must leave room for ordinary multiplexing, refused at {refused_at}"
    );

    // Closing one gives its place back.
    let channel = open.pop().unwrap();
    channel.close().await.unwrap();
    let mut reopened = None;
    for _ in 0..50 {
        if let Ok(channel) = session.channel_open_session().await {
            reopened = Some(channel);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(reopened.is_some(), "a closed channel's place came back");
}

/// A git exec that finds no place in the limiter the HTTP transport shares is
/// a failed command with the reason on stderr — no git process is started.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_git_exec_beyond_the_shared_ceiling_is_refused() {
    let fixture = fixture().await;
    let mut held = Vec::new();
    while let Ok(permit) = rg_core::git_sessions::global().try_acquire(None) {
        held.push(permit);
    }

    let session = authenticated(&fixture).await;
    let mut channel = session.channel_open_session().await.unwrap();
    channel
        .exec(true, "git-upload-pack '/ssh-limits-owner/repo.git'")
        .await
        .unwrap();

    let mut stderr = Vec::new();
    let mut exit_status = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while let Some(message) = tokio::time::timeout_at(deadline, channel.wait())
        .await
        .expect("SSH exec response timed out")
    {
        match message {
            ChannelMsg::ExtendedData { ext: 1, data } => stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus {
                exit_status: status,
            } => exit_status = Some(status),
            ChannelMsg::Data { .. } => panic!("a git process answered despite the full limiter"),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    let stderr = String::from_utf8(stderr).unwrap();
    assert_eq!(exit_status, Some(1));
    assert!(stderr.contains("try again"), "{stderr:?}");
    drop(held);
}
