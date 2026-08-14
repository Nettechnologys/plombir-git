//! A second factor the SSH door never asks for is not a second factor.
//!
//! `POST /users/login` answers an `mfa_enabled` account with a challenge and no
//! session. The SSH password door read the same row, verified the same Argon2
//! hash, and handed out a full identity — so the account whose browser is being
//! asked for a TOTP code opened on port 22 with the bare password. The gate was
//! a convention of one door.
//!
//! The policy these tests pin is the one GitHub and Gitea settle on: a channel
//! that cannot prompt for a code does not accept a password from an account
//! that has a second factor, and that account authenticates there with the
//! credential which already *is* one — its SSH key. So every test below carries
//! the key baseline in the same run: a refusal has to prove the policy, not a
//! fixture that broke.
//!
//! The registry half of the same class (`docker login`) is covered by
//! `rg-http/tests/integration/registry_mfa_tests.rs`.

use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

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

struct Harness {
    _dir: tempfile::TempDir,
    db: rg_db::DatabaseConnection,
    addr: String,
    username: String,
    user_id: i64,
    client_key: Arc<PrivateKey>,
    server: tokio::task::JoinHandle<()>,
}

impl Harness {
    /// One password attempt on a fresh connection.
    ///
    /// Fresh on purpose: the server applies `auth_rejection_time` from the
    /// *second* rejection on a given connection onwards, so reusing one handle
    /// would spend seconds proving nothing.
    async fn try_password(&self, password: &str) -> bool {
        let mut session = russh::client::connect(
            Arc::new(russh::client::Config::default()),
            self.addr.clone(),
            AcceptAnyServer,
        )
        .await
        .expect("connect to test SSH server");
        session
            .authenticate_password(&self.username, password)
            .await
            .expect("password auth exchange")
            .success()
    }

    /// The credential the policy leaves an MFA account: its registered key.
    async fn try_key(&self) -> bool {
        let mut session = russh::client::connect(
            Arc::new(russh::client::Config::default()),
            self.addr.clone(),
            AcceptAnyServer,
        )
        .await
        .expect("connect to test SSH server");
        session
            .authenticate_publickey(
                "git",
                PrivateKeyWithHashAlg::new(self.client_key.clone(), None),
            )
            .await
            .expect("publickey auth exchange")
            .success()
    }

    async fn enable_mfa(&self) {
        rg_db::ops::user_ops::enable_mfa(&self.db, self.user_id, "totp")
            .await
            .expect("enable MFA");
    }

    async fn user(&self) -> rg_db::entities::user::Model {
        rg_db::ops::user_ops::find_by_id(&self.db, self.user_id)
            .await
            .expect("load user")
            .expect("user exists")
    }

    async fn failed_log_rows(&self) -> Vec<rg_db::entities::login_log::Model> {
        rg_db::ops::login_log_ops::Entity::find()
            .filter(rg_db::entities::login_log::Column::Username.eq(self.username.clone()))
            .filter(rg_db::entities::login_log::Column::Success.eq(false))
            .all(&self.db)
            .await
            .expect("read login log")
    }
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
        db: db.clone(),
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
        client_key,
        server,
    }
}

/// The headline: switching the second factor on has to close the password door
/// that cannot ask for it — while leaving the key open, or the account has been
/// locked out rather than protected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mfa_closes_the_ssh_password_door() {
    let h = harness("ssh_mfa").await;

    assert!(
        h.try_password(PASSWORD).await,
        "baseline: the password authenticates while the account has no second factor"
    );

    h.enable_mfa().await;

    assert!(
        !h.try_password(PASSWORD).await,
        "an account with MFA still authenticates over SSH with the bare password"
    );
    assert!(
        h.try_key().await,
        "the credential the policy leaves an MFA account — its SSH key — stopped working, \
         so the refusal above proves a broken fixture and not the gate"
    );

    // The refusal is filed under its own reason: without it the account's owner
    // and the administrator read a run of `invalid_credentials` and go looking
    // for a wrong password that was never wrong.
    let rows = h.failed_log_rows().await;
    assert_eq!(rows.len(), 1, "the second-factor refusal went unrecorded");
    assert_eq!(rows[0].failure_reason.as_deref(), Some("mfa_required"));
    assert_eq!(rows[0].auth_provider, "ssh");
    assert_eq!(rows[0].user_id, Some(h.user_id));

    h.server.abort();
}

/// A correct password refused for want of a second factor is not a guess, and
/// counting it as one would turn MFA into a slow self-lockout: the owner's git
/// remote retries the password it has always had, and five of those would close
/// the web login too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_second_factor_refusal_is_not_a_brute_force_strike() {
    let h = harness("ssh_mfa_strikes").await;
    h.enable_mfa().await;

    let threshold = rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS;
    for _ in 0..=threshold {
        assert!(!h.try_password(PASSWORD).await);
    }

    let user = h.user().await;
    assert_eq!(
        user.login_attempts, 0,
        "the right password, refused for MFA, was counted as a failed guess"
    );
    assert_eq!(
        user.locked_until, None,
        "repeating the correct password locked the account out of the whole forge"
    );
    assert!(
        h.try_key().await,
        "the account's key stopped working after the refusals"
    );

    h.server.abort();
}

/// A wrong password on an MFA account is still a guess. The gate must not
/// become a place where the counter stops running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_password_still_counts_on_an_mfa_account() {
    let h = harness("ssh_mfa_wrong").await;
    h.enable_mfa().await;

    assert!(!h.try_password("not-the-password").await);
    assert!(!h.try_password("not-the-password-either").await);

    assert_eq!(
        h.user().await.login_attempts,
        2,
        "wrong passwords stopped advancing the brute-force counter once MFA was on"
    );

    h.server.abort();
}
