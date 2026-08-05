//! Integration tests for Issues, Labels, and Milestones.
//!
//! Guards:
//!   POST   /repos/:o/:r/issues             — create issue
//!   GET    /repos/:o/:r/issues             — list issues (state + label filters)
//!   PATCH  /repos/:o/:r/issues/:n          — update issue (close/reopen)
//!   POST   /repos/:o/:r/issues/:n/comments — add comment
//!   GET    /repos/:o/:r/issues/:n/comments — list comments
//!   POST   /repos/:o/:r/labels             — create label
//!   GET    /repos/:o/:r/labels             — list labels
//!   POST   /repos/:o/:r/milestones         — create milestone
//!   GET    /repos/:o/:r/milestones         — list milestones

use crate::common::{create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

async fn setup(suffix: &str) -> (String, String, String, String) {
    let base = spawn_test_app().await;
    let owner = format!("issueuser{suffix}");
    let token = register_user(&base, &owner, &format!("issueuser{suffix}@example.com"), PW).await;
    let repo = format!("issuerepo{suffix}");
    create_repo(&base, &token, &repo).await;
    (base, token, owner, repo)
}

// ── Issues ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_and_list_issues() {
    let (base, token, owner, repo) = setup("1").await;
    let client = reqwest::Client::new();
    let mut first_number = None;

    for title in &[
        "Bug: crash on startup",
        "Feature: dark mode",
        "Docs: update readme",
    ] {
        let resp = client
            .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
            .bearer_auth(&token)
            .json(&serde_json::json!({"title": title}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "create issue '{}' failed", title);
        let created: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(created["author"], owner);
        first_number.get_or_insert_with(|| created["number"].as_i64().unwrap());
    }

    let resp = client
        .get(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let issues = body["data"].as_array().unwrap();
    assert_eq!(issues.len(), 3);
    assert!(issues.iter().all(|issue| issue["author"] == owner));

    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues/{}",
            base,
            owner,
            repo,
            first_number.unwrap()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let issue: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(issue["author"], owner);
}

#[tokio::test]
async fn test_close_and_reopen_issue() {
    let (base, token, owner, repo) = setup("2").await;
    let client = reqwest::Client::new();

    let issue: serde_json::Value = client
        .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "Close me"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let number = issue["number"].as_i64().unwrap();

    // Close
    let resp = client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/issues/{}",
            base, owner, repo, number
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "closed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let updated: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(updated["state"], "closed");

    // Reopen
    let resp = client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/issues/{}",
            base, owner, repo, number
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "open"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let reopened: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(reopened["state"], "open");
}

#[tokio::test]
async fn test_issue_state_filter() {
    let (base, token, owner, repo) = setup("3").await;
    let client = reqwest::Client::new();

    // Create 3 issues
    let mut numbers = vec![];
    for i in 1..=3 {
        let issue: serde_json::Value = client
            .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
            .bearer_auth(&token)
            .json(&serde_json::json!({"title": format!("Issue {}", i)}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        numbers.push(issue["number"].as_i64().unwrap());
    }

    // Close the first one
    client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/issues/{}",
            base, owner, repo, numbers[0]
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "closed"}))
        .send()
        .await
        .unwrap();

    // List open — should be 2
    let body: serde_json::Value = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues?state=open",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["data"].as_array().unwrap().len(),
        2,
        "should have 2 open issues"
    );

    // List closed — should be 1
    let body: serde_json::Value = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues?state=closed",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["data"].as_array().unwrap().len(),
        1,
        "should have 1 closed issue"
    );
}

#[tokio::test]
async fn test_issue_comments() {
    let (base, token, owner, repo) = setup("4").await;
    let client = reqwest::Client::new();

    let issue: serde_json::Value = client
        .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "Commented issue"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let number = issue["number"].as_i64().unwrap();

    // Add two comments
    for body_text in &["First comment", "Second comment"] {
        let resp = client
            .post(format!(
                "{}/api/v1/repos/{}/{}/issues/{}/comments",
                base, owner, repo, number
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"body": body_text}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "add comment failed: {}", resp.status());
    }

    // List comments
    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues/{}/comments",
            base, owner, repo, number
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let comments: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(comments.as_array().unwrap().len(), 2);
}

// ── Labels ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_and_list_labels() {
    let (base, token, owner, repo) = setup("5").await;
    let client = reqwest::Client::new();

    for (name, color) in &[
        ("bug", "#ee0701"),
        ("enhancement", "#84b6eb"),
        ("docs", "#0075ca"),
    ] {
        let resp = client
            .post(format!("{}/api/v1/repos/{}/{}/labels", base, owner, repo))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": name, "color": color}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "create label '{}' failed", name);
    }

    let resp = client
        .get(format!("{}/api/v1/repos/{}/{}/labels", base, owner, repo))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let labels: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(labels.as_array().unwrap().len(), 3);
}

/// The label filter and the `total` it reports have to answer the same
/// question.
///
/// Before the fix `total` was counted from the labels alone while the page was
/// thinned afterwards by `state` and `repo_id`, so `?labels=bug&state=closed`
/// answered with one row and `total: 3` — `total_pages` computed from a
/// predicate the page had never been through. And an unknown label was dropped
/// from the AND instead of refused, so `?labels=bug,typo` quietly answered the
/// narrower question `?labels=bug`.
#[tokio::test]
async fn label_filter_counts_the_same_issues_it_returns() {
    let (base, token, owner, repo) = setup("8").await;
    let client = reqwest::Client::new();

    for name in &["bug", "urgent"] {
        let resp = client
            .post(format!("{}/api/v1/repos/{}/{}/labels", base, owner, repo))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": name, "color": "#ee0701"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "create label '{name}' failed");
    }

    let mut numbers = vec![];
    for (title, labels) in &[
        ("stays open", vec!["bug"]),
        ("gets closed", vec!["bug"]),
        ("both labels", vec!["bug", "urgent"]),
    ] {
        let issue: serde_json::Value = client
            .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
            .bearer_auth(&token)
            .json(&serde_json::json!({"title": title, "labels": labels}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        numbers.push(issue["number"].as_i64().unwrap());
    }

    client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/issues/{}",
            base, owner, repo, numbers[1]
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "closed"}))
        .send()
        .await
        .unwrap();

    let list = |query: String| {
        let client = client.clone();
        let url = format!("{}/api/v1/repos/{}/{}/issues?{}", base, owner, repo, query);
        async move { client.get(url).send().await.unwrap() }
    };

    // The headline case: one closed issue carries the label, and `total` has to
    // say one — not three.
    let body: serde_json::Value = list("labels=bug&state=closed".into())
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["data"].as_array().unwrap().len(),
        1,
        "one closed issue carries the label"
    );
    assert_eq!(
        body["pagination"]["total"], 1,
        "total must count the closed labelled issues, not every labelled issue"
    );
    assert_eq!(body["pagination"]["total_pages"], 1);

    // All three carry `bug`, and the open pair is two of them.
    let body: serde_json::Value = list("labels=bug".into()).await.json().await.unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 3);
    assert_eq!(body["pagination"]["total"], 3);

    let body: serde_json::Value = list("labels=bug&state=open".into())
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 2);
    assert_eq!(body["pagination"]["total"], 2);

    // A page cut short by the page size still reports the whole total. That
    // case is covered at the query level
    // (`a_limited_page_still_reports_the_whole_matching_total` in
    // `rg-db::ops::issue_label_ops`) rather than here, because `?per_page=N` is
    // itself rejected with a 400 on every route that flattens
    // `PaginationParams` — a separate defect, tracked on its own card.

    // The AND over two labels really is an intersection.
    let body: serde_json::Value = list("labels=bug,urgent".into()).await.json().await.unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert_eq!(body["pagination"]["total"], 1);
    assert_eq!(body["data"][0]["title"], "both labels");

    // A repeated label is one condition, not an intersection that cannot match.
    let body: serde_json::Value = list("labels=bug,bug".into()).await.json().await.unwrap();
    assert_eq!(
        body["pagination"]["total"], 3,
        "a repeated label must not turn into an unsatisfiable AND"
    );

    // An unknown label is refused by name instead of being dropped from the AND.
    let resp = list("labels=bug,typo".into()).await;
    assert_eq!(
        resp.status(),
        400,
        "an unknown label must not silently weaken the filter"
    );
    let error: serde_json::Value = resp.json().await.unwrap();
    assert!(
        error.to_string().contains("typo"),
        "the refusal must name the label the repo does not have, got: {error}"
    );
}

/// Writing labels has to mean the same thing as reading them.
///
/// The write path filtered the repo's labels by `names.contains(&label.name)`,
/// so a name the repo did not have simply produced no row: `POST` answered
/// `201` with `labels: ["bug", "typo"]` echoed from the denormalised JSON
/// column while `issue_labels` held one row. The same name was a `400` on
/// `GET /issues?labels=typo` — legal on the way in, unknown on the way out.
///
/// The junction is what the label filter reads, so this test asks the filter,
/// not the JSON column: an issue is only labelled if `?labels=…` finds it.
#[tokio::test]
async fn writing_an_unknown_label_is_refused_instead_of_dropped() {
    let (base, token, owner, repo) = setup("9").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/repos/{}/{}/labels", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "bug", "color": "#ee0701"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create label 'bug' failed");

    // ── create ────────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "half-labelled", "labels": ["bug", "typo"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        400,
        "creating with an unknown label must be refused, not silently narrowed"
    );
    let error: serde_json::Value = resp.json().await.unwrap();
    assert!(
        error.to_string().contains("typo"),
        "the refusal must name the label the repo does not have, got: {error}"
    );

    // Refused before the insert: no issue was left behind.
    let body: serde_json::Value = client
        .get(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        body["data"].as_array().unwrap().is_empty(),
        "a refused create must not leave an issue behind, got: {body}"
    );

    // A create whose labels all exist writes both stores.
    let created: serde_json::Value = client
        .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "properly labelled", "labels": ["bug"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let number = created["number"].as_i64().unwrap();

    let found: serde_json::Value = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues?labels=bug",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        found["data"].as_array().unwrap().len(),
        1,
        "the label the response listed must be the label the filter finds, got: {found}"
    );

    // ── update ────────────────────────────────────────────────────────────
    let resp = client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/issues/{}",
            base, owner, repo, number
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"labels": ["bug", "typo"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        400,
        "updating with an unknown label must be refused too"
    );
    let error: serde_json::Value = resp.json().await.unwrap();
    assert!(
        error.to_string().contains("typo"),
        "the refusal must name the label, got: {error}"
    );

    // The refused edit changed nothing: the issue still carries `bug`.
    let found: serde_json::Value = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues?labels=bug",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        found["data"].as_array().unwrap().len(),
        1,
        "a refused update must not drop the labels the issue already had, got: {found}"
    );

    // And a legal edit reaches the junction, not just the JSON column.
    let resp = client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/issues/{}",
            base, owner, repo, number
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"labels": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let found: serde_json::Value = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/issues?labels=bug",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        found["data"].as_array().unwrap().is_empty(),
        "clearing the labels must clear the junction as well, got: {found}"
    );
}

/// The issue response must be projected from the junction. If it ever reads a
/// stored name copy again, rename leaves the old badge behind and delete leaves
/// a ghost badge even though the label filter follows the normalized rows.
#[tokio::test]
async fn issue_responses_follow_label_rename_and_delete() {
    let (base, token, owner, repo) = setup("10").await;
    let client = reqwest::Client::new();

    let label: serde_json::Value = client
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/labels"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "old-name", "color": "#ee0701"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let label_id = label["id"].as_i64().unwrap();

    let issue: serde_json::Value = client
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/issues"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "rename me", "labels": ["old-name"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let number = issue["number"].as_i64().unwrap();

    let rename = client
        .patch(format!(
            "{base}/api/v1/repos/{owner}/{repo}/labels/{label_id}"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "new-name"}))
        .send()
        .await
        .unwrap();
    assert_eq!(rename.status(), 200);

    let renamed: serde_json::Value = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{number}"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let labels: Vec<String> = serde_json::from_str(renamed["labels"].as_str().unwrap()).unwrap();
    assert_eq!(labels, vec!["new-name"]);

    let delete = client
        .delete(format!(
            "{base}/api/v1/repos/{owner}/{repo}/labels/{label_id}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(delete.status(), 204);

    let deleted: serde_json::Value = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{number}"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        deleted["labels"].is_null(),
        "deleted label survived in issue response: {deleted}"
    );
}

// ── Milestones ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_and_list_milestones() {
    let (base, token, owner, repo) = setup("6").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/milestones",
            base, owner, repo
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "title": "v1.0",
            "description": "First stable release",
            "due_date": "2026-12-31"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "create milestone failed: {}",
        resp.status()
    );
    let ms: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(ms["title"], "v1.0");

    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/milestones",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mss: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(mss.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_update_and_delete_milestone() {
    let (base, token, owner, repo) = setup("7").await;
    let client = reqwest::Client::new();

    let ms: serde_json::Value = client
        .post(format!(
            "{}/api/v1/repos/{}/{}/milestones",
            base, owner, repo
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "beta"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ms_id = ms["id"].as_i64().unwrap();

    // Update
    let resp = client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/milestones/{}",
            base, owner, repo, ms_id
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"state": "closed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let updated: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(updated["state"], "closed");

    // Delete
    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/milestones/{}",
            base, owner, repo, ms_id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "delete milestone failed: {}",
        resp.status()
    );
}
