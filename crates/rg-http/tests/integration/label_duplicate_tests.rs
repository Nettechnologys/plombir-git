//! card_85e5dda6b3b1: a repeated label name is the caller's answer, not a 500.
//!
//! `create_label` had neither a pre-read nor a UNIQUE classification, so the
//! `DbErr` raised by `idx_labels_repo_name_unique` left unclassified and the API
//! answered `500` — for the single most ordinary mistake a labels UI can make.
//! Unlike the create paths guarded by a pre-check, this was not a race that only
//! a loser hits: *every* caller who reused a name landed on it. `update_label`
//! shared the defect one route over, because a rename lands on the same index.
//!
//! The junction-table half lives in `rg-db`, not here: no HTTP route takes
//! label *ids* — issue bodies carry label names, and the name-to-id resolution
//! in `issue::service` collapses repeats on its way to `set_labels`. See
//! `crates/rg-db/tests/issue_label_duplicates.rs`, which drives the op directly.
//!
//! Each conflict is paired with a request that must keep its old answer, so the
//! tests prove the outcomes stayed distinct rather than that everything now
//! answers 409.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
use reqwest::StatusCode;

const OWNER: &str = "label-dup-owner";
const REPO: &str = "label-dup-repo";

struct Fixture {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl Fixture {
    async fn new() -> Self {
        let (base, _db) = spawn_test_app_with_db().await;
        let (token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.test")).await;
        create_repo(&base, &token, REPO).await;
        Self {
            base,
            token,
            client: reqwest::Client::new(),
        }
    }

    fn labels_url(&self) -> String {
        format!("{}/api/v1/repos/{OWNER}/{REPO}/labels", self.base)
    }

    async fn create(&self, name: &str, color: &str) -> reqwest::Response {
        self.client
            .post(self.labels_url())
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "name": name, "color": color }))
            .send()
            .await
            .expect("create label")
    }

    async fn create_ok(&self, name: &str) -> i64 {
        let response = self.create(name, "#ff0000").await;
        assert_eq!(response.status(), StatusCode::CREATED, "creating {name}");
        response.json::<serde_json::Value>().await.expect("json")["id"]
            .as_i64()
            .expect("label id")
    }

    async fn rename(&self, id: i64, name: &str) -> reqwest::Response {
        self.client
            .patch(format!("{}/{id}", self.labels_url()))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .expect("rename label")
    }
}

/// The headline case: the second `POST` of a name this repository already uses.
#[tokio::test]
async fn a_repeated_label_name_is_a_conflict_not_a_server_failure() {
    let fixture = Fixture::new().await;
    fixture.create_ok("bug").await;

    let response = fixture.create("bug", "#00ff00").await;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert!(
        !status.is_server_error(),
        "reusing a label name is not the server breaking: {status} {body}"
    );
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert!(
        body.contains("bug"),
        "the answer must name the label that is in the way, got: {body}"
    );
    for leak in ["idx_labels", "UNIQUE", "db:", "constraint"] {
        assert!(
            !body.contains(leak),
            "the message reaches the client verbatim and must not carry {leak:?}: {body}"
        );
    }
}

/// The same name in a *different* repository is not a conflict — the index is
/// on `(repo_id, name)`, and the refusal must not become repository-wide.
#[tokio::test]
async fn the_same_label_name_in_another_repository_is_still_created() {
    let fixture = Fixture::new().await;
    fixture.create_ok("bug").await;
    create_repo(&fixture.base, &fixture.token, "label-dup-other").await;

    let response = fixture
        .client
        .post(format!(
            "{}/api/v1/repos/{OWNER}/label-dup-other/labels",
            fixture.base
        ))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "name": "bug", "color": "#ff0000" }))
        .send()
        .await
        .expect("create label");

    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "labels are scoped per repository"
    );
}

/// The other half of the claim: a genuinely malformed body on the same route
/// still answers 400, so 500 did not simply become 409 for everything.
#[tokio::test]
async fn a_malformed_label_is_still_a_bad_request() {
    let fixture = Fixture::new().await;

    assert_eq!(
        fixture.create("", "#ff0000").await.status(),
        StatusCode::BAD_REQUEST,
        "an empty name is the caller's to fix"
    );
    assert_eq!(
        fixture.create("colorless", "red").await.status(),
        StatusCode::BAD_REQUEST,
        "a colour that is not a hex string is the caller's to fix"
    );
}

/// A rename lands on the same unique index as a create, so it gets the same
/// answer — and a rename that collides with nothing still succeeds.
#[tokio::test]
async fn renaming_a_label_onto_an_existing_name_is_a_conflict() {
    let fixture = Fixture::new().await;
    fixture.create_ok("bug").await;
    let enhancement = fixture.create_ok("enhancement").await;

    let response = fixture.rename(enhancement, "bug").await;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert!(
        !status.is_server_error(),
        "renaming onto a taken name is not the server breaking: {status} {body}"
    );
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");

    assert_eq!(
        fixture.rename(enhancement, "feature").await.status(),
        StatusCode::OK,
        "a rename that collides with nothing still goes through"
    );
}

/// A `PATCH` that does not touch the name must not be able to reach the
/// conflict branch at all — the classification is armed by the rename, not by
/// every write to the row.
#[tokio::test]
async fn a_patch_that_does_not_rename_is_unaffected() {
    let fixture = Fixture::new().await;
    let bug = fixture.create_ok("bug").await;

    let response = fixture
        .client
        .patch(format!("{}/{bug}", fixture.labels_url()))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({ "color": "#00ff00" }))
        .send()
        .await
        .expect("recolour label");

    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("json");
    assert_eq!(body["color"], "#00ff00", "the colour was written: {body}");
    assert_eq!(body["name"], "bug", "the name was left alone: {body}");
}
