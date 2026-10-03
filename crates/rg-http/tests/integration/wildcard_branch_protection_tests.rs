//! A globbed branch rule must guard Git push and PR merge alike. An exact rule
//! on the same branch cannot hide the glob's approval or CI requirements.

use std::path::Path;

use sea_orm::ConnectionTrait;

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

const OWNER: &str = "glob-owner";
const REPO: &str = "glob-repo";

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(args, cwd)
        .expect("run git");
    output.ensure_success().expect("git succeeded");
    output.stdout_str().trim().to_string()
}

struct Fixture {
    base: String,
    token: String,
    db: sea_orm::DatabaseConnection,
    bare: std::path::PathBuf,
    worktree: tempfile::TempDir,
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
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        wait_for_listener(&addr).await;
        let base = format!("http://{addr}");

        let (token, owner_id) = register_full(&base, OWNER, &format!("{OWNER}@example.test")).await;
        rg_core::repo::service::create_repo(&db, owner_id, REPO, None, false, &repo_root, None)
            .await
            .expect("create repository");
        let bare = repo_root.join(format!("{OWNER}/{REPO}.git"));

        let worktree = tempfile::tempdir().unwrap();
        let path = worktree.path();
        git(&["init", "-q", "-b", "main"], Some(path));
        git(&["config", "user.name", "Glob Test"], Some(path));
        git(&["config", "user.email", "glob@example.test"], Some(path));
        std::fs::write(path.join("README.md"), "base\n").unwrap();
        git(&["add", "."], Some(path));
        git(&["commit", "-qm", "base"], Some(path));
        let bare_arg = bare.to_str().unwrap();
        git(&["remote", "add", "origin", bare_arg], Some(path));
        git(&["push", "-q", "origin", "main"], Some(path));
        git(&["checkout", "-q", "-b", "release/1"], Some(path));
        git(&["push", "-q", "origin", "release/1"], Some(path));
        git(&["checkout", "-q", "main"], Some(path));
        git(&["checkout", "-q", "-b", "feature"], Some(path));
        std::fs::write(path.join("feature.txt"), "feature\n").unwrap();
        git(&["add", "."], Some(path));
        git(&["commit", "-qm", "feature"], Some(path));
        git(&["push", "-q", "origin", "feature"], Some(path));

        let fixture = Self {
            base,
            token,
            db,
            bare,
            worktree,
            _app_dir: app_dir,
        };
        for (index, branch) in ["release/1", "main"].into_iter().enumerate() {
            let response = reqwest::Client::new()
                .post(format!(
                    "{}/api/v1/repos/{OWNER}/{REPO}/pulls",
                    fixture.base
                ))
                .bearer_auth(&fixture.token)
                .json(&serde_json::json!({
                    "title": format!("Merge into {branch}"),
                    "head": "feature", "base": branch,
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
            let opened: serde_json::Value = response.json().await.unwrap();
            assert_eq!(opened["base_branch"], branch, "{opened}");
            assert_eq!(opened["number"], index + 1, "{opened}");
        }
        fixture
    }

    async fn protect(&self, branch: &str, approval: bool, checks: bool, require_pr: bool) {
        let response = reqwest::Client::new()
            .post(format!(
                "{}/api/v1/repos/{OWNER}/{REPO}/branches/protection",
                self.base
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "branch_name": branch,
                "require_pr": require_pr,
                "require_approval": approval,
                "require_status_check": checks,
                "allow_force_push": true,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    }

    async fn merge(&self, number: u32) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!(
                "{}/api/v1/repos/{OWNER}/{REPO}/pulls/{number}/merge",
                self.base
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "strategy": "merge" }))
            .send()
            .await
            .unwrap()
    }

    fn ref_tip(&self, branch: &str) -> String {
        git(
            &["rev-parse", &format!("refs/heads/{branch}")],
            Some(&self.bare),
        )
    }
}

// Git's synchronous HTTP client must leave Tokio workers free for the router.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wildcard_rule_rejects_push_and_merge_but_not_an_unprotected_merge() {
    let fixture = Fixture::new().await;
    fixture.protect("release/*", true, true, true).await;
    // A permissive exact rule must not win over the overlapping glob.
    fixture.protect("release/1", false, false, false).await;
    let release_before = fixture.ref_tip("release/1");
    let main_before = fixture.ref_tip("main");

    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/users/tokens", fixture.base))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "name": "glob-push" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    let token: serde_json::Value = response.json().await.unwrap();
    let pat = token["token"].as_str().expect("PAT returned");
    let host = fixture.base.trim_start_matches("http://");
    let remote = format!("http://{OWNER}:{pat}@{host}/{OWNER}/{REPO}.git");
    let path = fixture.worktree.path();
    git(&["checkout", "-q", "release/1"], Some(path));
    std::fs::write(path.join("direct.txt"), "direct push\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "direct push"], Some(path));
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .unwrap()
        .run(&["push", &remote, "HEAD:release/1"], Some(path))
        .unwrap();
    assert!(!output.success(), "globbed rule allowed a direct Git push");
    assert!(
        output.stderr_str().contains("protected branch 'release/*'"),
        "{}",
        output.stderr_str()
    );
    assert_eq!(fixture.ref_tip("release/1"), release_before);

    let response = fixture.merge(1).await;
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("approval"), "{body}");
    assert_eq!(fixture.ref_tip("release/1"), release_before);

    // Reaching the second requirement must still fail: the missing approvals
    // cannot be the only reason this merge was refused.
    fixture
        .db
        .execute_unprepared(
            "UPDATE protected_branches SET require_approval = 0 WHERE branch_name = 'release/*'",
        )
        .await
        .unwrap();
    let response = fixture.merge(1).await;
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("status check"), "{body}");
    assert_eq!(fixture.ref_tip("release/1"), release_before);

    let response = fixture.merge(2).await;
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_ne!(
        fixture.ref_tip("main"),
        main_before,
        "baseline PR did not merge: {body}"
    );
}
