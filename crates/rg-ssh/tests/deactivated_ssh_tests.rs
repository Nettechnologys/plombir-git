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

use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg};
use sea_orm::Set;

const PASSWORD: &str = "Qz7$wRtm";

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

struct Harness {
    _dir: tempfile::TempDir,
    db: rg_db::DatabaseConnection,
    addr: String,
    user_id: i64,
    client_key: Arc<PrivateKey>,
    server: tokio::task::JoinHandle<()>,
}

async fn harness(username: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = rg_db::connect_with_pool(&format!("sqlite://{}?mode=rwc", db_path.display()), 5, 60, 2)
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
    rg_db::ops::ssh_key_ops::create(
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

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);
    let server_config = rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: addr.clone(),
        repo_root: dir.path().join("repos"),
        db: Some(db.clone()),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
    };
    let server = tokio::spawn(async move {
        rg_ssh::start_ssh_server(server_config).await.unwrap();
    });
    wait_for_listener(&addr).await;

    Harness {
        _dir: dir,
        db,
        addr,
        user_id: user.id,
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
