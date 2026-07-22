//! Live OpenSSH/Git regression coverage for authenticated SSH push.

use std::path::Path;
use std::process::Command;

use sea_orm::{ConnectOptions, Database, Set};

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

async fn wait_for_listener(addr: &str) {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("SSH listener did not start on {addr}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_key_can_push_and_clone_over_live_ssh() {
    let app_dir = tempfile::tempdir().unwrap();
    let db_path = app_dir.path().join("test.db");
    let mut options = ConnectOptions::new(format!("sqlite://{}?mode=rwc", db_path.display()));
    options.max_connections(2).min_connections(1);
    let db = Database::connect(options).await.unwrap();
    rg_db::run_migrations(&db).await.unwrap();

    let user = rg_db::ops::user_ops::create_user(
        &db,
        "ssh-owner",
        "ssh-owner@example.com",
        "",
        "SSH Owner",
    )
    .await
    .unwrap();
    let repo_root = app_dir.path().join("repos");
    let repo =
        rg_core::repo::service::create_repo(&db, user.id, "ssh-repo", None, true, &repo_root, None)
            .await
            .unwrap();
    let bare_path = repo_root.join("ssh-owner/ssh-repo.git");

    let client_key = app_dir.path().join("client_ed25519");
    let keygen = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&client_key)
        .status()
        .expect("ssh-keygen must be installed for SSH integration tests");
    assert!(keygen.success());
    let public_key = std::fs::read_to_string(client_key.with_extension("pub")).unwrap();
    let public_key = public_key.trim();
    let fingerprint = rg_core::auth::ssh_key::fingerprint_from_openssh(public_key).unwrap();
    let ssh_key = rg_db::ops::ssh_key_ops::create(
        &db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(user.id),
            title: Set("integration test".to_string()),
            public_key: Set(public_key.to_string()),
            fingerprint: Set(fingerprint),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let listen_addr = probe.local_addr().unwrap().to_string();
    drop(probe);
    let server_config = rg_ssh::SshServerConfig {
        host_key_path: app_dir.path().join("host_ed25519"),
        listen_addr: listen_addr.clone(),
        repo_root: repo_root.clone(),
        db: Some(db.clone()),
    };
    let server = tokio::spawn(async move {
        rg_ssh::start_ssh_server(server_config).await.unwrap();
    });
    wait_for_listener(&listen_addr).await;

    let worktree = tempfile::tempdir().unwrap();
    let worktree_arg = worktree.path().to_string_lossy();
    git(&["init", "--initial-branch=main", &worktree_arg], None);
    git(
        &["config", "user.name", "SSH Integration"],
        Some(worktree.path()),
    );
    git(
        &["config", "user.email", "ssh-integration@example.com"],
        Some(worktree.path()),
    );
    std::fs::write(worktree.path().join("README.md"), "pushed over SSH\n").unwrap();
    git(&["add", "."], Some(worktree.path()));
    git(&["commit", "-m", "initial SSH push"], Some(worktree.path()));
    let expected_sha = git(&["rev-parse", "HEAD"], Some(worktree.path()));

    let remote = format!("ssh://git@{}/ssh-owner/ssh-repo.git", listen_addr);
    git(&["remote", "add", "origin", &remote], Some(worktree.path()));
    let ssh_command = format!(
        "ssh -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes",
        client_key.display()
    );
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let pushed = gateway
        .run_with_env(
            &["push", "origin", "main"],
            Some(worktree.path()),
            &[("GIT_SSH_COMMAND", ssh_command.as_str())],
        )
        .unwrap();
    assert!(pushed.success(), "SSH push failed: {}", pushed.stderr_str());
    assert_eq!(
        git(&["rev-parse", "refs/heads/main"], Some(&bare_path)),
        expected_sha
    );

    let clone_parent = tempfile::tempdir().unwrap();
    let clone_path = clone_parent.path().join("clone");
    let clone_arg = clone_path.to_string_lossy();
    let cloned = gateway
        .run_with_env(
            &["clone", &remote, &clone_arg],
            None,
            &[("GIT_SSH_COMMAND", ssh_command.as_str())],
        )
        .unwrap();
    assert!(
        cloned.success(),
        "SSH clone failed: {}",
        cloned.stderr_str()
    );
    assert_eq!(
        std::fs::read_to_string(clone_path.join("README.md")).unwrap(),
        "pushed over SSH\n"
    );

    let used_key = rg_db::ops::ssh_key_ops::find_by_id(&db, ssh_key.id)
        .await
        .unwrap()
        .unwrap();
    assert!(used_key.last_used_at.is_some());
    assert_eq!(repo.owner_id, user.id);

    server.abort();
}
