//! One read gate for every repository-scoped read endpoint.
//!
//! The gate used to be copy-pasted per module (`issues.rs`, `ci.rs`,
//! `repo_content.rs`, `packages.rs`, `archive.rs`, `ai.rs`), and the copies had
//! drifted: some of them authenticated through `extract_bearer_claims`, which
//! ignores the HttpOnly session cookie the web UI logs in with, so the very
//! owner of a private repository was answered `401` on their own issues while
//! the neighbouring endpoint let them in.
//!
//! These tests pin the three answers the shared gate owes a private repo —
//! `401` anonymous, `403` outsider, "not a denial" for the owner — across the
//! endpoints that used to hold their own copy, and they pin them for a
//! cookie-authenticated session specifically.

use crate::common::{register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

async fn create_private_repo(base: &str, token: &str, name: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_private_repo '{name}' failed");
}

/// Every read endpoint that used to carry its own copy of the gate.
fn read_endpoints(base: &str, owner: &str, repo: &str) -> Vec<String> {
    vec![
        format!("{base}/api/v1/repos/{owner}/{repo}/issues"),
        format!("{base}/api/v1/repos/{owner}/{repo}/pipelines"),
        format!("{base}/api/v1/repos/{owner}/{repo}/tree"),
        format!("{base}/api/v1/repos/{owner}/{repo}/packages"),
        format!("{base}/api/v1/repos/{owner}/{repo}/archive/main.zip"),
        format!("{base}/api/v1/ai/repos/{owner}/{repo}/summary"),
    ]
}

async fn status(url: &str, auth: Option<(&str, bool)>) -> reqwest::StatusCode {
    let mut req = reqwest::Client::new().get(url);
    match auth {
        // (token, via_cookie): the web UI sends the HttpOnly cookie, API
        // clients send the bearer header — both are the same session.
        Some((token, true)) => req = req.header("cookie", format!("plombir_git_token={token}")),
        Some((token, false)) => req = req.bearer_auth(token),
        None => {}
    }
    req.send().await.unwrap().status()
}

async fn write_file_status(
    base: &str,
    owner: &str,
    repo: &str,
    token: &str,
) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/contents/README.md"
        ))
        .header("cookie", format!("plombir_git_token={token}"))
        .json(&serde_json::json!({
            "content": "# cookie write\n",
            "message": "write through cookie session",
        }))
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn private_repo_reads_reject_anonymous_and_outsiders_everywhere() {
    let base = spawn_test_app().await;
    let owner = "readgateowner";
    let outsider = "readgateoutsider";
    let repo = "readgaterepo";

    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let outsider_token =
        register_user(&base, outsider, &format!("{outsider}@example.com"), PW).await;
    create_private_repo(&base, &owner_token, repo).await;

    for url in read_endpoints(&base, owner, repo) {
        assert_eq!(
            status(&url, None).await,
            401,
            "anonymous read of a private repo must be 401 at {url}"
        );
        assert_eq!(
            status(&url, Some((&outsider_token, false))).await,
            403,
            "outsider read of a private repo must be 403 at {url}"
        );
    }
}

#[tokio::test]
async fn cookie_session_reads_its_own_private_repo() {
    let base = spawn_test_app().await;
    let owner = "cookiegateowner";
    let repo = "cookiegaterepo";

    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    create_private_repo(&base, &owner_token, repo).await;

    for url in read_endpoints(&base, owner, repo) {
        for (label, auth) in [
            ("cookie", (owner_token.as_str(), true)),
            ("bearer", (owner_token.as_str(), false)),
        ] {
            let status = status(&url, Some(auth)).await;
            // The endpoints answer differently once past the gate (an empty
            // list, a 404 for an archive of a repo with no commits, a 501 for
            // an unimplemented AI route) — the assertion is only that the gate
            // itself did not deny the repository's own owner.
            assert!(
                status != 401 && status != 403,
                "{label} session of the owner was denied ({status}) at {url}"
            );
        }
    }
}

#[tokio::test]
async fn cookie_session_writes_its_own_private_repo_contents() {
    let base = spawn_test_app().await;
    let owner = "cookiewriteowner";
    let repo = "cookiewriterepo";

    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    create_private_repo(&base, &owner_token, repo).await;

    let status = write_file_status(&base, owner, repo, &owner_token).await;
    assert_eq!(
        status, 200,
        "cookie session of the owner must be accepted by the contents write gate"
    );
}
