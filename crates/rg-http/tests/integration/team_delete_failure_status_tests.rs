//! Regression coverage for card_a253a34cf2f9: a *failed* team delete must not
//! be reported as an *absent* team.
//!
//! `DELETE /orgs/{name}/teams/{team_id}` had a single error branch,
//! `AppError::not_found(e)`, while `rg_core::org::delete_team` reported two
//! unrelated outcomes through one flattened `anyhow::Error`: "there is no such
//! team", and "the `teams` query failed" carrying the `.context("db: …")` chain
//! `rg_db::ops` wraps every failure in. Answering `404` to the second one is
//! worse than a wrong status on a read path — `DELETE` is idempotent, so the
//! client reads "the team is already gone" and never retries a delete that
//! never happened. On top of that a `404` body is *not* sanitized in
//! `IntoResponse`, so the `db: …` text reached the client verbatim (H-05).
//!
//! The outage is simulated by dropping the `teams` table rather than by closing
//! the pool: the endpoint is reached before any other query, but a closed pool
//! would also change *how* the request fails, and dropping exactly one table
//! keeps the rest of the app healthy so the baseline assertions below still run.

use crate::common::{register_full, spawn_test_app_with_db};
use sea_orm::ConnectionTrait;

#[tokio::test]
async fn broken_team_delete_is_not_reported_as_a_missing_team() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "teamdel-owner", "teamdel@example.com").await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "teamdel-org"}))
        .send()
        .await
        .expect("create org");

    let team: serde_json::Value = client
        .post(format!("{base}/api/v1/orgs/teamdel-org/teams"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "developers", "permission": "write"}))
        .send()
        .await
        .expect("create team")
        .json()
        .await
        .expect("json body");
    let team_id = team["id"].as_i64().expect("team id");

    // Baseline on a healthy database: a team that really is absent is still a
    // `404`, with the fixed message. Without this the assertion further down
    // cannot tell "the status got fixed" from "this endpoint 500s on anything".
    let resp = client
        .delete(format!(
            "{base}/api/v1/orgs/teamdel-org/teams/{}",
            team_id + 1
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent team is still a 404");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "team not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    // Break exactly the query the delete performs.
    db.execute_unprepared("DROP TABLE teams")
        .await
        .expect("drop teams");

    let resp = client
        .delete(format!("{base}/api/v1/orgs/teamdel-org/teams/{team_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a failed team delete must be a 5xx, not {status} — a 404 tells the \
         client the team was already deleted and puts nothing in the alerts \
         (body: {body})"
    );

    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("teams"),
        "the response body must not carry internal error detail, got: {message}"
    );
}
