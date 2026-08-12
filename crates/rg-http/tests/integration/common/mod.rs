use std::sync::Arc;

pub use rg_db;
pub use rg_http;

use rg_core::package_registry::oci::OciStorage;

pub mod answer;
pub mod fault;
pub mod route_path;
pub mod source_scan;
pub mod ws;

struct NoopCiEngine;

impl rg_core::ci::CiTrigger for NoopCiEngine {
    fn has_ci_config(&self, _repo_path: &std::path::Path, _commit_sha: &str) -> bool {
        false
    }

    /// Mirrors `has_ci_config`: this double has no workflow files to
    /// match an event against, so it answers the same for every event.
    fn has_workflow_for_event(&self, _query: rg_core::ci::WorkflowEventQuery<'_>) -> bool {
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
/// Measured again after that fix: the remaining ~450 ms is the 104 migrations
/// themselves, and every one of the tests in this binary ran all of them to
/// arrive at the identical schema. So the chain runs **once per test binary**
/// and each test starts from a copy of its result — see [`migrated_template`].
pub async fn setup_test_db() -> (rg_db::DatabaseConnection, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    copy_migrated_template(&db_path).await;
    let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
    // The connect timeout is the test-suite value, not production's: it also
    // bounds the eager first connect, and under a parallel run this harness
    // competes for the disk with every sibling test doing the same thing. See
    // `rg_db::TEST_CONNECT_TIMEOUT_SECS`.
    let db = rg_db::connect_with_pool(&db_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("failed to connect");
    (db, dir)
}

/// The migrated schema, built once per *binary build* and shared by every
/// process that runs it.
///
/// Not a hand-written schema dump: it is produced by `run_migrations` itself, so
/// what a test starts from is exactly what the chain produces — a migration
/// added tomorrow is in the template the moment it is in the chain, with nothing
/// to keep in sync. The bookkeeping table travels with it, so a test that runs
/// the migrations again gets the same no-op it always did.
///
/// It used to be a per-process `OnceCell`, which was right for `cargo test`
/// (one process per binary, so the chain ran once for hundreds of tests) and
/// wrong for the runner this suite is actually gated by: nextest gives **every
/// test its own process**, so the cache hit exactly once and every test paid the
/// full chain again — 0.46 s measured, times 2045 tests.
///
/// The cache key is this executable's own modification time. Any change to a
/// migration rebuilds `rg-db`, which relinks this binary, so a binary newer than
/// its template is exactly the condition under which the template is stale.
/// Nothing here can hand a test the schema of an older chain.
///
/// Publication is a directory rename, which is atomic and fails when the
/// destination exists — so concurrent first-starters race harmlessly: the loser
/// throws its copy away and reads the winner's. A lock would only save the few
/// duplicate builds in the first wave of a cold run.
async fn migrated_template() -> &'static std::path::Path {
    static TEMPLATE: tokio::sync::OnceCell<std::path::PathBuf> = tokio::sync::OnceCell::const_new();
    TEMPLATE
        .get_or_init(|| async {
            let published = template_dir_for_this_build();
            if published.join("template.db").exists() {
                return published;
            }

            let staging = published.with_file_name(format!(
                "{}.building-{}",
                published
                    .file_name()
                    .expect("template directory name")
                    .to_string_lossy(),
                std::process::id()
            ));
            discard_directory(&staging);
            std::fs::create_dir_all(&staging).expect("create the template staging directory");

            let path = staging.join("template.db");
            let db = rg_db::connect_with_pool(
                &format!("sqlite://{}?mode=rwc", path.display()),
                rg_db::TEST_CONNECT_TIMEOUT_SECS,
                60,
                1,
            )
            .await
            .expect("connect to the template database");
            rg_db::run_migrations(&db)
                .await
                .expect("migrate the template database");
            // Closing the pool is what checkpoints the WAL into the file being
            // copied. Without it a test would start from a database missing
            // every table the last checkpoint did not cover.
            db.close().await.expect("close the template database");

            if std::fs::rename(&staging, &published).is_err() {
                // Somebody published first. Theirs is the same schema by
                // construction, so drop ours rather than racing to replace it.
                discard_directory(&staging);
            }
            assert!(
                published.join("template.db").exists(),
                "no migrated template at {} after publication",
                published.display()
            );
            published
        })
        .await
        .as_path()
}

/// Remove a directory if it is there, and stay quiet when it is not.
///
/// `let _ =` on a `Result` is denied workspace-wide, and rightly — but here the
/// *absent* case is the normal one and carries no information, while a removal
/// that failed for any other reason is worth a line, because the next thing
/// this code does is try to create that same path.
fn discard_directory(path: &std::path::Path) {
    if let Err(error) = std::fs::remove_dir_all(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "could not clear the template staging directory {}: {error}",
                path.display()
            );
        }
    }
}

/// Where this build's template lives: beside the test binary, under a name
/// carrying the binary's modification time.
///
/// Under `target/`, so `cargo clean` takes it and nothing outside the build
/// directory is written. Per binary *and* per build, so two binaries — or the
/// same binary before and after a migration — never share a file.
fn template_dir_for_this_build() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("the test executable's own path");
    let stamp = exe
        .metadata()
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        // No mtime (an exotic filesystem): fall back to a per-process directory,
        // which is the old behaviour rather than a shared file we cannot date.
        .unwrap_or_else(|| u128::from(std::process::id()));
    let name = exe
        .file_name()
        .expect("the test executable's file name")
        .to_string_lossy()
        .into_owned();
    let base = exe
        .parent()
        .expect("the test executable's directory")
        .join(".forgekeep-test-schema");
    std::fs::create_dir_all(&base).expect("create the shared template directory");
    base.join(format!("{name}-{stamp}"))
}

/// Lay the migrated schema down at `db_path`.
async fn copy_migrated_template(db_path: &std::path::Path) {
    let template = migrated_template().await.join("template.db");
    std::fs::copy(&template, db_path).unwrap_or_else(|error| {
        panic!(
            "copy the migrated template {} -> {}: {error}",
            template.display(),
            db_path.display()
        )
    });
    // Belt and braces: the close above normally removes these, but a WAL left
    // behind belongs to the copy as much as the pages do — half of it would be
    // a database missing its most recent commits.
    for suffix in ["-wal", "-shm"] {
        let sidecar = template.with_file_name(format!("template.db{suffix}"));
        if sidecar.exists() {
            let target = db_path.with_file_name(format!(
                "{}{suffix}",
                db_path.file_name().expect("db file name").to_string_lossy()
            ));
            std::fs::copy(&sidecar, &target).expect("copy the template WAL sidecar");
        }
    }
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
    /// Replaces the default per-test delivery tracker.
    pub delivery_tracker: Option<rg_core::task_tracker::TaskTracker>,
    /// Shortens the interval at which an open WebSocket re-checks that it may
    /// still be open. Production samples every 30s; a test that has to observe
    /// the socket close cannot sit through that.
    pub ws_session_recheck_secs: Option<u64>,
    /// Closes self-service registration for this state.
    ///
    /// The default is `Open`, which is what almost every test needs: the
    /// harness's own `register_user` helper is how fixtures get accounts.
    pub registration: Option<rg_core::user::registration::RegistrationMode>,
    /// Replaces the empty private-import trust set.
    pub trusted_import_origins: Option<rg_core::import::trust::TrustedImportOrigins>,
    /// Overrides the decoded package artifact ceiling for boundary tests.
    pub package_upload_max_bytes: Option<usize>,
    /// Replaces this state's provenance signing identity.
    ///
    /// The default is derived from [`TEST_INSTANCE_KEY_SECRET`] rather than
    /// loaded from the database, so a state can be built synchronously. A test
    /// about the key's *lifetime* — that it survives a rotated `jwt_secret` —
    /// must load it through `rg_core::auth::instance_key::load_or_adopt` and
    /// inject it here, which is the production path.
    pub instance_key: Option<Arc<rg_core::auth::instance_key::InstanceKey>>,
}

/// The at-rest encryption key every test AppState carries.
///
/// Distinct from the JWT secret on purpose — see the field comment in
/// [`build_test_app_state_with`].
pub const TEST_ENCRYPTION_KEY: &str = "test-encryption-key";

/// The secret every test AppState derives its provenance identity from.
///
/// A third distinct string, for the same reason `TEST_ENCRYPTION_KEY` is a
/// second one: a fixture where the keys collide cannot tell a handler that
/// reached for the right key from one that reached for `jwt_secret`.
pub const TEST_INSTANCE_KEY_SECRET: &str = "test-instance-key";

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
    // Production publishes this in `forgekeep serve` and in `AppState::new`;
    // this fixture builds the state by hand, so it owes the same. Without it a
    // webhook delivery cannot open `webhooks.secret_encrypted` to sign with.
    // Every test app shares `TEST_ENCRYPTION_KEY`, which is what makes one
    // process-wide value correct here.
    rg_core::auth::at_rest_key::publish(TEST_ENCRYPTION_KEY);
    rg_http::AppState {
        blob_storage: blob_storage.clone(),
        repo_root: Arc::new(repo_root),
        spa_build_dir: Arc::new(std::path::PathBuf::from(rg_http::DEFAULT_SPA_BUILD_DIR)),
        db,
        jwt_secret: Arc::new("test-secret-key".to_string()),
        // Deliberately NOT the same string as `jwt_secret`. The two were one
        // value until card_d740512de0a8, and a fixture that keeps them equal
        // cannot tell a handler that reaches for the signing secret to decrypt
        // at-rest data from one that reaches for the right key — which is the
        // whole defect. Any test that stores ciphertext must key it with
        // `TEST_ENCRYPTION_KEY`.
        encryption_key: Arc::new(TEST_ENCRYPTION_KEY.to_string()),
        instance_key: overrides.instance_key.unwrap_or_else(|| {
            Arc::new(
                rg_core::auth::instance_key::InstanceKey::derived_from_secret(
                    TEST_INSTANCE_KEY_SECRET,
                ),
            )
        }),
        external_webhook_secret: None,
        docker_enabled: false,
        external_runners: false,
        allow_host_runner: false,
        registration: overrides.registration.unwrap_or_default(),
        trusted_import_origins: overrides.trusted_import_origins.unwrap_or_default(),
        import_workers: Default::default(),
        package_upload_max_bytes: overrides
            .package_upload_max_bytes
            .unwrap_or(rg_http::DEFAULT_PACKAGE_UPLOAD_MAX_BYTES),
        notification_hub: rg_http::ws::NotificationHub::new(),
        smtp_config: None,
        oci_storage: Arc::new(OciStorage::from_backend(blob_storage, oci_storage_path)),
        log_write_queue: rg_core::ci::log_write_queue::LogWriteQueue::spawn(db_for_queue),
        delivery_tracker: overrides.delivery_tracker.unwrap_or_default(),
        external_url: None,
        job_timeout_secs: 3600,
        git_stream_timeout_secs: 300,
        git_idle_timeout_secs: 30,
        ci_engine: Arc::new(NoopCiEngine),
        // Enabled in the test harness so attestation endpoints are reachable;
        // production defaults to off (opt-in).
        attestation_enabled: true,
        ws_session_recheck_secs: overrides
            .ws_session_recheck_secs
            .unwrap_or(rg_http::ws::DEFAULT_WS_SESSION_RECHECK_SECS),
        // Empty memo over this state's own database — see
        // `rg_http::instance::InstanceSettingsCache`.
        instance_settings: Default::default(),
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
    let (base, facts, db, _repo_root) = spawn_test_app_with_routes_and_db_and_repo_root().await;
    (base, facts, db)
}

/// The route-table harness plus the exact repository root used by its server.
/// Artifact metadata tests need that root to stage bytes under the owning job.
#[allow(dead_code)]
pub async fn spawn_test_app_with_routes_and_db_and_repo_root() -> (
    String,
    Vec<rg_http::route_table::RouteFact>,
    rg_db::DatabaseConnection,
    std::path::PathBuf,
) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let returned_repo_root = repo_root.clone();
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
    (base_url, facts, db, returned_repo_root)
}

/// Spawn the test app and keep the db handle alive for tests that need
/// to manipulate data directly (e.g. promoting admin users).
#[allow(dead_code)]
pub async fn spawn_test_app_with_db() -> (String, rg_db::DatabaseConnection) {
    spawn_test_app_with_overrides(StateOverrides::default()).await
}

/// Spawn the test app with handles for independently mutating both the database
/// and the git repository root.
#[allow(dead_code)]
pub async fn spawn_test_app_with_db_and_repo_root(
) -> (String, rg_db::DatabaseConnection, std::path::PathBuf) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let returned_repo_root = repo_root.clone();
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
    (base_url, db, returned_repo_root)
}

/// Spawn the test app and retain the exact state installed in its router.
///
/// Protocol tests normally exercise the server over HTTP. A `HEAD` response,
/// however, deliberately carries no body on the wire, so tests that must also
/// inspect a protocol-specific error envelope can invoke the same production
/// handler with this cloned state after proving the routed status separately.
#[allow(dead_code)]
pub async fn spawn_test_app_with_state() -> (String, rg_db::DatabaseConnection, rg_http::AppState) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let state = build_test_app_state(db.clone(), repo_root);
    let app = rg_http::create_router_for_test(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    (base_url, db, state)
}

/// [`spawn_test_app_with_db`] with individual pieces of the state replaced —
/// for tests whose subject is a seam rather than the default wiring.
#[allow(dead_code)]
pub async fn spawn_test_app_with_overrides(
    overrides: StateOverrides,
) -> (String, rg_db::DatabaseConnection) {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let state = build_test_app_state_with(db.clone(), repo_root, overrides);
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

/// Spawn a second server over a database that already exists — a process
/// restart, minus the process.
///
/// The new server gets a brand-new `AppState`, so every in-memory cache it
/// carries starts cold and anything it then reports has to have come off disk.
/// That is what makes it the honest test for "this setting is durable" as
/// opposed to "this setting is still in the same `RwLock` we wrote it to".
#[allow(dead_code)]
pub async fn spawn_test_app_over_db(db: rg_db::DatabaseConnection) -> String {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
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

/// Spawn a server over an existing database **and** repo root, with explicit
/// state overrides — a restart that keeps both halves of the instance.
///
/// [`spawn_test_app_over_db`] gives the new server a fresh repo root, which is
/// right for a test about a database-backed setting and wrong for one that has
/// to read bytes the previous server wrote (release assets, LFS objects). Use
/// this when durability spans both stores.
#[allow(dead_code)]
pub async fn spawn_test_app_over_db_with(
    db: rg_db::DatabaseConnection,
    repo_root: std::path::PathBuf,
    overrides: StateOverrides,
) -> String {
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let state = build_test_app_state_with(db, repo_root, overrides);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    base_url
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

/// One artifact in `repo`, uploaded through the runner route so its bytes are on
/// disk and the download route has something to serve. Returns its
/// instance-wide id.
///
/// Shared rather than copied because two sweeps seed it and both judge the
/// anchored routes by what it answers: `anchored_scope_sweep_tests` compares a
/// real id against an absent one, and `route_access_sweep_tests` owes every
/// non-owner persona the masked refusal that comparison defines. A fixture that
/// drifted between them would leave one of the two proving something else.
///
/// The walk — runner → pipeline → stage → job → artifact — is the shortest one
/// the upload route accepts; there is no API for the middle of it, which is why
/// this needs the database handle.
#[allow(dead_code)]
pub async fn seed_artifact(
    base: &str,
    client: &reqwest::Client,
    db: &rg_db::DatabaseConnection,
    repo_root: &std::path::Path,
    repo: i64,
    runner_name: &str,
) -> i64 {
    let (runner, runner_token) =
        rg_db::ops::runner_ops::register_runner(db, runner_name, "", None, None, None)
            .await
            .expect("register runner");
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .expect("create pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "unit", "echo ok", None, None, None, None, None, false, None, None, None,
    )
    .await
    .expect("create job");
    rg_db::ops::pipeline_ops::assign_job(db, job.id, runner.id)
        .await
        .expect("assign job");

    let response = upload_artifact_metadata(
        base,
        client,
        repo_root,
        runner.id,
        job.id,
        &runner_token,
        "report.txt",
        b"artifact bytes",
    )
    .await;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert_eq!(status, 201, "the fixture artifact was not uploaded: {body}");
    serde_json::from_str::<serde_json::Value>(&body).expect("upload json")["id"]
        .as_i64()
        .expect("artifact id")
}

/// Stage bytes where the runner and server share job storage, then publish only
/// bounded JSON metadata through the runner route. Artifact bytes deliberately
/// never become an HTTP request body.
#[allow(dead_code, clippy::too_many_arguments)]
pub async fn upload_artifact_metadata(
    base: &str,
    client: &reqwest::Client,
    repo_root: &std::path::Path,
    runner_id: i64,
    job_id: i64,
    runner_token: &str,
    name: &str,
    bytes: &[u8],
) -> reqwest::Response {
    let job_root = repo_root
        .join("_artifacts")
        .join("jobs")
        .join(job_id.to_string());
    std::fs::create_dir_all(&job_root).expect("create staged artifact job root");
    let staged = job_root.join(name);
    std::fs::write(&staged, bytes).expect("stage artifact bytes");

    client
        .post(format!(
            "{base}/api/v1/runners/{runner_id}/jobs/{job_id}/artifacts"
        ))
        .bearer_auth(runner_token)
        .json(&serde_json::json!({
            "name": name,
            "file_path": staged,
        }))
        .send()
        .await
        .expect("upload artifact metadata")
}

/// Assert a blob finalize answered `201`, and say what it answered instead.
///
/// A bare `assert_eq!(status, 201)` is how this suite reported three separate
/// flaky reds, in three different test files, over three days: `left: 400,
/// right: 201`, and not one word about *why* the registry refused. The registry
/// puts the reason in the body — a digest mismatch names both digests and the
/// staged byte count — and a helper that drops the body is why the failure had
/// to be reproduced before it could be read at all (card_c03bd9e96a66).
///
/// Shared rather than fixed per file for the same reason: the flake moved
/// between test files twice, so a message improved in one of them would have
/// gone quiet the next time it moved.
#[allow(dead_code)]
pub async fn assert_blob_push_created(finish: reqwest::Response, payload_len: usize) {
    let status = finish.status();
    if status == reqwest::StatusCode::CREATED {
        return;
    }
    let body = finish.text().await.unwrap_or_default();
    panic!(
        "blob push failed: expected 201, got {status} for a {payload_len}-byte payload; \
         body: {body}"
    );
}
