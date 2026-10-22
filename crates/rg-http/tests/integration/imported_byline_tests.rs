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
    // card_f6c67a05c6b0: who it was on at the source, as bylines.
    use rg_db::ops::imported_assignment_ops::{record, AssignedObject, SourcePerson};
    let person = |login: &'static str, id: i64, service: &'static str| SourcePerson {
        login,
        source_user_id: Some(id),
        service,
    };
    record(
        &db,
        AssignedObject::Issue(issue.id),
        "assignee",
        &[person("erin", 11, "github")],
    )
    .await
    .expect("seed the issue's source assignee");
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
    assert_eq!(
        issue_json["original_assignees"],
        serde_json::json!([{ "login": "erin", "service": "github" }]),
        "{issue_json}"
    );

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
    record(
        &db,
        AssignedObject::PullRequest(pr.id),
        "assignee",
        &[person("frank", 12, "gitlab")],
    )
    .await
    .expect("seed the pull request's source assignee");
    record(
        &db,
        AssignedObject::PullRequest(pr.id),
        "requested_reviewer",
        &[person("grace", 13, "gitlab")],
    )
    .await
    .expect("seed the pull request's requested reviewer");
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
    // A comment on a line of the diff, written in that review.
    let imported_review_id = {
        use rg_db::sea_orm::EntityTrait;
        rg_db::entities::pr_review::Entity::find()
            .all(&db)
            .await
            .expect("read the review back")
            .into_iter()
            .find(|review| review.pr_id == pr.id)
            .expect("the imported review")
            .id
    };
    rg_db::entities::review_comment::ActiveModel {
        id: NotSet,
        review_id: Set(imported_review_id),
        pr_id: Set(pr.id),
        author_id: Set(importer_id),
        path: Set("src/lib.rs".to_string()),
        position: Set(None),
        line: Set(Some(3)),
        start_line: Set(None),
        side: Set(Some("RIGHT".to_string())),
        start_side: Set(None),
        body: Set("on this line".to_string()),
        suggestion: Set(None),
        suggestion_applied_at: Set(None),
        suggestion_applied_by_id: Set(None),
        suggestion_commit_sha: Set(None),
        commit_id: Set(None),
        reply_to_id: Set(None),
        resolved_at: Set(None),
        resolved_by_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db)
    .await
    .expect("seed the imported diff comment");

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
    assert_eq!(
        (
            &pr_json["original_assignees"],
            &pr_json["original_reviewers"]
        ),
        (
            &serde_json::json!([{ "login": "frank", "service": "gitlab" }]),
            &serde_json::json!([{ "login": "grace", "service": "gitlab" }])
        ),
        "{pr_json}"
    );

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
    let on_the_line = timeline
        .iter()
        .find(|event| {
            event["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("comment:"))
        })
        .unwrap_or_else(|| panic!("no diff comment in {timeline:?}"));
    assert_eq!(
        on_the_line["original_author"],
        serde_json::json!({ "login": "dave", "service": "gitlab" }),
        "a diff comment is written by whoever wrote its review"
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

mod reattribution {
    use rg_db::sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, NotSet, QueryFilter, Set};

    use crate::common::{create_repo, register_full, spawn_test_app_with_db};

    const GITHUB_ID: i64 = 583231;

    async fn link_github(db: &rg_db::DatabaseConnection, user_id: i64) {
        let now = chrono::Utc::now();
        // Every instance carries the built-in GitHub sign-in row; the link
        // below names it by slug, as a real sign-in would.
        let provider = rg_db::ops::sso_provider_ops::find_by_slug(db, "github")
            .await
            .expect("read the sign-in providers")
            .expect("the built-in GitHub sign-in provider");
        assert_ne!(
            provider.provider_type, "oidc",
            "the built-in `github` sign-in asks api.github.com"
        );
        rg_db::entities::oauth_account::ActiveModel {
            id: NotSet,
            provider: Set("github".to_string()),
            provider_user_id: Set(GITHUB_ID.to_string()),
            provider_username: Set("alice-on-github".to_string()),
            email: Set("alice@example.com".to_string()),
            user_id: Set(user_id),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .expect("link alice's GitHub account");
    }

    /// An imported issue in `repo_id` written by `service` account `GITHUB_ID`.
    async fn imported_issue(
        db: &rg_db::DatabaseConnection,
        repo_id: i64,
        number: i64,
        importer_id: i64,
        service: &str,
    ) -> rg_db::entities::issue::Model {
        let now = chrono::Utc::now();
        rg_db::entities::issue::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(number),
            title: Set(format!("imported #{number}")),
            body: Set(None),
            state: Set("open".to_string()),
            author_id: Set(importer_id),
            original_author: Set(Some("alice-on-github".to_string())),
            original_author_id: Set(Some(GITHUB_ID)),
            original_service: Set(Some(service.to_string())),
            assignee_id: Set(None),
            milestone_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            deleted_at: Set(None),
        }
        .insert(db)
        .await
        .expect("seed an imported issue")
    }

    async fn authors(db: &rg_db::DatabaseConnection, repo_id: i64) -> Vec<(i64, i64)> {
        let mut rows: Vec<(i64, i64)> = rg_db::entities::issue::Entity::find()
            .filter(rg_db::entities::issue::Column::RepoId.eq(repo_id))
            .all(db)
            .await
            .unwrap()
            .into_iter()
            .map(|issue| (issue.number, issue.author_id))
            .collect();
        rows.sort_unstable();
        rows
    }

    /// card_2a006cf365f6: what a GitHub sign-in vouches the account wrote
    /// comes back to it — after it is shown and asked for, in repositories it
    /// can read, and only for github.com ids.
    #[tokio::test]
    async fn a_confirmed_claim_moves_exactly_what_the_sign_in_vouches_for() {
        let (base, db) = spawn_test_app_with_db().await;
        let (importer_token, importer_id) =
            register_full(&base, "claim-importer", "claim-importer@example.com").await;
        let (alice_token, alice_id) =
            register_full(&base, "claim-alice", "claim-alice@example.com").await;
        link_github(&db, alice_id).await;

        let public_id = create_repo(&base, &importer_token, "moved-public").await;
        let private: serde_json::Value = reqwest::Client::new()
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(&importer_token)
            .json(&serde_json::json!({ "name": "moved-private", "is_private": true }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let private_id = private["id"].as_i64().expect("private repository id");

        let issue = imported_issue(&db, public_id, 1, importer_id, "github").await;
        // The same account id on a GitHub Enterprise server is somebody else.
        imported_issue(&db, public_id, 2, importer_id, "github:ghe.example.com").await;
        imported_issue(&db, private_id, 1, importer_id, "github").await;
        let now = chrono::Utc::now();
        rg_db::entities::issue_comment::ActiveModel {
            id: NotSet,
            issue_id: Set(issue.id),
            author_id: Set(importer_id),
            original_author: Set(Some("alice-on-github".to_string())),
            original_author_id: Set(Some(GITHUB_ID)),
            original_service: Set(Some("github".to_string())),
            body: Set("alice's reply on GitHub".to_string()),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db)
        .await
        .unwrap();

        let client = reqwest::Client::new();
        let listed: Vec<serde_json::Value> = client
            .get(format!("{base}/api/v1/users/me/imported-authorship"))
            .bearer_auth(&alice_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            listed.len(),
            1,
            "only the repository alice can read: {listed:?}"
        );
        assert_eq!(listed[0]["repository"], "claim-importer/moved-public");
        assert_eq!(listed[0]["service"], "github");
        assert_eq!(listed[0]["login"], "alice-on-github");
        assert_eq!(
            listed[0]["issues"], 1,
            "the GitHub Enterprise issue is not hers"
        );
        assert_eq!(listed[0]["comments"], 1);
        assert_eq!(
            authors(&db, public_id).await,
            [(1, importer_id), (2, importer_id)],
            "listing moves nothing"
        );

        let refused = client
            .post(format!("{base}/api/v1/users/me/imported-authorship/claim"))
            .bearer_auth(&alice_token)
            .json(&serde_json::json!({ "repositories": [private_id] }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            refused.status(),
            404,
            "a repository she cannot read is not there"
        );
        assert_eq!(authors(&db, private_id).await, [(1, importer_id)]);

        let claimed: serde_json::Value = client
            .post(format!("{base}/api/v1/users/me/imported-authorship/claim"))
            .bearer_auth(&alice_token)
            .json(&serde_json::json!({ "repositories": [public_id] }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(claimed["issues"], 1, "{claimed}");
        assert_eq!(claimed["comments"], 1, "{claimed}");
        assert_eq!(
            authors(&db, public_id).await,
            [(1, alice_id), (2, importer_id)],
            "her github.com issue is hers; the GitHub Enterprise one stays"
        );
        let comment = rg_db::entities::issue_comment::Entity::find()
            .filter(rg_db::entities::issue_comment::Column::IssueId.eq(issue.id))
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(comment.author_id, alice_id);
        assert_eq!(
            comment.original_author.as_deref(),
            Some("alice-on-github"),
            "the byline stays"
        );

        let audit = rg_db::entities::audit_log::Entity::find()
            .filter(rg_db::entities::audit_log::Column::Action.eq("import.authorship_claimed"))
            .all(&db)
            .await
            .unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].user_id, Some(alice_id));

        let after: Vec<serde_json::Value> = client
            .get(format!("{base}/api/v1/users/me/imported-authorship"))
            .bearer_auth(&alice_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(after.is_empty(), "nothing left to claim: {after:?}");
    }

    /// An OIDC provider is whatever issuer its discovery URL names, whatever
    /// its slug says: its ids are not github.com's, so a link through it
    /// vouches for nothing even under the slug `github`.
    #[tokio::test]
    async fn an_oidc_provider_named_github_vouches_for_nothing() {
        let (base, db) = spawn_test_app_with_db().await;
        let (importer_token, importer_id) =
            register_full(&base, "oidc-importer", "oidc-importer@example.com").await;
        let (token, alice_id) = register_full(&base, "oidc-alice", "oidc-alice@example.com").await;
        link_github(&db, alice_id).await;
        let provider = rg_db::ops::sso_provider_ops::find_by_slug(&db, "github")
            .await
            .unwrap()
            .unwrap();
        let mut active: rg_db::entities::sso_provider::ActiveModel = provider.into();
        active.provider_type = Set("oidc".to_string());
        active.discovery_url = Set(Some("https://idp.example.com".to_string()));
        active
            .update(&db)
            .await
            .expect("turn the provider into OIDC");
        let repo_id = create_repo(&base, &importer_token, "moved").await;
        imported_issue(&db, repo_id, 1, importer_id, "github").await;

        let listed: Vec<serde_json::Value> = reqwest::Client::new()
            .get(format!("{base}/api/v1/users/me/imported-authorship"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(listed.is_empty(), "{listed:?}");
    }

    /// Without a sign-in to vouch for an account id there is nothing to claim,
    /// however the logins read.
    #[tokio::test]
    async fn without_a_linked_sign_in_nothing_is_claimable() {
        let (base, db) = spawn_test_app_with_db().await;
        let (importer_token, importer_id) =
            register_full(&base, "noclaim-importer", "noclaim-importer@example.com").await;
        let (token, _) = register_full(&base, "alice-on-github", "aog@example.com").await;
        let repo_id = create_repo(&base, &importer_token, "moved").await;
        imported_issue(&db, repo_id, 1, importer_id, "github").await;

        let listed: Vec<serde_json::Value> = reqwest::Client::new()
            .get(format!("{base}/api/v1/users/me/imported-authorship"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            listed.is_empty(),
            "a local login that spells the source login proves nothing: {listed:?}"
        );
        let claimed: serde_json::Value = reqwest::Client::new()
            .post(format!("{base}/api/v1/users/me/imported-authorship/claim"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "repositories": [repo_id] }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(claimed["issues"], 0);
        assert_eq!(authors(&db, repo_id).await, [(1, importer_id)]);
    }
}
