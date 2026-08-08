//! card_55c9cddfe9d6: a webhook may only subscribe to events that exist.
//!
//! The subscription list was joined into `webhooks.events` with no check at all,
//! so `{"events":["pull_request.merge"]}` answered `201 Created`. The hook then
//! appeared in the settings page looking configured and never fired once —
//! there is no failure anywhere in that story, only an event nobody raises.
//!
//! Driven through HTTP because the refusal has to *arrive*: the validation lives
//! in `rg_core::webhook::service`, and it only helps if the handler's error
//! mapping turns it into a `400` rather than the `500` an unclassified `anyhow`
//! comes out as.

use crate::common::{create_repo, register_full, spawn_test_app};

async fn setup(suffix: &str) -> (String, String) {
    let base = spawn_test_app().await;
    let owner = format!("wh-{suffix}");
    let (token, _) = register_full(&base, &owner, &format!("{owner}@example.test")).await;
    let repo = format!("wh-{suffix}");
    create_repo(&base, &token, &repo).await;
    let url = format!("{base}/api/v1/repos/{owner}/{repo}/hooks");
    (token, url)
}

fn body(events: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "url": "https://hooks.example.invalid/forgekeep",
        "events": events,
    })
}

#[tokio::test]
async fn a_hook_cannot_subscribe_to_an_event_that_will_never_be_raised() {
    let (token, url) = setup("create").await;
    let client = reqwest::Client::new();

    // A typo, a plausible-but-wrong name, and the prefix that used to work by
    // accident because matching was a substring test.
    for events in [
        vec!["pull_request.merge"],
        vec!["issues"],
        vec!["issue"],
        vec!["push", "deployment.created"],
    ] {
        let resp = client
            .post(&url)
            .bearer_auth(&token)
            .json(&body(&events))
            .send()
            .await
            .expect("create webhook");
        assert_eq!(
            resp.status(),
            400,
            "{events:?} was registered as a subscription nothing can deliver"
        );
    }

    let listed: serde_json::Value = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("list webhooks")
        .json()
        .await
        .expect("hook listing is JSON");
    assert_eq!(
        listed.as_array().map(Vec::len),
        Some(0),
        "a refused registration must leave no hook behind"
    );

    let ok = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body(&["push", "issue.opened", "pull_request.merged"]))
        .send()
        .await
        .expect("create webhook");
    assert_eq!(ok.status(), 201, "real events must still register");
}

#[tokio::test]
async fn an_update_cannot_replace_a_subscription_with_one_that_does_not_exist() {
    let (token, url) = setup("update").await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body(&["push"]))
        .send()
        .await
        .expect("create webhook")
        .json()
        .await
        .expect("created hook is JSON");
    let id = created["id"].as_i64().expect("created hook carries id");
    let one = format!("{url}/{id}");

    let refused = client
        .patch(&one)
        .bearer_auth(&token)
        .json(&serde_json::json!({"events": ["push", "tag.create"]}))
        .send()
        .await
        .expect("update webhook");
    assert_eq!(refused.status(), 400);

    let unchanged: serde_json::Value = client
        .get(&one)
        .bearer_auth(&token)
        .send()
        .await
        .expect("read webhook")
        .json()
        .await
        .expect("hook is JSON");
    // The API hands the column back as it is stored: one comma-joined string.
    assert_eq!(
        unchanged["events"], "push",
        "a refused update must not have rewritten the subscription"
    );
}
