//! card_04088376105b: what an import brought says who wrote it on the source.
//!
//! The rows belong to the account that ran the import — `author_id` stays the
//! importer, for permissions and notifications alike — and the source's own
//! record of the writer rides beside it, so a moved project does not read as
//! one person talking to themselves.

use rg_db::sea_orm::{ActiveModelTrait, NotSet, Set};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

#[tokio::test]
async fn imported_issues_comments_pull_requests_and_reviews_carry_their_source_author() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, importer_id) = register_full(&base, "byline-host", "byline-host@example.com").await;
    let repo_id = create_repo(&base, &token, "moved").await;
    let now = chrono::Utc::now();

    let issue = rg_db::entities::issue::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        number: Set(7),
        title: Set("filed on GitHub".to_string()),
        body: Set(None),
        state: Set("open".to_string()),
        author_id: Set(importer_id),
        original_author: Set(Some("alice".to_string())),
        original_author_id: Set(Some(583231)),
        original_service: Set(Some("github".to_string())),
        assignee_id: Set(None),
        milestone_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        closed_at: Set(None),
        deleted_at: Set(None),
    }
    .insert(&db)
    .await
    .expect("seed the imported issue");
    rg_db::entities::issue_comment::ActiveModel {
        id: NotSet,
        issue_id: Set(issue.id),
        author_id: Set(importer_id),
        original_author: Set(Some("bob".to_string())),
        original_author_id: Set(Some(42)),
        original_service: Set(Some("github".to_string())),
        body: Set("a reply from somebody else".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db)
    .await
    .expect("seed the imported comment");

    let issue_json: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/byline-host/moved/issues/7"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read the issue")
        .json()
        .await
        .expect("issue body");
    assert_eq!(issue_json["author"], "byline-host", "{issue_json}");
    assert_eq!(issue_json["original_author"], "alice", "{issue_json}");
    assert_eq!(issue_json["original_service"], "github", "{issue_json}");

    let comments: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/byline-host/moved/issues/7/comments"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read the comments")
        .json()
        .await
        .expect("comments body");
    assert_eq!(comments.len(), 1, "{comments:?}");
    assert_eq!(comments[0]["original_author"], "bob");

    let pr = rg_db::entities::pull_request::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        number: Set(8),
        title: Set("opened on GitHub".to_string()),
        body: Set(None),
        state: Set("closed".to_string()),
        is_draft: Set(false),
        auto_merge_enabled: Set(false),
        auto_merge_strategy: Set(None),
        auto_merge_enabled_by_id: Set(None),
        auto_merge_enabled_at: Set(None),
        author_id: Set(importer_id),
        original_author: Set(Some("carol".to_string())),
        original_author_id: Set(Some(7)),
        original_service: Set(Some("gitlab".to_string())),
        reviewer_id: Set(None),
        head_branch: Set("feature".to_string()),
        base_branch: Set("main".to_string()),
        head_sha: Set(None),
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
        closed_at: Set(Some(now)),
        merged_at: Set(None),
    }
    .insert(&db)
    .await
    .expect("seed the imported pull request");
    rg_db::entities::pr_review::ActiveModel {
        id: NotSet,
        pr_id: Set(pr.id),
        repo_id: Set(repo_id),
        reviewer_id: Set(importer_id),
        original_author: Set(Some("dave".to_string())),
        original_author_id: Set(Some(9)),
        original_service: Set(Some("gitlab".to_string())),
        action: Set("comment".to_string()),
        body: Set(Some("reviewed on GitLab".to_string())),
        commit_id: Set(None),
        created_at: Set(now),
        dismissed_at: Set(None),
        dismissed_by: Set(None),
    }
    .insert(&db)
    .await
    .expect("seed the imported review");

    let pr_json: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/byline-host/moved/pulls/8"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read the pull request")
        .json()
        .await
        .expect("pull request body");
    assert_eq!(pr_json["original_author"], "carol", "{pr_json}");

    let timeline: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/byline-host/moved/pulls/8/timeline"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read the timeline")
        .json()
        .await
        .expect("timeline body");
    let byline = |kind: &str| {
        timeline
            .iter()
            .find(|event| event["kind"] == kind)
            .map(|event| event["original_author"].clone())
            .unwrap_or_else(|| panic!("no {kind} event in {timeline:?}"))
    };
    assert_eq!(
        byline("pull_request_opened"),
        serde_json::json!({ "login": "carol", "service": "gitlab" })
    );
    assert_eq!(
        byline("review_comment"),
        serde_json::json!({ "login": "dave", "service": "gitlab" })
    );
}

/// An imported verdict is history, not an authorization here: it was given on
/// another platform, on a commit nobody verified on this instance. A pull
/// request with no recorded head — what a GitLab import wrote before it kept
/// heads — used to count it, so a review an import brought could open the
/// merge, as the importer today and as whoever it is re-attributed to later.
#[tokio::test]
async fn an_imported_approval_authorizes_nothing() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, importer_id) =
        register_full(&base, "verdict-host", "verdict-host@example.com").await;
    let (_, reviewer_id) =
        register_full(&base, "verdict-reviewer", "verdict-reviewer@example.com").await;
    let repo_id = create_repo(&base, &token, "verdicts").await;
    let now = chrono::Utc::now();
    let pr = rg_db::entities::pull_request::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        number: Set(1),
        title: Set("imported without a head".to_string()),
        body: Set(None),
        state: Set("open".to_string()),
        is_draft: Set(false),
        auto_merge_enabled: Set(false),
        auto_merge_strategy: Set(None),
        auto_merge_enabled_by_id: Set(None),
        auto_merge_enabled_at: Set(None),
        author_id: Set(importer_id),
        original_author: Set(None),
        original_author_id: Set(None),
        original_service: Set(None),
        reviewer_id: Set(None),
        head_branch: Set("feature".to_string()),
        base_branch: Set("main".to_string()),
        head_sha: Set(None),
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
    }
    .insert(&db)
    .await
    .expect("seed the pull request");
    let review = |reviewer: i64, imported: bool| rg_db::entities::pr_review::ActiveModel {
        id: NotSet,
        pr_id: Set(pr.id),
        repo_id: Set(repo_id),
        reviewer_id: Set(reviewer),
        original_author: Set(imported.then(|| "gitlab-reviewer".to_string())),
        original_author_id: Set(imported.then_some(77)),
        original_service: Set(imported.then(|| "gitlab".to_string())),
        action: Set("approve".to_string()),
        body: Set(None),
        commit_id: Set(None),
        created_at: Set(now),
        dismissed_at: Set(None),
        dismissed_by: Set(None),
    };
    review(importer_id, true)
        .insert(&db)
        .await
        .expect("seed the imported approval");
    review(reviewer_id, false)
        .insert(&db)
        .await
        .expect("seed an approval given here");

    let approvers: Vec<i64> = rg_db::ops::pr_review_ops::current_approvers(&db, pr.id, None)
        .await
        .expect("read the approvers")
        .into_iter()
        .map(|user| user.id)
        .collect();
    assert_eq!(
        approvers,
        [reviewer_id],
        "the approval given here counts, the imported one does not"
    );
}
