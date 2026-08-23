//! What the commit badge on a repository page reads: the per-context status
//! list and the single rolled-up verdict beside a commit.
//!
//! Writing a status was reachable from the CI integration; *reading* one was
//! named by no test at all, in either spelling. The two routes answer the same
//! rows differently — `/statuses` is the per-context list a person opens to see
//! which check failed, `/status` is the one word a merge decision is taken on —
//! so a roll-up that loses a failing check turns a red commit green on the very
//! screen the reviewer looks at, while the list next to it still shows the
//! failure.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

const SHA: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c";

async fn post_status(base: &str, token: &str, state: &str, context: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/signal-owner/signal-repo/statuses/{SHA}"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"state": state, "context": context}))
        .send()
        .await
        .expect("post commit status")
        .status()
}

async fn combined(base: &str, token: &str, sha: &str) -> serde_json::Value {
    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/signal-owner/signal-repo/commits/{sha}/status"
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("read the combined status");
    assert_eq!(response.status(), 200);
    response.json().await.expect("the roll-up decodes")
}

async fn statuses(base: &str, token: &str, sha: &str) -> Vec<serde_json::Value> {
    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/signal-owner/signal-repo/commits/{sha}/statuses"
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("read the status list");
    assert_eq!(response.status(), 200);
    response.json().await.expect("the list decodes")
}

#[tokio::test]
async fn the_commit_badge_reports_every_check_and_the_worst_of_them() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (owner_token, _owner_id) =
        register_full(&base, "signal-owner", "signal-owner@example.com").await;
    create_repo(&base, &owner_token, "signal-repo").await;

    // A commit nothing has reported on is not a green commit. The roll-up says
    // `pending` over an empty list, and a badge that read the absence as
    // success would wave through a commit no check has ever seen.
    let untouched = combined(&base, &owner_token, SHA).await;
    assert_eq!(untouched["state"], "pending");
    assert_eq!(untouched["total_count"], 0);
    assert!(untouched["statuses"].as_array().unwrap().is_empty());

    assert_eq!(
        post_status(&base, &owner_token, "success", "build").await,
        201
    );
    assert_eq!(
        post_status(&base, &owner_token, "pending", "test").await,
        201
    );

    let listed = statuses(&base, &owner_token, SHA).await;
    assert_eq!(listed.len(), 2, "both checks are listed: {listed:?}");
    let mut contexts: Vec<&str> = listed
        .iter()
        .map(|s| s["context"].as_str().expect("each row names its check"))
        .collect();
    contexts.sort_unstable();
    assert_eq!(contexts, ["build", "test"]);

    let pending = combined(&base, &owner_token, SHA).await;
    assert_eq!(
        pending["state"], "pending",
        "one check still running holds the whole commit at pending"
    );
    assert_eq!(pending["total_count"], 2);

    // A second report on the same context replaces the first rather than piling
    // up — otherwise a re-run would leave its own stale failure behind.
    assert_eq!(
        post_status(&base, &owner_token, "failure", "test").await,
        201
    );
    assert_eq!(
        statuses(&base, &owner_token, SHA).await.len(),
        2,
        "a re-run updates its check, it does not add another one"
    );
    let failed = combined(&base, &owner_token, SHA).await;
    assert_eq!(
        failed["state"], "failure",
        "one failing check is enough to make the commit red"
    );
    assert_eq!(failed["sha"], SHA);

    assert_eq!(
        post_status(&base, &owner_token, "success", "test").await,
        201
    );
    assert_eq!(
        combined(&base, &owner_token, SHA).await["state"],
        "success",
        "with every check green the commit is green"
    );

    // The state is a closed set; a typo must be refused rather than stored and
    // then silently counted as neither failing nor pending.
    assert_eq!(
        post_status(&base, &owner_token, "succes", "build").await,
        400
    );
}

#[tokio::test]
async fn a_private_repository_does_not_report_its_checks_to_strangers() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "signal-owner", "signal-owner@example.com").await;
    let (outsider_token, _outsider_id) =
        register_full(&base, "signal-outsider", "signal-outsider@example.com").await;
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"name": "signal-repo", "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    assert_eq!(
        post_status(&base, &owner_token, "success", "build").await,
        201
    );

    for url in [
        format!("{base}/api/v1/repos/signal-owner/signal-repo/commits/{SHA}/statuses"),
        format!("{base}/api/v1/repos/signal-owner/signal-repo/commits/{SHA}/status"),
    ] {
        let anonymous = client.get(&url).send().await.unwrap();
        assert_eq!(anonymous.status(), 401, "anonymous reader of {url}");

        // 403 rather than 404 on purpose: the repository gates answer an
        // authenticated outsider uniformly with a refusal, and that policy is
        // pinned next door by `repo_read_gate_tests`. Masking existence is the
        // organization gates' rule, not this one's (sol_c6cb909ed994).
        let outsider = client
            .get(&url)
            .bearer_auth(&outsider_token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            outsider.status(),
            403,
            "a private repository reports its checks to nobody else: {url}"
        );
    }
}
