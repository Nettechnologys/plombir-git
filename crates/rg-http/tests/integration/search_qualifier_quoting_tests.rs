//! Regression coverage for card_38338c84218d: `/search` could not be asked
//! about a label whose name contains a space.
//!
//! `SearchFilters::parse` cut the raw query on whitespace and only then looked
//! for a `:` and stripped quotes from each piece, so `label:"good first issue"`
//! — the label GitHub creates by default — became the filter `label = good`
//! plus the leftover text `first issue"` in the full-text half of the query.
//! The response was a `200` carrying either nothing or somebody else's issues,
//! and nothing in it said the server had answered a different question.
//!
//! The corpus below is built so both halves of that failure are visible from
//! the outside: a label named `good` exists next to `good first issue`, and
//! neither issue title contains any of the words in either label name. A
//! parser that keeps the first word filters by the wrong label; one that lets
//! the remainder reach the FTS predicate finds nothing at all.

use crate::common::{register_full, spawn_test_app};

const OWNER: &str = "quoting-owner";
const REPO: &str = "qualifier-lab";

/// The label name the whole card is about, and the prefix that used to be
/// mistaken for it.
const SPACED_LABEL: &str = "good first issue";
const PREFIX_LABEL: &str = "good";

async fn create_repo(base: &str, token: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": REPO,
            "description": "valve bench",
            "is_private": false,
        }))
        .send()
        .await
        .expect("create repo");
    assert_eq!(resp.status(), 201, "create repo");
}

async fn create_label(base: &str, token: &str, name: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/labels"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "color": "#0e8a16"}))
        .send()
        .await
        .expect("create label");
    assert_eq!(resp.status(), 201, "create label {name:?}");
}

/// File an issue carrying exactly one label, and return its number.
async fn create_issue(base: &str, token: &str, title: &str, label: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/issues"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "title": title,
            "body": "the bench log is attached",
            "labels": [label],
        }))
        .send()
        .await
        .expect("create issue");
    assert_eq!(resp.status(), 201, "create issue {title:?}");
    let body: serde_json::Value = resp.json().await.expect("issue json");
    body["number"].as_i64().expect("issue number")
}

/// Run one search and return the issue numbers it answered with.
async fn search_issue_numbers(base: &str, token: &str, q: &str) -> Vec<i64> {
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/search"))
        .query(&[("q", q), ("type", "issues"), ("per_page", "50")])
        .bearer_auth(token)
        .send()
        .await
        .expect("search request");
    assert_eq!(resp.status(), 200, "search {q:?}");
    let body: serde_json::Value = resp.json().await.expect("search json");
    body["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|result| result["number"].as_i64().expect("result number"))
        .collect()
}

/// A label name with a space in it must be addressable, and addressing it must
/// not also drag the rest of the name into the text search.
#[tokio::test]
async fn a_quoted_label_qualifier_addresses_the_label_with_the_space_in_it() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, OWNER, "quoting-owner@example.com").await;
    create_repo(&base, &token).await;
    create_label(&base, &token, SPACED_LABEL).await;
    create_label(&base, &token, PREFIX_LABEL).await;

    // Neither title shares a word with either label name, so a leftover
    // `first issue"` in the FTS predicate excludes both issues.
    let spaced = create_issue(&base, &token, "tank valve replacement", SPACED_LABEL).await;
    let prefix = create_issue(&base, &token, "tank valve inspection", PREFIX_LABEL).await;

    let found = search_issue_numbers(&base, &token, r#"label:"good first issue""#).await;
    assert_eq!(
        found,
        vec![spaced],
        "`label:{SPACED_LABEL:?}` must answer with the issue carrying that label \
         and nothing else — issue #{prefix} carries `{PREFIX_LABEL}`"
    );
}

/// The spelling without quotes is the one already in use, and it still means
/// the label it names — not the one whose name merely starts with it.
#[tokio::test]
async fn an_unquoted_label_qualifier_still_names_its_own_label() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, OWNER, "quoting-owner@example.com").await;
    create_repo(&base, &token).await;
    create_label(&base, &token, SPACED_LABEL).await;
    create_label(&base, &token, PREFIX_LABEL).await;

    let spaced = create_issue(&base, &token, "tank valve replacement", SPACED_LABEL).await;
    let prefix = create_issue(&base, &token, "tank valve inspection", PREFIX_LABEL).await;

    let found = search_issue_numbers(&base, &token, "label:good").await;
    assert_eq!(
        found,
        vec![prefix],
        "`label:good` must stay an exact label match, not a prefix of #{spaced}"
    );
}

/// The other qualifiers travel the same parser, so the documented example has
/// to keep working next to a quoted one.
#[tokio::test]
async fn the_documented_qualifier_example_survives_the_quote_aware_parser() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, OWNER, "quoting-owner@example.com").await;
    create_repo(&base, &token).await;
    create_label(&base, &token, SPACED_LABEL).await;

    let spaced = create_issue(&base, &token, "tank valve replacement", SPACED_LABEL).await;

    let query = format!(r#"valve label:"good first issue" repo:{OWNER}/{REPO} state:open"#);
    let found = search_issue_numbers(&base, &token, &query).await;
    assert_eq!(
        found,
        vec![spaced],
        "the text, the quoted label, the repo and the state must all still apply: {query}"
    );
}
