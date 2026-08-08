//! Second pass of the sweep in [`super::service_failure_status_tests`], for
//! card_a086f3421ee9 / card_10f706d87a6d.
//!
//! That sweep searched for the literal `AppError::bad_request(e)` and converted
//! 35 handlers. The `.to_string()` spelling of the same defect —
//! `Err(e) => AppError::bad_request(e.to_string())` wrapped around the *whole*
//! result of a service call — was invisible to it and survived in seven more
//! modules: issues, pulls, repos, collaborators, branch_protection, users and
//! admin. The consequence is identical: a dropped table, a dead connection pool
//! and a genuinely malformed request all left as `400`, so the client never
//! retried and the alerts stayed empty, with the `db: …` context chain shipped
//! in a body `IntoResponse` does not sanitize (H-05).
//!
//! Each test asserts both halves for the same reason as the file above: the
//! "still a 400" baseline fails an endpoint that now 500s on everything, and the
//! outage half fails an endpoint that kept the blanket `bad_request`.

use crate::common::{
    create_repo, register_full, register_user, spawn_test_app_with_db,
    spawn_test_app_with_db_and_repo_root,
};
use sea_orm::{ActiveValue::NotSet, ConnectionTrait, Set};

/// The failure half: a broken write must be a 5xx carrying no internal detail.
fn assert_not_blamed_on_the_client(
    status: reqwest::StatusCode,
    body: &serde_json::Value,
    what: &str,
) {
    assert!(
        status.is_server_error(),
        "a broken {what} write must be a 5xx, not {status} — a 400 tells the \
         client to fix a request that was never wrong and is never retried \
         (body: {body})"
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("no such table"),
        "the {what} response body must not carry internal error detail, got: {message}"
    );
}

fn git(args: &[&str], cwd: Option<&std::path::Path>) {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    output.ensure_success().unwrap();
}

fn git_stdout(args: &[&str], cwd: Option<&std::path::Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

struct PrSuggestionSeed<'a> {
    repo_id: i64,
    author_id: i64,
    number: i64,
    branch: &'a str,
    head_sha: &'a str,
}

async fn create_pr_suggestion(
    db: &sea_orm::DatabaseConnection,
    base: &str,
    token: &str,
    seed: PrSuggestionSeed<'_>,
) -> i64 {
    let now = chrono::Utc::now();
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(seed.repo_id),
            number: Set(seed.number),
            title: Set(format!("stale suggestion branch {}", seed.branch)),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(seed.author_id),
            reviewer_id: Set(None),
            head_branch: Set(seed.branch.to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some(seed.head_sha.to_string())),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("seed pull request");

    let comment = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/suggestion-stale/suggestion-stale/pulls/{}/comments",
            seed.number
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "path": "README.md",
            "line": 1,
            "side": "RIGHT",
            "body": "rewrite the base line",
            "suggestion": "reviewed base",
            "commit_id": seed.head_sha,
        }))
        .send()
        .await
        .expect("create suggestion comment");
    assert_eq!(
        comment.status(),
        201,
        "the fixture suggestion must be accepted before its branch is deleted"
    );
    comment
        .json::<serde_json::Value>()
        .await
        .expect("suggestion body")["id"]
        .as_i64()
        .expect("suggestion id")
}

async fn assert_deleted_pr_branch_conflict(
    response: reqwest::Response,
    branch: &str,
    operation: &str,
) {
    assert_eq!(
        response.status(),
        409,
        "{operation} must report a deleted persisted PR branch as stale state"
    );
    let body: serde_json::Value = response.json().await.expect("conflict body");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(branch) && message.contains("no longer exists"),
        "{operation} must name the deleted ref for the client, got: {body}"
    );
}

/// Seed the bare repository with commits on both sides of a real PR. Empty
/// repositories are useful in most tests, but must not stand in for a valid
/// `head` when this suite is distinguishing validation from server failures.
fn seed_pr_branches(bare_path: &std::path::Path) {
    let worktree = tempfile::tempdir().expect("create PR fixture worktree");
    let path = worktree.path();
    let path_arg = path.to_str().expect("UTF-8 worktree path");
    let bare_arg = bare_path.to_str().expect("UTF-8 bare repository path");

    git(&["init", "-q", "-b", "main", path_arg], None);
    git(&["config", "user.name", "PR failure test"], Some(path));
    git(
        &["config", "user.email", "pr-failure@example.invalid"],
        Some(path),
    );
    std::fs::write(path.join("README.md"), "base\n").expect("write base file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "base"], Some(path));
    git(&["remote", "add", "origin", bare_arg], Some(path));
    git(&["push", "origin", "main"], Some(path));

    git(&["checkout", "-q", "-b", "feature"], Some(path));
    std::fs::write(path.join("feature.txt"), "feature\n").expect("write feature file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "feature"], Some(path));
    git(&["push", "origin", "feature"], Some(path));
}

/// Add a real source branch to a fork that already has its inherited `main`.
fn seed_fork_feature_branch(bare_path: &std::path::Path, branch: &str) {
    let worktree = tempfile::tempdir().expect("create fork PR fixture worktree");
    let path = worktree.path();
    let path_arg = path.to_str().expect("UTF-8 worktree path");
    let bare_arg = bare_path.to_str().expect("UTF-8 bare repository path");

    git(&["clone", "-q", bare_arg, path_arg], None);
    git(&["config", "user.name", "Fork PR failure test"], Some(path));
    git(
        &["config", "user.email", "fork-pr-failure@example.invalid"],
        Some(path),
    );
    git(&["checkout", "-q", "-b", branch], Some(path));
    std::fs::write(path.join("fork-feature.txt"), "feature\n").expect("write fork feature file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "fork feature"], Some(path));
    git(&["push", "origin", branch], Some(path));
}

async fn app_with_repo(prefix: &str) -> (String, sea_orm::DatabaseConnection, String, i64) {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(
        &base,
        &format!("{prefix}-owner"),
        &format!("{prefix}@example.com"),
    )
    .await;
    let repo_id = create_repo(&base, &token, &format!("{prefix}-repo")).await;
    (base, db, token, repo_id)
}

/// `POST .../issues` — an empty title is the caller's, a dead `issues` table is
/// ours. Same split for `PATCH`, whose extra outcome is an issue that genuinely
/// is not there: that is a 404, and it used to be a 400 as well.
#[tokio::test]
async fn issue_create_separates_an_empty_title_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("issuefail").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/issuefail-owner/issuefail-repo/issues");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "   "}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "a blank issue title is still the caller's mistake"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "real work"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: a healthy create still works");

    // An issue number nobody ever used is a 404, not a bad request.
    let resp = client
        .patch(format!("{url}/4711"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "renamed"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent issue is a 404, not a 400");

    // An unknown target state is the request's fault and stays 400.
    let resp = client
        .patch(format!("{url}/1"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "sideways"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "an unknown issue state is still a 400");

    db.execute_unprepared("DROP TABLE issues")
        .await
        .expect("drop issues");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "after the outage"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "issue");
}

/// `POST .../pulls` — `head == base` is the caller's, a dead `pull_requests`
/// table is ours. Both used to leave through one `400`, and so did the second,
/// nested `bad_request` the handler had around `create_pr` itself.
#[tokio::test]
async fn pr_create_separates_an_identical_head_and_base_from_a_broken_insert() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, _) = register_full(&base, "prfail-owner", "prfail@example.com").await;
    create_repo(&base, &token, "prfail-repo").await;
    seed_pr_branches(&repo_root.join("prfail-owner/prfail-repo.git"));
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/prfail-owner/prfail-repo/pulls");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "same branch", "head": "main", "base": "main"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "head == base is still the caller's mistake"
    );

    // An owner prefix naming a user that does not exist is the other
    // request-shaped branch, this one inside `resolve_head_ref`.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "from nowhere",
            "head": "ghost:feature",
            "base": "main",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "an unknown head owner is still a 400");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "real", "head": "feature", "base": "main"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: a healthy create still works");

    db.execute_unprepared("DROP TABLE pull_requests")
        .await
        .expect("drop pull_requests");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "after", "head": "feature", "base": "main"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "pull request");
}

/// `POST .../pulls` — an absent head branch is client input, while a repository
/// that cannot be opened is a server failure. Neither may create a PR with a
/// nullable `head_sha`.
#[tokio::test]
async fn pr_create_separates_a_missing_head_ref_from_an_unreadable_repository() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, _) = register_full(&base, "pr-head-owner", "pr-head@example.com").await;
    create_repo(&base, &token, "pr-head-repo").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/pr-head-owner/pr-head-repo/pulls");

    let missing_ref = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "missing branch",
            "head": "does-not-exist",
            "base": "main",
        }))
        .send()
        .await
        .expect("missing-head request");
    assert_eq!(
        missing_ref.status(),
        400,
        "a missing head branch is the caller's mistake"
    );

    let repository_path = repo_root.join("pr-head-owner/pr-head-repo.git");
    seed_pr_branches(&repository_path);
    std::fs::remove_dir_all(&repository_path)
        .unwrap_or_else(|error| panic!("remove test repository {repository_path:?}: {error}"));

    let unreadable_repo = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "broken repository",
            "head": "feature",
            "base": "main",
        }))
        .send()
        .await
        .expect("unreadable-repository request");
    assert!(
        unreadable_repo.status().is_server_error(),
        "an unreadable repository is ours, not a nullable head SHA or a 4xx: {}",
        unreadable_repo.status()
    );
}

/// Both suggestion-apply routes consume a head branch stored when the PR was
/// valid. If that ref is deleted later, its old commit can still be read by SHA
/// but cloning the named branch used to leak a generic 500. The absent ref is
/// stale resource state; an unreadable repository remains a server failure.
#[tokio::test]
async fn applying_suggestions_to_a_deleted_pr_head_branch_returns_conflict_not_500() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, author_id) =
        register_full(&base, "suggestion-stale", "suggestion-stale@example.com").await;
    let repo_id = create_repo(&base, &token, "suggestion-stale").await;
    let repository_path = repo_root.join("suggestion-stale/suggestion-stale.git");
    seed_pr_branches(&repository_path);

    let main_sha = git_stdout(&["rev-parse", "refs/heads/main"], Some(&repository_path));
    let feature_sha = git_stdout(&["rev-parse", "refs/heads/feature"], Some(&repository_path));
    let single_comment = create_pr_suggestion(
        &db,
        &base,
        &token,
        PrSuggestionSeed {
            repo_id,
            author_id,
            number: 1,
            branch: "main",
            head_sha: &main_sha,
        },
    )
    .await;
    let batch_comment = create_pr_suggestion(
        &db,
        &base,
        &token,
        PrSuggestionSeed {
            repo_id,
            author_id,
            number: 2,
            branch: "feature",
            head_sha: &feature_sha,
        },
    )
    .await;

    git(
        &["update-ref", "-d", "refs/heads/main"],
        Some(&repository_path),
    );
    git(
        &["update-ref", "-d", "refs/heads/feature"],
        Some(&repository_path),
    );

    let client = reqwest::Client::new();
    let single_url = format!(
        "{base}/api/v1/repos/suggestion-stale/suggestion-stale/pulls/1/comments/{single_comment}/suggestion/apply"
    );
    assert_deleted_pr_branch_conflict(
        client
            .post(&single_url)
            .bearer_auth(&token)
            .send()
            .await
            .expect("single suggestion request"),
        "main",
        "single suggestion apply",
    )
    .await;

    assert_deleted_pr_branch_conflict(
        client
            .post(format!(
                "{base}/api/v1/repos/suggestion-stale/suggestion-stale/pulls/2/suggestions/apply"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "comment_ids": [batch_comment] }))
            .send()
            .await
            .expect("batch suggestion request"),
        "feature",
        "batch suggestion apply",
    )
    .await;

    std::fs::remove_dir_all(&repository_path)
        .unwrap_or_else(|error| panic!("remove test repository {repository_path:?}: {error}"));
    let unavailable_repository = client
        .post(&single_url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("unreadable repository request");
    assert!(
        unavailable_repository.status().is_server_error(),
        "a missing repository is an operational failure, not stale PR state: {}",
        unavailable_repository.status()
    );
}

/// `GET .../pulls/{number}/diff` reads the PR's stored head branch. Once the
/// branch has been deleted, the PR is stale but the request is not malformed:
/// tell the caller to refresh its view instead of hiding that state behind a
/// retryable 500.
#[tokio::test]
async fn pr_diff_names_a_deleted_head_branch_as_a_conflict() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, _) = register_full(&base, "pr-diff-owner", "pr-diff@example.com").await;
    create_repo(&base, &token, "pr-diff-repo").await;

    let repository_path = repo_root.join("pr-diff-owner/pr-diff-repo.git");
    seed_pr_branches(&repository_path);

    let client = reqwest::Client::new();
    let pulls_url = format!("{base}/api/v1/repos/pr-diff-owner/pr-diff-repo/pulls");
    let created = client
        .post(&pulls_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "stale head branch",
            "head": "feature",
            "base": "main",
        }))
        .send()
        .await
        .expect("create PR request");
    assert_eq!(
        created.status(),
        201,
        "fixture PR must be valid before deletion"
    );
    let pr: serde_json::Value = created.json().await.expect("created PR body");
    let number = pr["number"].as_i64().expect("created PR number");

    git(
        &["update-ref", "-d", "refs/heads/feature"],
        Some(&repository_path),
    );

    let response = client
        .get(format!("{pulls_url}/{number}/diff"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("diff request");
    assert_eq!(
        response.status(),
        409,
        "a deleted PR head is stale resource state, not an internal failure"
    );
    let body: serde_json::Value = response.json().await.expect("conflict body");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("feature") && message.contains("no longer exists"),
        "the client needs the missing branch and cause, got: {body}"
    );
}

/// `POST .../merge` consumes the PR's stored head branch just like the diff
/// endpoint. A branch deleted after the PR was opened is stale resource state,
/// not a Git outage: the client must be told to refresh the PR rather than
/// retrying a blank 500.
#[tokio::test]
async fn pr_merge_names_a_deleted_head_branch_as_a_conflict() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, _) = register_full(&base, "pr-merge-owner", "pr-merge@example.com").await;
    create_repo(&base, &token, "pr-merge-repo").await;

    let repository_path = repo_root.join("pr-merge-owner/pr-merge-repo.git");
    seed_pr_branches(&repository_path);

    let client = reqwest::Client::new();
    let pulls_url = format!("{base}/api/v1/repos/pr-merge-owner/pr-merge-repo/pulls");
    let created = client
        .post(&pulls_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "stale head branch",
            "head": "feature",
            "base": "main",
        }))
        .send()
        .await
        .expect("create PR request");
    assert_eq!(
        created.status(),
        201,
        "fixture PR must be valid before deletion"
    );
    let pr: serde_json::Value = created.json().await.expect("created PR body");
    let number = pr["number"].as_i64().expect("created PR number");

    git(
        &["update-ref", "-d", "refs/heads/feature"],
        Some(&repository_path),
    );

    let response = client
        .post(format!("{pulls_url}/{number}/merge"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .expect("merge request");
    assert_eq!(
        response.status(),
        409,
        "a deleted PR head is stale resource state, not an internal failure"
    );
    let body: serde_json::Value = response.json().await.expect("conflict body");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("feature") && message.contains("no longer exists"),
        "the client needs the missing branch and cause, got: {body}"
    );
}

/// Fork PRs used to reach `git fetch` directly, where a deleted source branch
/// is indistinguishable from an infrastructure failure. The core merge service
/// must apply the same stale-ref barrier before fetching the fork.
#[tokio::test]
async fn pr_merge_names_a_deleted_fork_head_branch_as_a_conflict() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner_token, _) = register_full(&base, "pr-fork-owner", "pr-fork-owner@example.com").await;
    let (fork_token, _) = register_full(&base, "pr-fork-head", "pr-fork-head@example.com").await;
    create_repo(&base, &owner_token, "pr-fork-repo").await;

    let target_path = repo_root.join("pr-fork-owner/pr-fork-repo.git");
    seed_pr_branches(&target_path);

    let client = reqwest::Client::new();
    let fork = client
        .post(format!(
            "{base}/api/v1/repos/pr-fork-owner/pr-fork-repo/fork"
        ))
        .bearer_auth(&fork_token)
        .send()
        .await
        .expect("fork request");
    assert_eq!(fork.status(), 201, "fixture fork must be created");

    let fork_path = repo_root.join("pr-fork-head/pr-fork-repo.git");
    let fork_branch = "fork-feature";
    seed_fork_feature_branch(&fork_path, fork_branch);

    let pulls_url = format!("{base}/api/v1/repos/pr-fork-owner/pr-fork-repo/pulls");
    let created = client
        .post(&pulls_url)
        .bearer_auth(&fork_token)
        .json(&serde_json::json!({
            "title": "stale fork head branch",
            "head": format!("pr-fork-head:{fork_branch}"),
            "base": "main",
        }))
        .send()
        .await
        .expect("create fork PR request");
    assert_eq!(
        created.status(),
        201,
        "fixture fork PR must be valid before deletion"
    );
    let pr: serde_json::Value = created.json().await.expect("created PR body");
    let number = pr["number"].as_i64().expect("created PR number");

    git(
        &["update-ref", "-d", &format!("refs/heads/{fork_branch}")],
        Some(&fork_path),
    );

    let response = client
        .post(format!("{pulls_url}/{number}/merge"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .expect("merge request");
    assert_eq!(
        response.status(),
        409,
        "a deleted fork PR head is stale resource state, not an internal failure"
    );
    let body: serde_json::Value = response.json().await.expect("conflict body");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(fork_branch) && message.contains("no longer exists"),
        "the client needs the missing branch and cause, got: {body}"
    );
}

/// `POST .../collaborators` — an unknown permission and an already-listed user
/// are the caller's, the `repo_collaborators` insert is ours. The handler had
/// *two* blanket `bad_request`s: one on the service, one on the helper that
/// resolves `username`/`email` to an id through a database lookup.
#[tokio::test]
async fn add_collaborator_separates_a_rejected_permission_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("collabfail").await;
    let (_other_token, other_id) =
        register_full(&base, "collabfail-guest", "collabfail-guest@example.com").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/collabfail-owner/collabfail-repo/collaborators");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"user_id": other_id, "permission": "sideways"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "an unknown permission is still the caller's mistake"
    );

    // Naming nobody at all, and naming a user that does not exist, are the two
    // request-shaped branches of the resolve helper.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"permission": "read"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "a request naming no collaborator is still a 400"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"username": "nobody-here", "permission": "read"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "an unknown username is still a 400");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"user_id": other_id, "permission": "write"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: a healthy add still works");

    db.execute_unprepared("DROP TABLE repo_collaborators")
        .await
        .expect("drop repo_collaborators");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"user_id": other_id, "permission": "read"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "collaborator");
}

/// `POST /users/register` — a taken username and a weak password are the
/// caller's, a dead `users` table is ours. This is the worst instance of the
/// class in the sweep: an unauthenticated endpoint that answered "pick another
/// username" to a database outage, so an operator watching only 5xx rates saw a
/// completely broken registration path as perfectly healthy traffic.
#[tokio::test]
async fn register_separates_a_taken_username_from_a_broken_insert() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/users/register");

    register_user(
        &base,
        "regfail-first",
        "regfail-first@example.com",
        "Qz7$wRtm",
    )
    .await;

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "username": "regfail-first",
            "email": "regfail-other@example.com",
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        409,
        "a taken username is a state conflict, not a malformed request"
    );

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "username": "regfail-second",
            "email": "regfail-first@example.com",
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        409,
        "a taken email is a state conflict, not a malformed request"
    );

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "username": "regfail-third",
            "email": "not-an-address",
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "a malformed email is still a 400");

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "username": "regfail-fourth",
            "email": "regfail-fourth@example.com",
            "password": "short",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 400, "a rejected password is still a 400");

    db.execute_unprepared("DROP TABLE users")
        .await
        .expect("drop users");

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "username": "regfail-fifth",
            "email": "regfail-fifth@example.com",
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "registration");
}

/// `POST .../branches/protection` — protecting an already-protected branch is
/// the caller's (a `409` since card_cfba32a77acd, next to its neighbour `tag
/// protection pattern already exists`), the `protected_branches` insert is
/// ours. The `PATCH`/`DELETE` pair adds the third outcome: a rule that is not
/// there is a 404, which is what `get_protection` on the same routes already
/// answered while its two siblings called it a bad request.
#[tokio::test]
async fn branch_protection_separates_a_duplicate_rule_from_a_broken_insert() {
    let (base, db, token, _repo_id) = app_with_repo("bpfail").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/bpfail-owner/bpfail-repo/branches/protection");
    let body = serde_json::json!({"branch_name": "main", "require_pr": true});

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: the first rule is created");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        409,
        "protecting the same branch twice is refused by the rule that exists"
    );

    let resp = client
        .patch(format!("{url}/999999"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"require_pr": false}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        404,
        "an absent protection rule is a 404, not a 400"
    );

    let resp = client
        .delete(format!("{url}/999999"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        404,
        "deleting an absent protection rule is a 404, not a 400"
    );

    db.execute_unprepared("DROP TABLE protected_branches")
        .await
        .expect("drop protected_branches");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"branch_name": "release", "require_pr": true}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "branch protection");
}

/// `POST .../statuses/{sha}` — an unknown status state is the caller's, the
/// `commit_statuses` upsert is ours. This route is what CI drives, so a `400`
/// on an outage is a build reported as failed-by-configuration.
#[tokio::test]
async fn commit_status_separates_a_bad_state_from_a_broken_upsert() {
    let (base, db, token, _repo_id) = app_with_repo("statusfail").await;
    let client = reqwest::Client::new();
    let sha = "0".repeat(40);
    let url = format!("{base}/api/v1/repos/statusfail-owner/statusfail-repo/statuses/{sha}");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "sideways", "context": "ci/build"}))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        400,
        "an unknown commit status state is still the caller's mistake"
    );

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "success", "context": "ci/build"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "baseline: a healthy status is recorded");

    db.execute_unprepared("DROP TABLE commit_statuses")
        .await
        .expect("drop commit_statuses");

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "failure", "context": "ci/build"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_not_blamed_on_the_client(status, &body, "commit status");
}

// These triggers make the check-then-insert window deterministic.  Each one
// inserts the otherwise-missing row while the target insert is in progress, so
// the target sees a real SQLite UNIQUE violation after its pre-check returned
// `None`.  This is the exact error shape a concurrent request produces, without
// relying on scheduler timing to make a test race happen.

#[tokio::test]
async fn register_unique_loss_after_the_precheck_is_the_same_conflict() {
    let (base, db) = spawn_test_app_with_db().await;
    db.execute_unprepared(
        r#"
        CREATE TRIGGER inject_register_unique_conflict
        BEFORE INSERT ON users
        WHEN NEW.username = 'race-register' AND NEW.email = 'race-register@example.test'
        BEGIN
            INSERT INTO users (username, email, password_hash, created_at, updated_at)
            VALUES (
                'race-register',
                'race-register-trigger@example.test',
                'not-used-by-this-test',
                CURRENT_TIMESTAMP,
                CURRENT_TIMESTAMP
            );
        END
        "#,
    )
    .await
    .expect("install registration race injector");

    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/register"))
        .json(&serde_json::json!({
            "username": "race-register",
            "email": "race-register@example.test",
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("request");

    assert_eq!(
        response.status(),
        409,
        "a registration that loses the UNIQUE race is the same client outcome as a sequential duplicate"
    );
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "username or email is already registered",
        "the synthetic UNIQUE violation must not leak database detail"
    );
}

#[tokio::test]
async fn org_unique_loss_after_the_precheck_is_the_same_conflict() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) =
        register_full(&base, "race-org-owner", "race-org-owner@example.test").await;
    db.execute_unprepared(
        r#"
        CREATE TRIGGER inject_org_unique_conflict
        BEFORE INSERT ON organizations
        WHEN NEW.name = 'race-org' AND NEW.description = 'outer request'
        BEGIN
            INSERT INTO organizations (name, description, owner_id, visibility, created_at, updated_at)
            VALUES (
                'race-org',
                'inserted by the deterministic race injector',
                NEW.owner_id,
                'public',
                CURRENT_TIMESTAMP,
                CURRENT_TIMESTAMP
            );
        END
        "#,
    )
    .await
    .expect("install organization race injector");

    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "race-org",
            "description": "outer request",
            "visibility": "public",
        }))
        .send()
        .await
        .expect("request");

    assert_eq!(
        response.status(),
        409,
        "an organization insert that loses the UNIQUE race is the same client outcome as a sequential duplicate"
    );
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "organization name 'race-org' is already taken",
        "the synthetic UNIQUE violation must preserve the sequential duplicate message"
    );
}

#[tokio::test]
async fn branch_protection_unique_loss_after_the_precheck_is_the_same_conflict() {
    let (base, db, token, _repo_id) = app_with_repo("racebranch").await;
    db.execute_unprepared(
        r#"
        CREATE TRIGGER inject_branch_protection_unique_conflict
        BEFORE INSERT ON protected_branches
        WHEN NEW.branch_name = 'race-branch' AND NEW.required_status_checks IS NULL
        BEGIN
            INSERT INTO protected_branches (repo_id, branch_name, required_status_checks)
            VALUES (NEW.repo_id, 'race-branch', '[]');
        END
        "#,
    )
    .await
    .expect("install branch-protection race injector");

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/racebranch-owner/racebranch-repo/branches/protection"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "branch_name": "race-branch",
            "require_pr": true,
        }))
        .send()
        .await
        .expect("request");

    assert_eq!(
        response.status(),
        409,
        "a branch-protection insert that loses the UNIQUE race is the same client outcome as a sequential duplicate"
    );
}

#[tokio::test]
async fn sso_provider_unique_loss_after_the_precheck_is_the_same_conflict() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) =
        register_full(&base, "race-sso-admin", "race-sso-admin@example.test").await;
    rg_db::ops::user_ops::update_by_id(&db, user_id, None, None, Some(true), None)
        .await
        .expect("promote test user")
        .expect("registered user must exist");
    db.execute_unprepared(
        r#"
        CREATE TRIGGER inject_sso_provider_unique_conflict
        BEFORE INSERT ON sso_providers
        WHEN NEW.slug = 'race-sso' AND NEW.name = 'Race SSO outer request'
        BEGIN
            INSERT INTO sso_providers (name, slug, provider_type, enabled, created_at, updated_at)
            VALUES (
                'Race SSO injected winner',
                'race-sso',
                'oauth2',
                false,
                CURRENT_TIMESTAMP,
                CURRENT_TIMESTAMP
            );
        END
        "#,
    )
    .await
    .expect("install SSO-provider race injector");

    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/admin/sso/providers"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "Race SSO outer request",
            "slug": "race-sso",
            "provider_type": "oauth2",
            "enabled": false,
        }))
        .send()
        .await
        .expect("request");

    assert_eq!(
        response.status(),
        409,
        "an SSO-provider insert that loses the UNIQUE race is the same client outcome as a sequential duplicate"
    );
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "an SSO provider with slug 'race-sso' already exists",
        "the synthetic UNIQUE violation must preserve the sequential duplicate message"
    );
}
