//! card_58d3af041513: ticking "require status checks" and naming no checks must
//! require a green CI run — not nothing.
//!
//! `check_merge_allowed` nested the whole status-check block inside
//! `if let Some(checks_json) = &protection.required_status_checks`, so a rule
//! with `require_status_check = true` and a `NULL` name list skipped the
//! head-sha lookup, the pipeline lookup and the job comparison in one go and
//! returned `Ok(())`. The rule stayed `true` in the database, listed as enabled
//! in the settings UI, and every merge into the protected branch went through
//! with no pipeline ever having run.
//!
//! **The test goes through the API, not the service, because the API is where
//! the `NULL` comes from.** The settings form builds its request body with
//! `parseStringList('')`, which returns `undefined` for a blank names field, so
//! the key never leaves the browser — and `Option<Vec<String>>` on the handler
//! turns "key absent" into the `NULL` column. An operator who ticks the box and
//! leaves the names blank is not writing a broken row by hand; they are using
//! the most natural spelling of "I want a green CI" the product offers. The
//! request below is byte-for-byte what that form sends.
//!
//! The sibling `undecodable_status_check_tests` pins the same block against an
//! *unreadable* list, and `rg-core`'s `status_check_gate_tests` pins the
//! service-level `Ok`/`Err` distinction for both.
//!
//! **Mutation check.** Restoring the `if let Some(checks_json) = ...` wrapper
//! makes `a_rule_with_no_named_checks_still_requires_a_pipeline` fail with
//! `200 OK` and a moved `refs/heads/main` — the branch really does merge with
//! nothing checked. The second test stays green under that revert, which is
//! exactly why it is here: on its own it would prove only that the fixture can
//! merge.

use std::path::Path;

use sea_orm::EntityTrait;

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

const OWNER: &str = "blankchecks-owner";
const REPO: &str = "blankchecks-repo";

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

/// `main` with one commit and `feature` one commit ahead of it — enough for the
/// merge to succeed if nothing stops it.
fn seed(bare_path: &Path) -> tempfile::TempDir {
    let worktree = tempfile::tempdir().unwrap();
    let path = worktree.path();
    let path_arg = path.to_string_lossy();
    git(&["init", "--initial-branch=main", &path_arg], None);
    git(&["config", "user.name", "Gate Test"], Some(path));
    git(&["config", "user.email", "gate@example.test"], Some(path));

    std::fs::write(path.join("README.md"), "base\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "base"], Some(path));
    let bare_arg = bare_path.to_string_lossy();
    git(&["remote", "add", "origin", &bare_arg], Some(path));
    git(&["push", "origin", "main"], Some(path));

    git(&["checkout", "-b", "feature"], Some(path));
    std::fs::write(path.join("feature.txt"), "feature\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "feature"], Some(path));
    git(&["push", "origin", "feature"], Some(path));

    worktree
}

struct Fixture {
    base: String,
    db: sea_orm::DatabaseConnection,
    token: String,
    repo_id: i64,
    bare_path: std::path::PathBuf,
    main_before: String,
    _worktree: tempfile::TempDir,
    _app_dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let (db, app_dir) = setup_test_db().await;
        let repo_root = app_dir.path().join("repos");
        std::fs::create_dir_all(&repo_root).unwrap();
        let app =
            rg_http::create_router_for_test(build_test_app_state(db.clone(), repo_root.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let base = format!("http://{addr}");
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        wait_for_listener(&addr).await;

        let (token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.test")).await;
        let client = reqwest::Client::new();

        let created = client
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": REPO, "is_private": false}))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), 201, "{}", created.text().await.unwrap());

        let bare_path = repo_root.join(format!("{OWNER}/{REPO}.git"));
        let worktree = seed(&bare_path);
        let main_before = git(&["rev-parse", "refs/heads/main"], Some(&bare_path));

        let opened = client
            .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/pulls"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "title": "Merge the feature",
                "head": "feature",
                "base": "main",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(opened.status(), 201, "{}", opened.text().await.unwrap());

        // Exactly the body the settings form sends when the names field is left
        // blank: `required_status_checks` is `undefined`, so the key is absent.
        let protected = client
            .post(format!(
                "{base}/api/v1/repos/{OWNER}/{REPO}/branches/protection"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "branch_name": "main",
                "require_pr": true,
                "require_status_check": true,
                "require_approval": false,
                "allow_force_push": false,
                "require_signed_commits": false,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            protected.status(),
            201,
            "the protection rule is the subject of this test and must exist: {}",
            protected.text().await.unwrap()
        );
        let rule: serde_json::Value = protected.json().await.expect("the created rule");
        assert!(
            rule["required_status_checks"].is_null(),
            "the fixture is only meaningful if the blank names field really \
             stored NULL, got: {rule}"
        );
        assert_eq!(
            rule["require_status_check"], true,
            "and only if the rule is stored as enabled: {rule}"
        );
        let repo_id = rule["repo_id"].as_i64().expect("the rule names its repo");

        Self {
            base,
            db,
            token,
            repo_id,
            bare_path,
            main_before,
            _worktree: worktree,
            _app_dir: app_dir,
        }
    }

    /// The head commit the gate looks a pipeline up by — read from the row
    /// rather than from git, so the seeded run lands on the sha the gate reads.
    async fn head_sha(&self) -> String {
        rg_db::entities::pull_request::Entity::find_by_id(1)
            .one(&self.db)
            .await
            .expect("read the pull request")
            .expect("the pull request the fixture opened")
            .head_sha
            .expect("an opened PR records its head commit")
    }

    /// A finished, successful CI run for the PR's head commit.
    async fn seed_successful_pipeline(&self) {
        let head_sha = self.head_sha().await;
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &self.db,
            self.repo_id,
            &head_sha,
            "refs/heads/feature",
            "push",
            None,
        )
        .await
        .expect("create the pipeline the gate will find");
        let now = chrono::Utc::now().naive_utc();
        rg_db::ops::pipeline_ops::update_pipeline_status(
            &self.db,
            pipeline.id,
            "success",
            Some(now),
            Some(now),
        )
        .await
        .expect("finish the pipeline green");
    }

    async fn merge(&self) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!(
                "{}/api/v1/repos/{OWNER}/{REPO}/pulls/1/merge",
                self.base
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({"strategy": "merge"}))
            .send()
            .await
            .expect("merge request")
    }

    /// The assertion that actually matters: did the protected branch move?
    fn main_moved(&self) -> bool {
        git(&["rev-parse", "refs/heads/main"], Some(&self.bare_path)) != self.main_before
    }
}

/// The defect. No pipeline has run for the head commit, and the rule says a
/// status check is required — so the merge must be refused. With the nested
/// `if let Some(..)` back it answers `200` and `main` advances.
#[tokio::test]
async fn a_rule_with_no_named_checks_still_requires_a_pipeline() {
    let fixture = Fixture::new().await;

    let response = fixture.merge().await;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    assert!(
        !fixture.main_moved(),
        "a rule that requires status checks must not merge a commit no pipeline \
         ever ran for ({status}, body: {body})"
    );
    assert_eq!(
        status, 403,
        "and the refusal is a policy refusal aimed at the merger, not a 5xx \
         (body: {body})"
    );
    assert!(
        body.contains("no CI pipeline has run"),
        "the refusal must name the rule that refused: {body}"
    );
}

/// The control. The same rule with a green pipeline on the head commit lets the
/// merge through — so the refusal above is the gate reading the CI state, not a
/// fixture that cannot merge and not a rule that refuses unconditionally.
#[tokio::test]
async fn the_same_rule_merges_once_the_pipeline_is_green() {
    let fixture = Fixture::new().await;
    fixture.seed_successful_pipeline().await;

    let response = fixture.merge().await;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    assert_eq!(
        status, 200,
        "an enabled rule with no named checks asks for a green run and got one \
         (body: {body})"
    );
    assert!(fixture.main_moved(), "and the branch moved: {body}");
}
