//! The LFS locking API against the stock `git lfs` client (card_e8afcaf3edf6).
//!
//! The protocol tests in `lfs_locks_tests` pin what the server answers; this
//! one pins that a real client does with it what locking is for: once one
//! person has locked a file, another person's push of a change to it is refused
//! by their own `git lfs` before anything is sent, and goes through once the
//! lock is released. Nothing is configured beyond the clone URL and
//! `lfs.locksverify = true`: without that setting the stock client (3.4.1,
//! measured) prints "Unable to push locked files … would have halted this
//! push" and pushes anyway — the protocol makes locking advisory unless the
//! client opts in, which is why the README tells a team to set it.
//!
//! Ignored by default: it needs the `git-lfs` binary on `PATH`, which not every
//! machine that runs this suite has. Run it with
//! `cargo nextest run -p rg-http --run-ignored only -E 'test(/lfs_locks_stock_client/)'`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common::{register_full, spawn_test_app};

const ALICE: &str = "lock_e2e_alice";
const BOB: &str = "lock_e2e_bob";
const REPO: &str = "levels";

async fn pat_for(base: &str, session: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(session)
        .json(&serde_json::json!({ "name": "git-lfs", "scopes": "repo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// One person's machine: a home directory of their own, so neither the host's
/// git configuration nor the other person's credentials leak in.
#[derive(Clone)]
struct Workstation {
    home: PathBuf,
    name: &'static str,
}

struct Outcome {
    success: bool,
    output: String,
}

impl Workstation {
    fn new(root: &Path, name: &'static str) -> Self {
        let home = root.join(name);
        std::fs::create_dir_all(&home).unwrap();
        Self { home, name }
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Outcome {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", self.name)
            .env("GIT_AUTHOR_EMAIL", format!("{}@example.com", self.name))
            .env("GIT_COMMITTER_NAME", self.name)
            .env("GIT_COMMITTER_EMAIL", format!("{}@example.com", self.name))
            .output()
            .expect("run git");
        Outcome {
            success: output.status.success(),
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        }
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let outcome = self.run(cwd, args);
        assert!(
            outcome.success,
            "{} ran git {args:?} and it failed:\n{}",
            self.name, outcome.output
        );
        outcome.output
    }
}

/// Run blocking git on the blocking pool: the server under test shares this
/// runtime, and a client waiting on it from a worker thread would wait forever.
async fn on_machine<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work).await.expect("git task")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives the stock git-lfs client; run with --run-ignored where git-lfs is installed"]
async fn a_locked_file_cannot_be_pushed_by_anyone_else_until_it_is_unlocked() {
    let base = spawn_test_app().await;
    let (alice_session, _) = register_full(&base, ALICE, "lock_e2e_alice@example.com").await;
    let (bob_session, _) = register_full(&base, BOB, "lock_e2e_bob@example.com").await;
    let alice_pat = pat_for(&base, &alice_session).await;
    let bob_pat = pat_for(&base, &bob_session).await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&alice_session)
        .json(&serde_json::json!({ "name": REPO, "is_private": true, "auto_init": true, "readme": "default" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let added = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{ALICE}/{REPO}/collaborators"))
        .bearer_auth(&alice_session)
        .json(&serde_json::json!({"username": BOB, "permission": "write"}))
        .send()
        .await
        .unwrap();
    assert_eq!(added.status(), 201);

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_path_buf();
    let alice = Workstation::new(&root_path, ALICE);
    let bob = Workstation::new(&root_path, BOB);
    // The clone URL the UI shows, with the PAT where a credential helper
    // would put it.
    let address = base.trim_start_matches("http://").to_string();
    let alice_url = format!("http://{ALICE}:{alice_pat}@{address}/git/{ALICE}/{REPO}");
    let bob_url = format!("http://{BOB}:{bob_pat}@{address}/git/{ALICE}/{REPO}");

    // Alice adds a level file under LFS, pushes it, and locks it.
    let (alice_checkout, lock_output) = {
        let (alice, root_path) = (alice.clone(), root_path.clone());
        on_machine(move || {
            alice.ok(&root_path, &["lfs", "install"]);
            alice.ok(&root_path, &["clone", "-q", &alice_url, "alice-work"]);
            let work = root_path.join("alice-work");
            alice.ok(&work, &["lfs", "track", "--lockable", "*.level"]);
            std::fs::write(work.join("castle.level"), b"\0castle v1\0").unwrap();
            alice.ok(&work, &["add", ".gitattributes", "castle.level"]);
            alice.ok(&work, &["commit", "-qm", "add the castle"]);
            alice.ok(&work, &["push", "-q", "origin", "HEAD"]);
            let locked = alice.ok(&work, &["lfs", "lock", "castle.level"]);
            (work, locked)
        })
        .await
    };
    assert!(lock_output.contains("Locked castle.level"), "{lock_output}");

    // Bob's change to the locked file is refused by his own client.
    let (bob_checkout, refused) = {
        let (bob, root_path) = (bob.clone(), root_path.clone());
        on_machine(move || {
            bob.ok(&root_path, &["lfs", "install"]);
            bob.ok(&root_path, &["clone", "-q", &bob_url, "bob-work"]);
            let work = root_path.join("bob-work");
            bob.ok(&work, &["config", "lfs.locksverify", "true"]);
            let listed = bob.ok(&work, &["lfs", "locks"]);
            assert!(
                listed.contains("castle.level") && listed.contains(ALICE),
                "{listed}"
            );
            // `--lockable` checks the file out read-only to everyone who does
            // not hold its lock — the client's first hint. Bob edits anyway.
            let level = work.join("castle.level");
            let mut permissions = std::fs::metadata(&level).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(&level, permissions).unwrap();
            std::fs::write(&level, b"\0castle v2 by bob\0").unwrap();
            bob.ok(&work, &["commit", "-qam", "bob edits the castle"]);
            let pushed = bob.run(&work, &["push", "origin", "HEAD"]);
            (work, pushed)
        })
        .await
    };
    assert!(
        !refused.success,
        "bob's push of a file alice has locked went through:\n{}",
        refused.output
    );
    assert!(
        refused.output.contains("castle.level") && refused.output.to_lowercase().contains("lock"),
        "the client did not say the file is locked:\n{}",
        refused.output
    );

    // A forced unlock is an administrator's; bob is a writer.
    {
        let (bob, work) = (bob.clone(), bob_checkout.clone());
        let forced =
            on_machine(move || bob.run(&work, &["lfs", "unlock", "--force", "castle.level"])).await;
        assert!(
            !forced.success,
            "a writer forced alice's lock off:\n{}",
            forced.output
        );
    }

    // Alice unlocks; Bob's push now goes through.
    {
        let (alice, work) = (alice.clone(), alice_checkout.clone());
        on_machine(move || alice.ok(&work, &["lfs", "unlock", "castle.level"])).await;
    }
    let pushed = {
        let (bob, work) = (bob.clone(), bob_checkout.clone());
        on_machine(move || bob.run(&work, &["push", "origin", "HEAD"])).await
    };
    assert!(
        pushed.success,
        "bob's push still failed after the unlock:\n{}",
        pushed.output
    );
    drop(root);
}
