//! The commit that applies review suggestions names whose lines it carries.
//!
//! The suggested text is the reviewer's; the click is the pull request
//! author's. The commit is authored by whoever clicked, and until
//! card_b3d513f83366 that was all it said — the reviewer was gone from the head
//! branch, and from the squash that later reads its commits' trailers. GitHub
//! writes one `Co-authored-by:` per suggestion author other than the person
//! applying them; so does this.

use sea_orm::{ActiveValue::NotSet, Set};

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

fn git(args: &[&str], cwd: &std::path::Path) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, Some(cwd)).unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

#[tokio::test]
async fn applied_suggestions_credit_every_other_suggestion_author_once() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db.clone(), repo_root.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let (owner_jwt, owner_id) = register_full(&base, "sugg-owner", "sugg-owner@example.com").await;
    let (reviewer_b_jwt, _) = register_full(&base, "reviewer-b", "reviewer-b@example.com").await;
    let (reviewer_c_jwt, _) = register_full(&base, "reviewer-c", "reviewer-c@example.com").await;
    let repo_id = crate::common::create_repo(&base, &owner_jwt, "sugg-co").await;
    let bare = repo_root.join("sugg-owner/sugg-co.git");

    let seeded = client
        .post(format!(
            "{base}/api/v1/repos/sugg-owner/sugg-co/contents/notes.md"
        ))
        .bearer_auth(&owner_jwt)
        .json(&serde_json::json!({
            "content": "one\ntwo\nthree\nfour\nfive\n",
            "message": "add notes",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(seeded.status(), 200, "seed write must succeed");
    let head_sha = seeded.json::<serde_json::Value>().await.unwrap()["commit_sha"]
        .as_str()
        .unwrap()
        .to_string();

    let now = chrono::Utc::now();
    rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("take the reviewers' wording".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(owner_id),
            reviewer_id: Set(None),
            head_branch: Set("main".to_string()),
            base_branch: Set("release".to_string()),
            head_sha: Set(Some(head_sha.clone())),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            base_sha: Set(None),
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
            original_author: NotSet,
            original_author_id: NotSet,
            original_service: NotSet,
        },
    )
    .await
    .expect("seed open PR");

    let suggest = |jwt: String, line: i64, text: &'static str, commit: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(format!(
                    "{base}/api/v1/repos/sugg-owner/sugg-co/pulls/1/comments"
                ))
                .bearer_auth(&jwt)
                .json(&serde_json::json!({
                    "path": "notes.md",
                    "line": line,
                    "side": "RIGHT",
                    "body": "reads better like this",
                    "suggestion": text,
                    "commit_id": commit,
                }))
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap();
            assert_eq!(status, 201, "suggestion comment: {body}");
            body["id"].as_i64().unwrap()
        }
    };
    let apply = |comment_ids: Vec<i64>| {
        let client = client.clone();
        let base = base.clone();
        let owner_jwt = owner_jwt.clone();
        async move {
            let response = client
                .post(format!(
                    "{base}/api/v1/repos/sugg-owner/sugg-co/pulls/1/suggestions/apply"
                ))
                .bearer_auth(&owner_jwt)
                .json(&serde_json::json!({ "comment_ids": comment_ids }))
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "apply: {body}");
        }
    };

    // Reviewer C first, then reviewer B twice, and the author's own
    // suggestion in between: C and B are named once each, in the order their
    // suggestions were chosen, and the author is not named at all.
    let from_c = suggest(reviewer_c_jwt.clone(), 1, "ONE", head_sha.clone()).await;
    let from_b = suggest(reviewer_b_jwt.clone(), 2, "TWO", head_sha.clone()).await;
    let own = suggest(owner_jwt.clone(), 3, "THREE", head_sha.clone()).await;
    let from_b_again = suggest(reviewer_b_jwt.clone(), 4, "FOUR", head_sha.clone()).await;
    apply(vec![from_c, from_b, own, from_b_again]).await;

    assert_eq!(
        git(&["log", "-1", "--format=%an <%ae>", "main"], &bare),
        "sugg-owner <sugg-owner@example.com>",
        "the commit stays the applier's"
    );
    assert_eq!(
        git(&["log", "-1", "--format=%B", "main"], &bare),
        "Apply 4 review suggestion(s)\n\n\
         Co-authored-by: reviewer-c <reviewer-c@example.com>\n\
         Co-authored-by: reviewer-b <reviewer-b@example.com>"
    );
    assert_eq!(
        git(
            &[
                "log",
                "-1",
                "--format=%(trailers:key=Co-authored-by,valueonly)",
                "main"
            ],
            &bare
        )
        .lines()
        .count(),
        2,
        "git must read both lines as trailers"
    );

    // The author applying only their own suggestion credits nobody else.
    let new_head = git(&["rev-parse", "main"], &bare);
    let own_only = suggest(owner_jwt.clone(), 5, "FIVE", new_head).await;
    apply(vec![own_only]).await;
    assert_eq!(
        git(&["log", "-1", "--format=%B", "main"], &bare),
        "Apply 1 review suggestion(s)"
    );

    server.abort();
}
