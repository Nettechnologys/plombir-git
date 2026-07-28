//! Live SSH coverage for the post-push hooks (card_b4fefeee8abf).
//!
//! The hooks — CI trigger, webhook fan-out, open-PR head-SHA refresh — used to
//! live inside `rg-http`'s Smart-HTTP handler and were reachable from nowhere
//! else, so a push over SSH ran *none* of them: no pipeline for a repo carrying
//! a `.forgekeep-ci.yml`, no `push` webhook, and an open PR left pointing at the
//! commit it was opened on. SSH is the default transport once a key is
//! registered, so half the users had no automation at all and nothing said so.
//!
//! This drives a real `git push` through a live sshd and asserts the effects on
//! the far side of the delivery-tracker drain, the same way the HTTP twin
//! (`rg-http::push_hook_drain_tests`) does.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::{ActiveValue::NotSet, Set};

/// One recorded `trigger_pipeline` call: the commit and ref it was fired for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TriggeredPipeline {
    commit_sha: String,
    ref_name: String,
    trigger_type: String,
}

/// A `CiTrigger` that claims every commit carries CI config and records what it
/// was asked to run. Keeps the test free of `rg-ci` (not a dependency here) while
/// still proving the push path reaches the engine with the right commit.
#[derive(Default)]
struct RecordingCi {
    triggered: Arc<Mutex<Vec<TriggeredPipeline>>>,
}

impl rg_core::ci::CiTrigger for RecordingCi {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        true
    }

    /// Mirrors `has_ci_config`: this double has no workflow files to
    /// match an event against, so it answers the same for every event.
    fn has_workflow_for_event(
        &self,
        _repo_path: &std::path::Path,
        _commit_sha: &str,
        _event: &str,
        _ref_name: &str,
        _base_branch: Option<&str>,
    ) -> bool {
        true
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        self.triggered.lock().unwrap().push(TriggeredPipeline {
            commit_sha: params.commit_sha.to_string(),
            ref_name: params.ref_name.to_string(),
            trigger_type: params.trigger_type.to_string(),
        });
        Box::pin(async { Ok(4242) })
    }

    fn resume_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// Stand-in for `rg_http::ws::NotificationHub` — the SSH crate can't depend on
/// the HTTP layer, and the seam under test is the `PushNotifier` trait anyway.
#[derive(Default)]
struct RecordingNotifier {
    events: Arc<Mutex<Vec<(i64, String)>>>,
}

impl rg_core::push_hooks::PushNotifier for RecordingNotifier {
    fn notify(&self, user_id: i64, event_type: &str, _data: serde_json::Value) {
        self.events
            .lock()
            .unwrap()
            .push((user_id, event_type.to_string()));
    }
}

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

/// Block until the SSH server bound its port (the bind happens in the spawned
/// task, so a connect can genuinely be refused for a moment).
async fn wait_for_listener(addr: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "SSH listener did not start on {addr} within 10s"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ssh_push_runs_the_post_push_hooks() {
    let app_dir = tempfile::tempdir().unwrap();
    let db_path = app_dir.path().join("test.db");
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", db_path.display()),
        5,
        60,
        2,
    )
    .await
    .unwrap();
    rg_db::run_migrations(&db).await.unwrap();

    let user = rg_db::ops::user_ops::create_user(
        &db,
        "hook-owner",
        "hook-owner@example.com",
        "",
        "Hook Owner",
    )
    .await
    .unwrap();
    let repo_root = app_dir.path().join("repos");
    let repo = rg_core::repo::service::create_repo(
        &db,
        user.id,
        "hook-repo",
        None,
        true,
        &repo_root,
        None,
    )
    .await
    .unwrap();

    // An open PR on the branch about to be pushed. Refreshing its head SHA is a
    // pure DB write inside the hook task — no CI engine, no outbound HTTP — so
    // observing it proves the hooks ran, not that something else did.
    let now = chrono::Utc::now();
    let stale_sha = "1111111111111111111111111111111111111111";
    let pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            number: Set(1),
            title: Set("hooks must fire over SSH too".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(user.id),
            reviewer_id: Set(None),
            head_branch: Set("main".to_string()),
            base_branch: Set("release".to_string()),
            head_sha: Set(Some(stale_sha.to_string())),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("seed open PR");

    // ── Client key, registered for the repo owner. ──
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
            id: NotSet,
            user_id: Set(user.id),
            title: Set("post-push hook test".to_string()),
            public_key: Set(public_key.to_string()),
            fingerprint: Set(fingerprint),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap();

    // ── SSH server, wired with the same hooks the HTTP transport runs. ──
    let triggered = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let ci_engine = Arc::new(RecordingCi {
        triggered: triggered.clone(),
    });
    let notifier = Arc::new(RecordingNotifier {
        events: events.clone(),
    });

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let listen_addr = probe.local_addr().unwrap().to_string();
    drop(probe);
    let server_config = rg_ssh::SshServerConfig {
        host_key_path: app_dir.path().join("host_ed25519"),
        listen_addr: listen_addr.clone(),
        repo_root: repo_root.clone(),
        db: Some(db.clone()),
        instance_settings: Default::default(),
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        post_push: Some(Arc::new(rg_core::push_hooks::PostPushContext {
            repo_root: repo_root.clone(),
            docker_enabled: false,
            external_runners: false,
            allow_host_runner: false,
            jwt_secret: Some("test-secret".to_string()),
            smtp_config: None,
            ci_engine,
            external_url: None,
            notifier: Some(notifier),
        })),
    };
    let server = tokio::spawn(async move {
        rg_ssh::start_ssh_server(server_config).await.unwrap();
    });
    wait_for_listener(&listen_addr).await;

    // ── A real commit, pushed over the live SSH transport. ──
    let worktree = tempfile::tempdir().unwrap();
    let worktree_arg = worktree.path().to_string_lossy();
    git(&["init", "--initial-branch=main", &worktree_arg], None);
    git(&["config", "user.name", "SSH Hooks"], Some(worktree.path()));
    git(
        &["config", "user.email", "ssh-hooks@example.com"],
        Some(worktree.path()),
    );
    std::fs::write(worktree.path().join("README.md"), "pushed over SSH\n").unwrap();
    git(&["add", "."], Some(worktree.path()));
    git(
        &["commit", "-m", "commit that must trigger hooks"],
        Some(worktree.path()),
    );
    let pushed_sha = git(&["rev-parse", "HEAD"], Some(worktree.path()));

    let remote = format!("ssh://git@{}/hook-owner/hook-repo.git", listen_addr);
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

    // ── The shutdown drain, as `rg_http::run` performs it. ──
    //
    // The hooks are detached; the tracker is what makes "detached" survivable,
    // and awaiting it here is also what makes the assertions deterministic
    // instead of a sleep-and-hope.
    let tracker = rg_core::task_tracker::delivery_tracker();
    tracker.close();
    // Hang-guard, not a deadline: untracked work makes `wait()` return
    // instantly (the assertions below are what fail then), while a tight bound
    // would just turn machine load into a red suite.
    tokio::time::timeout(Duration::from_secs(120), tracker.wait())
        .await
        .expect("delivery tracker drained within timeout");
    tracker.reopen();

    let refreshed = rg_db::ops::pull_request_ops::find_by_id(&db, pr.id)
        .await
        .expect("reload PR")
        .expect("PR still exists");
    assert_eq!(
        refreshed.head_sha.as_deref(),
        Some(pushed_sha.as_str()),
        "a push over SSH must refresh the open PR's head SHA — it is still the \
         pre-push value, so the post-push hooks never ran"
    );

    let triggered = triggered.lock().unwrap().clone();
    assert_eq!(
        triggered,
        vec![
            // The pushed branch heads an open PR, so the push synchronises it
            // and raises the `pull_request` event too (card_074d93bfe327).
            TriggeredPipeline {
                commit_sha: pushed_sha.clone(),
                ref_name: "refs/pull/1/head".to_string(),
                trigger_type: "pull_request".to_string(),
            },
            TriggeredPipeline {
                commit_sha: pushed_sha.clone(),
                ref_name: "refs/heads/main".to_string(),
                trigger_type: "push".to_string(),
            },
        ],
        "a push over SSH into a repo with CI config must trigger exactly one \
         pipeline per event the pushed commit raises"
    );

    let events = events.lock().unwrap().clone();
    assert!(
        events.contains(&(user.id, "ci_triggered".to_string())),
        "the repo owner must get the ci_triggered notification; got {events:?}"
    );
    assert!(
        events.contains(&(user.id, "push".to_string())),
        "the repo owner must get the push notification; got {events:?}"
    );

    server.abort();
}
