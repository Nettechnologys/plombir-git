//! A token issued for one attempt of an agent run (ADR-0001 D3,
//! card_dc65d537dba4): it writes only the refs its `write_ref_prefix` admits,
//! merges nothing, and with `allow_git` clones and pushes while REST outside
//! the MCP endpoint stays closed to it.
//!
//! Each write point the server has gets its own test below: a push, a
//! server-side commit (the contents editor — review suggestions take the same
//! door), a ref the server moves (branch and tag creation and deletion), and
//! the three ways to merge. The tokens here carry no MCP narrowing unless a
//! test is about it, so every request reaches the inner check itself: with the
//! MCP narrowing in front, a missing inner check would hide behind it.

use std::path::Path;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::common::{
    build_test_app_state_with, register_full, register_user, setup_test_db, wait_for_listener,
    StateOverrides,
};

const BOT: &str = "alice-agent";
const PREFIX: &str = "refs/heads/agent/alice-agent/";

fn git(args: &[&str], cwd: Option<&Path>) {
    let (success, stderr) = git_outcome(args, cwd);
    assert!(success, "git {args:?} failed: {}", stderr.trim());
}

fn git_outcome(args: &[&str], cwd: Option<&Path>) -> (bool, String) {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway must initialize");
    let output = gateway.run(args, cwd).expect("git invocation failed");
    (output.success(), output.stderr_str().to_string())
}

fn git_stdout(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway must initialize");
    let output = gateway.run(args, cwd).expect("git invocation failed");
    assert!(output.success(), "git {args:?}: {}", output.stderr_str());
    output.stdout_str().trim().to_string()
}

struct Fixture {
    base: String,
    db: rg_db::DatabaseConnection,
    alice: String,
    bot_id: i64,
    repo_id: i64,
    bare: std::path::PathBuf,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

/// `alice/app` with `main`, `feature`, the bot's own `agent/alice-agent/1-fix`
/// and the tag `v1`; the bot is a writer on it and nothing is protected, so
/// only the token's confinement can refuse anything.
async fn fixture() -> Fixture {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let state = build_test_app_state_with(db.clone(), repo_root.clone(), StateOverrides::default());
    let app = rg_http::create_router_for_test(state);
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
    seed(&["tag", "v1"]);
    let bare_arg = bare.to_string_lossy().to_string();
    for refspec in [
        "main",
        "main:feature",
        "main:agent/alice-agent/1-fix",
        "main:agent/alice-agent/2-other",
        "v1",
    ] {
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

    Fixture {
        base,
        db,
        alice,
        bot_id,
        repo_id: repo.id,
        bare,
        http,
        _dir: dir,
    }
}

impl Fixture {
    /// A token for the bot as an agent run gets it: one repository, kept off
    /// protected branches, confined to `write_ref_prefix`.
    async fn run_token(
        &self,
        write_ref_prefix: &str,
        mcp_tools: Option<&str>,
        allow_git: bool,
    ) -> String {
        rg_core::auth::access_token::issue(
            &self.db,
            rg_core::auth::access_token::NewAccessToken {
                user_id: self.bot_id,
                name: "agent run".to_string(),
                scopes: "repo".to_string(),
                expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                repository_ids: Some(vec![self.repo_id]),
                mcp_tools: mcp_tools.map(str::to_string),
                deny_protected_merge: true,
                write_ref_prefix: Some(write_ref_prefix.to_string()),
                allow_git,
                agent_run_attempt_id: None,
            },
        )
        .await
        .expect("issue the run token")
        .raw
    }

    fn remote(&self, token: &str) -> String {
        let host = self.base.trim_start_matches("http://");
        format!("http://{BOT}:{token}@{host}/alice/app.git")
    }

    fn head(&self, refname: &str) -> Option<String> {
        let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        let output = gateway
            .run(
                &[
                    "--git-dir",
                    self.bare.to_str().unwrap(),
                    "rev-parse",
                    "--verify",
                    "-q",
                    refname,
                ],
                None,
            )
            .unwrap();
        output
            .success()
            .then(|| output.stdout_str().trim().to_string())
    }

    /// A checkout of `main` with one new commit on top.
    fn checkout_with_commit(&self, token: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("app");
        git(
            &["clone", &self.remote(token), checkout.to_str().unwrap()],
            None,
        );
        git(&["config", "user.name", "Agent"], Some(&checkout));
        git(
            &["config", "user.email", "agent@example.test"],
            Some(&checkout),
        );
        std::fs::write(checkout.join("agent.txt"), "agent\n").unwrap();
        git(&["add", "."], Some(&checkout));
        git(&["commit", "-m", "agent work"], Some(&checkout));
        let sha = git_stdout(&["rev-parse", "HEAD"], Some(&checkout));
        (dir, checkout, sha)
    }

    async fn commit_file(&self, token: &str, branch: &str, path: &str) -> reqwest::StatusCode {
        self.http
            .post(format!(
                "{}/api/v1/repos/alice/app/contents/{path}",
                self.base
            ))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "branch": branch, "content": "agent\n", "message": "agent commit",
            }))
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn create_branch(&self, token: &str, name: &str) -> reqwest::StatusCode {
        self.http
            .post(format!("{}/api/v1/repos/alice/app/branches", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "name": name, "from": "main" }))
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn delete(&self, token: &str, kind: &str, name: &str) -> reqwest::StatusCode {
        self.http
            .delete(format!(
                "{}/api/v1/repos/alice/app/{kind}/{}",
                self.base,
                name.replace('/', "%2F")
            ))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn denials(&self) -> Vec<serde_json::Value> {
        rg_db::entities::audit_log::Entity::find()
            .filter(rg_db::entities::audit_log::Column::Action.eq("agent.scope_denied"))
            .order_by_asc(rg_db::entities::audit_log::Column::Id)
            .all(&self.db)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|row| row.details)
            .map(|details| serde_json::from_str::<serde_json::Value>(&details).unwrap())
            .filter(|details| details["reason"] == "write_ref_prefix")
            .collect()
    }
}

/// `git push` — receive-pack holds the whole push to the token's refs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_token_pushes_only_under_its_prefix() {
    let f = fixture().await;
    let token = f.run_token(PREFIX, None, false).await;
    let (_dir, checkout, sha) = f.checkout_with_commit(&token);
    let push = |refspecs: &[&str]| {
        let mut args = vec!["push".to_string(), f.remote(&token)];
        args.extend(refspecs.iter().map(|s| s.to_string()));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git_outcome(&args, Some(&checkout))
    };

    let main_before = f.head("refs/heads/main");
    let (pushed, stderr) = push(&["HEAD:main"]);
    assert!(!pushed, "the run token pushed to an unprotected main");
    assert!(
        stderr.contains("this token may not write ref 'refs/heads/main'"),
        "{stderr}"
    );
    assert_eq!(f.head("refs/heads/main"), main_before);
    let (pushed, stderr) = push(&["HEAD:refs/tags/agent"]);
    assert!(!pushed, "the run token pushed a tag: {stderr}");
    let (pushed, stderr) = push(&["HEAD:agent/alice-agent-two/x"]);
    assert!(!pushed, "a sibling prefix is not the bot's: {stderr}");

    // One ref outside takes the whole push down, the admitted one with it.
    let (pushed, stderr) = push(&["HEAD:agent/alice-agent/3-new", "HEAD:feature"]);
    assert!(!pushed, "{stderr}");
    assert_eq!(f.head("refs/heads/agent/alice-agent/3-new"), None);
    assert!(stderr.contains("refused with the whole push"), "{stderr}");

    let (pushed, stderr) = push(&["HEAD:agent/alice-agent/3-new"]);
    assert!(pushed, "a push under the prefix: {stderr}");
    assert_eq!(
        f.head("refs/heads/agent/alice-agent/3-new").as_deref(),
        Some(sha.as_str())
    );
    let (pushed, stderr) = push(&[":agent/alice-agent/3-new"]);
    assert!(pushed, "deleting its own branch under the prefix: {stderr}");

    let denied = f.denials().await;
    let refs: Vec<_> = denied.iter().map(|row| row["ref"].clone()).collect();
    assert_eq!(
        refs,
        [
            "refs/heads/main",
            "refs/tags/agent",
            "refs/heads/agent/alice-agent-two/x",
            "refs/heads/feature",
        ],
        "one row per out-of-scope ref, none for the ref it took down: {denied:?}"
    );
    assert!(denied.iter().all(|row| row["transport"] == "git"));
}

/// On an iteration the token holds exactly the pull request's branch: it may
/// move it, and nothing else — not a sibling of the same bot, not a tag, not
/// the branch's deletion, not a multi-ref push with a foreign ref in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_iteration_token_moves_only_its_pull_requests_branch() {
    let f = fixture().await;
    let own = "refs/heads/agent/alice-agent/1-fix";
    let token = f.run_token(own, None, false).await;
    let (_dir, checkout, sha) = f.checkout_with_commit(&token);
    let push = |refspecs: &[&str]| {
        let mut args = vec!["push".to_string(), f.remote(&token)];
        args.extend(refspecs.iter().map(|s| s.to_string()));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git_outcome(&args, Some(&checkout))
    };
    let before = f.head(own);

    for refused in [
        vec!["HEAD:agent/alice-agent/2-other"],
        vec!["HEAD:refs/tags/agent-iteration"],
        vec![":agent/alice-agent/1-fix"],
        vec![
            "HEAD:agent/alice-agent/1-fix",
            "HEAD:agent/alice-agent/2-other",
        ],
    ] {
        let (pushed, stderr) = push(&refused);
        assert!(!pushed, "{refused:?} was admitted: {stderr}");
        assert_eq!(f.head(own), before, "{refused:?} moved the branch");
    }
    assert_eq!(
        f.delete(&token, "branches", "agent/alice-agent/1-fix")
            .await,
        403,
        "the REST door refuses the deletion the push did"
    );

    let (pushed, stderr) = push(&["HEAD:agent/alice-agent/1-fix"]);
    assert!(pushed, "the iteration's own branch: {stderr}");
    assert_eq!(f.head(own).as_deref(), Some(sha.as_str()));
}

/// The contents editor — a server-side commit, which review suggestions make
/// through the same policy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_token_commits_through_the_editor_only_under_its_prefix() {
    let f = fixture().await;
    let token = f.run_token(PREFIX, None, false).await;
    let main_before = f.head("refs/heads/main");
    assert_eq!(f.commit_file(&token, "main", "main.txt").await, 403);
    assert_eq!(f.commit_file(&token, "feature", "feature.txt").await, 403);
    assert_eq!(f.head("refs/heads/main"), main_before);
    let own = f
        .commit_file(&token, "agent/alice-agent/1-fix", "fix.txt")
        .await;
    assert!(own.is_success(), "{own}");

    let denied = f.denials().await;
    assert_eq!(denied.len(), 2, "{denied:?}");
    assert_eq!(denied[0]["ref"], "refs/heads/main");
    assert_eq!(denied[1]["ref"], "refs/heads/feature");
}

/// Branches and tags the server creates or deletes on the token's behalf.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_token_creates_and_deletes_refs_only_under_its_prefix() {
    let f = fixture().await;
    let token = f.run_token(PREFIX, None, false).await;

    assert_eq!(f.create_branch(&token, "elsewhere").await, 403);
    assert_eq!(f.head("refs/heads/elsewhere"), None);
    assert_eq!(f.delete(&token, "branches", "feature").await, 403);
    assert!(f.head("refs/heads/feature").is_some());
    assert_eq!(f.delete(&token, "tags", "v1").await, 403);
    assert!(f.head("refs/tags/v1").is_some());

    let created = f.create_branch(&token, "agent/alice-agent/4-new").await;
    assert!(created.is_success(), "{created}");
    let deleted = f
        .delete(&token, "branches", "agent/alice-agent/4-new")
        .await;
    assert!(deleted.is_success(), "{deleted}");
    assert_eq!(f.head("refs/heads/agent/alice-agent/4-new"), None);

    let refs: Vec<_> = f
        .denials()
        .await
        .iter()
        .map(|row| row["ref"].clone())
        .collect();
    assert_eq!(
        refs,
        ["refs/heads/elsewhere", "refs/heads/feature", "refs/tags/v1"]
    );
}

/// A merge writes the base, and a run token writes no base: not `main`, not
/// even a branch under its own prefix.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_token_merges_nothing() {
    let f = fixture().await;
    let token = f.run_token(PREFIX, None, false).await;
    assert!(f
        .commit_file(&token, "agent/alice-agent/1-fix", "fix.txt")
        .await
        .is_success());

    let mut numbers = Vec::new();
    for base in ["main", "agent/alice-agent/2-other"] {
        let response = f
            .http
            .post(format!("{}/api/v1/repos/alice/app/pulls", f.base))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "title": format!("into {base}"), "head": "agent/alice-agent/1-fix", "base": base,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
        let created: serde_json::Value = response.json().await.unwrap();
        numbers.push(created["number"].as_i64().unwrap());
    }

    for number in &numbers {
        let merged = f
            .http
            .post(format!(
                "{}/api/v1/repos/alice/app/pulls/{number}/merge",
                f.base
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "strategy": "merge" }))
            .send()
            .await
            .unwrap();
        assert_eq!(merged.status(), 403, "merge of #{number}");
        for schedule in ["auto-merge", "merge-queue"] {
            let scheduled = f
                .http
                .put(format!(
                    "{}/api/v1/repos/alice/app/pulls/{number}/{schedule}",
                    f.base
                ))
                .bearer_auth(&token)
                .json(&serde_json::json!({ "strategy": "merge" }))
                .send()
                .await
                .unwrap();
            assert_eq!(scheduled.status(), 403, "{schedule} of #{number}");
        }
    }
    let actions: Vec<_> = f
        .denials()
        .await
        .iter()
        .map(|row| row["action"].clone())
        .collect();
    assert_eq!(
        actions,
        [
            "merge_pr",
            "enable_auto_merge",
            "enqueue_merge_queue",
            "merge_pr",
            "enable_auto_merge",
            "enqueue_merge_queue",
        ]
    );

    // The person merges what the agent could not.
    let merged = f
        .http
        .post(format!(
            "{}/api/v1/repos/alice/app/pulls/{}/merge",
            f.base, numbers[0]
        ))
        .bearer_auth(&f.alice)
        .json(&serde_json::json!({ "strategy": "merge" }))
        .send()
        .await
        .unwrap();
    assert!(
        merged.status().is_success(),
        "{}",
        merged.text().await.unwrap()
    );
}

/// `allow_git` opens the git transport to a token confined to MCP tools, and
/// nothing else: REST outside the MCP endpoint stays a 403.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_git_allowed_tool_token_clones_and_pushes_but_has_no_rest() {
    let f = fixture().await;
    let tools = Some("read_file,create_pr");
    let git_token = f.run_token(PREFIX, tools, true).await;
    let mcp_only = f.run_token(PREFIX, tools, false).await;

    let dir = tempfile::tempdir().unwrap();
    let refused = dir.path().join("refused");
    let (cloned, stderr) = git_outcome(
        &["clone", &f.remote(&mcp_only), refused.to_str().unwrap()],
        None,
    );
    assert!(
        !cloned,
        "a tool-confined token without allow_git cloned: {stderr}"
    );

    let (_dir, checkout, sha) = f.checkout_with_commit(&git_token);
    let (pushed, stderr) = git_outcome(
        &[
            "push",
            &f.remote(&git_token),
            "HEAD:agent/alice-agent/5-git",
        ],
        Some(&checkout),
    );
    assert!(pushed, "{stderr}");
    assert_eq!(
        f.head("refs/heads/agent/alice-agent/5-git").as_deref(),
        Some(sha.as_str())
    );
    let (pushed, stderr) = git_outcome(
        &["push", &f.remote(&git_token), "HEAD:main"],
        Some(&checkout),
    );
    assert!(
        !pushed,
        "the prefix holds over git for this token too: {stderr}"
    );

    for token in [&git_token, &mcp_only] {
        let response = f
            .http
            .get(format!("{}/api/v1/repos/alice/app", f.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403, "REST outside MCP");
        let response = f
            .http
            .post(format!(
                "{}/api/v1/repos/alice/app/contents/rest.txt",
                f.base
            ))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "branch": "agent/alice-agent/1-fix", "content": "x\n", "message": "rest",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403, "a REST write outside MCP");
    }
}
