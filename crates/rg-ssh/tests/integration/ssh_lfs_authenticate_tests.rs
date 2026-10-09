//! `git-lfs-authenticate` on the SSH port (card_d8d274ed134d).
//!
//! A clone made from the SSH address the UI shows has an `ssh://` remote, and
//! git-lfs asks that remote — before anything else — where the LFS endpoint is:
//! `git-lfs-authenticate <path> <download|upload>`, expecting JSON on stdout.
//! The server used to know only `git-upload-pack` / `git-receive-pack`; the
//! command failed with a handler error that russh turned into a dropped
//! connection, and git-lfs fell back to guessing `https://<ssh host>/...`
//! without the port. `git lfs pull` ended with rc 2.
//!
//! Driven here through a real russh client against a real listener. What is
//! pinned: the answer names the instance's public URL and carries a grant the
//! HTTP side accepts (`rg-http`'s `lfs_ssh_grant_tests` pins that half); the
//! grant is minted only past the gate `git-upload-pack` / `git-receive-pack`
//! would get; and every refusal — including the `git-lfs-transfer` probe the
//! client sends first — is a clean non-zero exit on a connection that keeps
//! working.

use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg};
use russh::ChannelMsg;
use sea_orm::Set;

use rg_core::lfs::service::{
    verify_ssh_grant, LfsActionKind, LfsActor, LfsCredential, SshLfsGrant, SSH_GRANT_AUTH_SCHEME,
    SSH_GRANT_TTL_SECONDS,
};

use crate::common::{self, TestSshServer};

const OWNER: &str = "lfs-owner";
const REPO: &str = "assets";
const SECRET: &str = "ssh-lfs-test-secret";
const EXTERNAL_URL: &str = "https://git.example.test:8443/";

struct Harness {
    _dir: tempfile::TempDir,
    repo_id: i64,
    owner_key_id: i64,
    owner_key: Arc<PrivateKey>,
    outsider_key: Arc<PrivateKey>,
    deploy_key_id: i64,
    deploy_key: Arc<PrivateKey>,
    server: TestSshServer,
}

fn random_key() -> Arc<PrivateKey> {
    Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap())
}

fn openssh_and_fingerprint(key: &PrivateKey) -> (String, String) {
    let openssh = key.public_key().to_openssh().unwrap();
    let fingerprint = rg_core::auth::ssh_key::fingerprint_from_openssh(&openssh).unwrap();
    (openssh, fingerprint)
}

async fn add_user_key(db: &rg_db::DatabaseConnection, user_id: i64, key: &PrivateKey) -> i64 {
    let (openssh, fingerprint) = openssh_and_fingerprint(key);
    rg_db::ops::ssh_key_ops::create(
        db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(user_id),
            title: Set("integration test".to_string()),
            public_key: Set(openssh),
            fingerprint: Set(fingerprint),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap()
    .id
}

async fn harness(lfs: bool) -> Harness {
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

    let owner =
        rg_db::ops::user_ops::create_user(&db, OWNER, "lfs-owner@example.com", "", "LFS Owner")
            .await
            .unwrap();
    let outsider = rg_db::ops::user_ops::create_user(
        &db,
        "lfs-outsider",
        "lfs-outsider@example.com",
        "",
        "LFS Outsider",
    )
    .await
    .unwrap();
    let owner_key = random_key();
    let owner_key_id = add_user_key(&db, owner.id, &owner_key).await;
    let outsider_key = random_key();
    add_user_key(&db, outsider.id, &outsider_key).await;

    let repo_root = dir.path().join("repos");
    let repo =
        rg_core::repo::service::create_repo(&db, owner.id, REPO, None, true, &repo_root, None)
            .await
            .unwrap();

    let deploy_key = random_key();
    let (openssh, fingerprint) = openssh_and_fingerprint(&deploy_key);
    let deploy_key_id = rg_db::ops::deploy_key_ops::create(
        &db,
        rg_db::entities::deploy_key::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo.id),
            created_by_id: Set(Some(owner.id)),
            title: Set("ci".to_string()),
            public_key: Set(openssh),
            fingerprint: Set(fingerprint),
            read_only: Set(true),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap()
    .id;

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
        lfs: lfs.then(|| rg_ssh::SshLfsConfig {
            external_url: EXTERNAL_URL.to_string(),
            signing_secret: SECRET.to_string(),
        }),
        shutdown: None,
        shutdown_grace_secs: 5,
    })
    .await;

    Harness {
        _dir: dir,
        repo_id: repo.id,
        owner_key_id,
        owner_key,
        outsider_key,
        deploy_key_id,
        deploy_key,
        server,
    }
}

async fn connect_as(server: &TestSshServer, key: &Arc<PrivateKey>) -> common::Client {
    let mut session = server.connect().await;
    assert!(
        session
            .authenticate_publickey("git", PrivateKeyWithHashAlg::new(key.clone(), None))
            .await
            .unwrap()
            .success(),
        "the registered key must authenticate"
    );
    session
}

struct Exec {
    exit_status: Option<u32>,
    stdout: String,
    stderr: String,
}

/// Run one command on a fresh channel and collect what a shell client would
/// see: stdout, stderr and the exit status.
async fn exec(session: &common::Client, command: &str) -> Exec {
    let mut channel = session.channel_open_session().await.unwrap();
    channel.exec(true, command).await.unwrap();
    let mut result = Exec {
        exit_status: None,
        stdout: String::new(),
        stderr: String::new(),
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, channel.wait()).await {
            Ok(Some(ChannelMsg::Data { data })) => {
                result.stdout.push_str(&String::from_utf8_lossy(&data))
            }
            Ok(Some(ChannelMsg::ExtendedData { data, .. })) => {
                result.stderr.push_str(&String::from_utf8_lossy(&data))
            }
            Ok(Some(ChannelMsg::ExitStatus { exit_status })) => {
                result.exit_status = Some(exit_status)
            }
            Ok(Some(ChannelMsg::Close) | None) => return result,
            Ok(Some(_)) => {}
            Err(_) => panic!("`{command}` did not finish within 10s"),
        }
    }
}

/// Parse the answer the way git-lfs does, and verify the grant it carries with
/// the key the HTTP side holds.
fn granted(result: &Exec) -> (serde_json::Value, SshLfsGrant) {
    assert_eq!(
        result.exit_status,
        Some(0),
        "git-lfs-authenticate must succeed; stderr: {}",
        result.stderr
    );
    let answer: serde_json::Value = serde_json::from_str(result.stdout.trim()).unwrap();
    let authorization = answer["header"]["Authorization"].as_str().unwrap();
    let token = authorization
        .strip_prefix(&format!("{SSH_GRANT_AUTH_SCHEME} "))
        .unwrap_or_else(|| panic!("unexpected Authorization scheme: {authorization}"));
    let grant = verify_ssh_grant(SECRET.as_bytes(), token, chrono::Utc::now().timestamp())
        .expect("the grant must verify under the instance secret");
    (answer, grant)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owner_is_sent_to_the_public_url_with_a_grant_for_their_key() {
    let h = harness(true).await;
    let session = connect_as(&h.server, &h.owner_key).await;

    // Both spellings git-lfs sends: `ssh://` remotes pass `/owner/repo.git`,
    // scp-style ones `owner/repo`.
    for (command, action) in [
        (
            format!("git-lfs-authenticate /{OWNER}/{REPO}.git download"),
            LfsActionKind::Download,
        ),
        (
            format!("git-lfs-authenticate {OWNER}/{REPO} upload"),
            LfsActionKind::Upload,
        ),
    ] {
        let (answer, grant) = granted(&exec(&session, &command).await);
        assert_eq!(
            answer["href"],
            format!("https://git.example.test:8443/api/v1/repos/{OWNER}/{REPO}/lfs"),
            "the endpoint keeps the public URL's scheme and port: {answer}"
        );
        assert_eq!(answer["expires_in"], SSH_GRANT_TTL_SECONDS);
        assert_eq!(
            grant,
            SshLfsGrant {
                action,
                repo_id: h.repo_id,
                actor: LfsActor::User {
                    user_id: grant.actor.user_id().unwrap(),
                    credential: LfsCredential::SshKey { id: h.owner_key_id },
                },
            },
            "{command}"
        );
    }

    h.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_grant_is_minted_only_past_the_git_gate() {
    let h = harness(true).await;

    // An account with no access to the private repository: the same refusal
    // `git-upload-pack` gives it, and nothing on stdout for git-lfs to use.
    let outsider = connect_as(&h.server, &h.outsider_key).await;
    let refused = exec(
        &outsider,
        &format!("git-lfs-authenticate /{OWNER}/{REPO}.git download"),
    )
    .await;
    assert_eq!(refused.exit_status, Some(1));
    assert_eq!(refused.stdout, "");
    assert!(
        refused.stderr.contains("repository access denied"),
        "{}",
        refused.stderr
    );
    let missing = exec(
        &outsider,
        &format!("git-lfs-authenticate /{OWNER}/no-such-repo.git download"),
    )
    .await;
    assert_eq!(missing.exit_status, Some(1));
    assert!(
        missing.stderr.contains("repository not found"),
        "{}",
        missing.stderr
    );

    // A read-only deploy key reads its repository and is refused the upload,
    // exactly as `git-receive-pack` would refuse it.
    let deploy = connect_as(&h.server, &h.deploy_key).await;
    let (_, grant) = granted(
        &exec(
            &deploy,
            &format!("git-lfs-authenticate /{OWNER}/{REPO}.git download"),
        )
        .await,
    );
    assert_eq!(
        grant.actor,
        LfsActor::DeployKey {
            key_id: h.deploy_key_id
        }
    );
    let upload = exec(
        &deploy,
        &format!("git-lfs-authenticate /{OWNER}/{REPO}.git upload"),
    )
    .await;
    assert_eq!(upload.exit_status, Some(1));
    assert_eq!(upload.stdout, "");
    assert!(
        upload.stderr.contains("repository access denied"),
        "{}",
        upload.stderr
    );

    h.server.abort();
}

/// git-lfs 3.x probes `git-lfs-transfer` first and moves on to
/// `git-lfs-authenticate` only when the probe fails cleanly. The probe used to
/// end in a handler error, which drops the whole connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsupported_command_is_a_clean_refusal_on_a_connection_that_keeps_working() {
    let h = harness(true).await;
    let session = connect_as(&h.server, &h.owner_key).await;

    for command in [
        format!("git-lfs-transfer /{OWNER}/{REPO}.git download"),
        format!("git-upload-archive '/{OWNER}/{REPO}.git'"),
        format!("git-lfs-authenticate /{OWNER}/{REPO}.git verify"),
        "git-upload-pack ''".to_string(),
    ] {
        let refused = exec(&session, &command).await;
        assert_eq!(refused.exit_status, Some(1), "{command}");
        assert!(!refused.stderr.is_empty(), "{command} must say why");
        assert_eq!(refused.stdout, "", "{command}");
    }
    assert!(
        exec(
            &session,
            &format!("git-lfs-transfer /{OWNER}/{REPO}.git download")
        )
        .await
        .stderr
        .contains("git-lfs-authenticate"),
        "the transfer refusal names the command that works"
    );

    // Same connection, after four refusals.
    granted(
        &exec(
            &session,
            &format!("git-lfs-authenticate /{OWNER}/{REPO}.git download"),
        )
        .await,
    );

    h.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_public_url_the_refusal_says_what_to_configure() {
    let h = harness(false).await;
    let session = connect_as(&h.server, &h.owner_key).await;

    let refused = exec(
        &session,
        &format!("git-lfs-authenticate /{OWNER}/{REPO}.git download"),
    )
    .await;
    assert_eq!(refused.exit_status, Some(1));
    assert_eq!(refused.stdout, "");
    assert!(
        refused.stderr.contains("external_url") && refused.stderr.contains("lfs.url"),
        "{}",
        refused.stderr
    );

    h.server.abort();
}
