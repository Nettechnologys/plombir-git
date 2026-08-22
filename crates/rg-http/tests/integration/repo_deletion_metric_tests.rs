//! card_a02cf2573184: `forgekeep_repos_deleted_total` must count every retired
//! repository, not only the ones a human deleted one at a time.
//!
//! The counter had a single producer and it sat in `delete_repo_handler`.
//! `rg_core::repo::service::delete_repo` is reached by two more live paths that
//! own no handler of their own — organization retirement
//! (`retire_org_repositories`) and account retirement
//! (`retire_account_repositories`) — so deleting an organization holding forty
//! repositories moved the `forgekeep_repositories` gauge by forty and the
//! counter by nothing. Two series describing the same event disagreed, and the
//! rate panel is built on the one that undercounted.
//!
//! The producer now sits on the funnel itself, which makes the second half of
//! this test necessary: the handler must no longer record the deletion a second
//! time on top of the service's.
//!
//! The assertion reads the number an operator's Prometheus would scrape, not an
//! observer the test installed for itself: the real registry, the real
//! `rg-core` → `rg-http` observer, and `GET /metrics`. That is the only wiring
//! in which "recorded twice" is distinguishable from "recorded once", because
//! the two producers reach the counter through different doors.
//!
//! Both halves live in one test on purpose. The registry is process-wide, so a
//! sibling test deleting a repository in the same process would be
//! indistinguishable from this one's own work.

use crate::common::{register_full, spawn_test_app_with_db};

/// The value an operator would scrape for `forgekeep_repos_deleted_total`.
///
/// A counter Prometheus has never been given a value for is absent from the
/// exposition entirely, which reads as zero.
async fn scraped_deletions(base: &str) -> u64 {
    let body = reqwest::Client::new()
        .get(format!("{base}/metrics"))
        .send()
        .await
        .expect("scrape /metrics")
        .text()
        .await
        .expect("read the exposition body");

    body.lines()
        .find_map(|line| line.strip_prefix("forgekeep_repos_deleted_total "))
        .map_or(0, |value| {
            value
                .trim()
                .parse::<f64>()
                .expect("the counter's exposed value is a number") as u64
        })
}

async fn create_org(base: &str, token: &str, name: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "display_name": name }))
        .send()
        .await
        .expect("create organization")
        .status();
    assert_eq!(status, 201, "baseline: the organization exists");
}

async fn create_repo_in(base: &str, token: &str, name: &str, org: Option<&str>) {
    let mut body = serde_json::json!({ "name": name });
    if let Some(org) = org {
        body["org"] = serde_json::json!(org);
    }
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("create repository")
        .status();
    assert_eq!(status, 201, "baseline: the repository exists");
}

#[tokio::test]
async fn every_path_that_retires_a_repository_counts_it_exactly_once() {
    // Both are idempotent installers; another test in this process may have
    // won the race, and either way the wiring under test is the same one
    // `run_with_listener` builds.
    if let Err(error) = rg_http::metrics::init_registry() {
        assert!(
            error.to_string().contains("already initialized"),
            "the metrics registry could not be built: {error}"
        );
    }
    rg_core::metrics_hook::set_repo_deleted_observer(rg_http::metrics::recorder::repo_deleted);

    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (token, _) = register_full(&base, "rdmowner", "rdmowner@example.invalid").await;

    // ── The path with no handler: one organization, three repositories. ──
    create_org(&base, &token, "rdm-org").await;
    for name in ["alpha", "beta", "gamma"] {
        create_repo_in(&base, &token, name, Some("rdm-org")).await;
    }

    let before_org_delete = scraped_deletions(&base).await;
    let status = client
        .delete(format!("{base}/api/v1/orgs/rdm-org"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete organization")
        .status();
    assert_eq!(status, 200, "baseline: the organization was deleted");

    assert_eq!(
        scraped_deletions(&base).await - before_org_delete,
        3,
        "deleting an organization retired three repositories; the deletion counter has to move by \
         three, the same as the repository gauge does"
    );

    // ── The path with a handler: exactly one, not two. ──
    create_repo_in(&base, &token, "solo", None).await;

    let before_handler_delete = scraped_deletions(&base).await;
    let status = client
        .delete(format!("{base}/api/v1/repos/rdmowner/solo"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete repository")
        .status();
    assert_eq!(status, 200, "baseline: the repository was deleted");

    assert_eq!(
        scraped_deletions(&base).await - before_handler_delete,
        1,
        "the REST handler must leave the recording to the service it calls; counting it in both \
         places doubles every deletion an operator performs by hand"
    );
}
