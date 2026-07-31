//! Maintenance mode, end to end (`card_42d82e91dbe3`,
//! `card_a71309a8a7d3`).
//!
//! Two things were wrong and only one of them was visible. The middleware
//! rejected mutating requests with a `200 OK` carrying an error body, so every
//! client saw a success; and `build_test_router` never mounted the layer, so no
//! test could have noticed. The second defect is what kept the first alive —
//! these tests exist so neither can come back quietly.

use std::path::Path;

use crate::common::{
    build_test_app_state, register_full, setup_test_db, spawn_test_app_with_db, wait_for_listener,
};
use base64::Engine as _;
use sha2::{Digest, Sha256};

fn git_output(args: &[&str], cwd: Option<&Path>) -> rg_git::cli_gateway::GitOutput {
    rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway must initialize")
        .run(args, cwd)
        .expect("git invocation must start")
}

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let output = git_output(args, cwd);
    assert!(
        output.success(),
        "git invocation failed: {}",
        output.stderr_str().trim()
    );
    output.stdout_str().trim().to_string()
}

/// Promote a freshly registered user to instance admin and hand back its token.
async fn admin_token(base: &str, db: &rg_db::DatabaseConnection, name: &str) -> String {
    let (token, id) = register_full(base, name, &format!("{name}@example.com")).await;
    rg_db::ops::user_ops::update_by_id(db, id, None, None, Some(true), None)
        .await
        .unwrap()
        .expect("registered user must exist");
    token
}

/// Flip maintenance mode through the public admin API and assert it took.
async fn set_maintenance(base: &str, token: &str, on: bool) -> reqwest::Response {
    reqwest::Client::new()
        .patch(format!("{base}/api/v1/admin/settings"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "maintenance_mode": on }))
        .send()
        .await
        .unwrap()
}

async fn create_pat(base: &str, token: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": "maintenance-git" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn maintenance_mode_rejects_a_mutating_request_with_503() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    // Registration is itself a POST, so every account this test needs has to
    // exist before the gate closes.
    let admin = admin_token(&base, &db, "maint_503_admin").await;
    let (user, user_id) =
        register_full(&base, "maint_503_user", "maint_503_user@example.com").await;

    assert_eq!(set_maintenance(&base, &admin, true).await.status(), 200);

    let resp = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&user)
        .json(&serde_json::json!({"name": "blocked-by-maintenance"}))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        503,
        "a request the instance refused to serve must not answer 2xx"
    );
    assert!(
        resp.headers().contains_key("retry-after"),
        "a 503 from a transient, self-clearing condition should tell the client when to come back"
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "MAINTENANCE_MODE");

    // The repo really was not created — the rejection happened before the
    // handler, not after it.
    assert!(
        rg_db::ops::repo_ops::find_personal_by_owner_and_name(
            &db,
            user_id,
            "blocked-by-maintenance"
        )
        .await
        .unwrap()
        .is_none(),
        "the request was rejected but the write still landed"
    );
}

#[tokio::test]
async fn maintenance_mode_still_serves_reads_and_the_admin_api() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let admin = admin_token(&base, &db, "maint_read_admin").await;
    let (user, _) = register_full(&base, "maint_read_user", "maint_read_user@example.com").await;
    crate::common::create_repo(&base, &user, "readable-in-maintenance").await;

    assert_eq!(set_maintenance(&base, &admin, true).await.status(), 200);

    // Safe method: read-only maintenance is still read.
    let read = client
        .get(format!(
            "{base}/api/v1/repos/maint_read_user/readable-in-maintenance"
        ))
        .bearer_auth(&user)
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), 200, "maintenance mode blocked a read");

    // Login is deliberately not in the read allow-list. It writes the audit
    // log, login-attempt log and last-login state, so admitting it would make
    // "read-only" false even though authentication itself starts with a lookup.
    let login = client
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({
            "login": "maint_read_user",
            "password": "Qz7$wRtm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        login.status(),
        503,
        "state-changing login bookkeeping must stay blocked"
    );

    // The admin API is the way out of the mode, so it cannot be blocked by it —
    // a mutating admin request has to go through even now.
    assert_eq!(
        set_maintenance(&base, &admin, false).await.status(),
        200,
        "maintenance mode locked out the only API that can turn it off"
    );

    // And with the mode off, the ordinary write path is open again.
    let resp = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&user)
        .json(&serde_json::json!({"name": "after-maintenance"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

/// The layer has to be mounted in the router the other tests drive, not just in
/// the production one — that gap is what hid the 200-instead-of-503 for as long
/// as it lived. This asserts the mounting itself, independently of any handler.
#[tokio::test]
async fn the_test_router_carries_the_maintenance_layer() {
    let (base, db) = spawn_test_app_with_db().await;
    let admin = admin_token(&base, &db, "maint_layer_admin").await;

    // An unauthenticated POST to a route no handler claims: whatever the router
    // answers on its own, it is not a 503 — so a 503 here can only come from a
    // gate mounted in front of the whole table.
    let path = format!("{base}/api/v1/there-is-no-such-route");
    let before = reqwest::Client::new().post(&path).send().await.unwrap();
    assert_ne!(before.status(), 503);

    assert_eq!(set_maintenance(&base, &admin, true).await.status(), 200);

    let after = reqwest::Client::new().post(&path).send().await.unwrap();
    assert_eq!(
        after.status(),
        503,
        "the maintenance layer is not mounted in the test router"
    );
}

#[tokio::test]
async fn maintenance_mode_allows_lfs_download_but_rejects_upload() {
    let (base, db) = spawn_test_app_with_db().await;
    let admin = admin_token(&base, &db, "maint_lfs_admin").await;
    let (token, _) = register_full(&base, "maint_lfs_user", "maint_lfs_user@example.com").await;
    crate::common::create_repo(&base, &token, "lfs-maintenance").await;

    let endpoint = format!("{base}/api/v1/repos/maint_lfs_user/lfs-maintenance/lfs/objects/batch");
    let content = b"content fetched during maintenance";
    let oid = hex::encode(Sha256::digest(content));
    let request = |operation: &str| {
        serde_json::json!({
            "operation": operation,
            "objects": [{
                "oid": oid,
                "size": content.len()
            }]
        })
    };
    let client = reqwest::Client::new();

    // Put a real object behind the read before closing the write gate. That
    // makes the later 200 prove the whole LFS download path, not merely that
    // maintenance let a request reach an early "object missing" branch.
    let prepare = client
        .post(&endpoint)
        .bearer_auth(&token)
        .json(&request("upload"))
        .send()
        .await
        .unwrap();
    assert_eq!(prepare.status(), 200);
    let prepare: serde_json::Value = prepare.json().await.unwrap();
    let upload_href = prepare["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap();
    let stored = client
        .put(upload_href)
        .body(content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(stored.status(), 200);

    assert_eq!(set_maintenance(&base, &admin, true).await.status(), 200);

    let download = client
        .post(&endpoint)
        .bearer_auth(&token)
        .json(&request("download"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        download.status(),
        200,
        "LFS batch download is a read even though the protocol uses POST"
    );

    let upload = client
        .post(&endpoint)
        .bearer_auth(&token)
        .json(&request("upload"))
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), 503, "LFS batch upload is a write");
    assert!(upload.headers().contains_key("retry-after"));
    let body: serde_json::Value = upload.json().await.unwrap();
    assert_eq!(body["error"]["code"], "MAINTENANCE_MODE");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maintenance_mode_allows_http_clone_and_fetch_but_rejects_push_with_503() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let state = build_test_app_state(db.clone(), repo_root.clone());
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;

    let admin = admin_token(&base, &db, "maint_git_admin").await;
    let (token, _) = register_full(&base, "maint_git_owner", "maint_git_owner@example.com").await;
    let create = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "transport",
            "is_private": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), 201);
    let pat = create_pat(&base, &token).await;

    let bare = repo_root.join("maint_git_owner/transport.git");
    let bare_str = bare.to_string_lossy().to_string();
    let seed = tempfile::tempdir().unwrap();
    git(&["init", "--initial-branch=main"], Some(seed.path()));
    git(
        &["config", "user.name", "Maintenance Test"],
        Some(seed.path()),
    );
    git(
        &["config", "user.email", "maintenance@example.com"],
        Some(seed.path()),
    );
    std::fs::write(seed.path().join("README.md"), "before maintenance\n").unwrap();
    git(&["add", "."], Some(seed.path()));
    git(&["commit", "-m", "seed"], Some(seed.path()));
    git(&["push", &bare_str, "main"], Some(seed.path()));
    git(
        &[
            "--git-dir",
            &bare_str,
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
        None,
    );

    assert_eq!(set_maintenance(&base, &admin, true).await.status(), 200);

    let url = format!("{base}/maint_git_owner/transport.git");
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("maint_git_owner:{pat}"));
    let auth_header = format!("Authorization: Basic {basic}");
    let auth_config = format!("http.extraHeader={auth_header}");
    let checkout_parent = tempfile::tempdir().unwrap();
    let checkout = checkout_parent.path().join("checkout");
    git(
        &[
            "-c",
            &auth_config,
            "clone",
            &url,
            &checkout.to_string_lossy(),
        ],
        None,
    );
    git(
        &["config", "http.extraHeader", &auth_header],
        Some(&checkout),
    );

    std::fs::write(
        seed.path().join("README.md"),
        "fetched during maintenance\n",
    )
    .unwrap();
    git(&["add", "README.md"], Some(seed.path()));
    git(
        &["commit", "-m", "server-side fixture update"],
        Some(seed.path()),
    );
    git(&["push", &bare_str, "main"], Some(seed.path()));
    let expected_remote = git(&["rev-parse", "HEAD"], Some(seed.path()));

    git(&["fetch", "origin"], Some(&checkout));
    let fetched_remote = git(&["rev-parse", "origin/main"], Some(&checkout));
    assert_eq!(
        fetched_remote, expected_remote,
        "fetch must receive the new upload-pack response"
    );

    git(
        &["config", "user.name", "Maintenance Client"],
        Some(&checkout),
    );
    git(
        &["config", "user.email", "maintenance-client@example.com"],
        Some(&checkout),
    );
    std::fs::write(checkout.join("blocked.txt"), "must not land\n").unwrap();
    git(&["add", "blocked.txt"], Some(&checkout));
    git(&["commit", "-m", "blocked client write"], Some(&checkout));

    let push = git_output(
        &["push", "origin", "HEAD:refs/heads/blocked-by-maintenance"],
        Some(&checkout),
    );
    assert!(!push.success(), "git push unexpectedly succeeded");
    assert!(
        push.stderr_str().contains("503"),
        "git client did not receive the maintenance status: {}",
        push.stderr_str()
    );
    let landed = git_output(
        &[
            "--git-dir",
            &bare_str,
            "show-ref",
            "--verify",
            "refs/heads/blocked-by-maintenance",
        ],
        None,
    );
    assert!(!landed.success(), "the rejected push still created its ref");

    server.abort();
}
