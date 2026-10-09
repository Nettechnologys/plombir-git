//! Regression coverage for card_ed5ae32fbde9: `/search?type=all` used to apply
//! the page's `offset`/`limit` twice — once in each backend's SQL, and once
//! more over their concatenation.
//!
//! The second cut is not a sort-order complaint. With more matching
//! repositories than fit on a page, page one kept the repositories and dropped
//! everything the issue and wiki backends had returned; on page two those rows
//! had moved out of the window, so the first `per_page` issues were served by
//! no page at all — while `total`, summed from the three counts, kept promising
//! them. A `200` the whole way, with matches unreachable behind it.
//!
//! So the checks here are about the walk, not about a single response: page
//! through `type=all` to the end and require that the matches handed out are
//! each handed out once, and that there are exactly `total` of them.

use crate::common::{register_full, spawn_test_app};

const TERM: &str = "zebrafish";

async fn create_repo(base: &str, token: &str, name: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "description": format!("{TERM} research"),
            "is_private": false,
        }))
        .send()
        .await
        .expect("create repo");
    assert_eq!(resp.status(), 201, "create_repo({name})");
}

async fn create_issue(base: &str, token: &str, owner: &str, repo: &str, n: usize) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/issues"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "title": format!("{TERM} defect {n}"),
            "body": format!("the {TERM} tank leaks"),
        }))
        .send()
        .await
        .expect("create issue");
    assert_eq!(resp.status(), 201, "create_issue({n})");
}

async fn create_wiki_page(base: &str, token: &str, owner: &str, repo: &str, n: usize) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/wiki"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "title": format!("{TERM} runbook {n}"),
            "content": format!("how to feed the {TERM}"),
        }))
        .send()
        .await
        .expect("create wiki page");
    assert_eq!(resp.status(), 201, "create_wiki_page({n})");
}

/// One search response: `(total, [(result_type, id)])`.
async fn search_page(
    base: &str,
    token: &str,
    search_type: &str,
    page: u64,
    per_page: u64,
) -> (i64, Vec<(String, i64)>) {
    let resp = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/search?q={TERM}&type={search_type}&page={page}&per_page={per_page}"
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("search request");
    assert_eq!(resp.status(), 200, "search page {page}");
    let body: serde_json::Value = resp.json().await.expect("search json");
    let total = body["total"].as_i64().expect("total");
    let results = body["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|result| {
            (
                result["result_type"]
                    .as_str()
                    .expect("result_type")
                    .to_string(),
                result["id"].as_i64().expect("id"),
            )
        })
        .collect();
    (total, results)
}

/// Corpus: 5 repositories, 4 issues, 3 wiki pages, all matching `TERM`. The
/// repositories alone outnumber the page size on every page size used below —
/// the shape that used to make the other two kinds unreachable.
async fn seed(base: &str, token: &str, owner: &str) -> i64 {
    for n in 0..5 {
        create_repo(base, token, &format!("{TERM}-lab-{n}")).await;
    }
    let repo = format!("{TERM}-lab-0");
    for n in 0..4 {
        create_issue(base, token, owner, &repo, n).await;
    }
    for n in 0..3 {
        create_wiki_page(base, token, owner, &repo, n).await;
    }
    5 + 4 + 3
}

#[tokio::test]
async fn type_all_pages_reach_every_match_exactly_once() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "paging-owner", "paging-owner@example.com").await;
    let expected_total = seed(&base, &token, "paging-owner").await;

    for per_page in [2u64, 3, 5] {
        let mut seen: Vec<(String, i64)> = Vec::new();
        let mut total = 0i64;

        for page in 1..=20u64 {
            let (page_total, results) = search_page(&base, &token, "all", page, per_page).await;
            total = page_total;
            if results.is_empty() {
                break;
            }
            let pages_before = page - 1;
            let remaining = total - (pages_before * per_page) as i64;
            assert_eq!(
                results.len() as i64,
                remaining.min(per_page as i64),
                "per_page={per_page} page={page}: a page that is neither full nor last"
            );
            seen.extend(results);
        }

        assert_eq!(
            total, expected_total,
            "per_page={per_page}: total is not the size of the corpus"
        );

        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            seen.len(),
            "per_page={per_page}: a match was served on two pages: {seen:?}"
        );
        assert_eq!(
            unique.len() as i64,
            total,
            "per_page={per_page}: walking every page served {} of the {total} matches \
             `total` promised — {unique:?}",
            unique.len()
        );
    }
}

#[tokio::test]
async fn the_first_page_of_type_all_is_not_one_kind_only() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "mix-owner", "mix-owner@example.com").await;
    seed(&base, &token, "mix-owner").await;

    // Three repositories match before the page is full, so a page cut from the
    // concatenation would be repositories and nothing else.
    let (_, results) = search_page(&base, &token, "all", 1, 3).await;
    let kinds: Vec<&str> = results.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["repo", "issue", "wiki"],
        "page one must draw from every kind that has matches"
    );
}

/// The page number used to be multiplied into an offset unchanged, so
/// `page=u64::MAX` asked the database for an `OFFSET` beyond `i64::MAX` and
/// came back a 500. It must answer an empty page instead.
#[tokio::test]
async fn a_page_beyond_the_i64_ceiling_is_an_empty_page() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "huge-page-owner", "huge-page-owner@example.com").await;
    seed(&base, &token, "huge-page-owner").await;

    let resp = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/search?q={TERM}&type=all&page=18446744073709551615"
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("search with a huge page");
    assert_eq!(
        resp.status(),
        200,
        "a page number beyond the offset ceiling must be an empty page, not an error"
    );
    let body: serde_json::Value = resp.json().await.expect("search json");
    assert_eq!(
        body["results"].as_array().map(Vec::len),
        Some(0),
        "no row lives at page u64::MAX"
    );
}

#[tokio::test]
async fn a_single_kind_search_still_pages_the_way_it_did() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "single-owner", "single-owner@example.com").await;
    seed(&base, &token, "single-owner").await;

    for (search_type, expected_total) in [("repos", 5i64), ("issues", 4), ("wiki", 3)] {
        let mut seen: Vec<(String, i64)> = Vec::new();
        let mut total = 0i64;
        for page in 1..=10u64 {
            let (page_total, results) = search_page(&base, &token, search_type, page, 2).await;
            total = page_total;
            if results.is_empty() {
                break;
            }
            seen.extend(results);
        }
        assert_eq!(total, expected_total, "{search_type}: total");

        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), seen.len(), "{search_type}: duplicate match");
        assert_eq!(
            unique.len() as i64,
            total,
            "{search_type}: matches served != total"
        );
    }
}
