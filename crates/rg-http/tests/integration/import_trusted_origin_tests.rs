use std::time::Duration;

use axum::http::StatusCode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::common::{
    build_test_app_state_with, register_full, setup_test_db, wait_for_listener, StateOverrides,
};

async fn spawn_app(
    trusted_import_origins: rg_core::import::trust::TrustedImportOrigins,
    import_transport_policy: rg_core::import::trust::ImportTransportPolicy,
) -> String {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create repo root");
    let state = build_test_app_state_with(
        db,
        repo_root,
        StateOverrides {
            trusted_import_origins: Some(trusted_import_origins),
            import_transport_policy: Some(import_transport_policy),
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

async fn start_credentialed_gitlab_import(
    base: &str,
    session_token: &str,
    source_url: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/imports"))
        .bearer_auth(session_token)
        .json(&serde_json::json!({
            "platform": "gitlab",
            "source_url": source_url,
            "target_owner": "private-importer",
            "target_name": "widgets",
            "auth_token": "private-import-token",
            "import_repo": true,
            "import_issues": false,
            "import_pull_requests": false,
            "import_wiki": false,
            "import_releases": false,
            "import_labels": false,
            "import_milestones": false
        }))
        .send()
        .await
        .expect("start credentialed import")
}

async fn read_headers(stream: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await.expect("read request");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn write_gitlab_project(stream: &mut tokio::net::TcpStream, source_origin: &str) {
    let body = serde_json::json!({
        "id": 1,
        "name": "widgets",
        "path_with_namespace": "team/widgets",
        "visibility": "private",
        "default_branch": "main",
        "web_url": format!("{source_origin}/team/widgets"),
        "http_url_to_repo": format!("{source_origin}/team/widgets.git"),
        "namespace": {"id": 2, "name": "team", "path": "team", "kind": "group"}
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write GitLab response");
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
    let untrusted = spawn_app(Default::default(), Default::default()).await;
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
    let trusted = spawn_app(trusted_origins, Default::default()).await;
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

#[tokio::test]
async fn credentialed_plaintext_import_needs_both_private_and_transport_grants() {
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind source sink");
    let source_origin = format!("http://{}", sink.local_addr().expect("sink address"));
    let source_url = format!("{source_origin}/team/widgets.git");
    let trusted_origins =
        rg_core::import::trust::TrustedImportOrigins::parse(std::slice::from_ref(&source_origin))
            .expect("private-origin trust");
    let transport_policy =
        rg_core::import::trust::ImportTransportPolicy::parse(std::slice::from_ref(&source_origin))
            .expect("plaintext transport opt-in");

    let private_only = spawn_app(trusted_origins.clone(), Default::default()).await;
    let (private_only_session, _) = register_full(
        &private_only,
        "private-importer",
        "private-only@example.com",
    )
    .await;
    let rejected =
        start_credentialed_gitlab_import(&private_only, &private_only_session, &source_url).await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), sink.accept())
            .await
            .is_err(),
        "private-origin SSRF trust silently doubled as plaintext credential authority"
    );

    let transport_only = spawn_app(Default::default(), transport_policy.clone()).await;
    let (transport_only_session, _) = register_full(
        &transport_only,
        "private-importer",
        "transport-only@example.com",
    )
    .await;
    let rejected =
        start_credentialed_gitlab_import(&transport_only, &transport_only_session, &source_url)
            .await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), sink.accept())
            .await
            .is_err(),
        "plaintext transport opt-in silently disabled the private-address guard"
    );

    let source_origin_for_server = source_origin.clone();
    let source = tokio::spawn(async move {
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), sink.accept())
            .await
            .expect("credentialed import never reached its approved source")
            .expect("accept approved source request");
        let request = read_headers(&mut stream).await;
        write_gitlab_project(&mut stream, &source_origin_for_server).await;
        request
    });

    let fully_approved = spawn_app(trusted_origins, transport_policy).await;
    let (approved_session, _) = register_full(
        &fully_approved,
        "private-importer",
        "fully-approved@example.com",
    )
    .await;
    let accepted =
        start_credentialed_gitlab_import(&fully_approved, &approved_session, &source_url).await;
    assert_eq!(accepted.status(), StatusCode::CREATED);

    let request = source.await.expect("source task");
    assert!(
        request.contains("PRIVATE-TOKEN: private-import-token")
            || request.contains("private-token: private-import-token"),
        "approved source did not receive the import PAT: {request}"
    );
}
