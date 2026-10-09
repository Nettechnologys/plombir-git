//! Live OpenSSH/Git regression coverage for authenticated SSH push.

use std::path::Path;
use std::process::Command;

use sea_orm::Set;

use crate::common;

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_key_can_push_and_clone_over_live_ssh() {
    let app_dir = tempfile::tempdir().unwrap();
    let db_path = app_dir.path().join("test.db");
    // Production connect path (WAL + synchronous=NORMAL + busy_timeout), not a
    // bare `Database::connect` on sqlx's DELETE/FULL defaults — same
    // configuration as the server, and ~6x less time in the migration run.
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", db_path.display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        2,
    )
    .await
    .unwrap();
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

    let server_config = rg_ssh::SshServerConfig {
        host_key_path: app_dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root: repo_root.clone(),
        db: db.clone(),
        db_write: db.clone(),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        lfs: None,
        shutdown: None,
        shutdown_grace_secs: 5,
    };
    let server = common::spawn_ssh_server(server_config).await;
    let listen_addr = server.addr().to_string();

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

/// A key the instance never registered opens nothing — and the registered key
/// in the same test proves the refusal is a refusal and not a broken fixture.
///
/// This is the branch `auth_publickey` used to be able to skip entirely: while
/// the database handle was an `Option`, its `None` arm answered `Auth::Accept`
/// to any key at all (card_6cb7471a52b2). Nothing constructed that arm, so the
/// hole was never live — but nothing tested the door either, which is how a
/// hole stays invisible. The handle is mandatory now; this asserts what the
/// door actually does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unregistered_key_is_refused_while_the_registered_one_still_works() {
    let app_dir = tempfile::tempdir().unwrap();
    let db_path = app_dir.path().join("test.db");
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", db_path.display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        2,
    )
    .await
    .unwrap();
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
    rg_core::repo::service::create_repo(&db, user.id, "ssh-repo", None, true, &repo_root, None)
        .await
        .unwrap();

    let keygen = |path: &Path| {
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(path)
            .status()
            .expect("ssh-keygen must be installed for SSH integration tests");
        assert!(status.success());
    };

    let owner_key = app_dir.path().join("owner_ed25519");
    keygen(&owner_key);
    let public_key = std::fs::read_to_string(owner_key.with_extension("pub")).unwrap();
    let public_key = public_key.trim();
    rg_db::ops::ssh_key_ops::create(
        &db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(user.id),
            title: Set("integration test".to_string()),
            public_key: Set(public_key.to_string()),
            fingerprint: Set(rg_core::auth::ssh_key::fingerprint_from_openssh(public_key).unwrap()),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();

    // Generated the same way, registered nowhere — the whole difference between
    // the two halves of this test is the missing `ssh_keys` row.
    let stranger_key = app_dir.path().join("stranger_ed25519");
    keygen(&stranger_key);

    let server_config = rg_ssh::SshServerConfig {
        host_key_path: app_dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root: repo_root.clone(),
        db: db.clone(),
        db_write: db.clone(),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        lfs: None,
        shutdown: None,
        shutdown_grace_secs: 5,
    };
    let server = common::spawn_ssh_server(server_config).await;
    let listen_addr = server.addr().to_string();

    // Seed one commit so a successful clone has something to show for itself.
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

    let remote = format!("ssh://git@{}/ssh-owner/ssh-repo.git", listen_addr);
    git(&["remote", "add", "origin", &remote], Some(worktree.path()));
    let ssh_command = |key: &Path| {
        format!(
            "ssh -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes",
            key.display()
        )
    };
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let pushed = gateway
        .run_with_env(
            &["push", "origin", "main"],
            Some(worktree.path()),
            &[("GIT_SSH_COMMAND", ssh_command(&owner_key).as_str())],
        )
        .unwrap();
    assert!(pushed.success(), "SSH push failed: {}", pushed.stderr_str());

    // Baseline: the registered key gets in.
    let clone_parent = tempfile::tempdir().unwrap();
    let owner_clone = clone_parent.path().join("owner-clone");
    let owner_clone_arg = owner_clone.to_string_lossy();
    let cloned = gateway
        .run_with_env(
            &["clone", &remote, &owner_clone_arg],
            None,
            &[("GIT_SSH_COMMAND", ssh_command(&owner_key).as_str())],
        )
        .unwrap();
    assert!(
        cloned.success(),
        "the registered key must still clone: {}",
        cloned.stderr_str()
    );
    assert_eq!(
        std::fs::read_to_string(owner_clone.join("README.md")).unwrap(),
        "pushed over SSH\n"
    );

    // The refusal: same repository, same server, an unknown key.
    let stranger_clone = clone_parent.path().join("stranger-clone");
    let stranger_clone_arg = stranger_clone.to_string_lossy();
    let refused = gateway
        .run_with_env(
            &["clone", &remote, &stranger_clone_arg],
            None,
            &[("GIT_SSH_COMMAND", ssh_command(&stranger_key).as_str())],
        )
        .unwrap();
    assert!(
        !refused.success(),
        "an unregistered key must not clone a private repository"
    );
    let stderr = refused.stderr_str().to_lowercase();
    assert!(
        stderr.contains("permission denied") || stderr.contains("publickey"),
        "the refusal must come from SSH authentication, not from something else: {stderr}"
    );
    assert!(
        !stranger_clone.join("README.md").exists(),
        "the refused clone must not have produced a working tree"
    );

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maintenance_mode_rejects_push_but_allows_clone_and_fetch_over_ssh() {
    let app_dir = tempfile::tempdir().unwrap();
    let db_path = app_dir.path().join("test.db");
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", db_path.display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        2,
    )
    .await
    .unwrap();
    rg_db::run_migrations(&db).await.unwrap();

    let user = rg_db::ops::user_ops::create_user(
        &db,
        "ssh-maint-owner",
        "ssh-maint-owner@example.com",
        "",
        "SSH Maintenance Owner",
    )
    .await
    .unwrap();
    let repo_root = app_dir.path().join("repos");
    rg_core::repo::service::create_repo(
        &db,
        user.id,
        "ssh-maint-repo",
        None,
        true,
        &repo_root,
        None,
    )
    .await
    .unwrap();
    let bare_path = repo_root.join("ssh-maint-owner/ssh-maint-repo.git");

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
    rg_db::ops::ssh_key_ops::create(
        &db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(user.id),
            title: Set("maintenance integration test".to_string()),
            public_key: Set(public_key.to_string()),
            fingerprint: Set(fingerprint),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();

    rg_db::ops::instance_settings_ops::save(&db, true, None, "warning")
        .await
        .unwrap();

    let server_config = rg_ssh::SshServerConfig {
        host_key_path: app_dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root: repo_root.clone(),
        db: db.clone(),
        db_write: db.clone(),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        lfs: None,
        shutdown: None,
        shutdown_grace_secs: 5,
    };
    let server = common::spawn_ssh_server(server_config).await;
    let listen_addr = server.addr().to_string();

    let remote = format!(
        "ssh://git@{}/ssh-maint-owner/ssh-maint-repo.git",
        listen_addr
    );
    let ssh_command = format!(
        "ssh -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes",
        client_key.display()
    );
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

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
        "SSH clone must remain available in maintenance mode: {}",
        cloned.stderr_str()
    );
    let fetched = gateway
        .run_with_env(
            &["fetch", "origin"],
            Some(&clone_path),
            &[("GIT_SSH_COMMAND", ssh_command.as_str())],
        )
        .unwrap();
    assert!(
        fetched.success(),
        "SSH fetch must remain available in maintenance mode: {}",
        fetched.stderr_str()
    );

    let worktree = tempfile::tempdir().unwrap();
    let worktree_arg = worktree.path().to_string_lossy();
    git(&["init", "--initial-branch=main", &worktree_arg], None);
    git(
        &["config", "user.name", "SSH Maintenance"],
        Some(worktree.path()),
    );
    git(
        &["config", "user.email", "ssh-maintenance@example.com"],
        Some(worktree.path()),
    );
    std::fs::write(worktree.path().join("README.md"), "should not land\n").unwrap();
    git(&["add", "."], Some(worktree.path()));
    git(
        &["commit", "-m", "push blocked by maintenance"],
        Some(worktree.path()),
    );
    git(&["remote", "add", "origin", &remote], Some(worktree.path()));

    let pushed = gateway
        .run_with_env(
            &["push", "origin", "main"],
            Some(worktree.path()),
            &[("GIT_SSH_COMMAND", ssh_command.as_str())],
        )
        .unwrap();
    assert!(
        !pushed.success(),
        "SSH push succeeded while maintenance mode was enabled"
    );
    let stderr = pushed.stderr_str();
    assert!(
        stderr.contains("maintenance") || stderr.contains("read-only"),
        "SSH push rejection should explain maintenance mode; stderr was: {stderr}"
    );
    assert!(
        !bare_path.join("refs/heads/main").exists(),
        "maintenance-mode SSH push created refs/heads/main"
    );

    server.abort();
}
