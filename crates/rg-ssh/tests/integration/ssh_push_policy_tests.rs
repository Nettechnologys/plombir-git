//! The SSH twin of `rg-http`'s `push_policy_tests`: a push over SSH is held to
//! the same policy as one over HTTP, because both load it from
//! `rg_core::branch_protection::push_rules::load_receive_pack_policy`.
//!
//! * The server's own namespaces refuse a client's write (card_e62ac71c4768).
//! * An enabled pull mirror refuses every push; a switched-off one accepts it
//!   (card_97a2c0209056).
//! * A path someone else has locked cannot be changed; the holder's push goes
//!   through (card_4a40b70a6796).

use std::path::{Path, PathBuf};
use std::process::Command;

use sea_orm::{ConnectionTrait, Set};

use crate::common;

struct Outcome {
    success: bool,
    output: String,
}

fn git(cwd: &Path, args: &[&str], ssh_command: Option<&str>) -> Outcome {
    let mut env = vec![
        ("GIT_AUTHOR_NAME", "ssh policy"),
        ("GIT_AUTHOR_EMAIL", "ssh-policy@example.com"),
        ("GIT_COMMITTER_NAME", "ssh policy"),
        ("GIT_COMMITTER_EMAIL", "ssh-policy@example.com"),
    ];
    if let Some(command) = ssh_command {
        env.push(("GIT_SSH_COMMAND", command));
    }
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .unwrap()
        .run_with_env(args, Some(cwd), &env)
        .unwrap();
    Outcome {
        success: output.success(),
        output: format!("{}{}", output.stdout_str(), output.stderr_str()),
    }
}

fn git_ok(cwd: &Path, args: &[&str]) -> String {
    let outcome = git(cwd, args, None);
    assert!(outcome.success, "git {args:?} failed:\n{}", outcome.output);
    outcome.output.trim().to_string()
}

fn commit(work: &Path, file: &str, contents: &str) -> String {
    std::fs::write(work.join(file), contents).unwrap();
    git_ok(work, &["add", file]);
    git_ok(work, &["commit", "-q", "-m", file]);
    git_ok(work, &["rev-parse", "HEAD"])
}

/// One account with a registered key, and the `GIT_SSH_COMMAND` that uses it.
struct Person {
    id: i64,
    ssh_command: String,
}

async fn person(db: &sea_orm::DatabaseConnection, dir: &Path, name: &str) -> Person {
    let user =
        rg_db::ops::user_ops::create_user(db, name, &format!("{name}@example.com"), "", name)
            .await
            .unwrap();
    let key = dir.join(format!("{name}_ed25519"));
    let keygen = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()
        .expect("ssh-keygen must be installed for SSH integration tests");
    assert!(keygen.success());
    let public_key = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    let public_key = public_key.trim();
    rg_db::ops::ssh_key_ops::create(
        db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(user.id),
            title: Set(format!("{name}'s laptop")),
            public_key: Set(public_key.to_string()),
            fingerprint: Set(rg_core::auth::ssh_key::fingerprint_from_openssh(public_key).unwrap()),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();
    Person {
        id: user.id,
        ssh_command: format!(
            "ssh -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes",
            key.display()
        ),
    }
}

/// A server with one repository, `owner/policy.git`, holding one commit on
/// `main`, and a working copy of it whose `origin` is the SSH address.
struct Fixture {
    _dir: tempfile::TempDir,
    db: sea_orm::DatabaseConnection,
    server: common::TestSshServer,
    bare: PathBuf,
    work: PathBuf,
    repo_id: i64,
    owner: Person,
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
    let owner = person(&db, dir.path(), "owner").await;
    let repo_root = dir.path().join("repos");
    let repo =
        rg_core::repo::service::create_repo(&db, owner.id, "policy", None, true, &repo_root, None)
            .await
            .unwrap();
    let server = common::spawn_ssh_server(rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root: repo_root.clone(),
        db: db.clone(),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        lfs: None,
        shutdown: None,
        shutdown_grace_secs: 5,
    })
    .await;

    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    git_ok(&work, &["init", "-q", "--initial-branch=main"]);
    commit(&work, "readme.txt", "policy\n");
    let remote = format!("ssh://git@{}/owner/policy.git", server.addr());
    git_ok(&work, &["remote", "add", "origin", &remote]);
    let pushed = git(
        &work,
        &["push", "-q", "origin", "main"],
        Some(&owner.ssh_command),
    );
    assert!(
        pushed.success,
        "the fixture push failed:\n{}",
        pushed.output
    );

    Fixture {
        bare: repo_root.join("owner/policy.git"),
        _dir: dir,
        db,
        server,
        work,
        repo_id: repo.id,
        owner,
    }
}

impl Fixture {
    async fn push(&self, who: &Person, refspecs: &[&str]) -> Outcome {
        let mut args = vec!["push", "origin"];
        args.extend_from_slice(refspecs);
        let (work, command) = (self.work.clone(), who.ssh_command.clone());
        let args: Vec<String> = args.into_iter().map(str::to_string).collect();
        tokio::task::spawn_blocking(move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            git(&work, &args, Some(&command))
        })
        .await
        .unwrap()
    }

    fn served(&self, refname: &str) -> Option<String> {
        let outcome = git(&self.bare, &["rev-parse", "--verify", "-q", refname], None);
        outcome.success.then(|| outcome.output.trim().to_string())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_into_a_server_namespace_is_refused_over_ssh_too() {
    let fixture = fixture().await;
    let next = commit(&fixture.work, "next.txt", "next\n");

    let pushed = fixture
        .push(
            &fixture.owner,
            &[
                "HEAD:refs/heads/main",
                "HEAD:refs/merge-queue/1",
                "HEAD:refs/forks/x",
            ],
        )
        .await;

    assert!(!pushed.success, "{}", pushed.output);
    for refname in ["refs/merge-queue/1", "refs/forks/x"] {
        assert!(
            pushed
                .output
                .lines()
                .any(|line| line.contains(refname) && line.contains("server-owned namespace")),
            "{refname} was not refused as a server namespace:\n{}",
            pushed.output
        );
        assert_eq!(fixture.served(refname), None, "{refname} was written");
    }
    assert_eq!(fixture.served("refs/heads/main"), Some(next));
    fixture.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_enabled_pull_mirror_refuses_an_ssh_push_and_a_switched_off_one_takes_it() {
    let fixture = fixture().await;
    let now = chrono::Utc::now();
    let mirror = rg_db::ops::mirror_ops::create(
        &fixture.db,
        rg_db::entities::mirror::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(fixture.repo_id),
            url: Set("https://example.com/upstream.git".to_string()),
            username: Set(None),
            password_encrypted: Set(None),
            sync_interval_seconds: Set(3600),
            next_sync_at: Set(None),
            last_sync_at: Set(None),
            last_sync_error: Set(None),
            status: Set(rg_db::entities::mirror::STATUS_ACTIVE.to_string()),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .unwrap();
    let before = fixture.served("refs/heads/main");
    let next = commit(&fixture.work, "local.txt", "a local change\n");

    let refused = fixture.push(&fixture.owner, &["main"]).await;
    assert!(!refused.success, "{}", refused.output);
    assert!(refused.output.contains("pull mirror"), "{}", refused.output);
    assert_eq!(fixture.served("refs/heads/main"), before);

    let mut switched_off: rg_db::entities::mirror::ActiveModel = mirror.into();
    switched_off.status = Set(rg_db::entities::mirror::STATUS_INACTIVE.to_string());
    rg_db::ops::mirror_ops::update(&fixture.db, switched_off)
        .await
        .unwrap();
    let accepted = fixture.push(&fixture.owner, &["main"]).await;
    assert!(accepted.success, "{}", accepted.output);
    assert_eq!(fixture.served("refs/heads/main"), Some(next));
    fixture.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_someone_else_has_locked_cannot_be_changed_over_ssh() {
    let fixture = fixture().await;
    let bob = person(&fixture.db, fixture._dir.path(), "bob").await;
    fixture
        .db
        .execute_unprepared(&format!(
            "INSERT INTO repo_collaborators (repo_id, user_id, permission, created_at) \
             VALUES ({}, {}, 'write', CURRENT_TIMESTAMP)",
            fixture.repo_id, bob.id
        ))
        .await
        .unwrap();
    rg_db::ops::lfs_lock_ops::create(
        &fixture.db,
        fixture.repo_id,
        "castle.level",
        None,
        fixture.owner.id,
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let before = fixture.served("refs/heads/main");
    commit(&fixture.work, "castle.level", "castle by bob\n");

    let refused = fixture.push(&bob, &["main"]).await;
    assert!(!refused.success, "{}", refused.output);
    assert!(
        refused
            .output
            .contains("path 'castle.level' is locked by owner"),
        "{}",
        refused.output
    );
    assert_eq!(fixture.served("refs/heads/main"), before);

    // The same commit, pushed by the lock holder.
    let accepted = fixture.push(&fixture.owner, &["main"]).await;
    assert!(accepted.success, "{}", accepted.output);
    fixture.server.abort();
}
