//! `DELETE /imports/{id}` is a lifecycle boundary, not a row-hiding shortcut.
//!
//! The fake GHES API accepts the worker's real HTTP request and then never
//! answers it. That leaves the production import future live at a known await
//! point. A successful DELETE must cancel that request before returning 204;
//! otherwise the peer connection stays open and this test times out.

use std::time::Duration;

use axum::http::StatusCode;
use chrono::Utc;
use sea_orm::Set;
use tokio::io::AsyncReadExt;
use tokio::sync::oneshot;

use crate::common::{
    build_test_app_state_with, register_full, setup_test_db, spawn_test_app_with_db,
    wait_for_listener, StateOverrides,
};

async fn spawn_hanging_github_api() -> (String, oneshot::Receiver<()>, oneshot::Receiver<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake GitHub API");
    let address = listener.local_addr().expect("fake API address");
    let (entered_tx, entered_rx) = oneshot::channel();
    let (disconnected_tx, disconnected_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept import API request");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream
                .read(&mut buffer)
                .await
                .expect("read import API request");
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        assert!(
            String::from_utf8_lossy(&request)
                .starts_with("GET /api/v3/repos/team/widgets/labels?per_page=100 "),
            "the import blocked at an unexpected request: {}",
            String::from_utf8_lossy(&request)
        );
        entered_tx
            .send(())
            .expect("cancellation test stopped before the worker reached the source");

        loop {
            match stream.read(&mut buffer).await {
                Ok(0) => {
                    disconnected_tx
                        .send(())
                        .expect("cancellation test stopped before observing disconnect");
                    return;
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                    disconnected_tx
                        .send(())
                        .expect("cancellation test stopped before observing reset");
                    return;
                }
                Err(error) => panic!("watch import API connection: {error}"),
            }
        }
    });

    (format!("http://{address}"), entered_rx, disconnected_rx)
}

#[tokio::test]
async fn deleting_a_running_import_stops_its_worker_before_204() {
    let (source_origin, entered, disconnected) = spawn_hanging_github_api().await;
    let trusted_import_origins =
        rg_core::import::trust::TrustedImportOrigins::parse(std::slice::from_ref(&source_origin))
            .expect("trust the fake GHES origin");

    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create repo root");
    let state = build_test_app_state_with(
        db,
        repo_root,
        StateOverrides {
            trusted_import_origins: Some(trusted_import_origins),
            ..Default::default()
        },
    );
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test app");
    let address = listener.local_addr().expect("test app address");
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("serve test app");
    });
    wait_for_listener(&address.to_string()).await;
    let base = format!("http://{address}");

    let (token, _) = register_full(&base, "cancel-importer", "cancel-importer@example.com").await;
    let client = reqwest::Client::new();
    let started = client
        .post(format!("{base}/api/v1/imports"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "platform": "github",
            "source_url": format!("{source_origin}/team/widgets.git"),
            "target_owner": "cancel-importer",
            "target_name": "widgets",
            "import_repo": false,
            "import_issues": false,
            "import_pull_requests": false,
            "import_wiki": false,
            "import_releases": false,
            "import_labels": true,
            "import_milestones": false
        }))
        .send()
        .await
        .expect("start import");
    assert_eq!(started.status(), StatusCode::CREATED);
    let task: serde_json::Value = started.json().await.expect("import task response");
    let task_id = task["id"].as_i64().expect("import task id");

    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .expect("import worker never reached the source API")
        .expect("fake source stopped before the request arrived");

    let deleted = tokio::time::timeout(
        Duration::from_secs(5),
        client
            .delete(format!("{base}/api/v1/imports/{task_id}"))
            .bearer_auth(&token)
            .send(),
    )
    .await
    .expect("DELETE did not wait for cancellation to settle")
    .expect("delete running import");
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    tokio::time::timeout(Duration::from_secs(5), disconnected)
        .await
        .expect("204 returned while the import still held its source connection")
        .expect("fake source stopped before observing worker cancellation");

    let absent = client
        .get(format!("{base}/api/v1/imports/{task_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read the deleted import");
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_does_not_acknowledge_a_running_import_without_a_local_worker() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) =
        register_full(&base, "remote-importer", "remote-importer@example.com").await;
    let now = Utc::now();
    let task = rg_db::ops::import_task_ops::create(
        &db,
        rg_db::entities::import_task::ActiveModel {
            user_id: Set(user_id),
            platform: Set("github".to_string()),
            source_url: Set("https://github.example/team/widgets.git".to_string()),
            target_owner: Set("remote-importer".to_string()),
            target_name: Set("widgets".to_string()),
            status: Set("importing".to_string()),
            progress: Set(50),
            import_repo: Set(true),
            import_issues: Set(false),
            import_pull_requests: Set(false),
            import_wiki: Set(false),
            import_releases: Set(false),
            import_labels: Set(false),
            import_milestones: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed import owned by another worker process");

    let response = reqwest::Client::new()
        .delete(format!("{base}/api/v1/imports/{}", task.id))
        .bearer_auth(token)
        .send()
        .await
        .expect("delete remotely owned import");
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let retained = rg_db::ops::import_task_ops::find_by_id(&db, task.id)
        .await
        .expect("read remotely owned import after rejected delete");
    assert!(retained.is_some(), "409 must leave the worker's row intact");
}
