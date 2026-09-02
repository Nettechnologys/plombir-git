//! card_9903905d92a3: the suggestion rule in `create_review_comment` was a
//! composite `if` — `reply_to_id.is_some() || line.is_none() || side != RIGHT`
//! — followed, one line later, by `line.unwrap()`. The two were independent
//! statements about the same caller-supplied payload, so the day either one
//! moved, a review comment posted from the web UI would end the process instead
//! of answering `400`.
//!
//! The rule now binds the check to the line it yields. These tests pin what the
//! boundary answers on each way of breaking it, so a future edit to the pattern
//! cannot quietly widen what reaches the row.

use rg_db::sea_orm::{NotSet, Set};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

const HEAD_SHA: &str = "5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";

async fn seed_pr(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    author_id: i64,
) -> rg_db::entities::pull_request::Model {
    let now = chrono::Utc::now();
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("A suggestion needs a line to attach to".to_string()),
            body: Set(None),
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
            head_sha: Set(Some(HEAD_SHA.to_string())),
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
    .expect("seed pull request")
}

#[tokio::test]
async fn a_suggestion_without_a_right_side_line_is_a_typed_refusal() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, author_id) =
        register_full(&base, "suggestion-host", "suggestion-host@example.com").await;
    let repo_id = create_repo(&base, &token, "suggestion-guard").await;
    seed_pr(&db, repo_id, author_id).await;

    let url = format!("{base}/api/v1/repos/suggestion-host/suggestion-guard/pulls/1/comments");
    let client = reqwest::Client::new();

    // The three ways the rule can be broken, each of which used to be one `||`
    // arm away from the `line.unwrap()` below it.
    for (case, payload) in [
        (
            "a suggestion with no line at all",
            serde_json::json!({
                "path": "src/main.rs",
                "body": "use the shorter form",
                "side": "RIGHT",
                "suggestion": "let x = 1;\n",
            }),
        ),
        (
            "a suggestion on the deleted side",
            serde_json::json!({
                "path": "src/main.rs",
                "body": "use the shorter form",
                "line": 4,
                "side": "LEFT",
                "suggestion": "let x = 1;\n",
            }),
        ),
        (
            "a suggestion with no side named at all",
            serde_json::json!({
                "path": "src/main.rs",
                "body": "use the shorter form",
                "line": 4,
                "suggestion": "let x = 1;\n",
            }),
        ),
    ] {
        let resp = client
            .post(&url)
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert_eq!(
            status.as_u16(),
            400,
            "{case} must be a client error, got {status} (body: {body})"
        );
        assert_eq!(
            body["error"]["message"], "suggestions require a top-level RIGHT-side line comment",
            "{case} must keep the rule's own wording: {body}"
        );
    }

    // The range rule sits behind the same pattern and answers separately, so a
    // fix that collapsed the two would show up here.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "path": "src/main.rs",
            "body": "use the shorter form",
            "line": 4,
            "start_line": 9,
            "side": "RIGHT",
            "suggestion": "let x = 1;\n",
        }))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(status.as_u16(), 400, "body: {body}");
    assert_eq!(
        body["error"]["message"], "suggestion range must be an ordered RIGHT-side range",
        "an inverted range keeps its own answer: {body}"
    );

    // And the shape the rule exists to admit is still accepted, so the
    // assertions above are about the payload and not about the endpoint.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "path": "src/main.rs",
            "body": "use the shorter form",
            "line": 4,
            "side": "RIGHT",
            "suggestion": "let x = 1;\n",
        }))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert_eq!(
        status.as_u16(),
        201,
        "a well-formed suggestion must still be accepted: {body}"
    );
}
