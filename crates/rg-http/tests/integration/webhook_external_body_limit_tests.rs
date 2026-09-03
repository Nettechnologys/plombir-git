//! The declared ceiling on an inbound external-CI webhook body.
//!
//! `POST /api/v1/repos/{owner}/{name}/webhooks/external/ci` reads its body as
//! raw `Bytes` — the HMAC has to be computed over the exact wire bytes, before
//! serde sees them — so the extractor buffers. The route used to be mounted
//! with no wrapper at all, which is not "no limit": a buffered extractor
//! inherits Axum's 2 MiB `DefaultBodyLimit`, so any account holding `RepoWrite`
//! could make the server hold 2 MiB, run HMAC-SHA256 over all of it and hand it
//! to serde, for a request whose legitimate size is measured in hundreds of
//! bytes. The ceiling is declared now, and this is both ends of it.

use crate::common::{create_repo, register_user, spawn_test_app_with_db};

/// The ceiling `api::webhooks_external::EXTERNAL_CI_WEBHOOK_MAX_BYTES`
/// declares. Spelled out rather than imported, for the reason
/// `ci_job_log_boundary_tests` spells its own out: these tests drive the server
/// over HTTP the way an external CI system does, and such a client knows the
/// number only from the contract.
const EXTERNAL_CI_WEBHOOK_MAX_BYTES: usize = 64 * 1024;

/// A repository whose owner may write commit statuses on it, plus the URL the
/// webhook is posted to.
async fn seed(base: &str, who: &str) -> (String, String) {
    let token = register_user(base, who, &format!("{who}@example.com"), "Qz7$wRtm").await;
    create_repo(base, &token, &format!("{who}-repo")).await;
    (
        token,
        format!("{base}/api/v1/repos/{who}/{who}-repo/webhooks/external/ci"),
    )
}

#[tokio::test]
async fn an_ordinary_commit_status_webhook_still_passes() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, url) = seed(&base, "hooklimitok").await;

    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(r#"{"context":"jenkins/pipe","state":"success"}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        200,
        "the payload this endpoint exists for is a few hundred bytes; the ceiling must not touch it"
    );
}

#[tokio::test]
async fn a_body_over_the_declared_ceiling_is_refused_before_the_hmac_runs() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, url) = seed(&base, "hooklimitover").await;

    // Valid JSON of the shape the handler accepts, padded past the ceiling. The
    // refusal has to come from the declared limit rather than from serde, so
    // the body is one this endpoint would otherwise process.
    let padding = "x".repeat(EXTERNAL_CI_WEBHOOK_MAX_BYTES);
    let body =
        format!(r#"{{"context":"jenkins/pipe","state":"success","description":"{padding}"}}"#);
    assert!(body.len() > EXTERNAL_CI_WEBHOOK_MAX_BYTES);

    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        413,
        "a body over the declared ceiling is refused as oversized, not accepted and not a 400"
    );

    // The number lives at the mount, so the refusal has to carry it back: a
    // bare 413 tells an operator wiring up their CI nothing about what to fit
    // under. `transport_refusal_envelope` reads `DeclaredBodyLimit` off the
    // response the limit layers produced.
    let payload = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(payload["error"]["code"], "PAYLOAD_TOO_LARGE");
    let message = payload["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("64 KiB"),
        "the refusal must name the limit this route declares, got: {message}"
    );
}

#[tokio::test]
async fn a_body_under_the_ceiling_but_over_axums_hidden_default_is_not_the_boundary() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, url) = seed(&base, "hooklimitmid").await;

    // The declared ceiling is *below* Axum's 2 MiB default, so the interesting
    // direction here is the opposite of the job log's: this asserts the ceiling
    // that is enforced is the small declared one and not the inherited default,
    // which would have accepted this body.
    let padding = "x".repeat(1024 * 1024);
    let body =
        format!(r#"{{"context":"jenkins/pipe","state":"success","description":"{padding}"}}"#);
    assert!(body.len() < 2 * 1024 * 1024);

    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        413,
        "1 MiB clears Axum's hidden default but not the 64 KiB this route declares"
    );
}
