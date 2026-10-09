//! card_a84b25c9efbe: an accepted SSH login stamps the key it used. That stamp
//! is a write on every login, so it queues on the write pool
//! (`rg_db::open_write_pool`) instead of parking a connection the
//! authentication reads need in SQLite's busy handler.

use std::sync::{Arc, Mutex};

use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg};
use sea_orm::Set;

use crate::common;

fn new_key() -> (Arc<PrivateKey>, String, String) {
    let key = Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap());
    let openssh = key.public_key().to_openssh().unwrap();
    let fingerprint = rg_core::auth::ssh_key::fingerprint_from_openssh(&openssh).unwrap();
    (key, openssh, fingerprint)
}

/// Every `UPDATE` statement `pool` sends from now on.
fn record_updates(pool: &mut rg_db::DatabaseConnection) -> Arc<Mutex<Vec<String>>> {
    let sent: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&sent);
    pool.set_metric_callback(move |info| {
        let sql = info.statement.sql.clone();
        if sql.trim_start().to_ascii_uppercase().starts_with("UPDATE") {
            sink.lock().unwrap().push(sql);
        }
    });
    sent
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_usage_stamps_are_written_through_the_write_pool() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
    let mut db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .unwrap();
    rg_db::run_migrations(&db).await.unwrap();
    let mut db_write = rg_db::open_write_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, &db)
        .await
        .unwrap();

    let owner = rg_db::ops::user_ops::create_user(
        &db,
        "ssh-pool-owner",
        "ssh-pool-owner@example.com",
        "",
        "SSH Pool Owner",
    )
    .await
    .unwrap();
    let repo_root = dir.path().join("repos");
    let repo = rg_core::repo::service::create_repo(
        &db, owner.id, "deployed", None, true, &repo_root, None,
    )
    .await
    .unwrap();
    let (user_key, openssh, fingerprint) = new_key();
    rg_db::ops::ssh_key_ops::create(
        &db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(owner.id),
            title: Set("account key".to_string()),
            public_key: Set(openssh),
            fingerprint: Set(fingerprint),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();
    let (deploy_key, openssh, fingerprint) = new_key();
    rg_db::ops::deploy_key_ops::create(
        &db,
        rg_db::entities::deploy_key::ActiveModel {
            repo_id: Set(repo.id),
            created_by_id: Set(Some(owner.id)),
            title: Set("deploy".to_string()),
            public_key: Set(openssh),
            fingerprint: Set(fingerprint),
            read_only: Set(true),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let on_shared = record_updates(&mut db);
    let on_write = record_updates(&mut db_write);
    let server = common::spawn_ssh_server(rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root,
        db: db.clone(),
        db_write: db_write.clone(),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        lfs: None,
        shutdown: None,
        shutdown_grace_secs: 5,
    })
    .await;

    for key in [user_key, deploy_key] {
        let mut session = server.connect().await;
        assert!(
            session
                .authenticate_publickey("git", PrivateKeyWithHashAlg::new(key, None))
                .await
                .expect("public-key authentication exchange")
                .success(),
            "a registered key did not authenticate"
        );
    }

    let stamped = on_write.lock().unwrap().clone();
    for table in ["ssh_keys", "deploy_keys"] {
        assert!(
            stamped.iter().any(|sql| sql.contains(table)),
            "the {table} usage stamp did not go through the write pool: {stamped:?}"
        );
    }
    let shared = on_shared.lock().unwrap().clone();
    assert!(
        !shared
            .iter()
            .any(|sql| sql.contains("ssh_keys") || sql.contains("deploy_keys")),
        "a key usage stamp went through the shared pool: {shared:?}"
    );
}
