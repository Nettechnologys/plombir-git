use std::time::Duration;

use axum::http::StatusCode;

use crate::common::{
    build_test_app_state_with, register_full, setup_test_db, wait_for_listener, StateOverrides,
};

async fn spawn_app(trusted_import_origins: rg_core::import::trust::TrustedImportOrigins) -> String {
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
    format!("http://{address}")
}

async fn start_noop_gitlab_import(base: &str, token: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/imports"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "platform": "gitlab",
            "source_url": "http://127.0.0.1:18443/team/widgets.git",
            "target_owner": "private-importer",
            "target_name": "widgets",
            "import_repo": false,
            "import_issues": false,
            "import_pull_requests": false,
            "import_wiki": false,
            "import_releases": false,
            "import_labels": false,
            "import_milestones": false
        }))
        .send()
        .await
        .expect("start import")
}

#[tokio::test]
async fn private_import_origin_requires_and_consumes_the_admin_trust_entry() {
    let untrusted = spawn_app(Default::default()).await;
    let (untrusted_token, _) = register_full(
        &untrusted,
        "private-importer",
        "private-importer-untrusted@example.com",
    )
    .await;
    let rejected = start_noop_gitlab_import(&untrusted, &untrusted_token).await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    let trusted_origins =
        rg_core::import::trust::TrustedImportOrigins::parse(&["http://127.0.0.1:18443".to_owned()])
            .expect("trusted origin config");
    let trusted = spawn_app(trusted_origins).await;
    let (trusted_token, _) = register_full(
        &trusted,
        "private-importer",
        "private-importer-trusted@example.com",
    )
    .await;
    let accepted = start_noop_gitlab_import(&trusted, &trusted_token).await;
    assert_eq!(accepted.status(), StatusCode::CREATED);
    let task: serde_json::Value = accepted.json().await.expect("import task response");
    let task_id = task["id"].as_i64().expect("task id");

    let client = reqwest::Client::new();
    for _ in 0..100 {
        let status = client
            .get(format!("{trusted}/api/v1/imports/{task_id}"))
            .bearer_auth(&trusted_token)
            .send()
            .await
            .expect("read import status");
        assert_eq!(status.status(), StatusCode::OK);
        let status: serde_json::Value = status.json().await.expect("status body");
        match status["status"].as_str() {
            Some("completed") => return,
            Some("failed") => panic!("trusted private import failed: {status}"),
            _ => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    panic!("trusted private import did not finish");
}
