//! Agents as first-class participants (card_60a80311d512): a bot account of
//! its own, a token narrowed to repositories / MCP tools / unprotected
//! branches, a budget of its own, and an audit trail that says which tool did
//! what.
//!
//! Every test drives the real router over HTTP: the narrowing is enforced by
//! layers (the PAT middleware, the per-route gate, the MCP endpoint's in-process
//! dispatch), so a test that called a handler directly would be testing
//! nothing.

use std::path::Path;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::common::{
    build_test_app_state_with, register_full, register_user, setup_test_db, wait_for_listener,
    StateOverrides,
};

fn git(args: &[&str], cwd: Option<&Path>) {
    let (success, stderr) = git_outcome(args, cwd);
    assert!(success, "git {args:?} failed: {}", stderr.trim());
}

/// Run git and report whether it succeeded, with what it printed to stderr.
fn git_outcome(args: &[&str], cwd: Option<&Path>) -> (bool, String) {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway must initialize");
    let output = gateway.run(args, cwd).expect("git invocation failed");
    (output.success(), output.stderr_str().to_string())
}

struct Fixture {
    base: String,
    db: rg_db::DatabaseConnection,
    /// Instance administrator — registered first, so `alice` is not one.
    root: String,
    /// The human who owns the bot.
    alice: String,
    alice_id: i64,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

/// A server with `alice/app` (main + feature, the bot is a writer on it) and
/// `alice/other` (the bot is a writer there too — so only the token's
/// confinement can keep it out).
async fn fixture(overrides: StateOverrides) -> Fixture {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let state = build_test_app_state_with(db.clone(), repo_root.clone(), overrides);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    let root = register_user(&base, "root", "root@example.test", "Qz7$wRtm").await;
    let (alice, alice_id) = register_full(&base, "alice", "alice@example.test").await;
    let http = reqwest::Client::new();

    for name in ["app", "other"] {
        let repo =
            rg_core::repo::service::create_repo(&db, alice_id, name, None, true, &repo_root, None)
                .await
                .unwrap();
        let bare = repo_root.join(format!("alice/{name}.git"));
        let bare = bare.to_string_lossy().to_string();
        let worktree = tempfile::tempdir().unwrap();
        git(&["init", "--initial-branch=main"], Some(worktree.path()));
        git(&["config", "user.name", "Seed"], Some(worktree.path()));
        git(
            &["config", "user.email", "seed@example.test"],
            Some(worktree.path()),
        );
        std::fs::write(worktree.path().join("README.md"), "seed\n").unwrap();
        git(&["add", "."], Some(worktree.path()));
        git(&["commit", "-m", "seed"], Some(worktree.path()));
        git(&["push", &bare, "main"], Some(worktree.path()));
        git(&["checkout", "-b", "feature"], Some(worktree.path()));
        std::fs::write(worktree.path().join("feature.txt"), "feature\n").unwrap();
        git(&["add", "."], Some(worktree.path()));
        git(&["commit", "-m", "feature"], Some(worktree.path()));
        git(&["push", &bare, "feature"], Some(worktree.path()));
        rg_db::ops::repo_ops::set_default_branch(&db, repo.id, "main")
            .await
            .unwrap();
    }

    let response = http
        .post(format!("{base}/api/v1/users/bots"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "username": "alice-agent", "display_name": "Alice's agent" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    for name in ["app", "other"] {
        let response = http
            .post(format!("{base}/api/v1/repos/alice/{name}/collaborators"))
            .bearer_auth(&alice)
            .json(&serde_json::json!({ "username": "alice-agent", "permission": "write" }))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "add the bot to {name}: {} {}",
            response.status(),
            response.text().await.unwrap()
        );
    }

    Fixture {
        base,
        db,
        root,
        alice,
        alice_id,
        http,
        _dir: dir,
    }
}

impl Fixture {
    /// Mint a token for the bot; returns the raw token.
    async fn bot_token(&self, body: serde_json::Value) -> String {
        let response = self
            .http
            .post(format!(
                "{}/api/v1/users/bots/alice-agent/tokens",
                self.base
            ))
            .bearer_auth(&self.alice)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["scopes"], "repo", "a bot's token carries `repo` only");
        body["token"].as_str().unwrap().to_string()
    }

    async fn get(&self, token: &str, path: &str) -> reqwest::StatusCode {
        self.http
            .get(format!("{}/api/v1{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status()
    }

    /// One JSON-RPC message to the MCP endpoint: the HTTP status and the body.
    async fn mcp(
        &self,
        token: &str,
        method: &str,
        params: serde_json::Value,
    ) -> (reqwest::StatusCode, serde_json::Value) {
        let response = self
            .http
            .post(format!("{}/api/v1/mcp", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }))
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or_default())
    }

    /// `tools/call`: whether the tool failed, and the text it answered.
    async fn tool(&self, token: &str, name: &str, arguments: serde_json::Value) -> (bool, String) {
        let (status, body) = self
            .mcp(
                token,
                "tools/call",
                serde_json::json!({ "name": name, "arguments": arguments }),
            )
            .await;
        assert_eq!(status, 200, "{name}: {body}");
        (
            body["result"]["isError"].as_bool().unwrap(),
            body["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string(),
        )
    }

    async fn audit(&self, action: &str) -> Vec<rg_db::entities::audit_log::Model> {
        rg_db::entities::audit_log::Entity::find()
            .filter(rg_db::entities::audit_log::Column::Action.eq(action))
            .order_by_asc(rg_db::entities::audit_log::Column::Id)
            .all(&self.db)
            .await
            .unwrap()
    }

    async fn denials(&self, reason: &str) -> Vec<serde_json::Value> {
        self.audit("agent.scope_denied")
            .await
            .into_iter()
            .filter_map(|row| row.details)
            .map(|details| serde_json::from_str::<serde_json::Value>(&details).unwrap())
            .filter(|details| details["reason"] == reason)
            .collect()
    }
}

/// The card's acceptance: an agent under a bot account opens an issue and a
/// pull request through MCP over HTTP, and an attempt outside its scope is a
/// `403` that the audit log records.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_opens_an_issue_and_a_pr_through_mcp_and_is_refused_outside_its_scope() {
    let f = fixture(StateOverrides::default()).await;
    let token = f
        .bot_token(serde_json::json!({ "name": "agent", "repositories": ["alice/app"] }))
        .await;

    let (_, init) = f
        .mcp(
            &token,
            "initialize",
            serde_json::json!({ "protocolVersion": "2025-03-26" }),
        )
        .await;
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");

    let (failed, issue) = f
        .tool(
            &token,
            "create_issue",
            serde_json::json!({ "owner": "alice", "repo": "app", "title": "Found by the agent" }),
        )
        .await;
    assert!(!failed, "create_issue failed: {issue}");
    let issue: serde_json::Value = serde_json::from_str(&issue).unwrap();
    let number = issue["number"].as_i64().unwrap();
    let issue: serde_json::Value = f
        .http
        .get(format!("{}/api/v1/repos/alice/app/issues/{number}", f.base))
        .bearer_auth(&f.alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        issue["author"], "alice-agent",
        "the issue is the bot's, not its owner's"
    );
    assert_eq!(
        issue["author_bot_owner"], "alice",
        "and it says on whose behalf the bot acts"
    );

    let (failed, pr) = f
        .tool(
            &token,
            "create_pr",
            serde_json::json!({
                "owner": "alice", "repo": "app",
                "title": "Agent change", "head": "feature", "base": "main",
            }),
        )
        .await;
    assert!(!failed, "create_pr failed: {pr}");

    // Outside its scope: the bot *account* may write to alice/other, the
    // token may not.
    let (failed, refused) = f
        .tool(
            &token,
            "create_issue",
            serde_json::json!({ "owner": "alice", "repo": "other", "title": "Out of scope" }),
        )
        .await;
    assert!(failed, "an out-of-scope tool call succeeded: {refused}");
    assert!(refused.contains("403"), "{refused}");
    assert_eq!(f.get(&token, "/repos/alice/other").await, 403);
    assert_eq!(f.get(&token, "/repos/alice/app").await, 200);
    // A route that is not about one repository answers from everything the
    // account can see — which is what the confinement cuts.
    assert_eq!(f.get(&token, "/repos/alice").await, 403);

    let denied = f.denials("repository_not_allowed").await;
    assert!(
        denied.len() >= 2,
        "both refusals are journalled: {denied:?}"
    );
    assert!(denied
        .iter()
        .all(|details| details["repository"] == "alice/other"));
    let via_mcp = denied
        .iter()
        .find(|details| details["credential"]["mcp_tool"] == "create_issue")
        .expect("the refusal behind the tool call names the tool");
    assert!(via_mcp["token_id"].is_i64());

    let calls = f.audit("agent.mcp_tool_call").await;
    let tools: Vec<String> = calls
        .iter()
        .map(|row| {
            let details: serde_json::Value =
                serde_json::from_str(row.details.as_deref().unwrap()).unwrap();
            assert_eq!(row.username.as_deref(), Some("alice-agent"));
            format!("{}:{}", details["tool"], details["is_error"])
        })
        .collect();
    assert_eq!(
        tools,
        vec![
            "\"create_issue\":false",
            "\"create_pr\":false",
            "\"create_issue\":true"
        ]
    );
}

/// A bot has no password, no session and no way to manage credentials of its
/// own: the account-management API is out of its token's scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bot_cannot_log_in_or_mint_credentials_for_itself() {
    let f = fixture(StateOverrides::default()).await;
    let token = f.bot_token(serde_json::json!({ "name": "agent" })).await;

    for password in ["", "x", "Qz7$wRtm"] {
        let response = f
            .http
            .post(format!("{}/api/v1/users/login", f.base))
            .json(&serde_json::json!({ "login": "alice-agent", "password": password }))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_client_error(),
            "a bot logged in with {password:?}: {}",
            response.status()
        );
    }

    let response = f
        .http
        .post(format!("{}/api/v1/users/tokens", f.base))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "name": "escalate", "scopes": "repo,user,admin" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert_eq!(f.get(&token, "/users/bots").await, 403);
    assert!(!f.denials("scope").await.is_empty());

    // Another person cannot see, mint for, or delete alice's bot.
    let (mallory, _) = register_full(&f.base, "mallory", "mallory@example.test").await;
    let response = f
        .http
        .post(format!("{}/api/v1/users/bots/alice-agent/tokens", f.base))
        .bearer_auth(&mallory)
        .json(&serde_json::json!({ "name": "stolen" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let response = f
        .http
        .delete(format!("{}/api/v1/users/bots/alice-agent", f.base))
        .bearer_auth(&mallory)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}

/// A token confined to MCP tools works only through the MCP endpoint, sees
/// only its tools, and is refused the others with a `403` the audit log keeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tool_confined_token_works_only_through_mcp_and_only_for_its_tools() {
    let f = fixture(StateOverrides::default()).await;
    let token = f
        .bot_token(
            serde_json::json!({ "name": "reader", "mcp_tools": ["get_issue", "create_issue"] }),
        )
        .await;

    assert_eq!(f.get(&token, "/repos/alice/app").await, 403);
    assert!(!f.denials("mcp_only").await.is_empty());

    let git = f
        .http
        .get(format!(
            "{}/git/alice/app/info/refs?service=git-upload-pack",
            f.base
        ))
        .basic_auth("alice-agent", Some(&token))
        .send()
        .await
        .unwrap();
    assert_eq!(git.status(), 403, "git is not an MCP tool");

    let (status, listed) = f.mcp(&token, "tools/list", serde_json::json!({})).await;
    assert_eq!(status, 200);
    let mut names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["create_issue", "get_issue"]);

    let (status, refused) = f
        .mcp(
            &token,
            "tools/call",
            serde_json::json!({
                "name": "create_pr",
                "arguments": { "owner": "alice", "repo": "app", "title": "t", "head": "feature", "base": "main" },
            }),
        )
        .await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["error"]["code"], -32003);
    assert!(f
        .denials("mcp_tool")
        .await
        .iter()
        .any(|details| details["tool"] == "create_pr"));

    let (failed, created) = f
        .tool(
            &token,
            "create_issue",
            serde_json::json!({ "owner": "alice", "repo": "app", "title": "Through a tool" }),
        )
        .await;
    assert!(!failed, "{created}");
}

/// A token kept off protected branches cannot merge into one, schedule a merge
/// into one, or commit to one through the contents API — while a token of the
/// same bot without that narrowing can, which is what proves each refusal is
/// the token's and not the branch rule's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_kept_off_protected_branches_cannot_write_there() {
    let f = fixture(StateOverrides::default()).await;
    // Protected, but open to direct pushes by writers: the rule itself refuses
    // nothing the bot account may do, so only the token can.
    let response = f
        .http
        .post(format!(
            "{}/api/v1/repos/alice/app/branches/protection",
            f.base
        ))
        .bearer_auth(&f.alice)
        .json(&serde_json::json!({
            "branch_name": "main", "require_pr": false, "allow_force_push": true,
        }))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );

    let kept_off = f.bot_token(serde_json::json!({ "name": "kept-off" })).await;
    let trusted = f
        .bot_token(serde_json::json!({ "name": "trusted", "deny_protected_merge": false }))
        .await;

    let response = f
        .http
        .post(format!("{}/api/v1/repos/alice/app/pulls", f.base))
        .bearer_auth(&kept_off)
        .json(&serde_json::json!({ "title": "change", "head": "feature", "base": "main" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    let created: serde_json::Value = response.json().await.unwrap();
    assert_eq!(created["author"], "alice-agent");
    assert_eq!(created["author_bot_owner"], "alice");
    let number = created["number"].as_i64().unwrap();

    let commit = |token: &str, branch: &str, path: &str| {
        f.http
            .post(format!("{}/api/v1/repos/alice/app/contents/{path}", f.base))
            .bearer_auth(token.to_string())
            .json(&serde_json::json!({
                "branch": branch, "content": "agent\n", "message": "agent commit",
            }))
            .send()
    };
    assert_eq!(
        commit(&kept_off, "main", "kept-off.txt")
            .await
            .unwrap()
            .status(),
        403
    );

    // Both ways of scheduling a merge for later are refused up front: nothing
    // will ask the token again when the merge finally happens.
    for schedule in ["auto-merge", "merge-queue"] {
        let response = f
            .http
            .put(format!(
                "{}/api/v1/repos/alice/app/pulls/{number}/{schedule}",
                f.base
            ))
            .bearer_auth(&kept_off)
            .json(&serde_json::json!({ "strategy": "merge" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403, "{schedule}");
    }

    let merge = |token: &str| {
        f.http
            .post(format!(
                "{}/api/v1/repos/alice/app/pulls/{number}/merge",
                f.base
            ))
            .bearer_auth(token.to_string())
            .json(&serde_json::json!({ "strategy": "merge" }))
            .send()
    };
    assert_eq!(merge(&kept_off).await.unwrap().status(), 403);
    let denied = f.denials("protected_branch").await;
    assert_eq!(
        denied.len(),
        4,
        "commit, auto-merge, merge queue and merge are each journalled: {denied:?}"
    );

    // The same writes through an unnarrowed token of the same bot.
    let merged = merge(&trusted).await.unwrap();
    assert!(
        merged.status().is_success(),
        "{} {}",
        merged.status(),
        merged.text().await.unwrap()
    );
    let committed = commit(&trusted, "main", "trusted.txt").await.unwrap();
    assert!(
        committed.status().is_success(),
        "{} {}",
        committed.status(),
        committed.text().await.unwrap()
    );
    // And an unprotected branch stays open to the narrowed one.
    let committed = commit(&kept_off, "feature", "kept-off.txt").await.unwrap();
    assert!(
        committed.status().is_success(),
        "{} {}",
        committed.status(),
        committed.text().await.unwrap()
    );

    // `git push` is the same write by another door.
    let host = f.base.trim_start_matches("http://");
    let remote = |token: &str| format!("http://alice-agent:{token}@{host}/alice/app.git");
    let worktree = tempfile::tempdir().unwrap();
    let checkout = worktree.path().join("app");
    git(
        &["clone", &remote(&trusted), checkout.to_str().unwrap()],
        None,
    );
    git(&["config", "user.name", "Agent"], Some(&checkout));
    git(
        &["config", "user.email", "agent@example.test"],
        Some(&checkout),
    );
    std::fs::write(checkout.join("pushed.txt"), "pushed\n").unwrap();
    git(&["add", "."], Some(&checkout));
    git(&["commit", "-m", "pushed by the agent"], Some(&checkout));
    let (pushed, stderr) = git_outcome(&["push", &remote(&kept_off), "HEAD:main"], Some(&checkout));
    assert!(
        !pushed,
        "a token kept off protected branches pushed to main"
    );
    assert!(
        stderr.contains("this token may not write to protected branch 'main'"),
        "{stderr}"
    );
    let (pushed, stderr) = git_outcome(&["push", &remote(&trusted), "HEAD:main"], Some(&checkout));
    assert!(pushed, "the unnarrowed token's push: {stderr}");
}

/// A token confined to repositories reaches only those over git, too — and a
/// person's own token can carry the same confinement.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repository_confinement_holds_over_git_and_for_a_persons_own_token() {
    let f = fixture(StateOverrides::default()).await;
    let token = f
        .bot_token(serde_json::json!({ "name": "agent", "repositories": ["alice/app"] }))
        .await;
    let info_refs = |repo: &str| {
        f.http
            .get(format!(
                "{}/git/alice/{repo}/info/refs?service=git-upload-pack",
                f.base
            ))
            .basic_auth("alice-agent", Some(token.clone()))
            .send()
    };
    assert_eq!(info_refs("app").await.unwrap().status(), 200);
    assert_eq!(info_refs("other").await.unwrap().status(), 403);

    // LFS is declared by its credential mechanism, not by a repository level,
    // and is still about the repository its path names.
    let lfs_batch = |repo: &str| {
        f.http
            .post(format!(
                "{}/api/v1/repos/alice/{repo}/lfs/objects/batch",
                f.base
            ))
            .basic_auth("alice-agent", Some(token.clone()))
            .header("Content-Type", "application/vnd.git-lfs+json")
            .header("Accept", "application/vnd.git-lfs+json")
            .json(&serde_json::json!({
                "operation": "download",
                "objects": [{ "oid": "0".repeat(64), "size": 1 }],
            }))
            .send()
    };
    let allowed = lfs_batch("app").await.unwrap();
    assert_eq!(allowed.status(), 200, "{}", allowed.text().await.unwrap());
    assert_eq!(lfs_batch("other").await.unwrap().status(), 403);

    // The registry's own token names repositories by scope string, not by this
    // allow-list, so a confined token is refused `docker login` outright — while
    // an unconfined token of the same bot gets one.
    let registry_login = |token: String| {
        f.http
            .get(format!(
                "{}/v2/auth/token?service=plombir-git-registry&scope=repository:alice/app:pull",
                f.base
            ))
            .basic_auth("alice-agent", Some(token))
            .send()
    };
    assert_eq!(registry_login(token.clone()).await.unwrap().status(), 401);
    assert!(f
        .denials("registry")
        .await
        .iter()
        .any(|details| details["transport"] == "oci"));
    let unconfined = f.bot_token(serde_json::json!({ "name": "registry" })).await;
    assert_eq!(registry_login(unconfined).await.unwrap().status(), 200);

    let response = f
        .http
        .post(format!("{}/api/v1/users/tokens", f.base))
        .bearer_auth(&f.alice)
        .json(&serde_json::json!({
            "name": "laptop-agent", "scopes": "repo,user", "repositories": ["alice/app"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let own: serde_json::Value = response.json().await.unwrap();
    assert_eq!(own["repositories"], serde_json::json!(["alice/app"]));
    assert_eq!(own["deny_protected_merge"], false);
    let own = own["token"].as_str().unwrap();
    assert_eq!(f.get(own, "/repos/alice/app").await, 200);
    assert_eq!(f.get(own, "/repos/alice/other").await, 403);
    assert_eq!(
        f.get(own, "/users/me").await,
        403,
        "a confined token is kept off account-wide routes even with the `user` scope"
    );

    // A repository the person cannot see is refused exactly like one that
    // does not exist.
    let (mallory, _) = register_full(&f.base, "mallory", "mallory@example.test").await;
    for name in ["alice/app", "alice/nope"] {
        let response = f
            .http
            .post(format!("{}/api/v1/users/tokens", f.base))
            .bearer_auth(&mallory)
            .json(&serde_json::json!({ "name": "probe", "repositories": [name] }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"]["message"],
            format!("repository '{name}' does not exist or is not visible to you")
        );
    }
}

/// The bot answers to its owner: disabling the owner stops the agent, and the
/// owner cannot be deleted from under a bot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bot_stops_with_its_owner_and_holds_the_owner_back_from_deletion() {
    let f = fixture(StateOverrides::default()).await;
    let token = f.bot_token(serde_json::json!({ "name": "agent" })).await;
    assert_eq!(f.get(&token, "/repos/alice/app").await, 200);

    let delete = f
        .http
        .delete(format!("{}/api/v1/admin/users/{}", f.base, f.alice_id))
        .bearer_auth(&f.root)
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status(), 409);
    assert!(delete.text().await.unwrap().contains("alice-agent"));

    rg_db::ops::user_ops::update_by_id(&f.db, f.alice_id, None, None, None, Some(false))
        .await
        .unwrap();
    assert_eq!(f.get(&token, "/repos/alice/app").await, 401);
}

/// Every token of one bot spends one budget; people are not counted against it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_token_of_a_bot_spends_one_budget() {
    let f = fixture(StateOverrides {
        agent_rate_limiter: Some(rg_http::rate_limit::RateLimiter::new(3, 60)),
        ..Default::default()
    })
    .await;
    let first = f.bot_token(serde_json::json!({ "name": "one" })).await;
    let second = f.bot_token(serde_json::json!({ "name": "two" })).await;

    assert_eq!(f.get(&first, "/repos/alice/app").await, 200);
    assert_eq!(f.get(&second, "/repos/alice/app").await, 200);
    // One tool call is one request, however many API calls the tool makes.
    let (failed, _) = f
        .tool(
            &first,
            "list_pipelines",
            serde_json::json!({ "owner": "alice", "repo": "app" }),
        )
        .await;
    assert!(!failed);
    assert_eq!(f.get(&second, "/repos/alice/app").await, 429);
    assert_eq!(f.get(&f.alice, "/repos/alice/app").await, 200);
}

impl Fixture {
    /// `POST` as `token` and return the status with the JSON body.
    async fn post(
        &self,
        token: &str,
        path: &str,
        body: serde_json::Value,
    ) -> (reqwest::StatusCode, serde_json::Value) {
        let response = self
            .http
            .post(format!("{}/api/v1{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or_default())
    }

    async fn get_json(&self, token: &str, path: &str) -> serde_json::Value {
        let response = self
            .http
            .get(format!("{}/api/v1{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "GET {path}");
        response.json().await.unwrap()
    }

    /// Wait until the pull request's head is `sha`. The row is moved by the
    /// detached post-push hook, so it lags the commit that answered.
    async fn await_pr_head(&self, number: i64, sha: &str) {
        let path = format!("/repos/alice/app/pulls/{number}");
        for _ in 0..300 {
            if self.get_json(&self.alice, &path).await["head_sha"] == sha {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("pull request #{number} never moved its head to {sha}");
    }

    /// Approve `alice/app#number` as `token`.
    async fn approve(&self, token: &str, number: i64) {
        let (status, body) = self
            .post(
                token,
                &format!("/repos/alice/app/pulls/{number}/reviews"),
                serde_json::json!({ "action": "approve" }),
            )
            .await;
        assert_eq!(status, 201, "{body}");
    }

    /// `tools/call` whose answer is JSON; panics with the text if it failed.
    async fn tool_json(
        &self,
        token: &str,
        name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let (failed, text) = self.tool(token, name, arguments).await;
        assert!(!failed, "{name} failed: {text}");
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{name}: {error}: {text}"))
    }
}

/// The scenario the agent accounts exist for (card_25220b69c295): an agent
/// starts a branch, commits and opens a pull request through MCP; CODEOWNERS
/// puts a person on it; the agent reads that person's inline comment, fixes the
/// code, answers in the thread and reads CI — and the change reaches the
/// protected branch only once that person approves.
///
/// Three approvals are given on the way that must not open the gate, and each
/// is the only thing standing between its own rule and a green merge: the
/// owner's second bot (two agents of one person would otherwise approve each
/// other), the bot's owner (the bot acts for them, so that is the author
/// approving themselves), and a collaborator who may only read. Remove any one
/// of the three exclusions in `tally_approvals` and the merge attempted before
/// the code owner's approval goes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_pull_request_merges_only_after_a_code_owner_approves() {
    let f = fixture(StateOverrides::default()).await;
    let (bob, _) = register_full(&f.base, "bob", "bob@example.test").await;
    let (carol, _) = register_full(&f.base, "carol", "carol@example.test").await;
    for (username, permission) in [("bob", "write"), ("carol", "read")] {
        let (status, body) = f
            .post(
                &f.alice,
                "/repos/alice/app/collaborators",
                serde_json::json!({ "username": username, "permission": permission }),
            )
            .await;
        assert!(status.is_success(), "add {username}: {status} {body}");
    }
    // A second agent of the same owner, a writer like the first.
    let (status, body) = f
        .post(
            &f.alice,
            "/users/bots",
            serde_json::json!({ "username": "alice-reviewer" }),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = f
        .post(
            &f.alice,
            "/repos/alice/app/collaborators",
            serde_json::json!({ "username": "alice-reviewer", "permission": "write" }),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    let (status, body) = f
        .post(
            &f.alice,
            "/users/bots/alice-reviewer/tokens",
            serde_json::json!({ "name": "review" }),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    let reviewer_bot = body["token"].as_str().unwrap().to_string();

    let (status, body) = f
        .post(
            &f.alice,
            "/repos/alice/app/contents/.github/CODEOWNERS",
            serde_json::json!({ "branch": "main", "content": "* @bob\n", "message": "owners" }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = f
        .post(
            &f.alice,
            "/repos/alice/app/branches/protection",
            serde_json::json!({
                "branch_name": "main", "require_pr": true,
                "require_approval": true, "required_approvals": 1,
            }),
        )
        .await;
    assert_eq!(status, 201, "{body}");

    let agent = f
        .bot_token(serde_json::json!({ "name": "agent", "repositories": ["alice/app"] }))
        .await;

    // ── The agent starts a branch with its first commit and opens a PR. ──
    let written = f
        .tool_json(
            &agent,
            "write_file",
            serde_json::json!({
                "owner": "alice", "repo": "app", "path": "src/agent.rs",
                "branch": "agent/answer", "message": "Add the answer",
                "content": "pub fn answer() -> i32 { 41 }\n",
            }),
        )
        .await;
    let first_commit = written["commit_sha"].as_str().unwrap().to_string();
    let file = f
        .tool_json(
            &agent,
            "read_file",
            serde_json::json!({
                "owner": "alice", "repo": "app", "path": "README.md", "ref": "agent/answer",
            }),
        )
        .await;
    assert_eq!(
        file["content"], "seed\n",
        "the new branch starts from the default branch, the rest of the tree intact"
    );
    let pr = f
        .tool_json(
            &agent,
            "create_pr",
            serde_json::json!({
                "owner": "alice", "repo": "app", "title": "Add the answer",
                "head": "agent/answer", "base": "main",
            }),
        )
        .await;
    let number = pr["number"].as_i64().unwrap();
    assert_eq!(pr["author"], "alice-agent");
    f.await_pr_head(number, &first_commit).await;

    let reviewers = f
        .get_json(
            &f.alice,
            &format!("/repos/alice/app/pulls/{number}/reviewers"),
        )
        .await;
    let reviewers: Vec<&str> = reviewers
        .as_array()
        .unwrap()
        .iter()
        .map(|reviewer| reviewer["username"].as_str().unwrap())
        .collect();
    assert_eq!(reviewers, ["bob"], "CODEOWNERS put the person on the PR");

    // ── The code owner comments inline; the agent reads it and fixes. ──
    let (status, comment) = f
        .post(
            &bob,
            &format!("/repos/alice/app/pulls/{number}/comments"),
            serde_json::json!({
                "path": "src/agent.rs", "line": 1, "side": "RIGHT",
                "body": "The answer is 42.",
            }),
        )
        .await;
    assert_eq!(status, 201, "{comment}");
    let comments = f
        .tool_json(
            &agent,
            "list_review_comments",
            serde_json::json!({ "owner": "alice", "repo": "app", "number": number }),
        )
        .await;
    let seen = comments
        .as_array()
        .unwrap()
        .iter()
        .find(|seen| seen["body"] == "The answer is 42.")
        .unwrap_or_else(|| panic!("the agent does not see the comment: {comments}"));
    assert_eq!(seen["id"], comment["id"]);

    let current = f
        .tool_json(
            &agent,
            "read_file",
            serde_json::json!({
                "owner": "alice", "repo": "app", "path": "src/agent.rs", "ref": "agent/answer",
            }),
        )
        .await;
    let fixed = f
        .tool_json(
            &agent,
            "write_file",
            serde_json::json!({
                "owner": "alice", "repo": "app", "path": "src/agent.rs",
                "branch": "agent/answer", "message": "Make it 42",
                "content": "pub fn answer() -> i32 { 42 }\n", "sha": current["sha"],
            }),
        )
        .await;
    let head = fixed["commit_sha"].as_str().unwrap().to_string();
    assert_ne!(head, first_commit);
    let reply = f
        .tool_json(
            &agent,
            "create_review_comment",
            serde_json::json!({
                "owner": "alice", "repo": "app", "number": number, "path": "src/agent.rs",
                "body": "Fixed.", "reply_to_id": comment["id"],
            }),
        )
        .await;
    assert_eq!(
        reply["reply_to_id"], comment["id"],
        "the answer is in the thread"
    );
    f.await_pr_head(number, &head).await;

    // ── CI reports on the new head, and the agent sees it. ──
    let (status, body) = f
        .post(
            &f.alice,
            &format!("/repos/alice/app/statuses/{head}"),
            serde_json::json!({ "state": "success", "context": "ci/build" }),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");
    let ci = f
        .tool_json(
            &agent,
            "get_commit_status",
            serde_json::json!({ "owner": "alice", "repo": "app", "sha": head }),
        )
        .await;
    assert_eq!(ci["state"], "success", "{ci}");

    // ── Approvals that must not count. ──
    f.approve(&reviewer_bot, number).await;
    f.approve(&f.alice, number).await;
    f.approve(&carol, number).await;
    let merge_path = format!("/repos/alice/app/pulls/{number}/merge");
    let (status, refused) = f
        .post(
            &f.alice,
            &merge_path,
            serde_json::json!({ "strategy": "merge" }),
        )
        .await;
    assert_eq!(status, 403, "a merge with no counted approval: {refused}");
    assert!(
        refused
            .to_string()
            .contains("requires at least 1 approval(s), got 0 (3 more approval(s) do not count"),
        "{refused}"
    );

    // ── The code owner approves; the agent sees it and still cannot merge. ──
    f.approve(&bob, number).await;
    let reviews = f
        .tool_json(
            &agent,
            "list_reviews",
            serde_json::json!({ "owner": "alice", "repo": "app", "number": number }),
        )
        .await;
    assert!(
        reviews
            .as_array()
            .unwrap()
            .iter()
            .any(|review| review["action"] == "approve" && review["commit_id"] == head.as_str()),
        "{reviews}"
    );
    let (failed, refused) = f
        .tool(
            &agent,
            "merge_pr",
            serde_json::json!({ "owner": "alice", "repo": "app", "number": number, "strategy": "merge" }),
        )
        .await;
    assert!(
        failed,
        "the agent merged into a protected branch: {refused}"
    );
    assert!(refused.contains("403"), "{refused}");

    let (status, merged) = f
        .post(
            &f.alice,
            &merge_path,
            serde_json::json!({ "strategy": "merge" }),
        )
        .await;
    assert_eq!(
        status, 200,
        "the person merges once a code owner approved: {merged}"
    );
    let landed = f
        .get_json(&f.alice, "/repos/alice/app/blob/src/agent.rs?ref=main")
        .await;
    assert_eq!(landed["content"], "pub fn answer() -> i32 { 42 }\n");

    let tools: Vec<String> = f
        .audit("agent.mcp_tool_call")
        .await
        .iter()
        .map(|row| {
            let details: serde_json::Value =
                serde_json::from_str(row.details.as_deref().unwrap()).unwrap();
            details["tool"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(
        tools,
        [
            "write_file",
            "read_file",
            "create_pr",
            "list_review_comments",
            "read_file",
            "write_file",
            "create_review_comment",
            "get_commit_status",
            "list_reviews",
            "merge_pr",
        ],
        "every step the agent took is in the audit log under its tool"
    );
}
