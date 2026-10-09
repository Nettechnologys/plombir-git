//! card_60961272e1ba: what was written into an issue or a pull request can be
//! taken back — a comment by its author or a repository administrator, an issue
//! by an administrator — and what was deleted is gone from every reader.

use std::path::Path;

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

async fn api(
    method: reqwest::Method,
    url: String,
    token: &str,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut request = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::json!(text)),
    )
}

fn git(cwd: &Path, args: &[&str]) {
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .unwrap()
        .run_with_env(
            args,
            Some(cwd),
            &[
                ("GIT_AUTHOR_NAME", "mod"),
                ("GIT_AUTHOR_EMAIL", "mod@example.com"),
                ("GIT_COMMITTER_NAME", "mod"),
                ("GIT_COMMITTER_EMAIL", "mod@example.com"),
            ],
        )
        .unwrap();
    assert!(
        output.success(),
        "git {args:?}: {}{}",
        output.stdout_str(),
        output.stderr_str()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn comments_and_issues_are_edited_and_deleted_by_whom_they_belong_to() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, "cm_owner", "cm_owner@example.com").await;
    let (alice, _) = register_full(&base, "cm_alice", "cm_alice@example.com").await;
    let (bob, _) = register_full(&base, "cm_bob", "cm_bob@example.com").await;
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        &owner,
        Some(serde_json::json!({ "name": "proj", "auto_init": true, "readme": "default" })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let repo = format!("{base}/api/v1/repos/cm_owner/proj");

    let (status, issue) = api(
        reqwest::Method::POST,
        format!("{repo}/issues"),
        &alice,
        Some(serde_json::json!({ "title": "zebracorn sighting", "body": "details" })),
    )
    .await;
    assert_eq!(status, 201, "{issue}");
    let number = issue["number"].as_i64().unwrap();
    let mut comment_ids = Vec::new();
    for (who, text) in [(&alice, "alice wrote this"), (&bob, "bob pasted a secret")] {
        let (status, comment) = api(
            reqwest::Method::POST,
            format!("{repo}/issues/{number}/comments"),
            who,
            Some(serde_json::json!({ "body": text })),
        )
        .await;
        assert_eq!(status, 201, "{comment}");
        comment_ids.push(comment["id"].as_i64().unwrap());
    }
    let (alice_comment, bob_comment) = (comment_ids[0], comment_ids[1]);

    // Edit: somebody else's comment is refused; one's own is edited.
    let (status, body) = api(
        reqwest::Method::PATCH,
        format!("{repo}/issues/comments/{alice_comment}"),
        &bob,
        Some(serde_json::json!({ "body": "rewritten by bob" })),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let (status, edited) = api(
        reqwest::Method::PATCH,
        format!("{repo}/issues/comments/{alice_comment}"),
        &alice,
        Some(serde_json::json!({ "body": "alice fixed a typo" })),
    )
    .await;
    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["body"], "alice fixed a typo");
    assert_ne!(
        edited["updated_at"], edited["created_at"],
        "the edit is marked"
    );
    let (status, body) = api(
        reqwest::Method::PATCH,
        format!("{repo}/issues/comments/{alice_comment}"),
        &alice,
        Some(serde_json::json!({ "body": "  " })),
    )
    .await;
    assert_eq!(status, 400, "{body}");

    // Delete: the administrator takes down bob's secret; it is gone.
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/issues/comments/{bob_comment}"),
        &alice,
        None,
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/issues/comments/{bob_comment}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");
    let (_, listed) = api(
        reqwest::Method::GET,
        format!("{repo}/issues/{number}/comments"),
        &owner,
        None,
    )
    .await;
    let listed = listed.to_string();
    assert!(!listed.contains("bob pasted a secret"), "{listed}");
    assert!(listed.contains("alice fixed a typo"), "{listed}");
    let (status, _) = api(
        reqwest::Method::DELETE,
        format!("{repo}/issues/comments/{bob_comment}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "a second delete finds nothing");

    // A comment is addressed through the repository it lives in.
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        &bob,
        Some(serde_json::json!({ "name": "elsewhere", "auto_init": true, "readme": "default" })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, _) = api(
        reqwest::Method::DELETE,
        format!("{base}/api/v1/repos/cm_bob/elsewhere/issues/comments/{alice_comment}"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, 404, "another repository's comment was reached");

    // The issue: an attachment, then deletion by the administrator only.
    let uploaded = reqwest::Client::new()
        .post(format!("{repo}/issues/{number}/assets"))
        .bearer_auth(&alice)
        .multipart(
            reqwest::multipart::Form::new().part(
                "attachment",
                reqwest::multipart::Part::bytes(b"attached".to_vec())
                    .file_name("log.txt")
                    .mime_str("text/plain")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), 201, "{}", uploaded.text().await.unwrap());
    let attachments_before = count_files(&repo_root);
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/issues/{number}"),
        &alice,
        None,
    )
    .await;
    assert_eq!(status, 403, "the author is no administrator: {body}");
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/issues/{number}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");
    let (status, _) = api(
        reqwest::Method::GET,
        format!("{repo}/issues/{number}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 404);
    let (status, found) = api(
        reqwest::Method::GET,
        format!("{base}/api/v1/search?q=zebracorn&type=issues"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{found}");
    assert!(
        !found.to_string().contains("zebracorn sighting"),
        "a deleted issue is still found: {found}"
    );
    assert!(
        count_files(&repo_root) < attachments_before,
        "the issue's attachment was left in storage"
    );

    // Review comments on a pull request: edit, a refused delete of a thread
    // others replied to, and the reply's own delete.
    let root = tempfile::tempdir().unwrap();
    let address = base.trim_start_matches("http://").to_string();
    let (status, pat) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/users/tokens"),
        &owner,
        Some(serde_json::json!({ "name": "git", "scopes": "repo" })),
    )
    .await;
    assert_eq!(status, 201, "{pat}");
    let pat = pat["token"].as_str().unwrap().to_string();
    let root_path = root.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let url = format!("http://cm_owner:{pat}@{address}/git/cm_owner/proj");
        git(&root_path, &["clone", "-q", &url, "work"]);
        let work = root_path.join("work");
        git(&work, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(work.join("feature.txt"), "one\ntwo\n").unwrap();
        git(&work, &["add", "feature.txt"]);
        git(&work, &["commit", "-q", "-m", "feature"]);
        git(&work, &["push", "-q", "origin", "feature"]);
    })
    .await
    .unwrap();
    let (status, pr) = api(
        reqwest::Method::POST,
        format!("{repo}/pulls"),
        &owner,
        Some(serde_json::json!({ "title": "feature", "head": "feature", "base": "main" })),
    )
    .await;
    assert_eq!(status, 201, "{pr}");
    let pr_number = pr["number"].as_i64().unwrap();
    let (status, root_comment) = api(
        reqwest::Method::POST,
        format!("{repo}/pulls/{pr_number}/comments"),
        &alice,
        Some(
            serde_json::json!({ "path": "feature.txt", "line": 1, "side": "RIGHT", "body": "nit" }),
        ),
    )
    .await;
    assert_eq!(status, 201, "{root_comment}");
    let root_id = root_comment["id"].as_i64().unwrap();
    let (status, reply) = api(
        reqwest::Method::POST,
        format!("{repo}/pulls/{pr_number}/comments"),
        &bob,
        Some(serde_json::json!({
            "path": "feature.txt", "line": 1, "side": "RIGHT", "body": "agreed",
            "reply_to_id": root_id
        })),
    )
    .await;
    assert_eq!(status, 201, "{reply}");
    let reply_id = reply["id"].as_i64().unwrap();

    let (status, edited) = api(
        reqwest::Method::PATCH,
        format!("{repo}/pulls/{pr_number}/comments/{root_id}"),
        &alice,
        Some(serde_json::json!({ "body": "nit: rename" })),
    )
    .await;
    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["body"], "nit: rename");
    // The timeline recorded the comment with a copy of its text: the copy
    // follows the edit, or the old text is still served from there.
    let timeline_bodies = |timeline: &serde_json::Value| -> Vec<String> {
        timeline
            .as_array()
            .or_else(|| timeline["events"].as_array())
            .or_else(|| timeline["data"].as_array())
            .unwrap_or_else(|| panic!("timeline is not a list: {timeline}"))
            .iter()
            .filter_map(|event| event["body"].as_str().map(str::to_string))
            .collect()
    };
    let (status, timeline) = api(
        reqwest::Method::GET,
        format!("{repo}/pulls/{pr_number}/timeline"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{timeline}");
    let bodies = timeline_bodies(&timeline);
    assert!(bodies.contains(&"nit: rename".to_string()), "{timeline}");
    assert!(!bodies.contains(&"nit".to_string()), "{timeline}");
    let (status, body) = api(
        reqwest::Method::PATCH,
        format!("{repo}/pulls/{}/comments/{root_id}", pr_number + 1),
        &alice,
        Some(serde_json::json!({ "body": "wrong pull request" })),
    )
    .await;
    assert_eq!(status, 404, "{body}");
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/pulls/{pr_number}/comments/{root_id}"),
        &alice,
        None,
    )
    .await;
    assert_eq!(status, 409, "a thread others replied to: {body}");
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/pulls/{pr_number}/comments/{reply_id}"),
        &bob,
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/pulls/{pr_number}/comments/{root_id}"),
        &alice,
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");
    let (status, timeline) = api(
        reqwest::Method::GET,
        format!("{repo}/pulls/{pr_number}/timeline"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{timeline}");
    let bodies = timeline_bodies(&timeline);
    assert!(
        !bodies
            .iter()
            .any(|body| body == "nit: rename" || body == "agreed"),
        "a deleted comment is still in the timeline: {timeline}"
    );
}

/// Every file under the server's storage root — what a leaked attachment blob
/// would add to.
fn count_files(root: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                count += 1;
            }
        }
    }
    count
}
