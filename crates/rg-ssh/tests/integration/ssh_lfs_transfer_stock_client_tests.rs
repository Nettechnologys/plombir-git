//! `git-lfs-transfer` against the stock `git lfs` client (card_f11136311b12).
//!
//! Without it, a stock git-lfs 3.x pushing to an SSH remote opened five SSH
//! sessions per push — `git-receive-pack`, two refused `git-lfs-transfer`
//! probes, `git-upload-pack`, `git-lfs-authenticate` — and then went to HTTPS
//! for the objects themselves. This drives a real client, with no LFS settings
//! of any kind, against a real listener, and counts the sessions it opens
//! through an `ssh` wrapper that logs every invocation: a push may take at most
//! two besides `git-receive-pack`, and nothing — push, pull or locks — may go
//! near `git-lfs-authenticate` or HTTP, which this server does not even run.
//!
//! Ignored by default: it needs `git-lfs` and an OpenSSH client on `PATH`,
//! which not every machine that runs this suite has. Run it with
//! `cargo nextest run -p rg-ssh --test integration --run-ignored only -E 'test(/^ssh_lfs_transfer_stock_client_tests::/)'`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey};
use sea_orm::Set;

use crate::common;

const OWNER: &str = "lfs-transfer-owner";
const REPO: &str = "assets";

/// A machine with git, git-lfs and an `ssh` that writes down every session.
struct Workstation {
    home: PathBuf,
    ssh_log: PathBuf,
    ssh_wrapper: PathBuf,
}

impl Workstation {
    fn new(root: &Path, key: &PrivateKey) -> Self {
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let key_path = root.join("id_ed25519");
        std::fs::write(
            &key_path,
            key.to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        set_mode(&key_path, 0o600);

        // Named `ssh`, so git and git-lfs both treat it as OpenSSH (`-p`
        // for the port). It calls the real client by absolute path.
        let real_ssh = which("ssh");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let ssh_log = root.join("ssh.log");
        let ssh_wrapper = bin.join("ssh");
        std::fs::write(
            &ssh_wrapper,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{log}'\nexec '{ssh}' -i '{key}' -o IdentitiesOnly=yes \
                 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes \
                 -o LogLevel=ERROR \"$@\"\n",
                log = ssh_log.display(),
                ssh = real_ssh.display(),
                key = key_path.display(),
            ),
        )
        .unwrap();
        set_mode(&ssh_wrapper, 0o755);
        Self {
            home,
            ssh_log,
            ssh_wrapper,
        }
    }

    async fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> String {
        let output = tokio::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_SSH_COMMAND", &self.ssh_wrapper)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "Alice")
            .env("GIT_AUTHOR_EMAIL", "alice@example.test")
            .env("GIT_COMMITTER_NAME", "Alice")
            .env("GIT_COMMITTER_EMAIL", "alice@example.test")
            .output()
            .await
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "{program} {args:?} failed:\n{text}"
        );
        text
    }

    async fn git(&self, cwd: &Path, args: &[&str]) -> String {
        self.run(cwd, "git", args).await
    }

    /// The commands this machine has sent over SSH since the last call.
    fn sessions(&self) -> Vec<String> {
        let log = std::fs::read_to_string(&self.ssh_log).unwrap_or_default();
        std::fs::write(&self.ssh_log, "").unwrap();
        log.lines().map(str::to_string).collect()
    }
}

fn which(program: &str) -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(program))
                .find(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| panic!("`{program}` is not on PATH"))
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// The sessions that opened a connection of their own.
fn connections(sessions: &[String]) -> Vec<&String> {
    sessions
        .iter()
        .filter(|session| !session.contains("ControlMaster=no"))
        .collect()
}

fn count(sessions: &[String], command: &str) -> usize {
    sessions
        .iter()
        .filter(|session| session.contains(command))
        .count()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives the stock git-lfs and OpenSSH clients; run with --run-ignored where both are installed"]
async fn a_stock_client_pushes_pulls_and_locks_over_ssh_alone() {
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
    let owner = rg_db::ops::user_ops::create_user(&db, OWNER, "owner@example.test", "", "Owner")
        .await
        .unwrap();
    let key = Arc::new(PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap());
    let openssh = key.public_key().to_openssh().unwrap();
    rg_db::ops::ssh_key_ops::create(
        &db,
        rg_db::entities::ssh_key::ActiveModel {
            id: sea_orm::NotSet,
            user_id: Set(owner.id),
            title: Set("laptop".to_string()),
            fingerprint: Set(rg_core::auth::ssh_key::fingerprint_from_openssh(&openssh).unwrap()),
            public_key: Set(openssh),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();
    let repo_root = dir.path().join("repos");
    let repo =
        rg_core::repo::service::create_repo(&db, owner.id, REPO, None, true, &repo_root, None)
            .await
            .unwrap();

    let server = common::spawn_ssh_server(rg_ssh::SshServerConfig {
        host_key_path: dir.path().join("host_ed25519"),
        listen_addr: "127.0.0.1:0".to_string(),
        repo_root,
        db_write: db.clone(),
        db: db.clone(),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: None,
        // No public URL: `git-lfs-authenticate` refuses, so only the transfer
        // protocol can carry an object.
        lfs: None,
        storage_limits: Default::default(),
        shutdown: None,
        shutdown_grace_secs: 5,
    })
    .await;
    let port = server.addr().rsplit(':').next().unwrap().to_string();
    let remote = format!("ssh://git@127.0.0.1:{port}/{OWNER}/{REPO}.git");

    let machine = Workstation::new(dir.path(), &key);
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    machine.git(&work, &["init", "--initial-branch=main"]).await;
    machine.git(&work, &["lfs", "install", "--local"]).await;
    machine.git(&work, &["lfs", "track", "*.bin"]).await;
    let big: Vec<u8> = (0..(3 * 1024 * 1024 + 17))
        .map(|i: u32| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let small = b"a small object\n".to_vec();
    std::fs::write(work.join("big.bin"), &big).unwrap();
    std::fs::write(work.join("small.bin"), &small).unwrap();
    machine.git(&work, &["add", "."]).await;
    machine.git(&work, &["commit", "-m", "objects"]).await;
    machine
        .git(&work, &["remote", "add", "origin", &remote])
        .await;
    machine.sessions();

    // ── Push ───────────────────────────────────────────────────────────
    machine.git(&work, &["push", "origin", "main"]).await;
    let pushed = machine.sessions();
    eprintln!("push sessions: {pushed:#?}");
    assert_eq!(count(&pushed, "git-receive-pack"), 1, "{pushed:#?}");
    assert!(count(&pushed, "git-lfs-transfer") >= 1, "{pushed:#?}");
    assert_eq!(count(&pushed, "git-lfs-authenticate"), 0, "{pushed:#?}");
    // What costs a handshake and an authentication is a connection; a channel
    // multiplexed onto one (`ControlMaster=no`) costs neither. The client opens
    // its own `git-upload-pack` and one `git-lfs-transfer` for the lock check
    // and one for the objects (git-lfs 3.4.1, measured) — three besides
    // `git-receive-pack`, where the HTTP fallback took four plus HTTPS.
    let connections = connections(&pushed);
    assert!(
        connections.len() <= 4,
        "a push may open at most three SSH connections besides git-receive-pack: {connections:#?}"
    );
    for (name, bytes) in [("big.bin", &big), ("small.bin", &small)] {
        let oid = sha256_hex(bytes);
        let row = rg_db::ops::lfs_object_ops::find_by_repo_and_oid(&db, repo.id, &oid)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{name} ({oid}) never reached the server"));
        assert!(row.uploaded, "{name} is registered but not stored");
        assert_eq!(row.size, bytes.len() as i64, "{name}");
    }

    // ── Pull into a fresh clone ────────────────────────────────────────
    let clone = dir.path().join("clone");
    let parent = dir.path().to_path_buf();
    machine
        .run(
            &parent,
            "env",
            &[
                "GIT_LFS_SKIP_SMUDGE=1",
                "git",
                "clone",
                &remote,
                clone.to_str().unwrap(),
            ],
        )
        .await;
    machine.git(&clone, &["lfs", "install", "--local"]).await;
    machine.sessions();
    machine.git(&clone, &["lfs", "pull"]).await;
    let pulled = machine.sessions();
    eprintln!("pull sessions: {pulled:#?}");
    assert_eq!(count(&pulled, "git-lfs-authenticate"), 0, "{pulled:#?}");
    assert!(count(&pulled, "git-lfs-transfer") >= 1, "{pulled:#?}");
    // The client checks every object's digest against its oid as it lands.
    assert_eq!(std::fs::read(clone.join("big.bin")).unwrap(), big);
    assert_eq!(std::fs::read(clone.join("small.bin")).unwrap(), small);

    // ── Locks ──────────────────────────────────────────────────────────
    machine.git(&work, &["lfs", "lock", "big.bin"]).await;
    let listed = machine.git(&work, &["lfs", "locks"]).await;
    assert!(
        listed.contains("big.bin") && listed.contains(OWNER),
        "{listed}"
    );
    let verified = machine.git(&work, &["lfs", "locks", "--verify"]).await;
    assert!(verified.contains("big.bin"), "{verified}");
    machine.git(&work, &["lfs", "unlock", "big.bin"]).await;
    let listed = machine.git(&work, &["lfs", "locks"]).await;
    assert!(!listed.contains("big.bin"), "{listed}");
    let locking = machine.sessions();
    eprintln!("lock sessions: {locking:#?}");
    assert_eq!(count(&locking, "git-lfs-authenticate"), 0, "{locking:#?}");

    server.abort();
}
