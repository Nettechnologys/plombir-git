//! Regression coverage for card_c9c15e3bc36c: a *failed* label lookup must not
//! be reported as an *absent* label.
//!
//! `GET /repos/{owner}/{name}/labels/{id}` had a single error branch,
//! `Err(_) => AppError::not_found("label not found")`, which is the worst shape
//! of the family: the error value was dropped without ever being converted, so
//! a broken `labels` query became "there is no such label" for the client *and*
//! nothing at all for the operator — no 5xx, no log line, no alert.
//!
//! The fix types the two genuine "no such row" outcomes of
//! `rg_core::label::service::{get_label, resolve_repo}` with
//! `rg_core::error::NotFound` and lets `AppError::from` classify the rest. That
//! also promotes the sibling `list_labels`, which used to answer `500` for a
//! repository that simply does not exist.
//!
//! As elsewhere in this family the outage is simulated by dropping exactly one
//! table rather than by closing the pool: authentication and the owner/repo
//! resolution stay healthy, so the baseline assertions below still run and the
//! test can tell "the status got fixed" from "this endpoint 5xxs on anything".

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
use sea_orm::ConnectionTrait;

#[tokio::test]
async fn broken_label_lookup_is_not_reported_as_a_missing_label() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "labelfail-owner", "labelfail@example.com").await;
    create_repo(&base, &token, "labelfail-repo").await;
    let client = reqwest::Client::new();

    let label: serde_json::Value = client
        .post(format!(
            "{base}/api/v1/repos/labelfail-owner/labelfail-repo/labels"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "bug", "color": "#ff0000"}))
        .send()
        .await
        .expect("create label")
        .json()
        .await
        .expect("json body");
    let label_id = label["id"].as_i64().expect("label id");

    // Baseline on a healthy database: a label that really is absent is a 404
    // with the fixed message.
    let resp = client
        .get(format!(
            "{base}/api/v1/repos/labelfail-owner/labelfail-repo/labels/{}",
            label_id + 1
        ))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent label is still a 404");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "label not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    // ... and so is a repository that does not exist — the typed `resolve_repo`
    // reports that as `NotFound`, where the untyped error used to reach
    // `AppError::from` as a plain `anyhow` and become a 500 on `list_labels`.
    let resp = client
        .get(format!(
            "{base}/api/v1/repos/labelfail-owner/no-such-repo/labels"
        ))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        404,
        "listing labels of an absent repository is a 404, not a 500"
    );
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "repository not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    // Break exactly the query the lookup performs. `users` and `repositories`
    // stay intact, so the handler gets all the way to the label row.
    db.execute_unprepared("DROP TABLE labels")
        .await
        .expect("drop labels");

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/labelfail-owner/labelfail-repo/labels/{label_id}"
        ))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a failed label lookup must be a 5xx, not {status} — a 404 tells the \
         client the label was deleted and puts nothing in the alerts \
         (body: {body})"
    );

    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("labels"),
        "the response body must not carry internal error detail, got: {message}"
    );
}
