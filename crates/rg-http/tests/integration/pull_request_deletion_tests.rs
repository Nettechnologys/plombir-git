//! card_ee4f318c50f1: a repository administrator deletes a pull request — the
//! second half of "issues and pull requests cannot be deleted"
//! (card_60961272e1ba took issues). An open or closed pull request goes with
//! its review comments, timeline, attachments, notifications and
//! subscriptions; a merged one is the record of commits in its base branch
//! and is refused with `409`; anybody but an administrator gets `403`.
//!
//! And the number stays spent, for pull requests and issues alike: both used
//! to be `max(number) + 1`, so deleting the newest one handed its number to
//! the next — every `#N` link, and a pull request's CI ref
//! `refs/pull/N/head`, then named somebody else's work.

use std::path::Path;

use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

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
                ("GIT_AUTHOR_NAME", "del"),
                ("GIT_AUTHOR_EMAIL", "del@example.com"),
                ("GIT_COMMITTER_NAME", "del"),
                ("GIT_COMMITTER_EMAIL", "del@example.com"),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_administrator_deletes_an_unmerged_pull_request_and_its_number_stays_spent() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, "prdel_owner", "prdel_owner@example.com").await;
    let (alice, _) = register_full(&base, "prdel_alice", "prdel_alice@example.com").await;
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        &owner,
        Some(serde_json::json!({ "name": "proj", "auto_init": true, "readme": "default" })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let repo = format!("{base}/api/v1/repos/prdel_owner/proj");

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
        let url = format!("http://prdel_owner:{pat}@{address}/git/prdel_owner/proj");
        git(&root_path, &["clone", "-q", &url, "work"]);
        let work = root_path.join("work");
        for branch in ["merged-one", "doomed", "after"] {
            git(&work, &["checkout", "-q", "-b", branch, "origin/main"]);
            std::fs::write(work.join(format!("{branch}.txt")), "one\ntwo\n").unwrap();
            git(&work, &["add", "."]);
            git(&work, &["commit", "-q", "-m", branch]);
            git(&work, &["push", "-q", "origin", branch]);
        }
    })
    .await
    .unwrap();
    let open = |head: &'static str| {
        let (repo, owner) = (repo.clone(), owner.clone());
        async move {
            let (status, pr) = api(
                reqwest::Method::POST,
                format!("{repo}/pulls"),
                &owner,
                Some(serde_json::json!({ "title": head, "head": head, "base": "main" })),
            )
            .await;
            assert_eq!(status, 201, "{pr}");
            (pr["number"].as_i64().unwrap(), pr["id"].as_i64().unwrap())
        }
    };
    let (merged_number, _) = open("merged-one").await;
    let (doomed, doomed_id) = open("doomed").await;
    assert_eq!((merged_number, doomed), (1, 2));

    // The doomed pull request collects what deletion has to take with it: a
    // review comment from somebody else (which notifies the author and
    // subscribes both), and an attachment.
    let (status, comment) = api(
        reqwest::Method::POST,
        format!("{repo}/pulls/{doomed}/comments"),
        &alice,
        Some(serde_json::json!({
            "path": "doomed.txt", "line": 1, "side": "RIGHT", "body": "zebracorn review"
        })),
    )
    .await;
    assert_eq!(status, 201, "{comment}");
    let uploaded = reqwest::Client::new()
        .post(format!("{repo}/pulls/{doomed}/assets"))
        .bearer_auth(&owner)
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
    let subscriptions = || {
        let db = db.clone();
        async move {
            rg_db::entities::thread_subscription::Entity::find()
                .filter(
                    rg_db::entities::thread_subscription::Column::SubjectType.eq("pull_request"),
                )
                .filter(rg_db::entities::thread_subscription::Column::SubjectId.eq(doomed_id))
                .count(&db)
                .await
                .unwrap()
        }
    };
    assert!(
        subscriptions().await > 0,
        "the fixture subscribed nobody, so the cleanup below would prove nothing"
    );
    let files_before = count_files(&repo_root);

    // Not an administrator: refused, and nothing is gone.
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/pulls/{doomed}"),
        &alice,
        None,
    )
    .await;
    assert_eq!(status, 403, "{body}");

    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/pulls/{doomed}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");
    let (status, _) = api(
        reqwest::Method::GET,
        format!("{repo}/pulls/{doomed}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 404);
    let (status, listed) = api(
        reqwest::Method::GET,
        format!("{repo}/pulls?state=all"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "{listed}");
    assert!(!listed.to_string().contains("\"doomed\""), "{listed}");
    assert_eq!(
        subscriptions().await,
        0,
        "subscriptions to the deleted thread remain"
    );
    let notifications = rg_db::entities::notification::Entity::find()
        .filter(rg_db::entities::notification::Column::SubjectType.eq("pull_request"))
        .filter(rg_db::entities::notification::Column::SubjectId.eq(doomed_id))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(
        notifications, 0,
        "notifications about the deleted pull request remain"
    );
    let review_comments = rg_db::entities::review_comment::Entity::find()
        .filter(rg_db::entities::review_comment::Column::PrId.eq(doomed_id))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(review_comments, 0);
    let events = rg_db::entities::pr_event::Entity::find()
        .filter(rg_db::entities::pr_event::Column::PrId.eq(doomed_id))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(events, 0);
    assert!(
        count_files(&repo_root) < files_before,
        "the pull request's attachment was left in storage"
    );
    let (status, _) = api(
        reqwest::Method::DELETE,
        format!("{repo}/pulls/{doomed}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 404, "a second delete finds nothing");

    // The next pull request does not inherit #2.
    let (after, _) = open("after").await;
    assert_eq!(
        after, 3,
        "a deleted pull request's number was handed out again"
    );

    // Merged: the record of commits in `main` stays.
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{repo}/pulls/{merged_number}/merge"),
        &owner,
        Some(serde_json::json!({ "strategy": "merge" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/pulls/{merged_number}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 409, "{body}");
    let (status, _) = api(
        reqwest::Method::GET,
        format!("{repo}/pulls/{merged_number}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 200);

    // Issues: the same rule for their own sequence.
    for title in ["first", "second"] {
        let (status, issue) = api(
            reqwest::Method::POST,
            format!("{repo}/issues"),
            &owner,
            Some(serde_json::json!({ "title": title })),
        )
        .await;
        assert_eq!(status, 201, "{issue}");
    }
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/issues/2"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");
    let (status, issue) = api(
        reqwest::Method::POST,
        format!("{repo}/issues"),
        &owner,
        Some(serde_json::json!({ "title": "third" })),
    )
    .await;
    assert_eq!(status, 201, "{issue}");
    assert_eq!(
        issue["number"], 3,
        "a deleted issue's number was handed out again"
    );
}
