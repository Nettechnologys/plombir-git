//! card_349c2b6a0d7c: a notification reaches the person an event is about,
//! not only the repository's watchers.
//!
//! * A review requested through CODEOWNERS notifies the reviewer, and — their
//!   mail setting being on by default — owes them a mail, which the dispatcher
//!   sends batched with everything else that piled up for them.
//! * `@mentions` notify who they name, but only someone who can read the
//!   repository.
//! * Taking part subscribes; an explicit unsubscribe stops the thread's
//!   activity and survives taking part again.
//! * A second event in a thread folds into the unread notification about it.
//! * An assignee, the author of a pull request whose CI failed, are told.
//! * Deleting an issue removes the notifications about it.

use std::sync::Mutex;
use std::time::Duration;

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

/// The caller's notifications, newest first.
async fn inbox(base: &str, token: &str) -> Vec<serde_json::Value> {
    let (status, body) = api(
        reqwest::Method::GET,
        format!("{base}/api/v1/notifications?per_page=100"),
        token,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["data"].as_array().cloned().unwrap_or_default()
}

/// Wait for the detached delivery to land a notification `matches` accepts.
async fn eventually(
    base: &str,
    token: &str,
    what: &str,
    matches: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    for _ in 0..100 {
        if let Some(found) = inbox(base, token).await.into_iter().find(&matches) {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no notification {what}: {:?}", inbox(base, token).await);
}

/// Let detached deliveries that should *not* produce anything have their
/// chance to (wrongly) do so before the absence is asserted.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(600)).await;
}

#[derive(Default)]
struct RecordingOutbox {
    sent: Mutex<Vec<(String, rg_core::notification::mail::Digest)>>,
}

impl rg_core::notification::mail::Outbox for RecordingOutbox {
    fn send<'a>(
        &'a self,
        to: &'a str,
        mail: &'a rg_core::notification::mail::Digest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        self.sent
            .lock()
            .unwrap()
            .push((to.to_string(), mail.clone()));
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn notifications_reach_the_people_an_event_is_about() {
    let (base, db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, "nr_owner", "nr_owner@example.com").await;
    let (reviewer, _) = register_full(&base, "nr_rev", "nr_rev@example.com").await;
    let (member, _) = register_full(&base, "nr_member", "nr_member@example.com").await;
    let (outsider, _) = register_full(&base, "nr_out", "nr_out@example.com").await;
    let repo = format!("{base}/api/v1/repos/nr_owner/proj");

    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        &owner,
        Some(serde_json::json!({
            "name": "proj", "is_private": true, "auto_init": true, "readme": "default"
        })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    for (who, permission) in [("nr_rev", "write"), ("nr_member", "read")] {
        let (status, body) = api(
            reqwest::Method::POST,
            format!("{repo}/collaborators"),
            &owner,
            Some(serde_json::json!({ "username": who, "permission": permission })),
        )
        .await;
        assert!(status == 200 || status == 201, "{status} {body}");
    }
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{repo}/contents/CODEOWNERS"),
        &owner,
        Some(serde_json::json!({
            "branch": "main", "content": "* @nr_rev\n", "message": "owners"
        })),
    )
    .await;
    assert!(status == 200 || status == 201, "{status} {body}");
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{repo}/branches"),
        &owner,
        Some(serde_json::json!({ "name": "feature" })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{repo}/contents/feature.txt"),
        &owner,
        Some(serde_json::json!({
            "branch": "feature", "content": "feature\n", "message": "feature"
        })),
    )
    .await;
    assert!(status == 200 || status == 201, "{status} {body}");

    // The member turns mention mails off; the reviewer keeps the defaults.
    let (status, settings) = api(
        reqwest::Method::PUT,
        format!("{base}/api/v1/users/me/notification-settings"),
        &member,
        Some(serde_json::json!({ "email": { "mention": false } })),
    )
    .await;
    assert_eq!(status, 200, "{settings}");
    assert_eq!(settings["email"]["mention"], false);
    assert_eq!(settings["email"]["review_requested"], true);
    assert_eq!(
        settings["email"]["ci_triggered"], false,
        "the per-push CI mail is off unless chosen"
    );

    // A pull request: CODEOWNERS asks the reviewer, the body mentions the
    // member and somebody who cannot read the repository.
    let (status, pr) = api(
        reqwest::Method::POST,
        format!("{repo}/pulls"),
        &owner,
        Some(serde_json::json!({
            "title": "Feature", "head": "feature", "base": "main",
            "body": "cc @nr_member and @nr_out"
        })),
    )
    .await;
    assert_eq!(status, 201, "{pr}");
    let pr_number = pr["number"].as_i64().unwrap();
    let requested = eventually(&base, &reviewer, "for the CODEOWNERS review request", |n| {
        n["reason"] == "review_requested"
    })
    .await;
    assert_eq!(
        requested["link"],
        format!("/nr_owner/proj/pulls/{pr_number}")
    );
    assert_eq!(requested["subject_type"], "pull_request");
    eventually(&base, &member, "for the mention", |n| {
        n["reason"] == "mention"
    })
    .await;
    settle().await;
    assert!(
        inbox(&base, &outsider).await.is_empty(),
        "a mention told someone who cannot read the private repository"
    );

    // The dispatcher mails the reviewer (default on) and not the member (off).
    let outbox = RecordingOutbox::default();
    let mails =
        rg_core::notification::mail::deliver_pending(&db, &outbox, Some("https://forge.example"))
            .await
            .unwrap();
    assert_eq!(mails, 1);
    {
        let sent = outbox.sent.lock().unwrap();
        let (to, digest) = &sent[0];
        assert_eq!(to, "nr_rev@example.com");
        assert!(digest.subject.contains("Feature"), "{digest:?}");
        assert_eq!(
            digest.entries[0].url.as_deref(),
            Some(format!("https://forge.example/nr_owner/proj/pulls/{pr_number}").as_str())
        );
        assert_eq!(
            digest.settings_url.as_deref(),
            Some("https://forge.example/settings/notifications")
        );
    }
    // Nothing is mailed twice.
    let again = RecordingOutbox::default();
    assert_eq!(
        rg_core::notification::mail::deliver_pending(&db, &again, None)
            .await
            .unwrap(),
        0
    );

    // CI fails on the pull request's head: its author is told.
    let head_sha = {
        let (status, pr) = api(
            reqwest::Method::GET,
            format!("{repo}/pulls/{pr_number}"),
            &owner,
            None,
        )
        .await;
        assert_eq!(status, 200, "{pr}");
        pr["head_sha"].as_str().unwrap().to_string()
    };
    let repo_id = pr["repo_id"].as_i64().unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        &head_sha,
        "refs/heads/feature",
        "push",
        None,
    )
    .await
    .unwrap();
    rg_db::ops::pipeline_ops::update_pipeline_status(&db, pipeline.id, "failed", None, None)
        .await
        .unwrap();
    assert_eq!(
        rg_core::notification::thread::deliver_ci_failed(&db, pipeline.id)
            .await
            .unwrap(),
        1
    );
    let failed = eventually(&base, &owner, "for the failed CI", |n| {
        n["reason"] == "ci_failed"
    })
    .await;
    assert!(
        failed["body"].as_str().unwrap().contains("failed"),
        "{failed}"
    );

    // An issue: the member comments and so follows it.
    let (status, issue) = api(
        reqwest::Method::POST,
        format!("{repo}/issues"),
        &owner,
        Some(serde_json::json!({ "title": "Thread" })),
    )
    .await;
    assert_eq!(status, 201, "{issue}");
    let number = issue["number"].as_i64().unwrap();
    let comment = |token: String, body: &'static str| {
        let url = format!("{repo}/issues/{number}/comments");
        async move {
            let (status, reply) = api(
                reqwest::Method::POST,
                url,
                &token,
                Some(serde_json::json!({ "body": body })),
            )
            .await;
            assert_eq!(status, 201, "{reply}");
        }
    };
    comment(member.clone(), "I can reproduce").await;
    let (status, subscription) = api(
        reqwest::Method::GET,
        format!("{repo}/issues/{number}/subscription"),
        &member,
        None,
    )
    .await;
    assert_eq!(status, 200, "{subscription}");
    assert_eq!(subscription["subscribed"], true);
    assert_eq!(subscription["reason"], "commented");

    // Two replies fold into one unread notification about the thread.
    comment(owner.clone(), "Thanks").await;
    eventually(&base, &member, "for the reply", |n| {
        n["reason"] == "participating" && n["subject_type"] == "issue"
    })
    .await;
    comment(owner.clone(), "Fixed on main").await;
    settle().await;
    let about_issue: Vec<_> = inbox(&base, &member)
        .await
        .into_iter()
        .filter(|n| n["subject_type"] == "issue" && n["is_read"] == false)
        .collect();
    assert_eq!(about_issue.len(), 1, "{about_issue:?}");

    // Unsubscribed, the member hears nothing more — even after commenting
    // again, which must not quietly re-subscribe them.
    let (status, body) = api(
        reqwest::Method::POST,
        format!("{base}/api/v1/notifications/mark-all-read"),
        &member,
        None,
    )
    .await;
    assert!(status == 200 || status == 204, "{status} {body}");
    let (status, subscription) = api(
        reqwest::Method::DELETE,
        format!("{repo}/issues/{number}/subscription"),
        &member,
        None,
    )
    .await;
    assert_eq!(status, 200, "{subscription}");
    assert_eq!(subscription["subscribed"], false);
    comment(member.clone(), "One more data point").await;
    comment(owner.clone(), "Noted").await;
    settle().await;
    let unread: Vec<_> = inbox(&base, &member)
        .await
        .into_iter()
        .filter(|n| n["is_read"] == false)
        .collect();
    assert!(unread.is_empty(), "unsubscribed and still told: {unread:?}");
    let (_, subscription) = api(
        reqwest::Method::GET,
        format!("{repo}/issues/{number}/subscription"),
        &member,
        None,
    )
    .await;
    assert_eq!(subscription["subscribed"], false);

    // Assigning the issue tells the assignee.
    let (status, body) = api(
        reqwest::Method::PATCH,
        format!("{repo}/issues/{number}"),
        &owner,
        Some(serde_json::json!({ "assignee": "nr_rev" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    eventually(&base, &reviewer, "for the assignment", |n| {
        n["reason"] == "assigned" && n["subject_type"] == "issue"
    })
    .await;

    // Deleting the issue takes the notifications about it along.
    let (status, body) = api(
        reqwest::Method::DELETE,
        format!("{repo}/issues/{number}"),
        &owner,
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");
    assert!(
        inbox(&base, &reviewer)
            .await
            .iter()
            .all(|n| n["subject_type"] != "issue"),
        "a deleted issue is still in the inbox"
    );
}
