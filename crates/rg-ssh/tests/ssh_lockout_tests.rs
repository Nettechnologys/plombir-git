//! The brute-force lockout has to cover the SSH password door too.
//!
//! `POST /users/login` stops guessing after five tries by writing
//! `login_attempts` / `locked_until` on the user row. The SSH password path
//! verified the Argon2 hash and nothing else, so an attacker who moved from
//! port 443 to port 22 got an unmetered retry loop against the same accounts —
//! and left nothing behind, because those attempts never reached `login_log`
//! either. Neither the counter on the admin's user page nor the audit view
//! showed the guessing was happening.
//!
//! The registry half of the same class (`docker login`) is covered by
//! `rg-http/tests/integration/registry_lockout_tests.rs`.

use std::sync::Arc;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

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
    user_id: i64,
    server: tokio::task::JoinHandle<()>,
}

impl Harness {
    /// One password attempt on a fresh connection.
    ///
    /// Fresh on purpose: the server sets `auth_rejection_time` to a second,
    /// applied from the *second* rejection on a given connection onwards, so
    /// reusing one handle would spend five seconds proving nothing.
    async fn try_password(&self, username: &str, password: &str) -> bool {
        let mut session = russh::client::connect(
            Arc::new(russh::client::Config::default()),
            self.addr.clone(),
            AcceptAnyServer,
        )
        .await
        .expect("connect to test SSH server");
        session
            .authenticate_password(username, password)
            .await
            .expect("password auth exchange")
            .success()
    }

    async fn user(&self) -> rg_db::entities::user::Model {
        rg_db::ops::user_ops::find_by_id(&self.db, self.user_id)
            .await
            .expect("load user")
            .expect("user exists")
    }

    async fn failed_log_rows(&self, username: &str) -> Vec<rg_db::entities::login_log::Model> {
        rg_db::ops::login_log_ops::Entity::find()
            .filter(rg_db::entities::login_log::Column::Username.eq(username))
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
        5,
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

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap().to_string();
    drop(probe);
    let server_config = rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: addr.clone(),
        repo_root: dir.path().join("repos"),
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
        user_id: user.id,
        server,
    }
}

/// The headline: the SSH door counts to the same five the web login does, and
/// the fifth strike closes the account everywhere — including against the
/// password that was right all along.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_ssh_passwords_lock_the_account() {
    let h = harness("ssh_lock").await;

    assert!(
        h.try_password("ssh_lock", PASSWORD).await,
        "baseline: the real password authenticates before any strike"
    );

    let threshold = rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS;
    for attempt in 1..=threshold {
        assert!(
            !h.try_password("ssh_lock", "not-the-password").await,
            "a wrong password was accepted on attempt {attempt}"
        );
    }

    let user = h.user().await;
    assert_eq!(
        user.login_attempts, threshold,
        "SSH password failures did not advance the brute-force counter"
    );
    assert!(
        user.locked_until
            .is_some_and(|until| until > chrono::Utc::now()),
        "{threshold} failed SSH passwords did not lock the account"
    );

    // The point of the lock: the correct password stops working too. Without
    // this the counter would be bookkeeping an attacker could ignore.
    assert!(
        !h.try_password("ssh_lock", PASSWORD).await,
        "a locked account still authenticates over SSH with the right password"
    );

    h.server.abort();
}

/// A lock set anywhere — the web login, an administrator, this SSH door — has
/// to hold on this one, and the rejection has to be filed as such.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lock_from_elsewhere_closes_the_ssh_door() {
    let h = harness("ssh_locked_elsewhere").await;

    for _ in 0..rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS {
        rg_db::ops::user_ops::record_failed_login(
            &h.db,
            h.user_id,
            rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS,
        )
        .await
        .expect("record failed login");
    }

    assert!(
        !h.try_password("ssh_locked_elsewhere", PASSWORD).await,
        "an account locked by the web login still authenticates over SSH"
    );

    let rows = h.failed_log_rows("ssh_locked_elsewhere").await;
    assert_eq!(
        rows.len(),
        1,
        "the SSH rejection of a locked account was not recorded"
    );
    assert_eq!(rows[0].failure_reason.as_deref(), Some("account_locked"));
    assert_eq!(rows[0].auth_provider, "ssh");

    h.server.abort();
}

/// Every rejection reaches `login_log`, attributed to the SSH door and to the
/// account when there is one — an unknown username is recorded too, since a
/// sweep across invented logins is the shape a scan actually has.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_ssh_passwords_reach_the_login_log() {
    let h = harness("ssh_logged").await;

    assert!(!h.try_password("ssh_logged", "wrong-1").await);
    assert!(!h.try_password("ssh_logged", "wrong-2").await);
    assert!(!h.try_password("nobody_here", "wrong-3").await);

    let known = h.failed_log_rows("ssh_logged").await;
    assert_eq!(
        known.len(),
        2,
        "SSH password failures missing from login_log"
    );
    for row in &known {
        assert_eq!(row.auth_provider, "ssh", "the door is not named in the log");
        assert_eq!(row.user_id, Some(h.user_id));
        assert_eq!(row.failure_reason.as_deref(), Some("invalid_credentials"));
        assert_eq!(
            row.ip_address.as_deref(),
            Some("127.0.0.1"),
            "the client address was dropped, so a run of failures names no source"
        );
    }

    let unknown = h.failed_log_rows("nobody_here").await;
    assert_eq!(
        unknown.len(),
        1,
        "an attempt on an unknown login went unrecorded"
    );
    assert_eq!(unknown[0].user_id, None);

    h.server.abort();
}

/// The counter must not be a ratchet: nothing decays `login_attempts`, so
/// without a reset on success two mistyped passwords this morning would still
/// be counting against the account months later.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successful_ssh_password_clears_the_strikes() {
    let h = harness("ssh_reset").await;

    assert!(!h.try_password("ssh_reset", "wrong-1").await);
    assert!(!h.try_password("ssh_reset", "wrong-2").await);
    assert_eq!(
        h.user().await.login_attempts,
        2,
        "baseline: strikes recorded"
    );

    assert!(h.try_password("ssh_reset", PASSWORD).await);
    assert_eq!(
        h.user().await.login_attempts,
        0,
        "a successful SSH login left the earlier strikes on the account"
    );

    h.server.abort();
}
