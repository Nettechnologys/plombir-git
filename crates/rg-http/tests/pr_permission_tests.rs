//! Regression coverage for repository-scoped PR and review authorization.

mod common;

use chrono::Utc;
use common::{build_test_app_state, register_full, setup_test_db, spawn_test_app_with_db};
use sea_orm::Set;

async fn create_private_repo(base: &str, token: &str, name: &str) -> i64 {
    create_repo_with_visibility(base, token, name, true).await
}

async fn create_repo_with_visibility(base: &str, token: &str, name: &str, is_private: bool) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": is_private}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn draft_pr_cannot_be_merged_and_can_be_marked_ready() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "draft-owner", "draft-owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "draft-prs").await;
    insert_pr(&db, repo_id, owner_id, 1).await;
    let client = reqwest::Client::new();
    let pr_url = format!("{base}/api/v1/repos/draft-owner/draft-prs/pulls/1");

    let draft = client
        .patch(&pr_url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"draft": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(draft.status(), 200);
    assert!(draft.json::<serde_json::Value>().await.unwrap()["is_draft"]
        .as_bool()
        .unwrap());

    let merge = client
        .post(format!("{pr_url}/merge"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(merge.status(), 409);

    let ready = client
        .patch(&pr_url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"draft": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(ready.status(), 200);
    assert!(
        !ready.json::<serde_json::Value>().await.unwrap()["is_draft"]
            .as_bool()
            .unwrap()
    );

    let timeline = client
        .get(format!("{pr_url}/timeline"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(timeline.status(), 200);
    let kinds = timeline
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| event["kind"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"pull_request_converted_to_draft".to_string()));
    assert!(kinds.contains(&"pull_request_marked_ready".to_string()));
}

#[tokio::test]
async fn reviewer_requests_and_thread_resolution_enforce_permissions() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "flow-owner", "flow-owner@example.com").await;
    let (reviewer_token, reviewer_id) =
        register_full(&base, "flow-reviewer", "flow-reviewer@example.com").await;
    let (outsider_token, _) =
        register_full(&base, "flow-outsider", "flow-outsider@example.com").await;
    let repo_id = create_repo_with_visibility(&base, &owner_token, "review-workflow", false).await;
    let pr = insert_pr(&db, repo_id, owner_id, 1).await;
    let client = reqwest::Client::new();
    let reviewers_url = format!("{base}/api/v1/repos/flow-owner/review-workflow/pulls/1/reviewers");

    let requested = client
        .post(&reviewers_url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"username": "flow-reviewer"}))
        .send()
        .await
        .unwrap();
    assert_eq!(requested.status(), 201);

    let duplicate = client
        .post(&reviewers_url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"username": "flow-reviewer"}))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), 409);

    let forbidden_remove = client
        .delete(format!("{reviewers_url}/flow-reviewer"))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden_remove.status(), 403);

    let review = rg_db::ops::pr_review_ops::create(
        &db,
        rg_db::entities::pr_review::ActiveModel {
            id: sea_orm::NotSet,
            pr_id: Set(pr.id),
            repo_id: Set(repo_id),
            reviewer_id: Set(reviewer_id),
            action: Set("comment".to_string()),
            body: Set(None),
            commit_id: Set(None),
            created_at: Set(Utc::now()),
        },
    )
    .await
    .unwrap();
    let comment = rg_db::ops::review_comment_ops::create(
        &db,
        rg_db::entities::review_comment::ActiveModel {
            id: sea_orm::NotSet,
            review_id: Set(review.id),
            pr_id: Set(pr.id),
            author_id: Set(reviewer_id),
            path: Set("src/lib.rs".to_string()),
            position: Set(None),
            line: Set(Some(7)),
            start_line: Set(None),
            side: Set(Some("RIGHT".to_string())),
            start_side: Set(None),
            body: Set("Please adjust this line".to_string()),
            suggestion: Set(None),
            suggestion_applied_at: Set(None),
            suggestion_applied_by_id: Set(None),
            suggestion_commit_sha: Set(None),
            commit_id: Set(None),
            reply_to_id: Set(None),
            resolved_at: Set(None),
            resolved_by_id: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
        },
    )
    .await
    .unwrap();
    let resolution_url = format!(
        "{base}/api/v1/repos/flow-owner/review-workflow/pulls/1/comments/{}/resolution",
        comment.id
    );

    let forbidden_resolution = client
        .patch(&resolution_url)
        .bearer_auth(&outsider_token)
        .json(&serde_json::json!({"resolved": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden_resolution.status(), 403);

    let resolved = client
        .patch(&resolution_url)
        .bearer_auth(&reviewer_token)
        .json(&serde_json::json!({"resolved": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resolved.status(), 200);
    assert!(resolved.json::<serde_json::Value>().await.unwrap()["resolved_at"].is_string());

    let reopened = client
        .patch(&resolution_url)
        .bearer_auth(&reviewer_token)
        .json(&serde_json::json!({"resolved": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(reopened.status(), 200);
    assert!(reopened.json::<serde_json::Value>().await.unwrap()["resolved_at"].is_null());

    let removed = client
        .delete(format!("{reviewers_url}/flow-reviewer"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), 204);

    let timeline = client
        .get(format!(
            "{base}/api/v1/repos/flow-owner/review-workflow/pulls/1/timeline"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(timeline.status(), 200);
    let kinds = timeline
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| event["kind"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    for expected in [
        "reviewer_requested",
        "reviewer_removed",
        "thread_resolved",
        "thread_reopened",
    ] {
        assert!(kinds.contains(&expected.to_string()), "missing {expected}");
    }
}

async fn insert_pr(
    db: &sea_orm::DatabaseConnection,
    repo_id: i64,
    author_id: i64,
    number: i64,
) -> rg_db::entities::pull_request::Model {
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: Set(number),
            title: Set(format!("PR {number}")),
            body: Set(Some("private changes".to_string())),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(author_id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(None),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn private_prs_require_repo_read_access_and_updates_require_ownership() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) = register_full(&base, "pr-owner", "pr-owner@example.com").await;
    let (outsider_token, outsider_id) =
        register_full(&base, "pr-outsider", "pr-outsider@example.com").await;
    assert_ne!(owner_id, outsider_id);

    let repo_id = create_private_repo(&base, &owner_token, "private-prs").await;
    insert_pr(&db, repo_id, owner_id, 1).await;
    let client = reqwest::Client::new();
    let pr_url = format!("{base}/api/v1/repos/pr-owner/private-prs/pulls/1");

    let anonymous = client.get(&pr_url).send().await.unwrap();
    assert_eq!(anonymous.status(), 401);

    let outsider = client
        .get(&pr_url)
        .bearer_auth(&outsider_token)
        .send()
        .await
        .unwrap();
    assert_eq!(outsider.status(), 403);

    let update = client
        .patch(&pr_url)
        .bearer_auth(&outsider_token)
        .json(&serde_json::json!({"title": "hijacked"}))
        .send()
        .await
        .unwrap();
    assert_eq!(update.status(), 403);

    let merge = client
        .post(format!("{pr_url}/merge"))
        .bearer_auth(&outsider_token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(merge.status(), 403);

    let owner_update = client
        .patch(&pr_url)
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"title": "owner edit"}))
        .send()
        .await
        .unwrap();
    assert_eq!(owner_update.status(), 200);
}

#[tokio::test]
async fn review_id_must_belong_to_the_pr_in_the_route() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "review-owner", "review-owner@example.com").await;
    let repo_one = create_private_repo(&base, &owner_token, "repo-one").await;
    let repo_two = create_private_repo(&base, &owner_token, "repo-two").await;
    let pr_one = insert_pr(&db, repo_one, owner_id, 1).await;
    insert_pr(&db, repo_two, owner_id, 1).await;

    let review = rg_db::ops::pr_review_ops::create(
        &db,
        rg_db::entities::pr_review::ActiveModel {
            id: sea_orm::NotSet,
            pr_id: Set(pr_one.id),
            repo_id: Set(repo_one),
            reviewer_id: Set(owner_id),
            action: Set("comment".to_string()),
            body: Set(Some("belongs to repo one".to_string())),
            commit_id: Set(None),
            created_at: Set(Utc::now()),
        },
    )
    .await
    .unwrap();

    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/review-owner/repo-two/pulls/1/reviews/{}",
            review.id
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn inline_comment_can_create_its_comment_review_implicitly() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "inline-owner", "inline-owner@example.com").await;
    let repo_id = create_repo_with_visibility(&base, &owner_token, "inline-comments", false).await;
    let pr = insert_pr(&db, repo_id, owner_id, 1).await;

    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/inline-owner/inline-comments/pulls/1/comments"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "path": "src/lib.rs",
            "line": 12,
            "side": "RIGHT",
            "body": "Please cover this branch"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let comment = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(comment["pr_id"], pr.id);
    assert_eq!(comment["path"], "src/lib.rs");
    assert_eq!(comment["line"], 12);

    let review = rg_db::ops::pr_review_ops::find_by_id(&db, comment["review_id"].as_i64().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(review.pr_id, pr.id);
    assert_eq!(review.action, "comment");
    assert_eq!(review.reviewer_id, owner_id);
}

#[tokio::test]
async fn creating_pr_requests_matching_codeowner_user() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db.clone(), repo_root.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let (owner_token, owner_id) =
        register_full(&base, "owners-owner", "owners-owner@example.com").await;
    let (reviewer_token, reviewer_id) =
        register_full(&base, "rust-reviewer", "rust-reviewer@example.com").await;
    let repo_id = create_repo_with_visibility(&base, &owner_token, "owned-code", false).await;

    let worktree = tempfile::tempdir().unwrap();
    let worktree_path = worktree.path();
    let bare_path = repo_root.join("owners-owner/owned-code.git");
    let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let worktree_arg = worktree_path.to_string_lossy();
    git.run(&["init", "--initial-branch=main", &worktree_arg], None)
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["config", "user.name", "Test Owner"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(
        &["config", "user.email", "owner@example.com"],
        Some(worktree_path),
    )
    .unwrap()
    .ensure_success()
    .unwrap();
    std::fs::create_dir_all(worktree_path.join(".github")).unwrap();
    std::fs::create_dir_all(worktree_path.join("src")).unwrap();
    std::fs::write(
        worktree_path.join(".github/CODEOWNERS"),
        "src/** @rust-reviewer\n",
    )
    .unwrap();
    std::fs::write(
        worktree_path.join("src/lib.rs"),
        "pub fn value() -> i32 { 1 }\n",
    )
    .unwrap();
    git.run(&["add", "."], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["commit", "-m", "base"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    let bare_arg = bare_path.to_string_lossy();
    git.run(&["remote", "add", "origin", &bare_arg], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["push", "origin", "main"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["checkout", "-b", "feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    std::fs::write(
        worktree_path.join("src/lib.rs"),
        "pub fn value() -> i32 { 2 }\n",
    )
    .unwrap();
    git.run(&["commit", "-am", "feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["push", "origin", "feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();

    let client = reqwest::Client::new();
    let pr = client
        .post(format!("{base}/api/v1/repos/owners-owner/owned-code/pulls"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "title": "Use the new value",
            "head": "feature",
            "base": "main"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pr.status(), 201, "{}", pr.text().await.unwrap());

    let reviewers = client
        .get(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/1/reviewers"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(reviewers.status(), 200);
    let reviewers = reviewers.json::<Vec<serde_json::Value>>().await.unwrap();
    assert_eq!(reviewers.len(), 1);
    assert_eq!(reviewers[0]["username"], "rust-reviewer");

    let protection = client
        .post(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/branches/protection"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "require_approval": true,
            "required_approvals": 1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(protection.status(), 201);

    let first_approval = client
        .post(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/1/reviews"
        ))
        .bearer_auth(&reviewer_token)
        .json(&serde_json::json!({"action": "approve", "body": "First revision looks good"}))
        .send()
        .await
        .unwrap();
    assert_eq!(first_approval.status(), 201);

    std::fs::write(
        worktree_path.join("src/lib.rs"),
        "pub fn value() -> i32 { 3 }\n",
    )
    .unwrap();
    git.run(&["commit", "-am", "updated feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["push", "origin", "feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    let new_head = git
        .run(&["rev-parse", "HEAD"], Some(worktree_path))
        .unwrap()
        .stdout_str()
        .trim()
        .to_string();
    let updated_prs =
        rg_db::ops::pull_request_ops::update_open_head_sha(&db, repo_id, "feature", &new_head)
            .await
            .unwrap();
    assert_eq!(updated_prs.len(), 1);
    let pr_id = updated_prs[0].id;
    assert!(
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr_id,)
            .await
            .is_err()
    );

    let enabled = client
        .put(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/1/auto-merge"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .unwrap();
    assert_eq!(enabled.status(), 200);
    assert_eq!(
        enabled.json::<serde_json::Value>().await.unwrap()["status"],
        "pending"
    );

    let current_approval = client
        .post(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/1/reviews"
        ))
        .bearer_auth(&reviewer_token)
        .json(&serde_json::json!({"action": "approve", "body": "Current revision looks good"}))
        .send()
        .await
        .unwrap();
    assert_eq!(current_approval.status(), 201);
    let merged = client
        .get(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/1"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(merged["state"], "merged");
    assert_eq!(merged["auto_merge_enabled"], false);

    git.run(&["fetch", "origin", "main"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(
        &["checkout", "-B", "queue-feature", "origin/main"],
        Some(worktree_path),
    )
    .unwrap()
    .ensure_success()
    .unwrap();
    std::fs::write(
        worktree_path.join("src/queued.rs"),
        "pub fn queued() -> bool {\n    let enabled = true;\n    // redundant\n    enabled\n}\n",
    )
    .unwrap();
    git.run(&["add", "."], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["commit", "-m", "queued change"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["push", "origin", "queue-feature"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();

    let queued_pr = client
        .post(format!("{base}/api/v1/repos/owners-owner/owned-code/pulls"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "title": "Queued change",
            "head": "queue-feature",
            "base": "main"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(queued_pr.status(), 201);

    let suggestion = client
        .post(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/2/comments"
        ))
        .bearer_auth(&reviewer_token)
        .json(&serde_json::json!({
            "path": "src/queued.rs",
            "start_line": 1,
            "start_side": "RIGHT",
            "line": 2,
            "side": "RIGHT",
            "body": "Make the queued behavior explicit",
            "suggestion": "pub fn queued() -> bool {\n    let enabled = false;"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(suggestion.status(), 201);
    let suggestion = suggestion.json::<serde_json::Value>().await.unwrap();
    let suggestion_id = suggestion["id"].as_i64().unwrap();

    let deletion = client
        .post(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/2/comments"
        ))
        .bearer_auth(&reviewer_token)
        .json(&serde_json::json!({
            "path": "src/queued.rs",
            "start_line": 3,
            "start_side": "RIGHT",
            "line": 3,
            "side": "RIGHT",
            "body": "Remove the body line",
            "suggestion": ""
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(deletion.status(), 201);
    let deletion = deletion.json::<serde_json::Value>().await.unwrap();
    let deletion_id = deletion["id"].as_i64().unwrap();
    let head_before_batch = git
        .run(&["rev-parse", "refs/heads/queue-feature"], Some(&bare_path))
        .unwrap();
    head_before_batch.ensure_success().unwrap();
    let head_before_batch = head_before_batch.stdout_str().trim().to_string();
    let overlap = client
        .post(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/2/comments"
        ))
        .bearer_auth(&reviewer_token)
        .json(&serde_json::json!({
            "path": "src/queued.rs",
            "start_line": 2,
            "start_side": "RIGHT",
            "line": 3,
            "side": "RIGHT",
            "body": "This overlaps the other suggestions",
            "suggestion": "    false"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(overlap.status(), 201);
    let overlap_id = overlap.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let rejected_overlap = client
        .post(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/2/suggestions/apply"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"comment_ids": [suggestion_id, overlap_id]}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected_overlap.status(), 400);
    assert!(rejected_overlap.text().await.unwrap().contains("overlap"));
    let unchanged_head = git
        .run(&["rev-parse", "refs/heads/queue-feature"], Some(&bare_path))
        .unwrap();
    unchanged_head.ensure_success().unwrap();
    assert_eq!(unchanged_head.stdout_str().trim(), head_before_batch);

    let applied = client
        .post(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/2/suggestions/apply"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"comment_ids": [suggestion_id, deletion_id]}))
        .send()
        .await
        .unwrap();
    assert_eq!(applied.status(), 200, "{}", applied.text().await.unwrap());
    let applied = applied.json::<serde_json::Value>().await.unwrap();
    assert_eq!(applied["comments"].as_array().unwrap().len(), 2);
    assert_eq!(applied["comments"][0]["id"], suggestion_id);
    assert_eq!(applied["comments"][1]["id"], deletion_id);
    assert_eq!(
        applied["comments"][0]["suggestion_commit_sha"],
        applied["commit_sha"]
    );
    let applied_content = git
        .run(
            &["show", "refs/heads/queue-feature:src/queued.rs"],
            Some(&bare_path),
        )
        .unwrap();
    applied_content.ensure_success().unwrap();
    assert_eq!(
        applied_content.stdout_str(),
        "pub fn queued() -> bool {\n    let enabled = false;\n    enabled\n}\n"
    );
    let batch_commit_count = git
        .run(
            &[
                "rev-list",
                "--count",
                &format!("{head_before_batch}..refs/heads/queue-feature"),
            ],
            Some(&bare_path),
        )
        .unwrap();
    batch_commit_count.ensure_success().unwrap();
    assert_eq!(batch_commit_count.stdout_str().trim(), "1");

    let already_applied = client
        .post(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/2/comments/{suggestion_id}/suggestion/apply"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(already_applied.status(), 400);

    let enqueue = client
        .put(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/2/merge-queue"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"strategy": "squash"}))
        .send()
        .await
        .unwrap();
    assert_eq!(enqueue.status(), 200);
    assert!(
        enqueue.json::<serde_json::Value>().await.unwrap()["process"]["waiting_reason"]
            .as_str()
            .unwrap()
            .contains("approval")
    );

    let queue = client
        .get(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/merge-queue"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0]["position"], 1);
    assert_eq!(queue[0]["pr_number"], 2);

    let queue_approval = client
        .post(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/2/reviews"
        ))
        .bearer_auth(&reviewer_token)
        .json(&serde_json::json!({"action": "approve", "body": "Ready from the queue"}))
        .send()
        .await
        .unwrap();
    assert_eq!(queue_approval.status(), 201);
    let queued_pr = client
        .get(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/2"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(queued_pr["state"], "merged");
    assert!(rg_db::ops::merge_queue_ops::list_by_repo(&db, repo_id)
        .await
        .unwrap()
        .is_empty());
    let timeline = client
        .get(format!(
            "{base}/api/v1/repos/owners-owner/owned-code/pulls/2/timeline"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(timeline.status(), 200);
    let timeline = timeline.json::<Vec<serde_json::Value>>().await.unwrap();
    let timeline_kinds = timeline
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect::<Vec<_>>();
    for expected in [
        "pull_request_opened",
        "code_suggestion",
        "suggestion_applied",
        "merge_queue_enqueued",
        "review_approve",
        "merge_queue_merged",
        "pull_request_merged",
    ] {
        assert!(timeline_kinds.contains(&expected), "missing {expected}");
    }
    assert!(timeline.windows(2).all(|events| {
        events[0]["created_at"].as_str().unwrap() <= events[1]["created_at"].as_str().unwrap()
    }));
    assert_eq!(timeline[0]["actor"]["username"], "owners-owner");

    let org = rg_db::ops::org_ops::create_org(
        &db,
        "code-org",
        Some("Code Org"),
        None,
        owner_id,
        "public",
    )
    .await
    .unwrap();
    let team = rg_db::ops::org_ops::create_team(&db, org.id, "reviewers", None, "write")
        .await
        .unwrap();
    rg_db::ops::org_ops::add_team_member(&db, team.id, reviewer_id, "member")
        .await
        .unwrap();
    git.run(&["fetch", "origin", "main"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(
        &["checkout", "-B", "main", "origin/main"],
        Some(worktree_path),
    )
    .unwrap()
    .ensure_success()
    .unwrap();
    std::fs::write(
        worktree_path.join(".github/CODEOWNERS"),
        "src/** @code-org/reviewers @rust-reviewer\n",
    )
    .unwrap();
    git.run(&["commit", "-am", "team codeowners"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();
    git.run(&["push", "origin", "main"], Some(worktree_path))
        .unwrap()
        .ensure_success()
        .unwrap();

    let team_pr = insert_pr(&db, repo_id, owner_id, 3).await;
    let mut repository = rg_db::ops::repo_ops::find_by_owner_and_name(&db, owner_id, "owned-code")
        .await
        .unwrap()
        .unwrap();
    repository.org_id = Some(org.id);
    let requested = rg_core::review::codeowners::request_codeowners(
        &db,
        &bare_path,
        "main",
        &["src/team_owned.rs".to_string()],
        &repository,
        team_pr.id,
        owner_id,
        owner_id,
    )
    .await
    .unwrap();
    assert_eq!(requested, ["rust-reviewer"]);
    assert_eq!(
        rg_db::ops::pr_reviewer_request_ops::list_by_pr(&db, team_pr.id)
            .await
            .unwrap()
            .len(),
        1
    );
    server.abort();
}
