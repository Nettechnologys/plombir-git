//! card_09b2665584ed: `milestones.state` is a free-form string column that the
//! listing filters on with exact equality, and both doors used to let a value
//! past that no reader can match.
//!
//! * `POST` stored whatever arrived (`state: Set(body.state.unwrap_or("open"))`),
//!   so `{"state":"clsoed"}` answered `201 Created` and produced a milestone
//!   that appears under neither `?state=open` nor `?state=closed` — it exists
//!   and cannot be reached.
//! * `PATCH` compared the value against the two it knew and had no `else`, so a
//!   typo answered `200 OK` with the milestone unchanged: validated, and the
//!   failure of that validation reacted to by nothing.

use crate::common::{create_repo, register_full, spawn_test_app};

struct Fixture {
    token: String,
    url: String,
}

async fn setup(suffix: &str) -> Fixture {
    let base = spawn_test_app().await;
    let owner = format!("ms-{suffix}");
    let (token, _) = register_full(&base, &owner, &format!("{owner}@example.test")).await;
    let repo = format!("ms-{suffix}");
    create_repo(&base, &token, &repo).await;
    let url = format!("{base}/api/v1/repos/{owner}/{repo}/milestones");
    Fixture { token, url }
}

async fn list(fixture: &Fixture, state: Option<&str>) -> Vec<serde_json::Value> {
    let url = match state {
        Some(state) => format!("{}?state={state}", fixture.url),
        None => fixture.url.clone(),
    };
    reqwest::Client::new()
        .get(url)
        .bearer_auth(&fixture.token)
        .send()
        .await
        .expect("list milestones")
        .json::<serde_json::Value>()
        .await
        .expect("milestone listing is JSON")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_milestone_cannot_be_created_in_a_state_no_listing_can_find() {
    let fixture = setup("create").await;
    let client = reqwest::Client::new();

    for typo in ["clsoed", "OPEN", "active", ""] {
        let resp = client
            .post(&fixture.url)
            .bearer_auth(&fixture.token)
            .json(&serde_json::json!({"title": "v1", "state": typo}))
            .send()
            .await
            .expect("create milestone");
        assert_eq!(
            resp.status(),
            400,
            "`state: {typo}` was accepted and stored verbatim"
        );
    }
    assert!(
        list(&fixture, None).await.is_empty(),
        "a refused create must leave no row behind"
    );

    // Both real states still work, and each one is reachable through the tab
    // that filters for it — which is the property the typo broke.
    for state in ["open", "closed"] {
        let resp = client
            .post(&fixture.url)
            .bearer_auth(&fixture.token)
            .json(&serde_json::json!({"title": format!("v-{state}"), "state": state}))
            .send()
            .await
            .expect("create milestone");
        assert_eq!(resp.status(), 201, "`state: {state}` was refused");
    }
    let default_state = client
        .post(&fixture.url)
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({"title": "v-default"}))
        .send()
        .await
        .expect("create milestone");
    assert_eq!(default_state.status(), 201, "state stays optional");

    assert_eq!(list(&fixture, Some("open")).await.len(), 2);
    assert_eq!(list(&fixture, Some("closed")).await.len(), 1);
    assert_eq!(list(&fixture, None).await.len(), 3);
}

#[tokio::test]
async fn an_unknown_state_on_update_is_refused_rather_than_dropped() {
    let fixture = setup("update").await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(&fixture.url)
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({"title": "v1"}))
        .send()
        .await
        .expect("create milestone")
        .json()
        .await
        .expect("created milestone is JSON");
    let id = created["id"]
        .as_i64()
        .expect("created milestone carries id");
    let one = format!("{}/{id}", fixture.url);

    for typo in ["clsoed", "closed ", "reopened"] {
        let resp = client
            .patch(&one)
            .bearer_auth(&fixture.token)
            .json(&serde_json::json!({"state": typo}))
            .send()
            .await
            .expect("update milestone");
        assert_eq!(
            resp.status(),
            400,
            "`state: {typo}` was answered 200 with nothing changed"
        );
    }

    let unchanged = list(&fixture, Some("open")).await;
    assert_eq!(unchanged.len(), 1, "a refused update must change nothing");

    let closed = client
        .patch(&one)
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({"state": "closed"}))
        .send()
        .await
        .expect("update milestone");
    assert_eq!(closed.status(), 200, "the real transition must still work");
    assert_eq!(list(&fixture, Some("closed")).await.len(), 1);
    assert!(list(&fixture, Some("open")).await.is_empty());
}
