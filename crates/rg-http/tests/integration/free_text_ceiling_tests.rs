//! card_9ea48ee6a422: text past the ceiling is the caller's mistake and is
//! answered `400` with the field's name, on every backend.
//!
//! On MySQL the columns behind these fields were `VARCHAR(255)`, so a longer
//! body reached the database and came back as `500`; on SQLite and PostgreSQL
//! it was stored. The ceiling now sits in front of the write
//! (`rg_core::text_limits`), and the columns hold what it allows — see
//! `free_text_at_the_ceiling_round_trips` in rg-core's multi-backend smoke.

use rg_core::text_limits::{MAX_BODY_CHARS, MAX_LINE_CHARS};

use crate::common::{create_repo, register_full, spawn_test_app};

struct Fixture {
    token: String,
    repo_url: String,
}

async fn setup(suffix: &str) -> Fixture {
    let base = spawn_test_app().await;
    let owner = format!("ft-{suffix}");
    let (token, _) = register_full(&base, &owner, &format!("{owner}@example.test")).await;
    let repo = format!("ft-{suffix}");
    create_repo(&base, &token, &repo).await;
    Fixture {
        token,
        repo_url: format!("{base}/api/v1/repos/{owner}/{repo}"),
    }
}

/// Send `body` and expect the refusal to name `field`.
async fn assert_refused(
    fixture: &Fixture,
    method: reqwest::Method,
    path: &str,
    body: serde_json::Value,
    field: &str,
) {
    let resp = reqwest::Client::new()
        .request(method.clone(), format!("{}{path}", fixture.repo_url))
        .bearer_auth(&fixture.token)
        .json(&body)
        .send()
        .await
        .expect("send");
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    assert_eq!(status, 400, "{method} {path}: {text}");
    assert!(
        text.contains(&format!("{field} must be at most")),
        "{method} {path}: the refusal must name `{field}`, got: {text}"
    );
}

async fn post(fixture: &Fixture, path: &str, body: serde_json::Value) -> serde_json::Value {
    let resp = reqwest::Client::new()
        .post(format!("{}{path}", fixture.repo_url))
        .bearer_auth(&fixture.token)
        .json(&body)
        .send()
        .await
        .expect("send");
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.expect("JSON");
    assert_eq!(status, 201, "POST {path}: {json}");
    json
}

#[tokio::test]
async fn bodies_at_the_ceiling_are_stored_and_one_more_character_is_a_bad_request() {
    let fixture = setup("bodies").await;
    let body = "ž".repeat(MAX_BODY_CHARS);
    let over_body = format!("{body}x");
    let over_title = "t".repeat(MAX_LINE_CHARS + 1);

    let issue = post(
        &fixture,
        "/issues",
        serde_json::json!({"title": "t".repeat(MAX_LINE_CHARS), "body": body}),
    )
    .await;
    assert_eq!(issue["body"].as_str().map(str::len), Some(body.len()));
    let number = issue["number"].as_i64().expect("issue number");
    post(
        &fixture,
        &format!("/issues/{number}/comments"),
        serde_json::json!({"body": body}),
    )
    .await;
    post(
        &fixture,
        "/wiki",
        serde_json::json!({"title": "Home", "content": body}),
    )
    .await;

    use reqwest::Method;
    assert_refused(
        &fixture,
        Method::POST,
        "/issues",
        serde_json::json!({"title": "t", "body": over_body}),
        "body",
    )
    .await;
    assert_refused(
        &fixture,
        Method::POST,
        "/issues",
        serde_json::json!({"title": over_title}),
        "title",
    )
    .await;
    assert_refused(
        &fixture,
        Method::PATCH,
        &format!("/issues/{number}"),
        serde_json::json!({"body": over_body}),
        "body",
    )
    .await;
    assert_refused(
        &fixture,
        Method::POST,
        &format!("/issues/{number}/comments"),
        serde_json::json!({"body": over_body}),
        "body",
    )
    .await;
    assert_refused(
        &fixture,
        Method::POST,
        "/wiki",
        serde_json::json!({"title": "Other", "content": over_body}),
        "content",
    )
    .await;
    assert_refused(
        &fixture,
        Method::POST,
        "/labels",
        serde_json::json!({"name": over_title, "color": "#00ff00"}),
        "name",
    )
    .await;
    assert_refused(
        &fixture,
        Method::POST,
        "/milestones",
        serde_json::json!({"title": over_title}),
        "title",
    )
    .await;

    let milestone = post(&fixture, "/milestones", serde_json::json!({"title": "v1"})).await;
    let id = milestone["id"].as_i64().expect("milestone id");
    assert_refused(
        &fixture,
        Method::PATCH,
        &format!("/milestones/{id}"),
        serde_json::json!({"description": over_body}),
        "description",
    )
    .await;
}
