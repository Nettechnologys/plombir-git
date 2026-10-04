//! card_2e84eeed4ff4: the `owner:` half of a fork PR's head ref is a namespace.
//!
//! `resolve_head_ref` read it out of `users` instead, which failed in both
//! directions at once. An organization is not a row in `users`, so `acme:branch`
//! could never name a head repository at all. And an organization's repositories
//! carry `owner_id = org.owner_id`, so the "is this the same owner?" comparison
//! against `target_repo.owner_id` answered yes for the organization owner's
//! *personal* repository — a different namespace since card_92019cc97dcd, and
//! one that may hold a repository of the same name.
//!
//! The second failure is the worse one: it does not refuse, it agrees. The head
//! ref resolved to `(branch, None)`, which means "same-repo PR", so a PR whose
//! head was meant to be somebody's personal fork was opened as a branch of the
//! organization's own repository.
//!
//! These call `resolve_head_ref` directly. The answer under test is which
//! repository the prefix resolves to, and going through `POST /pulls` would put
//! a Git ref walk between the assertion and what it is about.

use rg_db::entities::repository;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::common::{register_full, spawn_test_app_with_db};

/// Whether an error is the "the caller named something wrong" kind — the one
/// `AppError` turns into a 400. Asserted rather than the HTTP status because
/// these tests call the resolver directly; the mapping itself is covered by
/// `service_failure_status_sweep_tests`.
fn is_client_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<rg_core::error::InvalidRequest>()
        .is_some()
}

async fn create_repo_in(base: &str, token: &str, name: &str, org: Option<&str>) -> i64 {
    let mut body = serde_json::json!({ "name": name });
    if let Some(org) = org {
        body["org"] = serde_json::json!(org);
    }
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("create repo");
    let status = resp.status();
    let payload: serde_json::Value = resp.json().await.expect("create repo body");
    assert_eq!(status, 201, "fixture repository must be created: {payload}");
    payload["id"].as_i64().expect("created repository id")
}

async fn create_org(base: &str, token: &str, name: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "visibility": "public" }))
        .send()
        .await
        .expect("create org")
        .status();
    assert_eq!(status, 201, "fixture organization must be created");
}

/// Mark `repo_id` as a fork of `origin_id`.
///
/// The fork endpoint only forks into the caller's personal namespace, so an
/// organization-owned fork has to be built here. It is the same row shape
/// `fork_repo` writes — `origin_repo_id` is the whole of what makes a
/// repository a fork as far as `resolve_head_ref` is concerned.
async fn mark_as_fork(db: &rg_db::DatabaseConnection, repo_id: i64, origin_id: i64) {
    let repo = repository::Entity::find_by_id(repo_id)
        .one(db)
        .await
        .expect("read fixture repository")
        .expect("fixture repository exists");
    let mut active: repository::ActiveModel = repo.into();
    active.origin_repo_id = Set(Some(origin_id));
    active.update(db).await.expect("mark fixture as a fork");
}

#[tokio::test]
async fn an_organization_repository_can_be_the_head_of_a_fork_pr() {
    let (base, db) = spawn_test_app_with_db().await;
    let (upstream_token, _) = register_full(&base, "hn-upstream", "hn-upstream@example.test").await;
    let upstream = create_repo_in(&base, &upstream_token, "shared", None).await;

    let (org_owner, _) = register_full(&base, "hn-orgowner", "hn-orgowner@example.test").await;
    create_org(&base, &org_owner, "hn-acme").await;
    let org_repo = create_repo_in(&base, &org_owner, "shared", Some("hn-acme")).await;
    mark_as_fork(&db, org_repo, upstream).await;

    let (branch, head_repo_id) =
        rg_core::pull_request::resolve_head_ref(&db, upstream, "hn-acme:feature")
            .await
            .expect("an organization namespace must resolve as a head owner");
    assert_eq!(branch, "feature");
    assert_eq!(
        head_repo_id,
        Some(org_repo),
        "the head ref must resolve to the organization's repository"
    );

    // Baseline in the same test: a personal fork, the case that always worked,
    // still resolves the same way. A namespace fix proves nothing if it moved
    // the answer for the namespace that was already right.
    let (forker, _) = register_full(&base, "hn-forker", "hn-forker@example.test").await;
    let personal_fork = create_repo_in(&base, &forker, "shared", None).await;
    mark_as_fork(&db, personal_fork, upstream).await;

    let (branch, head_repo_id) =
        rg_core::pull_request::resolve_head_ref(&db, upstream, "hn-forker:feature")
            .await
            .expect("a personal fork must still resolve");
    assert_eq!(branch, "feature");
    assert_eq!(head_repo_id, Some(personal_fork));
}

#[tokio::test]
async fn the_org_owners_personal_repository_is_not_the_organizations() {
    let (base, db) = spawn_test_app_with_db().await;
    let (org_owner, _) = register_full(&base, "hn2-owner", "hn2-owner@example.test").await;
    create_org(&base, &org_owner, "hn2-acme").await;
    // The target of the PR is the ORGANIZATION's repository. Its row carries
    // `owner_id = org.owner_id`, which is what the old comparison read.
    let org_repo = create_repo_in(&base, &org_owner, "shared", Some("hn2-acme")).await;
    // The same account also owns a personal repository of the same name — legal
    // since card_92019cc97dcd, and unrelated to the organization's.
    let personal = create_repo_in(&base, &org_owner, "shared", None).await;
    assert_ne!(personal, org_repo, "the fixture needs two distinct rows");

    // Naming the personal namespace must not read as "same repository". Before
    // the fix this returned `Ok((branch, None))` — a same-repo PR on the
    // organization's repository, opened off a head ref that pointed elsewhere.
    let error = rg_core::pull_request::resolve_head_ref(&db, org_repo, "hn2-owner:feature")
        .await
        .expect_err("a personal repository that is not a fork must be refused");
    assert!(
        is_client_error(&error),
        "an unrelated head repository is the caller's mistake, not a server failure: {error:#}"
    );

    // And once it really is a fork of the organization's repository, it resolves
    // as one — with its own id, not with `None`.
    mark_as_fork(&db, personal, org_repo).await;
    let (branch, head_repo_id) =
        rg_core::pull_request::resolve_head_ref(&db, org_repo, "hn2-owner:feature")
            .await
            .expect("a personal fork of an organization repository must resolve");
    assert_eq!(branch, "feature");
    assert_eq!(head_repo_id, Some(personal));

    // The organization's own prefix still means "this repository" — the long
    // spelling of a same-repo PR, not a fork of itself.
    let (branch, head_repo_id) =
        rg_core::pull_request::resolve_head_ref(&db, org_repo, "hn2-acme:feature")
            .await
            .expect("the target's own namespace must resolve");
    assert_eq!(branch, "feature");
    assert_eq!(
        head_repo_id, None,
        "the target repository named through its own namespace is a same-repo PR"
    );
}

#[tokio::test]
async fn an_unknown_head_namespace_stays_the_callers_mistake() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "hn3-owner", "hn3-owner@example.test").await;
    let upstream = create_repo_in(&base, &token, "shared", None).await;

    let error = rg_core::pull_request::resolve_head_ref(&db, upstream, "ghost:feature")
        .await
        .expect_err("a namespace that does not exist names no head repository");
    assert!(
        is_client_error(&error),
        "an unknown head namespace must stay a 400, not become a server failure: {error:#}"
    );

    // The target's own namespace is not an error — it is the long spelling of a
    // same-repo PR, and it must keep resolving to `None` rather than to a fork.
    let (branch, head_repo_id) =
        rg_core::pull_request::resolve_head_ref(&db, upstream, "hn3-owner:feature")
            .await
            .expect("the target's own namespace must resolve");
    assert_eq!(branch, "feature");
    assert_eq!(head_repo_id, None);

    // A bare branch name is a same-repo PR and never touches the namespace path.
    let (branch, head_repo_id) = rg_core::pull_request::resolve_head_ref(&db, upstream, "feature")
        .await
        .expect("a bare branch name must resolve");
    assert_eq!(branch, "feature");
    assert_eq!(head_repo_id, None);
}

fn git(args: &[&str], cwd: Option<&std::path::Path>) {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    output.ensure_success().unwrap();
}

fn git_stdout(args: &[&str], cwd: &std::path::Path) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, Some(cwd)).unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

/// Give `upstream` a `main` and `fork` the same `main` plus a `feature` branch.
///
/// One worktree pushed to both bare repositories, so the two share history and
/// the diff between them is the feature commit alone.
fn seed_shared_history(upstream: &std::path::Path, fork: &std::path::Path) {
    let worktree = tempfile::tempdir().expect("create fixture worktree");
    let path = worktree.path();
    let path_arg = path.to_str().expect("UTF-8 worktree path");

    git(&["init", "-q", "-b", "main", path_arg], None);
    git(&["config", "user.name", "Head namespace test"], Some(path));
    git(
        &["config", "user.email", "head-namespace@example.invalid"],
        Some(path),
    );
    std::fs::write(path.join("README.md"), "base\n").expect("write base file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "base"], Some(path));

    let upstream_arg = upstream.to_str().expect("UTF-8 upstream path");
    let fork_arg = fork.to_str().expect("UTF-8 fork path");
    git(&["push", upstream_arg, "main"], Some(path));
    git(&["push", fork_arg, "main"], Some(path));

    git(&["checkout", "-q", "-b", "feature"], Some(path));
    std::fs::write(path.join("feature.txt"), "feature\n").expect("write feature file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "feature"], Some(path));
    git(&["push", fork_arg, "feature"], Some(path));
}

/// End to end: an organization's repository opens a PR against the upstream and
/// the diff comes back.
///
/// The resolver tests above prove the head ref resolves. This one proves the
/// rest of the path can carry the answer — `compute_diff` built the fork's
/// on-disk path from `users.username` of `head_repo.owner_id`, which for an
/// organization's repository names a directory that does not exist. That code
/// was unreachable while an organization could not be a head owner at all, so
/// fixing only the resolver would have moved the failure one endpoint down.
#[tokio::test]
async fn an_org_fork_pr_is_created_and_its_diff_is_readable() {
    let (base, db, repo_root) = crate::common::spawn_test_app_with_db_and_repo_root().await;
    let (upstream_token, _) = register_full(&base, "hn4-up", "hn4-up@example.test").await;
    let upstream = create_repo_in(&base, &upstream_token, "shared", None).await;

    let (org_owner, _) = register_full(&base, "hn4-owner", "hn4-owner@example.test").await;
    create_org(&base, &org_owner, "hn4-acme").await;
    let org_repo = create_repo_in(&base, &org_owner, "shared", Some("hn4-acme")).await;
    mark_as_fork(&db, org_repo, upstream).await;

    seed_shared_history(
        &repo_root.join("hn4-up/shared.git"),
        &repo_root.join("hn4-acme/shared.git"),
    );

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&org_owner)
        .json(&serde_json::json!({
            "name": "base-only", "scopes": "repo", "repositories": ["hn4-up/shared"],
        }))
        .send()
        .await
        .expect("mint repository-confined PAT");
    let status = response.status();
    let token: serde_json::Value = response.json().await.expect("PAT response");
    assert_eq!(status, 201, "{token}");
    let base_only = token["token"].as_str().expect("raw PAT");
    let create = serde_json::json!({
        "title": "from the organization",
        "head": "hn4-acme:feature",
        "base": "main",
    });
    let refused = client
        .post(format!("{base}/api/v1/repos/hn4-up/shared/pulls"))
        .bearer_auth(base_only)
        .json(&create)
        .send()
        .await
        .expect("try fork PR with base-only PAT");
    assert_eq!(refused.status(), 403);
    let denied: serde_json::Value = refused.json().await.expect("refusal body");
    assert_eq!(denied["error"]["code"], "FORBIDDEN", "{denied}");
    let mut missing_request = create.clone();
    missing_request["head"] = serde_json::json!("hn4-ghost:feature");
    let missing = client
        .post(format!("{base}/api/v1/repos/hn4-up/shared/pulls"))
        .bearer_auth(base_only)
        .json(&missing_request)
        .send()
        .await
        .expect("try unknown fork with base-only PAT");
    assert_eq!(missing.status(), 403);
    let missing: serde_json::Value = missing.json().await.expect("refusal body");
    assert_eq!(missing["error"]["code"], "FORBIDDEN", "{missing}");
    assert_eq!(missing["error"]["message"], denied["error"]["message"]);

    // The account can open this exact PR without repository confinement; the
    // refusal above must be the PAT boundary, not a broken fork fixture.
    let resp = client
        .post(format!("{base}/api/v1/repos/hn4-up/shared/pulls"))
        .bearer_auth(&org_owner)
        .json(&create)
        .send()
        .await
        .expect("create PR");
    let status = resp.status();
    let pr: serde_json::Value = resp.json().await.expect("create PR body");
    assert_eq!(status, 201, "an org fork PR must be created: {pr}");
    assert_eq!(
        pr["head_repo_id"].as_i64(),
        Some(org_repo),
        "the PR must record the organization's repository as its head: {pr}"
    );

    let number = pr["number"].as_i64().expect("PR number");
    let resp = client
        .get(format!(
            "{base}/api/v1/repos/hn4-up/shared/pulls/{number}/diff"
        ))
        .bearer_auth(&upstream_token)
        .send()
        .await
        .expect("read PR diff");
    let status = resp.status();
    let diff: serde_json::Value = resp.json().await.expect("diff body");
    assert_eq!(
        status, 200,
        "the org fork PR's diff must be readable: {diff}"
    );
    assert!(
        diff["files_changed"]
            .as_array()
            .is_some_and(|files| files.iter().any(|file| file["path"] == "feature.txt")),
        "the diff must contain the fork's feature commit: {diff}"
    );
}

#[tokio::test]
async fn a_base_only_pat_cannot_apply_suggestions_to_a_fork_head() {
    let (base, db, repo_root) = crate::common::spawn_test_app_with_db_and_repo_root().await;
    let (base_token, _) = register_full(&base, "sg-base", "sg-base@example.test").await;
    let base_id = create_repo_in(&base, &base_token, "shared", None).await;
    let (fork_token, _) = register_full(&base, "sg-owner", "sg-owner@example.test").await;
    create_org(&base, &fork_token, "sg-org").await;
    let fork_id = create_repo_in(&base, &fork_token, "shared", Some("sg-org")).await;
    mark_as_fork(&db, fork_id, base_id).await;
    let fork_path = repo_root.join("sg-org/shared.git");
    seed_shared_history(&repo_root.join("sg-base/shared.git"), &fork_path);

    let client = reqwest::Client::new();
    let pr_url = format!("{base}/api/v1/repos/sg-base/shared/pulls");
    let created = client
        .post(&pr_url)
        .bearer_auth(&fork_token)
        .json(&serde_json::json!({
            "title": "fork suggestion", "head": "sg-org:feature", "base": "main"
        }))
        .send()
        .await
        .unwrap();
    let status = created.status();
    let pr: serde_json::Value = created.json().await.unwrap();
    assert_eq!(status, 201, "{pr}");
    assert_eq!(pr["head_repo_id"].as_i64(), Some(fork_id));
    let number = pr["number"].as_i64().unwrap();
    let head_before = git_stdout(&["rev-parse", "refs/heads/feature"], &fork_path);
    assert_eq!(pr["head_sha"].as_str(), Some(head_before.as_str()));
    let comment = client
        .post(format!("{pr_url}/{number}/comments"))
        .bearer_auth(&base_token)
        .json(&serde_json::json!({
            "path": "feature.txt", "line": 1, "side": "RIGHT",
            "body": "improve the feature", "suggestion": "reviewed feature",
            "commit_id": head_before,
        }))
        .send()
        .await
        .unwrap();
    let status = comment.status();
    let comment: serde_json::Value = comment.json().await.unwrap();
    assert_eq!(status, 201, "{comment}");
    let comment_id = comment["id"].as_i64().unwrap();

    let minted = client
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&fork_token)
        .json(&serde_json::json!({
            "name": "suggestion-base-only", "scopes": "repo",
            "repositories": ["sg-base/shared"],
        }))
        .send()
        .await
        .unwrap();
    let status = minted.status();
    let token: serde_json::Value = minted.json().await.unwrap();
    assert_eq!(status, 201, "{token}");
    let base_only = token["token"].as_str().unwrap();
    let single_url = format!("{pr_url}/{number}/comments/{comment_id}/suggestion/apply");
    let batch_url = format!("{pr_url}/{number}/suggestions/apply");
    for (url, batch) in [(&single_url, false), (&batch_url, true)] {
        let request = client.post(url).bearer_auth(base_only);
        let request = if batch {
            request.json(&serde_json::json!({"comment_ids": [comment_id]}))
        } else {
            request
        };
        let response = request.send().await.unwrap();
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(status, 403, "{url}: {body}");
        assert_eq!(body["error"]["code"], "FORBIDDEN", "{body}");
        assert_eq!(
            git_stdout(&["rev-parse", "refs/heads/feature"], &fork_path),
            head_before
        );
    }

    let denials = rg_db::entities::audit_log::Entity::find()
        .filter(rg_db::entities::audit_log::Column::Action.eq("agent.scope_denied"))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(denials.len(), 2, "each refused route must be audited");

    // The same account can apply the same suggestion when its credential is
    // not confined to the base repository.
    let applied = client
        .post(&single_url)
        .bearer_auth(&fork_token)
        .send()
        .await
        .unwrap();
    let status = applied.status();
    let body: serde_json::Value = applied.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    let head_after = git_stdout(&["rev-parse", "refs/heads/feature"], &fork_path);
    assert_ne!(head_after, head_before);
    assert_eq!(body["commit_sha"].as_str(), Some(head_after.as_str()));
}
