use crate::common::{
    register_full, spawn_test_app_with_db, spawn_test_app_with_overrides, StateOverrides,
};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Error};

async fn create_private_job(
    base: &str,
    db: &rg_db::DatabaseConnection,
    token: &str,
    owner_id: i64,
) -> i64 {
    create_job_in_repo(base, db, token, owner_id, "private-ws", true)
        .await
        .1
}

async fn create_job_in_repo(
    base: &str,
    db: &rg_db::DatabaseConnection,
    token: &str,
    owner_id: i64,
    name: &str,
    is_private: bool,
) -> (i64, i64) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": is_private}))
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
    let job_id = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "test", "true", None, None, None, None, None, false, None, None, None,
    )
    .await
    .unwrap()
    .id;
    (repo_id, job_id)
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

/// card_f7128fd4a2e2, the job-log half: `check_read_for` runs once, at the
/// handshake, and a socket that is already open never asks again. The account
/// need not go anywhere for that answer to expire — flipping the repository to
/// private revokes an outsider's read on the spot, and the log stream must not
/// be the one place that keeps honouring the old answer.
#[tokio::test]
async fn a_repository_turning_private_closes_an_outsider_job_log_socket() {
    use futures::StreamExt;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};
    use tokio_tungstenite::tungstenite::Message;

    let (base, db) = spawn_test_app_with_overrides(StateOverrides {
        ws_session_recheck_secs: Some(1),
        ..Default::default()
    })
    .await;
    let (owner_token, owner_id) = register_full(&base, "ws_pub_owner", "ws_pub@example.com").await;
    let (reader_token, _) = register_full(&base, "ws_pub_reader", "ws_reader@example.com").await;
    let (repo_id, job_id) =
        create_job_in_repo(&base, &db, &owner_token, owner_id, "public-ws", false).await;

    let (mut socket, response) =
        tokio_tungstenite::connect_async(websocket_request(&base, job_id, &reader_token))
            .await
            .expect("a public repository's job logs are readable by anyone");
    assert_eq!(response.status(), 101);

    let welcome = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("the socket must greet a reader who may read")
        .expect("the socket closed before the welcome frame")
        .expect("the welcome frame must be readable");
    assert!(
        matches!(&welcome, Message::Text(text) if text.contains("\"connected\"")),
        "unexpected welcome frame: {welcome:?}"
    );

    // Baseline on live access: several re-check intervals with the repository
    // still public, and the socket stays up.
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(4), socket.next())
            .await
            .is_err(),
        "the re-check closed a socket whose reader may still read"
    );

    let repository = rg_db::entities::repository::Entity::find_by_id(repo_id)
        .one(&db)
        .await
        .unwrap()
        .unwrap();
    let mut active: rg_db::entities::repository::ActiveModel = repository.into();
    active.is_private = Set(true);
    active.update(&db).await.unwrap();

    let ended = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while let Some(frame) = socket.next().await {
            match frame {
                Ok(Message::Close(_)) | Err(_) => return true,
                Ok(_) => continue,
            }
        }
        true
    })
    .await;
    assert!(
        ended.unwrap_or(false),
        "the job-log socket kept streaming a repository its reader may no longer read"
    );
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
