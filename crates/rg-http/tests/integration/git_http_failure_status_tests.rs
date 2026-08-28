//! Regression coverage for card_de1377f8da43: the git smart-HTTP endpoints must
//! not answer a *failed lookup* with "no such repository", and must not hand the
//! git client the internals of the failure.
//!
//! `check_git_access` had one catch-all error arm that turned every failure of
//! `can_read` / `can_write` into `404 repository not found: <the whole anyhow
//! chain>`. Two defects rode in that one line:
//!
//! * A `404` tells git the repository is gone. A human goes and checks the URL
//!   and their permissions instead of waiting, and CI, mirrors and cron pushes
//!   do not retry a `404` at all — the job just fails.
//! * The body carried `db: find repo by owner and name`, the internal context of
//!   the operation. The git transport writes its own bodies, so nothing
//!   sanitizes them the way `IntoResponse for AppError` sanitizes the JSON API
//!   (H-05).
//!
//! The differential sweep (`failure_semantics_sweep_tests`) is what found this,
//! and it guards the status half for every route at once. It never looks at a
//! body, which is why the leak half is asserted here.

use crate::common::fault::{drop_every_table_except, spawn_test_app_for_fault_sweep, AUTH_TABLES};
use crate::common::register_user;

const PW: &str = "Qz7$wRtm";
const OWNER: &str = "gitfail";
const REPO: &str = "gitfailrepo";

/// One smart-HTTP pack request, independently spelled the way `routes.rs`
/// promises it. Keeping the method with the route matters: an authentication
/// layer can answer `401` before Axum reports a wrong method as `405`.
struct PackRoute {
    method: reqwest::Method,
    route: &'static str,
}

/// Both mounts of both services. The table is executable rather than an
/// inventory annotation: [`pack_routes`] derives the requests below from these
/// exact rows, so a path or method drift fails the live test and the UI
/// inventory oracle together.
const PACK_ROUTES: [PackRoute; 4] = [
    PackRoute {
        method: reqwest::Method::POST,
        route: "/git/{owner}/{repo}/git-upload-pack",
    },
    PackRoute {
        method: reqwest::Method::POST,
        route: "/git/{owner}/{repo}/git-receive-pack",
    },
    PackRoute {
        method: reqwest::Method::POST,
        route: "/{owner}/{repo}/git-upload-pack",
    },
    PackRoute {
        method: reqwest::Method::POST,
        route: "/{owner}/{repo}/git-receive-pack",
    },
];

fn pack_routes(base: &str) -> Vec<(reqwest::Method, String)> {
    PACK_ROUTES
        .iter()
        .map(|row| {
            let path = row.route.replace("{owner}", OWNER).replace("{repo}", REPO);
            (row.method.clone(), format!("{base}{path}"))
        })
        .collect()
}

/// `info/refs` shares `check_git_access` with the two pack routes, and it is the
/// *first* request of every clone — so it is the one a user actually sees. The
/// sweep cannot see it: it drives every route without a query string, and this
/// one answers `400 invalid or missing service parameter` to that, so it never
/// gets a healthy baseline to compare against.
fn info_refs_routes(base: &str) -> Vec<String> {
    vec![
        format!("{base}/git/{OWNER}/{REPO}/info/refs?service=git-upload-pack"),
        format!("{base}/{OWNER}/{REPO}/info/refs?service=git-upload-pack"),
    ]
}

/// Fragments of our own plumbing that must never reach a git client. The first
/// is the exact string the card was filed on; the rest are the neighbouring
/// shapes the same body would carry if the sanitization were dropped again.
const INTERNAL_FRAGMENTS: &[&str] = &[
    "db:",
    "find repo by owner and name",
    "no such table",
    "sqlx",
    "Execution Error",
];

fn assert_no_internal_detail(url: &str, status: reqwest::StatusCode, body: &str) {
    for fragment in INTERNAL_FRAGMENTS {
        assert!(
            !body.contains(fragment),
            "{url} answered {status} with our internals in the body: found '{fragment}' \
             in {body:?}. The git transport writes its own bodies, so the detail has to \
             be dropped at the return path — it belongs in the log."
        );
    }
}

/// A database that cannot answer "does this repository exist?" is a failure of
/// ours: 5xx, retryable, and nothing of ours in the body.
#[tokio::test]
async fn a_dead_database_is_not_a_missing_repository_over_git_http() {
    let app = spawn_test_app_for_fault_sweep().await;
    let token = register_user(&app.base, OWNER, &format!("{OWNER}@example.com"), PW).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/repos", app.base))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": REPO, "is_private": false}))
        .send()
        .await
        .expect("create repo");
    assert_eq!(resp.status(), 201, "fixture: creating '{REPO}' failed");

    // Baseline first: with everything healthy these routes reach git, so a 5xx
    // below is the fault talking and not a route that is broken anyway.
    for (method, url) in pack_routes(&app.base) {
        let resp = client
            .request(method, &url)
            .bearer_auth(&token)
            .send()
            .await
            .expect("healthy request");
        assert!(
            resp.status().is_success(),
            "fixture: {url} answered {} on a healthy server, so the fault pass below \
             would prove nothing",
            resp.status()
        );
    }
    for url in info_refs_routes(&app.base) {
        let resp = client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .expect("healthy request");
        assert!(
            resp.status().is_success(),
            "fixture: {url} answered {} on a healthy server",
            resp.status()
        );
    }

    // The bare repository stays on disk — the handlers guard on `exists()`
    // first, and a removed directory would exercise the honest 404 instead.
    let dropped = drop_every_table_except(&app.db, AUTH_TABLES).await;
    assert!(
        dropped > 40,
        "only {dropped} table(s) dropped — the database is not broken"
    );

    for (method, url) in pack_routes(&app.base) {
        let resp = client
            .request(method, &url)
            .bearer_auth(&token)
            .send()
            .await
            .expect("request under the fault");
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        assert!(
            status.is_server_error(),
            "{url} answered {status} while the database was gone. A git client does not \
             retry a 4xx: a clone stops and a mirror push fails for good. (body: {body:?})"
        );
        assert_no_internal_detail(&url, status, &body);
    }
    for url in info_refs_routes(&app.base) {
        let resp = client
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .expect("request under the fault");
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        assert!(
            status.is_server_error(),
            "{url} answered {status} while the database was gone — and this is the very \
             first request of a clone. (body: {body:?})"
        );
        assert_no_internal_detail(&url, status, &body);
    }
}

/// The other half of the split: a repository that genuinely is not in the
/// database must still be a `404`, or the fix above would have bought the retry
/// semantics by making every missing repository page an operator.
#[tokio::test]
async fn a_repository_that_is_really_gone_is_still_a_404_over_git_http() {
    let app = spawn_test_app_for_fault_sweep().await;
    let token = register_user(&app.base, OWNER, &format!("{OWNER}@example.com"), PW).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/repos", app.base))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": REPO, "is_private": false}))
        .send()
        .await
        .expect("create repo");
    assert_eq!(resp.status(), 201, "fixture: creating '{REPO}' failed");

    // Soft-delete the row while the bare directory stays put: the `exists()`
    // guard passes, so the request reaches the lookup and the *lookup* is what
    // reports the repository absent. That is the arm under test.
    use sea_orm::ConnectionTrait;
    app.db
        .execute_unprepared(
            "UPDATE repositories SET deleted_at = '2020-01-01 00:00:00' WHERE name = 'gitfailrepo'",
        )
        .await
        .expect("soft-delete the repository row");
    assert!(
        app.repo_root.join(format!("{OWNER}/{REPO}.git")).is_dir(),
        "fixture: the bare repository must survive the soft delete"
    );

    let pack = pack_routes(&app.base);
    let refs = info_refs_routes(&app.base)
        .into_iter()
        .map(|url| (reqwest::Method::GET, url));
    for (method, url) in pack.into_iter().chain(refs) {
        let resp = client
            .request(method, &url)
            .bearer_auth(&token)
            .send()
            .await
            .expect("request for the deleted repository");
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        assert_eq!(
            status, 404,
            "{url}: a repository that is really gone must stay a 404 (body: {body:?})"
        );
        assert_eq!(
            body.trim(),
            "repository not found",
            "{url}: the 404 body must be the fixed message with no lookup detail in it"
        );
    }
}
