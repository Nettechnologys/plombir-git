//! card_1e3c1cff05b4: walking every page of the admin listings has to hand back
//! every row, once.
//!
//! `/admin/users` and `/admin/orgs` passed `PaginationParams::offset()` — a row
//! offset — into functions whose argument reached `sea_orm`'s
//! `Paginator::fetch_page`, which takes a 0-based *page index* and builds
//! `OFFSET page_size * page` itself. The unit was therefore squared: the real
//! SQL offset came out as `per_page² × (page − 1)`, so at `per_page = 20` page 2
//! asked for row 400.
//!
//! What an operator saw was `200`, a correct `total`, a `total_pages` computed
//! from it — and an empty `data` on every page but the first. At `per_page = 20`
//! that is 1/20 of the accounts on the instance; at `per_page = 100`, 1/100.
//!
//! Asserted by walking, not by spot-checking one page. `pagination_total_order`
//! already guards the *shape* of these queries (a total order), and it passed
//! throughout — the order was total and the argument was the wrong quantity.
//! Only walking the pages the response itself advertises catches that.

use std::collections::BTreeSet;

use crate::common::{register_full, spawn_test_app_with_db};

async fn promote_user_to_admin(db: &rg_db::DatabaseConnection, user_id: i64) {
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, Some(true), None)
        .await
        .expect("promote user to admin")
        .expect("registered user must exist");
}

/// Small enough that the seeded rows span several pages, so a boundary error
/// has somewhere to show.
const PER_PAGE: u64 = 3;

/// Deliberately not a multiple of `PER_PAGE`: the last page is a short one, and
/// a walk that miscounts it is a walk that would pass on round numbers.
const USERS: usize = 8;
const ORGS: usize = 7;

/// One page of a listing: the `id` of every row, plus what the response says
/// about the whole set.
struct Page {
    ids: Vec<i64>,
    total: u64,
    total_pages: u64,
}

async fn fetch_page(base: &str, path: &str, token: &str, page: u64) -> Page {
    let url = format!("{base}/api/v1/{path}?page={page}&per_page={PER_PAGE}");
    let response = reqwest::Client::new()
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .expect("send the listing request");
    let status = response.status();
    let body = response.text().await.expect("read the listing body");
    assert_eq!(status, 200, "GET {url} answered {status}: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).expect("listing body is JSON");

    Page {
        ids: json["data"]
            .as_array()
            .unwrap_or_else(|| panic!("GET {url} has no `data` array: {body}"))
            .iter()
            .map(|row| {
                row["id"]
                    .as_i64()
                    .unwrap_or_else(|| panic!("a row of {url} has no id: {row}"))
            })
            .collect(),
        total: json["pagination"]["total"]
            .as_u64()
            .unwrap_or_else(|| panic!("GET {url} has no `pagination.total`: {body}")),
        total_pages: json["pagination"]["total_pages"]
            .as_u64()
            .unwrap_or_else(|| panic!("GET {url} has no `pagination.total_pages`: {body}")),
    }
}

/// Walk `page=1..=total_pages` exactly as a client that trusts the response
/// would, and assert the union is the whole set with no row served twice.
async fn assert_the_walk_covers_everything(
    base: &str,
    path: &str,
    token: &str,
    expected: &BTreeSet<i64>,
) {
    let first = fetch_page(base, path, token, 1).await;
    assert_eq!(
        first.total as usize,
        expected.len(),
        "/{path} reports a `total` that is not the number of rows seeded — the \
         rest of this walk would be measured against the wrong number"
    );
    assert!(
        first.total_pages > 1,
        "/{path} fits in one page at per_page={PER_PAGE}, so walking it proves \
         nothing about page boundaries"
    );

    let mut seen = Vec::new();
    for page in 1..=first.total_pages {
        seen.extend(fetch_page(base, path, token, page).await.ids);
    }

    let unique: BTreeSet<i64> = seen.iter().copied().collect();
    assert_eq!(
        unique.len(),
        seen.len(),
        "/{path} served some row on more than one page: {seen:?}"
    );
    assert_eq!(
        &unique,
        expected,
        "walking every page of /{path} that the response itself advertises \
         ({} pages of {PER_PAGE}) returned {} of {} rows — the rest are \
         unreachable on every page, while `total` keeps reporting them",
        first.total_pages,
        unique.len(),
        expected.len()
    );

    // The tail specifically: it is the page a wrong offset empties first, and
    // `USERS`/`ORGS` are deliberately not multiples of `PER_PAGE` so a short
    // last page is part of the claim.
    let last = fetch_page(base, path, token, first.total_pages).await;
    let remainder = expected.len() as u64 % PER_PAGE;
    assert_eq!(
        last.ids.len() as u64,
        if remainder == 0 { PER_PAGE } else { remainder },
        "the last page of /{path} is not the short page the row count implies"
    );
}

#[tokio::test]
async fn walking_every_page_of_the_admin_user_listing_returns_every_account() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, admin_id) = register_full(&base, "walk-admin", "walk-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;

    let mut expected = BTreeSet::from([admin_id]);
    for index in 0..USERS - 1 {
        let (_, id) = register_full(
            &base,
            &format!("walk-user-{index}"),
            &format!("walk-user-{index}@example.com"),
        )
        .await;
        expected.insert(id);
    }
    assert_eq!(expected.len(), USERS, "the fixture seeded the wrong count");

    assert_the_walk_covers_everything(&base, "admin/users", &token, &expected).await;
}

#[tokio::test]
async fn walking_every_page_of_the_admin_org_listing_returns_every_organization() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, admin_id) = register_full(&base, "org-admin", "org-admin@example.com").await;
    promote_user_to_admin(&db, admin_id).await;

    let client = reqwest::Client::new();
    let mut expected = BTreeSet::new();
    for index in 0..ORGS {
        let response = client
            .post(format!("{base}/api/v1/orgs"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"name": format!("walk-org-{index}")}))
            .send()
            .await
            .expect("create an organization");
        let status = response.status();
        let body = response.text().await.expect("read the create body");
        assert_eq!(status, 201, "creating an organization failed: {body}");
        let json: serde_json::Value = serde_json::from_str(&body).expect("create body is JSON");
        expected.insert(json["id"].as_i64().expect("the new organization's id"));
    }
    assert_eq!(expected.len(), ORGS, "the fixture seeded the wrong count");

    assert_the_walk_covers_everything(&base, "admin/orgs", &token, &expected).await;
}
