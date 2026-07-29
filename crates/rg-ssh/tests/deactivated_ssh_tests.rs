//! Deactivating an account has to close the SSH door too.
//!
//! SSH is the most durable way back into a ForgeKeep instance: the key is
//! already on the laptop, `git push` needs no browser, and nothing in the
//! pubkey path ever looked at the key's *owner* — the fingerprint matched, so
//! the session was accepted. An administrator who deactivates a departing
//! developer would have had to hunt down and delete every key by hand.
//!
//! Both SSH authentication methods are driven here through a real russh client
//! against a real listener; the HTTP half of the same class (PAT, docker login,
//! password reset) lives in `rg-http/tests/integration/deactivated_account_tests.rs`.
//!
//! Closing the door is only half of it. Authentication happens once and an SSH
//! connection carries any number of `exec`s — under `ControlMaster`, or a held
//! `ssh -N`, that once was the entire lifetime of an offboarded developer's
//! access. So the tests below come in pairs: one drives a *fresh* connection
//! after the revocation, the other keeps the connection it already had and
//! asks again on it. The `_on_an_open_connection` half is the one that reads
//! the account and the key row on every exec rather than trusting what
//! authentication decided.

use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg};
use russh::ChannelMsg;
use sea_orm::Set;

const PASSWORD: &str = "Qz7$wRtm";
/// The repository every exec in this file names.
const REPO: &str = "ssh-scope";

struct AcceptAnyServer;

impl russh::client::Handler for AcceptAnyServer {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Block until the SSH server bound its port — the bind happens inside the
/// spawned task, so there is a genuine window where a connect is refused.
async fn wait_for_listener(addr: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "SSH listener did not start on {addr} within 10s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

async fn connect(addr: &str) -> russh::client::Handle<AcceptAnyServer> {
    russh::client::connect(
        Arc::new(russh::client::Config::default()),
        addr,
        AcceptAnyServer,
    )
    .await
    .expect("connect to test SSH server")
}

/// Ask the server to serve `git-upload-pack` on a fresh channel of an
/// already-authenticated connection, and report whether it agreed.
///
/// The two answers are unambiguous on the wire: an accepted exec is
/// `channel_success` followed by the ref advertisement, a refused one is
/// `channel_failure` — and because the handler returns an error after refusing,
/// the connection is torn down, which reaches the client as the channel simply
/// ending. Both of those read as `false` here; only an explicit acceptance
/// reads as `true`, so a broken fixture cannot be mistaken for a revocation.
async fn upload_pack_allowed(
    session: &russh::client::Handle<AcceptAnyServer>,
    owner: &str,
) -> bool {
    let Ok(mut channel) = session.channel_open_session().await else {
        return false;
    };
    if channel
        .exec(true, format!("git-upload-pack '/{owner}/{REPO}.git'"))
        .await
        .is_err()
    {
        return false;
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, channel.wait()).await {
            Ok(Some(ChannelMsg::Success | ChannelMsg::Data { .. })) => return true,
            Ok(Some(ChannelMsg::Failure) | None) | Err(_) => return false,
            Ok(Some(_)) => {}
        }
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    db: rg_db::DatabaseConnection,
    addr: String,
    username: String,
    user_id: i64,
    ssh_key_id: i64,
    client_key: Arc<PrivateKey>,
    server: tokio::task::JoinHandle<()>,
}

async fn harness(username: &str) -> Harness {
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

    let password_hash = rg_core::auth::password::hash_password(PASSWORD).unwrap();
    let user = rg_db::ops::user_ops::create_user(
        &db,
        username,
        &format!("{username}@example.com"),
        &password_hash,
        "SSH Owner",
    )
    .await
    .unwrap();

    let client_key = Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap());
    let openssh = client_key.public_key().to_openssh().unwrap();
    let fingerprint = rg_core::auth::ssh_key::fingerprint_from_openssh(&openssh).unwrap();
    let ssh_key = rg_db::ops::ssh_key_ops::create(
        &db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(user.id),
            title: Set("integration test".to_string()),
            public_key: Set(openssh),
            fingerprint: Set(fingerprint),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();

    // Private on purpose: after the revocation the owner still *owns* the row,
    // so `can_read_repo` keeps answering yes. Any denial the exec tests see is
    // therefore the identity check and nothing else.
    let repo_root = dir.path().join("repos");
    rg_core::repo::service::create_repo(&db, user.id, REPO, None, true, &repo_root, None)
        .await
        .unwrap();

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);
    let server_config = rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: addr.clone(),
        repo_root,
        db: Some(db.clone()),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
    };
    let server = tokio::spawn(async move {
        rg_ssh::start_ssh_server(server_config).await.unwrap();
    });
    wait_for_listener(&addr).await;

    Harness {
        _dir: dir,
        db,
        addr,
        username: username.to_string(),
        user_id: user.id,
        ssh_key_id: ssh_key.id,
        client_key,
        server,
    }
}

async fn deactivate(db: &rg_db::DatabaseConnection, user_id: i64) {
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, None, Some(false))
        .await
        .expect("deactivate user");
}

/// The registered key keeps matching after deactivation — what must change is
/// that the match no longer buys a session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deactivating_an_account_rejects_its_ssh_key() {
    let h = harness("ssh_deact_key").await;

    let key = || PrivateKeyWithHashAlg::new(h.client_key.clone(), None);

    let mut before = connect(&h.addr).await;
    assert!(
        before
            .authenticate_publickey("git", key())
            .await
            .unwrap()
            .success(),
        "baseline: a registered key authenticates while the account is active"
    );

    deactivate(&h.db, h.user_id).await;

    let mut after = connect(&h.addr).await;
    assert!(
        !after
            .authenticate_publickey("git", key())
            .await
            .unwrap()
            .success(),
        "SSH key of a deactivated account still authenticates"
    );

    h.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deactivating_an_account_rejects_its_ssh_password() {
    let h = harness("ssh_deact_pw").await;

    let mut before = connect(&h.addr).await;
    assert!(
        before
            .authenticate_password("ssh_deact_pw", PASSWORD)
            .await
            .unwrap()
            .success(),
        "baseline: the password authenticates while the account is active"
    );

    deactivate(&h.db, h.user_id).await;

    let mut after = connect(&h.addr).await;
    assert!(
        !after
            .authenticate_password("ssh_deact_pw", PASSWORD)
            .await
            .unwrap()
            .success(),
        "SSH password auth of a deactivated account still succeeds"
    );

    h.server.abort();
}

/// The connection the departing developer already has open.
///
/// `authenticate_publickey` runs once; every `git push`/`git fetch` after it is
/// another `exec` on the same connection, and multiplexing (`ControlMaster`) or
/// a held `ssh -N` makes "the same connection" last for days. Until the exec
/// path re-read the account, deactivation reached this session never.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deactivating_an_account_stops_execs_on_an_open_connection() {
    let h = harness("ssh_deact_live").await;

    let mut session = connect(&h.addr).await;
    assert!(
        session
            .authenticate_publickey(
                "git",
                PrivateKeyWithHashAlg::new(h.client_key.clone(), None)
            )
            .await
            .unwrap()
            .success(),
        "baseline: the key authenticates while the account is active"
    );
    assert!(
        upload_pack_allowed(&session, &h.username).await,
        "baseline: the account may read its own repository over this connection"
    );

    deactivate(&h.db, h.user_id).await;

    assert!(
        !upload_pack_allowed(&session, &h.username).await,
        "a deactivated account kept serving git over the connection it already had"
    );

    h.server.abort();
}

/// Deleting the key is the other half of offboarding, and it has the same gap:
/// the fingerprint was resolved once, at authentication, and the session went
/// on speaking for a key row that no longer exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_the_ssh_key_stops_execs_on_the_connection_it_opened() {
    let h = harness("ssh_key_revoked").await;

    let mut session = connect(&h.addr).await;
    assert!(
        session
            .authenticate_publickey(
                "git",
                PrivateKeyWithHashAlg::new(h.client_key.clone(), None)
            )
            .await
            .unwrap()
            .success(),
        "baseline: the key authenticates while it is registered"
    );
    assert!(
        upload_pack_allowed(&session, &h.username).await,
        "baseline: the key may read the repository over this connection"
    );

    rg_db::ops::ssh_key_ops::delete_by_id(&h.db, h.ssh_key_id)
        .await
        .expect("delete the SSH key");

    assert!(
        !upload_pack_allowed(&session, &h.username).await,
        "a deleted SSH key kept serving git over the connection it had opened"
    );

    h.server.abort();
}

/// A deploy key is the same story with no account behind it: the key row *is*
/// the identity, so revoking it has to reach the connection it opened. The
/// session used to carry a copy of the key's repository and its read-only flag,
/// taken at authentication and never looked at again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_deploy_key_stops_execs_on_the_connection_it_opened() {
    let h = harness("ssh_deploy_revoked").await;

    let repo = rg_core::repo::service::find_repo_by_owner_name(&h.db, &h.username, REPO)
        .await
        .unwrap()
        .expect("the fixture repository");
    let deploy_private = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let openssh = deploy_private.public_key().to_openssh().unwrap();
    let fingerprint = rg_core::auth::ssh_key::fingerprint_from_openssh(&openssh).unwrap();
    let deploy_key = rg_db::ops::deploy_key_ops::create(
        &h.db,
        rg_db::entities::deploy_key::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo.id),
            created_by_id: Set(h.user_id),
            title: Set("integration test".to_string()),
            public_key: Set(openssh),
            fingerprint: Set(fingerprint),
            read_only: Set(true),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();

    let deploy_private = Arc::new(deploy_private);
    let mut session = connect(&h.addr).await;
    assert!(
        session
            .authenticate_publickey("git", PrivateKeyWithHashAlg::new(deploy_private, None))
            .await
            .unwrap()
            .success(),
        "baseline: a registered deploy key authenticates"
    );
    assert!(
        upload_pack_allowed(&session, &h.username).await,
        "baseline: a read-only deploy key may fetch its repository"
    );

    rg_db::ops::deploy_key_ops::delete_by_id(&h.db, deploy_key.id)
        .await
        .expect("delete the deploy key");

    assert!(
        !upload_pack_allowed(&session, &h.username).await,
        "a deleted deploy key kept serving git over the connection it had opened"
    );

    h.server.abort();
}
