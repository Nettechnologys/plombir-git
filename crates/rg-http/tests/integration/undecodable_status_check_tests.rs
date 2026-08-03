//! card_7ad8276168a0: an unreadable `required_status_checks` must not turn the
//! branch's status-check gate off.
//!
//! `check_merge_allowed` read the stored list with
//! `if let Ok(required_checks) = serde_json::from_str::<Vec<String>>(..)`, and
//! the *entire* check lived inside that arm — the head-sha lookup, the pipeline
//! lookup, the job comparison. A column that failed to decode simply skipped all
//! of it and the function returned `Ok(())`, while `require_status_check` stayed
//! `true` in the database and lit up in the UI. The branch looked protected and
//! merged with no CI checked at all.
//!
//! This is the fail-*open* member of the family in
//! `super::undecodable_allow_list_tests`. Those cost a listed approver an
//! unexplained 403 for a row of ours that was broken; this one costs the branch
//! its protection, which is why it is the worse of the two even though the code
//! shape is the same.
//!
//! **This test needs a real seeded repository, and that is the whole point.**
//! The first version of it inserted a bare PR row and asserted "the merge
//! answers a 5xx and the PR is not merged". That passes with the fix *reverted*:
//! past the gate the handler goes on to do real git work, which fails in a
//! fixture with no objects and produces its own 5xx, so both the guarded and the
//! unguarded run look identical from outside. Verified by reverting the fix and
//! watching it stay green. With `main` and `feature` actually pushed, a skipped
//! gate merges for real — `200` and a moved `refs/heads/main` — so the assertion
//! below has something to fail on.
//!
//! The `NULL` half ("no list configured" keeps its behaviour) is pinned in
//! `rg-core`'s `status_check_gate_tests`, together with the service-level
//! `Ok`/`Err` distinction.

use std::path::Path;

use sea_orm::ConnectionTrait;

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

const OWNER: &str = "statuscheck-owner";
const REPO: &str = "statuscheck-repo";

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

        let protected = client
            .post(format!(
                "{base}/api/v1/repos/{OWNER}/{REPO}/branches/protection"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "branch_name": "main",
                "require_status_check": true,
                "required_status_checks": ["build", "test"],
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

        Self {
            base,
            db,
            token,
            bare_path,
            main_before,
            _worktree: worktree,
            _app_dir: app_dir,
        }
    }

    /// Overwrite the stored list with valid UTF-8 that is not a JSON array of
    /// strings — the shape a half-written migration or a hand-edited row leaves.
    async fn corrupt_the_checks(&self) {
        self.db
            .execute_unprepared(
                "UPDATE protected_branches SET required_status_checks = '{\"build\": true}' \
                 WHERE branch_name = 'main';",
            )
            .await
            .expect("corrupt the stored check list");
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

/// The defect. With the fix reverted this merges for real: `200`, and
/// `refs/heads/main` advances onto a feature nothing ever checked.
#[tokio::test]
async fn an_undecodable_check_list_does_not_let_the_merge_through() {
    let fixture = Fixture::new().await;
    fixture.corrupt_the_checks().await;

    let response = fixture.merge().await;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    assert!(
        !fixture.main_moved(),
        "a rule the server cannot read must not be treated as no rule: the \
         protected branch moved anyway ({status}, body: {body})"
    );
    assert!(
        status.is_server_error(),
        "an unreadable rule of ours is our failure, not the caller's — got \
         {status} (body: {body})"
    );
    for leak in ["required_status_checks", "db:", "expected value"] {
        assert!(
            !body.contains(leak),
            "the 5xx body must not carry internal detail ({leak:?}): {body}"
        );
    }
}

/// The control, in two directions. The same rule stored *readably* refuses with
/// its own `403` and leaves the branch alone — and once the rule is dropped the
/// very same request merges, which proves the fixture can merge at all and that
/// the refusal above came from the gate rather than from a broken setup.
#[tokio::test]
async fn a_readable_rule_refuses_and_dropping_it_lets_the_merge_through() {
    let fixture = Fixture::new().await;

    let response = fixture.merge().await;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert_eq!(
        status, 403,
        "no pipeline has run, so a readable rule refuses the caller (body: {body})"
    );
    assert!(
        body.contains("status check"),
        "the refusal must say which rule refused, got: {body}"
    );
    assert!(!fixture.main_moved(), "and the branch must not have moved");

    fixture
        .db
        .execute_unprepared("DELETE FROM protected_branches WHERE branch_name = 'main';")
        .await
        .expect("drop the protection rule");

    let response = fixture.merge().await;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert_eq!(
        status, 200,
        "with no rule left the merge goes through — otherwise the assertions \
         above prove nothing about the gate (body: {body})"
    );
    assert!(fixture.main_moved(), "and the branch moved: {body}");
}
