//! card_3bd390bcff70: an SSH key the server does not know costs the client
//! nothing; a wrong password still costs a second.
//!
//! russh applied its `auth_rejection_time` to every rejected request, and
//! OpenSSH offers each key in the agent and in `~/.ssh` in turn — so a client
//! whose registered key came sixth waited five seconds before every
//! connection, and an LFS push over SSH opens several. The delay exists to
//! slow password guessing, and that is now the only place it is paid.

use std::sync::Arc;
use std::time::{Duration, Instant};

use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg};
use sea_orm::Set;

use crate::common::{self, TestSshServer};

const PASSWORD: &str = "Qz7$wRtm";

struct Harness {
    _dir: tempfile::TempDir,
    client_key: Arc<PrivateKey>,
    server: TestSshServer,
}

async fn harness(username: &str) -> Harness {
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
    let password_hash = rg_core::auth::password::hash_password(PASSWORD)
        .await
        .unwrap();
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
    rg_db::ops::ssh_key_ops::create(
        &db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(user.id),
            title: Set("registered".to_string()),
            fingerprint: Set(rg_core::auth::ssh_key::fingerprint_from_openssh(&openssh).unwrap()),
            public_key: Set(openssh),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();

    let server = common::spawn_ssh_server(rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root: dir.path().join("repos"),
        db: db.clone(),
        db_write: db.clone(),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        lfs: None,
        shutdown: None,
        shutdown_grace_secs: 5,
    })
    .await;
    Harness {
        _dir: dir,
        client_key,
        server,
    }
}

/// What a client with a full agent does: five keys nobody registered, then
/// its own, on one connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn five_unknown_keys_before_the_registered_one_cost_no_delay() {
    let h = harness("ssh_many_keys").await;
    let mut session = h.server.connect().await;

    let started = Instant::now();
    for _ in 0..5 {
        let stranger = Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap());
        assert!(
            !session
                .authenticate_publickey("git", PrivateKeyWithHashAlg::new(stranger, None))
                .await
                .expect("publickey exchange")
                .success(),
            "a key nobody registered must not authenticate"
        );
    }
    assert!(
        session
            .authenticate_publickey(
                "git",
                PrivateKeyWithHashAlg::new(h.client_key.clone(), None)
            )
            .await
            .expect("publickey exchange")
            .success(),
        "the registered key must still authenticate after the strangers"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "six key offers took {elapsed:?}; an unknown key must not be charged the password delay"
    );

    h.server.abort();
}

/// The brake stays where guessing is possible: on one connection, the second
/// wrong password is answered no sooner than a second after it was sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repeated_wrong_password_still_costs_a_second() {
    let h = harness("ssh_slow_password").await;
    let mut session = h.server.connect().await;

    assert!(!session
        .authenticate_password("ssh_slow_password", "not-the-password")
        .await
        .expect("password exchange")
        .success());
    let started = Instant::now();
    assert!(!session
        .authenticate_password("ssh_slow_password", "still-not-the-password")
        .await
        .expect("password exchange")
        .success());
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(1),
        "a second wrong password on one connection was answered after {elapsed:?}"
    );

    // An unknown key before the password does not buy the password a free try.
    let mut session = h.server.connect().await;
    let stranger = Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap());
    assert!(!session
        .authenticate_publickey("git", PrivateKeyWithHashAlg::new(stranger, None))
        .await
        .expect("publickey exchange")
        .success());
    let started = Instant::now();
    assert!(!session
        .authenticate_password("ssh_slow_password", "not-the-password")
        .await
        .expect("password exchange")
        .success());
    assert!(started.elapsed() >= Duration::from_secs(1));

    h.server.abort();
}
