use std::sync::Arc;

pub use rg_db;
pub use rg_http;

use rg_core::package_registry::oci::OciStorage;

pub mod fault;

struct NoopCiEngine;

impl rg_core::ci::CiTrigger for NoopCiEngine {
    fn has_ci_config(&self, _repo_path: &std::path::Path, _commit_sha: &str) -> bool {
        false
    }

    /// Mirrors `has_ci_config`: this double has no workflow files to
    /// match an event against, so it answers the same for every event.
    fn has_workflow_for_event(
        &self,
        _repo_path: &std::path::Path,
        _commit_sha: &str,
        _event: &str,
        _ref_name: &str,
        _base_branch: Option<&str>,
    ) -> bool {
        false
    }

    fn trigger_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<i64>> + Send + 'a>> {
        Box::pin(async { Ok(0) })
    }

    fn resume_pipeline<'a>(
        &'a self,
        _params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

/// Create a temporary file-based SQLite database with all migrations applied.
///
/// Connect through `rg_db::connect_with_pool` rather than a bare
/// `Database::connect`, so the tests exercise the SQLite configuration the
/// server actually runs with: WAL journalling, `synchronous = NORMAL`, a
/// `busy_timeout` and the cache/mmap tuning. A raw `Database::connect` leaves
/// sqlx's defaults in place — `journal_mode = DELETE` and
/// `synchronous = FULL` — which is both a different configuration from
/// production and an fsync on every one of the ~75 migration commits.
///
/// Measured on this tree, connect + `run_migrations` on a fresh database
/// (4 samples per configuration, configurations interleaved):
/// `journal=DELETE`/`synchronous=FULL` 2977 ms median vs WAL/`NORMAL` 455 ms.
/// Every test pays that once, so it dominated the suite's wall-clock.
pub async fn setup_test_db() -> (rg_db::DatabaseConnection, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    // Pool size and connect timeout kept as they were; only the PRAGMA
    // configuration changes.
    let db = rg_db::connect_with_pool(&db_url, 5, 60, 2)
        .await
        .expect("failed to connect");
    rg_db::run_migrations(&db).await.expect("migration failed");
    (db, dir)
}

/// Test-only replacements for individual pieces of [`rg_http::AppState`].
///
/// A struct with a `Default` rather than extra parameters on
/// [`build_test_app_state`]: that function is called from a dozen places, and a
/// harness that has to grow a parameter for every new seam is a harness nobody
/// adds a seam to.
#[derive(Default)]
pub struct StateOverrides {
    /// Replaces the `LocalBlobStorage` this harness would otherwise build.
    ///
    /// The OCI storage is layered on the same backend, so overriding this one
    /// value reaches both the attachment/LFS write paths and the registry's.
    pub blob_storage: Option<Arc<dyn rg_core::blob_storage::BlobStorage>>,
}

pub fn build_test_app_state(
    db: rg_db::DatabaseConnection,
    repo_root: std::path::PathBuf,
) -> rg_http::AppState {
    build_test_app_state_with(db, repo_root, StateOverrides::default())
}

pub fn build_test_app_state_with(
    db: rg_db::DatabaseConnection,
    repo_root: std::path::PathBuf,
    overrides: StateOverrides,
) -> rg_http::AppState {
    let db_for_queue = db.clone();
    // Keep the registry inside this test's own temp tree. A fixed
    // `$TMPDIR/forgekeep-test-oci` is shared by every run on the machine, so the
    // first user to create it owns it and every other user gets
    // `Permission denied (os error 13)` — and concurrent runs stomp each
    // other's uploads even when the uid happens to line up.
    let oci_storage_path = repo_root
        .parent()
        .unwrap_or(repo_root.as_path())
        .join("oci-storage");
    let blob_storage: Arc<dyn rg_core::blob_storage::BlobStorage> = overrides
        .blob_storage
        .unwrap_or_else(|| Arc::new(rg_core::blob_storage::LocalBlobStorage::new(&repo_root)));
    rg_http::AppState {
        blob_storage: blob_storage.clone(),
        repo_root: Arc::new(repo_root),
        db,
        jwt_secret: Arc::new("test-secret-key".to_string()),
        external_webhook_secret: None,
        docker_enabled: false,
        external_runners: false,
        allow_host_runner: false,
        rate_limiter: rg_http::rate_limit::RateLimiter::new(10000, 60),
        notification_hub: rg_http::ws::NotificationHub::new(),
        smtp_config: None,
        oci_storage: Arc::new(OciStorage::from_backend(blob_storage, oci_storage_path)),
        log_write_queue: rg_core::ci::log_write_queue::LogWriteQueue::spawn(db_for_queue),
        external_url: None,
        job_timeout_secs: 3600,
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        ci_engine: Arc::new(NoopCiEngine),
        // Enabled in the test harness so attestation endpoints are reachable;
        // production defaults to off (opt-in).
        attestation_enabled: true,
    }
}

/// Block until the freshly spawned server accepts connections on `addr`
/// (a bare `host:port`, no scheme).
///
/// This replaces a fixed `sleep(100ms)` after `tokio::spawn(axum::serve(..))`.
/// A constant is a bet that 100 ms is always enough: on a loaded machine it is
/// not, and the first request dies with `connection refused`; on an idle one
/// the listener is up in single-digit milliseconds and the rest is thrown
/// away. The bound here is wall-clock rather than an iteration count, so a
/// process that gets descheduled mid-poll still gets its full budget instead
/// of burning the budget on the scheduler (`sol_7d16590273ef`).
#[allow(dead_code)]
pub async fn wait_for_listener(addr: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "HTTP listener did not start on {addr} within 10s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

#[allow(dead_code)]
pub async fn spawn_test_app() -> String {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let state = build_test_app_state(db, repo_root);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    base_url
}

/// Spawn the test app and hand back `(base_url, route_facts)`.
///
/// The facts are the `(method, path, access)` rows recorded by the very build
/// that produced this router — see `rg_http::route_table`. A test that walks
/// them is therefore walking the real route set, not a hand-kept copy of it.
#[allow(dead_code)]
pub async fn spawn_test_app_with_routes() -> (String, Vec<rg_http::route_table::RouteFact>) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let state = build_test_app_state(db, repo_root);
    let (app, facts) = rg_http::create_router_for_test_with_routes(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    (base_url, facts)
}

/// Spawn the test app and hand back `(base_url, route_facts, db)`.
///
/// The cross-repository id-scope sweep needs both halves at once: the route
/// table to walk, and the database to seed the few resources this harness has
/// no API for — a pull request needs commits on two branches, a pipeline needs
/// a CI engine, a webhook delivery needs a webhook that actually fired.
#[allow(dead_code)]
pub async fn spawn_test_app_with_routes_and_db() -> (
    String,
    Vec<rg_http::route_table::RouteFact>,
    rg_db::DatabaseConnection,
) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let state = build_test_app_state(db.clone(), repo_root);
    let (app, facts) = rg_http::create_router_for_test_with_routes(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    (base_url, facts, db)
}

/// Spawn the test app and keep the db handle alive for tests that need
/// to manipulate data directly (e.g. promoting admin users).
#[allow(dead_code)]
pub async fn spawn_test_app_with_db() -> (String, rg_db::DatabaseConnection) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let state = build_test_app_state(db.clone(), repo_root);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    (base_url, db)
}

/// Spawn the test app with an inbound-webhook HMAC secret configured.
///
/// The secret is instance-wide: every external CI wired to this server holds
/// the same one, which is why the `/webhooks/external/*` endpoints treat a
/// valid signature as defense-in-depth on top of their access gate rather than
/// as the gate itself. Tests on either side of that line need this state.
#[allow(dead_code)]
pub async fn spawn_test_app_with_webhook_secret(
    secret: &str,
) -> (String, rg_db::DatabaseConnection) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let mut state = build_test_app_state(db.clone(), repo_root);
    state.external_webhook_secret = Some(std::sync::Arc::new(secret.to_string()));
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    (base_url, db)
}

/// Spawn the test app and hand back `(base_url, repo_root)`.
///
/// The repo-browsing endpoints fail for two unrelated reasons — "no such file
/// at this ref" and "this repository cannot be opened" — and only the second
/// one lives on disk. A test that wants to break the git layer therefore needs
/// the `repo_root` the server was built with; closing the database pool instead
/// fails the *authentication* lookup, so the request never reaches the handler
/// under test.
#[allow(dead_code)]
pub async fn spawn_test_app_with_repo_root() -> (String, std::path::PathBuf) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let returned_repo_root = repo_root.clone();
    let state = build_test_app_state(db, repo_root);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    (base_url, returned_repo_root)
}

/// Spawn the test app and hand back `(base_url, repo_root, oci_upload_root)`.
///
/// A test can make a registry write fail for a *client-side* reason just by
/// sending the wrong bytes, but the server-side reasons — an unreadable staging
/// file, a blob store that cannot host the published blob — live on paths built
/// from `repo_root` and a generated UUID. Without them a test cannot tell the
/// two apart, which is exactly the distinction the status code is supposed to
/// carry.
#[allow(dead_code)]
pub async fn spawn_test_app_with_oci_root() -> (String, std::path::PathBuf, std::path::PathBuf) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    // Mirrors the layout `build_test_app_state` derives from `repo_root`.
    let oci_root = dir.path().join("oci-storage");
    let returned_repo_root = repo_root.clone();
    let state = build_test_app_state(db, repo_root);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    (base_url, returned_repo_root, oci_root)
}

pub async fn register_user(base: &str, username: &str, email: &str, password: &str) -> String {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/users/register", base))
        .json(&serde_json::json!({"username": username, "email": email, "password": password}))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "register failed for '{}': {}",
        username,
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

/// Register and return (token, user_id).
#[allow(dead_code)]
pub async fn register_full(base: &str, username: &str, email: &str) -> (String, i64) {
    let token = register_user(base, username, email, "Qz7$wRtm").await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/v1/users/me", base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (token, body["id"].as_i64().unwrap())
}

/// Create a repo and return its id.
#[allow(dead_code)]
pub async fn create_repo(base: &str, token: &str, name: &str) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "create_repo '{}' failed: {}",
        name,
        resp.status()
    );
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// Create an issue and return (id, number).
#[allow(dead_code)]
pub async fn create_issue(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    title: &str,
) -> (i64, i64) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos/{}/{}/issues", base, owner, repo))
        .bearer_auth(token)
        .json(&serde_json::json!({"title": title}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "create_issue '{}' failed: {}",
        title,
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    (
        body["id"].as_i64().unwrap(),
        body["number"].as_i64().unwrap(),
    )
}
