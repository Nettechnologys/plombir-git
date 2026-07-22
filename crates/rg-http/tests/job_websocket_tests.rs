mod common;

use common::{register_full, spawn_test_app_with_db};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Error};

async fn create_private_job(
    base: &str,
    db: &rg_db::DatabaseConnection,
    token: &str,
    owner_id: i64,
) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": "private-ws", "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let repo_id = response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "abcdefabcdefabcdefabcdefabcdefabcdefabcd",
        "refs/heads/main",
        "push",
        Some(owner_id),
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .unwrap();
    rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "test", "true", None, None, None, None, None, false, None, None, None,
    )
    .await
    .unwrap()
    .id
}

fn websocket_request(
    base: &str,
    job_id: i64,
    token: &str,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let url = format!(
        "{}/api/v1/ws/job/{job_id}",
        base.replacen("http://", "ws://", 1)
    );
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        "sec-websocket-protocol",
        format!("bearer.{token}").parse().unwrap(),
    );
    request
}

#[tokio::test]
async fn private_job_websocket_requires_repository_read_access() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) = register_full(&base, "ws_owner", "ws_owner@example.com").await;
    let (outsider_token, _) = register_full(&base, "ws_outsider", "ws_outsider@example.com").await;
    let job_id = create_private_job(&base, &db, &owner_token, owner_id).await;

    let denied =
        tokio_tungstenite::connect_async(websocket_request(&base, job_id, &outsider_token)).await;
    match denied {
        Err(Error::Http(response)) => assert_eq!(response.status(), 403),
        other => panic!("expected forbidden WebSocket handshake, got {other:?}"),
    }

    let (mut socket, response) =
        tokio_tungstenite::connect_async(websocket_request(&base, job_id, &owner_token))
            .await
            .unwrap();
    assert_eq!(response.status(), 101);
    assert_eq!(
        response.headers()["sec-websocket-protocol"]
            .to_str()
            .unwrap(),
        format!("bearer.{owner_token}")
    );
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn job_websocket_rejects_invalid_token_before_upgrade() {
    let (base, _) = spawn_test_app_with_db().await;
    let denied = tokio_tungstenite::connect_async(websocket_request(&base, 999, "invalid")).await;
    match denied {
        Err(Error::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("expected unauthorized WebSocket handshake, got {other:?}"),
    }
}
