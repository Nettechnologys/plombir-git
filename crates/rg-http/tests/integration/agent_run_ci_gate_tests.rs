//! CI waits for a person on what an agent run wrote (ADR-0001 D4,
//! card_f573f8f39585).
//!
//! A run's token writes as its bot. The run never sees the repository's CI
//! secrets, but a pipeline its write started would, and the workflow files on
//! the agent's branch are the agent's to write. So:
//!
//! - none of the three doors an agent writes through — `git push`, a commit
//!   through the contents editor (MCP `write_file` is that route), a review
//!   suggestion it applies — starts a pipeline, while the pull request's head
//!   SHA still follows the write;
//! - the pull request the run opens is that run's for good, and its CI runs
//!   only for the head a person with write access approved — after the token
//!   is gone too;
//! - one run opens one pull request.
//!
//! Each test drives the bot with a plain token first, through the same door:
//! that one must start a pipeline, so a door that starts none for everybody
//! cannot pass for a gate.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use crate::common::{
    build_test_app_state_with, register_full, register_user, setup_test_db, wait_for_listener,
    StateOverrides,
};

const BOT: &str = "alice-agent";
const PREFIX: &str = "refs/heads/agent/alice-agent/";
const BRANCH: &str = "agent/alice-agent/1-fix";

#[derive(Default)]
struct RecordingCiEngine {
    /// `(trigger_type, ref_name, commit_sha)` of every pipeline asked for.
    triggered: Mutex<Vec<(String, String, String)>>,
}

impl RecordingCiEngine {
    fn take(&self) -> Vec<(String, String, String)> {
        std::mem::take(&mut *self.triggered.lock().unwrap())
    }
}

impl rg_core::ci::CiTrigger for RecordingCiEngine {
    fn has_ci_config(&self, _repo_path: &Path, _commit_sha: &str) -> bool {
        true
    }

    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
        true
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        let entry = (
            params.trigger_type.to_string(),
            params.ref_name.to_string(),
            params.commit_sha.to_string(),
        );
        Box::pin(async move {
            let mut triggered = self.triggered.lock().unwrap();
            triggered.push(entry);
            Ok(i64::try_from(triggered.len()).unwrap())
        })
    }

    fn resume_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    assert!(output.success(), "git {args:?}: {}", output.stderr_str());
    output.stdout_str().trim().to_string()
}

struct Fixture {
    base: String,
    db: rg_db::DatabaseConnection,
    state: rg_http::AppState,
    ci: Arc<RecordingCiEngine>,
    alice: String,
    bot_id: i64,
    repo_id: i64,
    repo_root: PathBuf,
    bare: PathBuf,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

/// `alice/app` with `main` and the bot's `agent/alice-agent/1-fix`; the bot is
/// a writer, and an agent run of it is in progress with one attempt.
async fn fixture() -> Fixture {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let ci = Arc::new(RecordingCiEngine::default());
    let state = build_test_app_state_with(
        db.clone(),
        repo_root.clone(),
        StateOverrides {
            ci_engine: Some(ci.clone()),
            ..StateOverrides::default()
        },
    );
    let app = rg_http::create_router_for_test(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    register_user(&base, "root", "root@example.test", "Qz7$wRtm").await;
    let (alice, alice_id) = register_full(&base, "alice", "alice@example.test").await;
    let http = reqwest::Client::new();

    let repo =
        rg_core::repo::service::create_repo(&db, alice_id, "app", None, true, &repo_root, None)
            .await
            .unwrap();
    let bare = repo_root.join("alice/app.git");
    let worktree = tempfile::tempdir().unwrap();
    let seed = |args: &[&str]| git(args, Some(worktree.path()));
    seed(&["init", "--initial-branch=main"]);
    seed(&["config", "user.name", "Seed"]);
    seed(&["config", "user.email", "seed@example.test"]);
    std::fs::write(worktree.path().join("README.md"), "seed\n").unwrap();
    seed(&["add", "."]);
    seed(&["commit", "-m", "seed"]);
    let bare_arg = bare.to_string_lossy().to_string();
    for refspec in ["main", &format!("main:{BRANCH}")] {
        seed(&["push", &bare_arg, refspec]);
    }
    rg_db::ops::repo_ops::set_default_branch(&db, repo.id, "main")
        .await
        .unwrap();

    let response = http
        .post(format!("{base}/api/v1/users/bots"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "username": BOT, "display_name": "Alice's agent" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    let response = http
        .post(format!("{base}/api/v1/repos/alice/app/collaborators"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "username": BOT, "permission": "write" }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "add the bot");
    let bot_id = rg_db::ops::user_ops::find_by_username(&db, BOT)
        .await
        .unwrap()
        .expect("the bot exists")
        .id;

    let f = Fixture {
        base,
        db,
        state,
        ci,
        alice,
        bot_id,
        repo_id: repo.id,
        repo_root,
        bare,
        http,
        _dir: dir,
    };
    f.settle().await;
    f.ci.take();
    f
}

impl Fixture {
    /// An agent run of the bot in `alice/app` with one attempt; the attempt's id.
    async fn run_attempt(&self, trigger_key: &str) -> (i64, i64) {
        let now = chrono::Utc::now();
        let run = rg_db::entities::agent_run::ActiveModel {
            bot_id: Set(self.bot_id),
            repository_id: Set(self.repo_id),
            issue_id: Set(None),
            pull_request_id: Set(None),
            iteration: Set(1),
            triggered_by: Set(1),
            trigger_key: Set(trigger_key.to_string()),
            status: Set("running".to_string()),
            active: Set(Some(true)),
            result_pull_request_id: Set(None),
            failure_reason: Set(None),
            created_at: Set(now),
            finished_at: Set(None),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("record the run");
        let attempt = rg_db::entities::agent_run_attempt::ActiveModel {
            run_id: Set(run.id),
            attempt: Set(1),
            pipeline_id: Set(None),
            created_at: Set(now),
            finished_at: Set(None),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("record the attempt");
        (run.id, attempt.id)
    }

    /// A token of the bot confined to its prefix — the attempt's when
    /// `attempt` is given, a plain one of the same bot otherwise.
    async fn token(&self, attempt: Option<i64>) -> (i64, String) {
        let issued = rg_core::auth::access_token::issue(
            &self.db,
            rg_core::auth::access_token::NewAccessToken {
                user_id: self.bot_id,
                name: "agent".to_string(),
                scopes: "repo".to_string(),
                expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                repository_ids: Some(vec![self.repo_id]),
                mcp_tools: None,
                deny_protected_merge: true,
                write_ref_prefix: Some(PREFIX.to_string()),
                allow_git: false,
                agent_run_attempt_id: attempt,
            },
        )
        .await
        .expect("issue the token");
        (issued.token.id, issued.raw)
    }

    /// Wait until every detached post-push hook has finished.
    async fn settle(&self) {
        let tracker = &self.state.delivery_tracker;
        tracker.close();
        tokio::time::timeout(Duration::from_secs(120), tracker.wait())
            .await
            .expect("the post-push hooks drained");
        tracker.reopen();
    }

    fn head(&self, refname: &str) -> String {
        git(
            &[
                "--git-dir",
                self.bare.to_str().unwrap(),
                "rev-parse",
                "--verify",
                refname,
            ],
            None,
        )
    }

    /// Push one new commit onto the bot's branch with `token`; its sha.
    fn push_commit(&self, token: &str, file: &str) -> String {
        let host = self.base.trim_start_matches("http://");
        let remote = format!("http://{BOT}:{token}@{host}/alice/app.git");
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("app");
        git(
            &[
                "clone",
                "--branch",
                BRANCH,
                &remote,
                checkout.to_str().unwrap(),
            ],
            None,
        );
        git(&["config", "user.name", "Agent"], Some(&checkout));
        git(
            &["config", "user.email", "agent@example.test"],
            Some(&checkout),
        );
        std::fs::write(checkout.join(file), format!("{file}\n")).unwrap();
        git(&["add", "."], Some(&checkout));
        git(&["commit", "-m", file], Some(&checkout));
        git(
            &["push", &remote, &format!("HEAD:{BRANCH}")],
            Some(&checkout),
        );
        git(&["rev-parse", "HEAD"], Some(&checkout))
    }

    async fn commit_file(&self, token: &str, path: &str) -> String {
        let response = self
            .http
            .post(format!(
                "{}/api/v1/repos/alice/app/contents/{path}",
                self.base
            ))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "branch": BRANCH, "content": format!("{path}\n"), "message": path,
            }))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        self.head(&format!("refs/heads/{BRANCH}"))
    }

    async fn open_pr(&self, token: &str, title: &str) -> reqwest::Response {
        self.http
            .post(format!("{}/api/v1/repos/alice/app/pulls", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "title": title, "head": BRANCH, "base": "main" }))
            .send()
            .await
            .unwrap()
    }

    async fn pr(&self, number: i64) -> rg_db::entities::pull_request::Model {
        rg_db::ops::pull_request_ops::find_by_repo_and_number(&self.db, self.repo_id, number)
            .await
            .unwrap()
            .expect("the pull request exists")
    }

    /// Ask for the pull request's CI the way every trigger does; `true` when a
    /// pipeline was started.
    async fn pull_request_ci_runs(&self, number: i64) -> bool {
        let pr = self.pr(number).await;
        rg_core::pull_request::trigger_pull_request_ci(
            &self.db,
            &self.repo_root,
            &pr,
            None,
            &self.state.pipeline_ci(),
        )
        .await
        .expect("the gate answers")
        .is_some()
    }

    async fn approve(&self, number: i64) {
        let response = self
            .http
            .post(format!(
                "{}/api/v1/repos/alice/app/pulls/{number}/ci-approval",
                self.base
            ))
            .bearer_auth(&self.alice)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
    }

    /// Alice suggests `text` for the first line of `notes.txt` on PR #1.
    async fn suggest(&self, text: &str) -> i64 {
        let response = self
            .http
            .post(format!(
                "{}/api/v1/repos/alice/app/pulls/1/comments",
                self.base
            ))
            .bearer_auth(&self.alice)
            .json(&serde_json::json!({
                "path": "notes.txt", "line": 1, "side": "RIGHT",
                "body": "Change it", "suggestion": text,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
        response.json::<serde_json::Value>().await.unwrap()["id"]
            .as_i64()
            .unwrap()
    }

    async fn apply_suggestion(&self, token: &str, comment: i64) {
        let response = self
            .http
            .post(format!(
                "{}/api/v1/repos/alice/app/pulls/1/comments/{comment}/suggestion/apply",
                self.base
            ))
            .bearer_auth(token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    }

    fn events(entries: &[(String, String, String)]) -> Vec<&str> {
        entries.iter().map(|(event, _, _)| event.as_str()).collect()
    }
}

/// `git push`: the agent's push starts nothing, and its pull request's head
/// follows it. The same push with a plain token of the bot is a `push`
/// pipeline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_push_starts_no_pipeline_and_still_moves_the_head() {
    let f = fixture().await;
    let (_, attempt) = f.run_attempt("push").await;
    let (_, agent) = f.token(Some(attempt)).await;
    let (_, plain) = f.token(None).await;
    let response = f.open_pr(&agent, "agent work").await;
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    f.settle().await;
    assert!(
        f.ci.take().is_empty(),
        "opening the agent's pull request ran CI"
    );

    let control = f.push_commit(&plain, "plain.txt");
    f.settle().await;
    assert!(
        f.ci.take()
            .iter()
            .any(|(event, _, sha)| event == "push" && *sha == control),
        "the control push with a plain token started no push pipeline"
    );

    let sha = f.push_commit(&agent, "agent.txt");
    f.settle().await;
    let triggered = f.ci.take();
    assert!(
        triggered.is_empty(),
        "an agent's push started CI: {triggered:?}"
    );
    assert_eq!(
        f.pr(1).await.head_sha.as_deref(),
        Some(sha.as_str()),
        "the agent's push did not move its pull request's head"
    );
}

/// The contents editor — the route MCP `write_file` calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_editor_commit_starts_no_pipeline_and_still_moves_the_head() {
    let f = fixture().await;
    let (_, attempt) = f.run_attempt("write-file").await;
    let (_, agent) = f.token(Some(attempt)).await;
    let (_, plain) = f.token(None).await;
    assert_eq!(f.open_pr(&agent, "agent work").await.status(), 201);
    f.settle().await;
    f.ci.take();

    let control = f.commit_file(&plain, "plain.txt").await;
    f.settle().await;
    assert!(
        f.ci.take()
            .iter()
            .any(|(event, _, sha)| event == "push" && *sha == control),
        "the control commit with a plain token started no push pipeline"
    );

    let sha = f.commit_file(&agent, "agent.txt").await;
    f.settle().await;
    let triggered = f.ci.take();
    assert!(
        triggered.is_empty(),
        "an agent's editor commit started CI: {triggered:?}"
    );
    assert_eq!(f.pr(1).await.head_sha.as_deref(), Some(sha.as_str()));
}

/// A review suggestion the agent applies to its own pull request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_applied_suggestion_starts_no_pipeline_and_still_moves_the_head() {
    let f = fixture().await;
    let (_, attempt) = f.run_attempt("suggestion").await;
    let (_, agent) = f.token(Some(attempt)).await;
    let (_, plain) = f.token(None).await;
    // Something on the branch to suggest against.
    f.commit_file(&plain, "notes.txt").await;
    assert_eq!(f.open_pr(&agent, "agent work").await.status(), 201);
    f.settle().await;
    f.ci.take();

    let first = f.suggest("first\n").await;
    f.apply_suggestion(&plain, first).await;
    f.settle().await;
    let control = f.head(&format!("refs/heads/{BRANCH}"));
    assert!(
        f.ci.take()
            .iter()
            .any(|(event, _, sha)| event == "push" && *sha == control),
        "the control suggestion with a plain token started no push pipeline"
    );

    let second = f.suggest("second\n").await;
    f.apply_suggestion(&agent, second).await;
    f.settle().await;
    let triggered = f.ci.take();
    assert!(
        triggered.is_empty(),
        "an agent's applied suggestion started CI: {triggered:?}"
    );
    let head = f.head(&format!("refs/heads/{BRANCH}"));
    assert_ne!(head, control, "the suggestion was not committed");
    assert_eq!(f.pr(1).await.head_sha.as_deref(), Some(head.as_str()));
}

/// The pull request the run opened waits for a person's approval of exactly
/// its head, every head, and after the run's token is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_agents_pull_request_runs_ci_only_for_an_approved_head() {
    let f = fixture().await;
    let (run_id, attempt) = f.run_attempt("gate").await;
    let (token_id, agent) = f.token(Some(attempt)).await;
    assert_eq!(f.open_pr(&agent, "agent work").await.status(), 201);
    f.settle().await;
    assert!(f.ci.take().is_empty(), "CI ran on the agent's opened head");

    let pr = f.pr(1).await;
    let run = rg_db::entities::agent_run::Entity::find_by_id(run_id)
        .one(&f.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        run.result_pull_request_id,
        Some(pr.id),
        "the run does not name its pull request"
    );
    assert!(
        !f.pull_request_ci_runs(1).await,
        "an unapproved agent head ran"
    );

    f.approve(1).await;
    f.settle().await;
    let released = f.ci.take();
    assert_eq!(
        Fixture::events(&released),
        ["pull_request"],
        "approving the agent's head did not run its CI"
    );
    assert_eq!(released[0].2, pr.head_sha.clone().unwrap());

    // A new head is a new question.
    f.push_commit(&agent, "more.txt");
    f.settle().await;
    assert!(f.ci.take().is_empty());
    assert!(
        !f.pull_request_ci_runs(1).await,
        "the approval of the old head released the new one"
    );

    // The token goes when the run ends; the gate stays.
    rg_db::entities::access_token::Entity::delete_by_id(token_id)
        .exec(&f.db)
        .await
        .unwrap();
    assert!(
        !f.pull_request_ci_runs(1).await,
        "the gate went with the token"
    );
    f.approve(1).await;
    f.settle().await;
    assert_eq!(Fixture::events(&f.ci.take()), ["pull_request"]);
}

/// One run, one pull request: a second one would carry the same agent's code
/// past the gate the first is held by.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_opens_one_pull_request() {
    let f = fixture().await;
    let (run_id, attempt) = f.run_attempt("one-pr").await;
    let (_, agent) = f.token(Some(attempt)).await;
    assert_eq!(f.open_pr(&agent, "first").await.status(), 201);
    let first = f.pr(1).await;

    git(
        &[
            "--git-dir",
            f.bare.to_str().unwrap(),
            "branch",
            "agent/alice-agent/2-more",
            "main",
        ],
        None,
    );
    let second = f
        .http
        .post(format!("{}/api/v1/repos/alice/app/pulls", f.base))
        .bearer_auth(&agent)
        .json(&serde_json::json!({
            "title": "second", "head": "agent/alice-agent/2-more", "base": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 409, "{}", second.text().await.unwrap());
    assert!(
        rg_db::ops::pull_request_ops::find_by_repo_and_number(&f.db, f.repo_id, 2)
            .await
            .unwrap()
            .is_none(),
        "the refused pull request was stored"
    );

    // The refusal above is the up-front check. Behind it the link itself is
    // conditional, which is what holds when two pull requests race out of
    // one run past that check: the second link is refused, the first kept.
    assert!(
        !rg_db::ops::agent_run_ops::record_opened_pull_request(&f.db, run_id, first.id + 1000)
            .await
            .unwrap(),
        "a run's pull request was replaced"
    );
    let run = rg_db::entities::agent_run::Entity::find_by_id(run_id)
        .one(&f.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(run.result_pull_request_id, Some(first.id));
}
