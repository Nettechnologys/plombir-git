//! The SSH twin of `rg-http`'s `push_policy_tests`: a push over SSH is held to
//! the same policy as one over HTTP, because both load it from
//! `rg_core::branch_protection::push_rules::load_receive_pack_policy`.
//!
//! * The server's own namespaces refuse a client's write (card_e62ac71c4768).
//! * An enabled pull mirror refuses every push; a switched-off one accepts it
//!   (card_97a2c0209056).
//! * A path someone else has locked cannot be changed; the holder's push goes
//!   through (card_4a40b70a6796).
//! * A branch that forbids force push takes fast-forwards and refuses
//!   rewrites, from the allow-list too (card_a5c343996db3).
//!
//! And the fetch half of the same stream: a protocol v0 fetch over SSH gets
//! what it lacks and never a hidden ref's objects (card_ad83ad72d14a).

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

/// card_a5c343996db3, over SSH: a rule that forbids force push refuses only a
/// rewrite — a fast-forward goes through — and the direct-push allow-list,
/// which lets its members skip the pull request, does not let them rewrite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_push_protection_refuses_rewrites_and_admits_fast_forwards_over_ssh() {
    let fixture = fixture().await;
    rg_core::branch_protection::service::create_protection(
        &fixture.db,
        "owner",
        "policy",
        "main".to_string(),
        true,
        false,
        None,
        false,
        None,
        false,
        false,
        Some(vec![fixture.owner.id]),
    )
    .await
    .unwrap();

    let ff_head = commit(&fixture.work, "next.txt", "next\n");
    let fast_forward = fixture.push(&fixture.owner, &["HEAD:main"]).await;
    assert!(fast_forward.success, "{}", fast_forward.output);
    assert_eq!(fixture.served("refs/heads/main"), Some(ff_head.clone()));

    git_ok(&fixture.work, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&fixture.work, "other.txt", "rewritten history\n");
    let rewrite = fixture
        .push(&fixture.owner, &["--force", "HEAD:main"])
        .await;
    assert!(!rewrite.success, "{}", rewrite.output);
    assert!(
        rewrite
            .output
            .contains("force push to protected branch 'main' is not allowed"),
        "{}",
        rewrite.output
    );
    assert_eq!(fixture.served("refs/heads/main"), Some(ff_head));

    // The same rewrite under a rule that allows it.
    fixture
        .db
        .execute_unprepared("UPDATE protected_branches SET allow_force_push = 1")
        .await
        .unwrap();
    let allowed = fixture
        .push(&fixture.owner, &["--force", "HEAD:main"])
        .await;
    assert!(allowed.success, "{}", allowed.output);
    fixture.server.abort();
}

/// card_ad83ad72d14a, over SSH: a stock git client pinned to protocol v0
/// clones without the objects of a server-private ref, and an incremental
/// fetch from a clone with commits of its own — several rounds of haves on
/// one stream — receives only what it lacks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_protocol_v0_fetch_over_ssh_gets_what_it_lacks_and_never_a_hidden_ref() {
    let fixture = fixture().await;
    for index in 0..20 {
        commit(&fixture.work, &format!("history-{index}.txt"), "history\n");
    }
    git_ok(&fixture.work, &["checkout", "-q", "-b", "secret"]);
    let secret = commit(&fixture.work, "secret.txt", "secret\n");
    git_ok(&fixture.work, &["checkout", "-q", "main"]);
    let pushed = fixture.push(&fixture.owner, &["main"]).await;
    assert!(pushed.success, "{}", pushed.output);
    let bare = fixture.bare.to_string_lossy().to_string();
    git_ok(&fixture.work, &["push", "-q", &bare, "secret:refs/forks/x"]);

    let clone = fixture._dir.path().join("v0-clone");
    let remote = format!("ssh://git@{}/owner/policy.git", fixture.server.addr());
    let (root, command, clone_arg) = (
        fixture._dir.path().to_path_buf(),
        fixture.owner.ssh_command.clone(),
        clone.to_string_lossy().to_string(),
    );
    let cloned = tokio::task::spawn_blocking(move || {
        git(
            &root,
            &[
                "-c",
                "protocol.version=0",
                "clone",
                "-q",
                &remote,
                &clone_arg,
            ],
            Some(&command),
        )
    })
    .await
    .unwrap();
    assert!(cloned.success, "{}", cloned.output);
    assert!(
        !git(&clone, &["cat-file", "-e", &secret], None).success,
        "a v0 clone received the commit only refs/forks/x holds"
    );

    for index in 0..40 {
        commit(&clone, &format!("local-{index}.txt"), "local\n");
    }
    let before = git_ok(&fixture.work, &["rev-parse", "main"]);
    let upstream = commit(&fixture.work, "upstream.txt", "upstream\n");
    let pushed = fixture.push(&fixture.owner, &["main"]).await;
    assert!(pushed.success, "{}", pushed.output);

    let packs = |dir: &Path| -> std::collections::BTreeSet<PathBuf> {
        std::fs::read_dir(dir.join(".git/objects/pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
            .collect()
    };
    let packs_before = packs(&clone);
    let (clone_dir, command) = (clone.clone(), fixture.owner.ssh_command.clone());
    let fetched = tokio::task::spawn_blocking(move || {
        git(
            &clone_dir,
            &[
                "-c",
                "protocol.version=0",
                "-c",
                "fetch.unpackLimit=1",
                "fetch",
                "-q",
                "origin",
            ],
            Some(&command),
        )
    })
    .await
    .unwrap();
    assert!(fetched.success, "{}", fetched.output);
    assert_eq!(git_ok(&clone, &["rev-parse", "origin/main"]), upstream);
    let new_packs: Vec<_> = packs(&clone).difference(&packs_before).cloned().collect();
    let [pack] = new_packs.as_slice() else {
        panic!("one fetched pack expected, got {new_packs:?}");
    };
    let bytes = std::fs::read(pack).unwrap();
    let count = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
    let lacking = git_ok(
        &fixture.bare,
        &["rev-list", "--objects", &upstream, "--not", &before],
    )
    .lines()
    .count() as u32;
    let whole = git_ok(&fixture.bare, &["rev-list", "--objects", "--all"])
        .lines()
        .count() as u32;
    // `index-pack --fix-thin` appends the delta bases a thin pack needed, so
    // the kept pack may hold a few more objects than were sent.
    assert!(
        (lacking..=2 * lacking).contains(&count) && count < whole / 4,
        "the incremental v0 fetch kept {count} objects; it lacked {lacking}, the repository holds {whole}"
    );
    fixture.server.abort();
}
