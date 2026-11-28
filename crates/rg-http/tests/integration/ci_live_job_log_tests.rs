//! A job's log from an external runner while the job runs (ADR-0001 D10,
//! card_e925c7cedd46, card_dffba24313ed).
//!
//! Driven over HTTP the way `rg-runner` sends it: numbered pieces with
//! `x-job-log-seq`, then the whole log with `x-job-log-mode: replace`. What is
//! proven here is the server's half — the masking across pieces, the numbers,
//! the order against the final log, and that a subscriber hears it live.

use crate::common::ws::handshake_request;
use crate::common::{register_full, spawn_test_app_with_db};
use futures::StreamExt;
use sea_orm::DatabaseConnection;
use tokio_tungstenite::tungstenite::Message;

const SECRET: &str = "live-secret-0123456789";

async fn create_repo(base: &str, token: &str, name: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// A running job held by a fresh runner, issued [`SECRET`] — what the
/// server masks the log with.
async fn seed(base: &str, db: &DatabaseConnection, who: &str) -> (String, i64, i64, String) {
    let (owner_token, owner_id) = register_full(base, who, &format!("{who}@example.com")).await;
    let repo_id = create_repo(base, &owner_token, &format!("{who}-repo")).await;
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        db,
        repo_id,
        &format!("{who}-runner"),
        "",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "push",
        Some(owner_id),
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "build", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "agent", "true", None, None, None, None, None, None, false, None, None, None,
    )
    .await
    .unwrap();
    let key = rg_core::auth::encryption::derive_key(crate::common::TEST_ENCRYPTION_KEY);
    let mask =
        rg_core::auth::encryption::encrypt(&serde_json::json!([SECRET]).to_string(), &key).unwrap();
    rg_db::ops::pipeline_ops::assign_job_with_mask_snapshot(db, job.id, runner.id, &mask, None)
        .await
        .unwrap();
    (owner_token, runner.id, job.id, runner_token)
}

async fn piece(
    base: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    seq: u64,
    text: &str,
) -> (u16, serde_json::Value) {
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/log"
        ))
        .bearer_auth(token)
        .header("x-job-log-seq", seq.to_string())
        .body(text.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or_default())
}

async fn stored(db: &DatabaseConnection, job_id: i64) -> String {
    let total = rg_db::ops::pipeline_ops::get_job_log_slice(db, job_id, 0, 0)
        .await
        .unwrap()
        .unwrap()
        .total_length
        .unwrap();
    rg_db::ops::pipeline_ops::get_job_log_slice(db, job_id, 0, total)
        .await
        .unwrap()
        .unwrap()
        .content
        .unwrap()
}

#[tokio::test]
async fn a_live_log_is_masked_across_pieces_written_once_and_heard_before_the_job_ends() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, runner_id, job_id, runner_token) = seed(&base, &db, "live_log").await;

    let (mut socket, _) = tokio_tungstenite::connect_async(handshake_request(
        &base,
        &format!("/api/v1/ws/job/{job_id}"),
        Some(&owner_token),
    ))
    .await
    .expect("the owner may follow the job's log");
    let welcome = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(&welcome, Message::Text(text) if text.contains("\"connected\"")));

    // The secret is cut by the border of two pieces.
    let (cut, rest) = SECRET.split_at(9);
    let first = format!("step one done\nusing {cut}");
    // The end of what arrived is held back until it can no longer be the
    // start of a secret; a long enough line after it lets it out.
    let padding = "-".repeat(80);
    let second = format!("{rest} for the agent\nstep two running\n{padding}\n");
    assert_eq!(
        piece(&base, runner_id, job_id, &runner_token, 0, &first)
            .await
            .0,
        200
    );
    // A retry of a piece already taken: acknowledged, written once.
    let (status, body) = piece(&base, runner_id, job_id, &runner_token, 0, &first).await;
    assert_eq!((status, body["status"].as_str()), (200, Some("duplicate")));
    assert_eq!(
        piece(&base, runner_id, job_id, &runner_token, 1, &second)
            .await
            .0,
        200
    );

    let live = stored(&db, job_id).await;
    assert!(live.starts_with("step one done\nusing "), "{live}");
    assert!(live.contains("***"), "{live}");
    for half in [cut, rest] {
        assert!(
            !live.contains(half),
            "half of the secret reached the database: {live}"
        );
    }
    assert_eq!(
        live.matches("step one done").count(),
        1,
        "the retried piece doubled: {live}"
    );

    // The job is still running, and its subscriber already heard the text.
    let job = rg_db::ops::pipeline_ops::get_job(&db, job_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        rg_db::ops::pipeline_ops::is_active_pipeline_work(&job.status),
        "{}",
        job.status
    );
    assert!(live.contains("step two running"), "{live}");
    let mut heard = String::new();
    while !heard.contains("step two running") {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("the live text reaches the subscriber while the job runs")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = frame {
            let event: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(event["event_type"], "job_log");
            assert_eq!(event["data"]["replaced"], false);
            heard.push_str(event["data"]["log"].as_str().unwrap());
        }
    }
    for half in [cut, rest] {
        assert!(
            !heard.contains(half),
            "half of the secret reached the WebSocket: {heard}"
        );
    }

    // The whole log replaces the live one, and closes it.
    let whole =
        format!("step one done\nusing {SECRET} for the agent\nstep two running\nall done\n");
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/log"
        ))
        .bearer_auth(&runner_token)
        .header("x-job-log-mode", "replace")
        .body(whole)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        stored(&db, job_id).await,
        "step one done\nusing *** for the agent\nstep two running\nall done\n"
    );
    let replaced = loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = frame {
            let event: serde_json::Value = serde_json::from_str(&text).unwrap();
            if event["data"]["replaced"] == true {
                break event;
            }
        }
    };
    assert!(replaced["data"]["log"]
        .as_str()
        .unwrap()
        .ends_with("all done\n"));

    // A live piece after the final log is refused and changes nothing.
    let (status, _) = piece(&base, runner_id, job_id, &runner_token, 2, "late\n").await;
    assert_eq!(status, 409);
    assert!(!stored(&db, job_id).await.contains("late"));
}

#[tokio::test]
async fn a_piece_number_that_is_not_a_number_is_refused() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_, runner_id, job_id, runner_token) = seed(&base, &db, "live_log_bad").await;
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/log"
        ))
        .bearer_auth(&runner_token)
        .header("x-job-log-seq", "first")
        .body("text")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(stored(&db, job_id).await, "");
}
