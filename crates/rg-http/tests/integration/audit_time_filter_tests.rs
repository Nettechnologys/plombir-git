//! card_c171562b4558: an audit-log time filter that does not parse must not be
//! dropped, leaving the unfiltered journal to pass for a filtered one.
//!
//! `list_audit_logs` parsed `start_time` / `end_time` with
//! `DateTime::from_str(s).ok()`. A bound that failed to parse became `None`,
//! which is the same value as "no bound was sent", and the query then ran
//! across all of time. The admin walking an incident got `200` and a
//! convincing list — the only hint that their window had never been applied
//! was that it held more rows than they expected, which is exactly what an
//! incident looks like.
//!
//! The sibling route `/admin/login-attempts` already rejected the same input.
//! Two admin routes on one screen disagreeing about what a bad timestamp means
//! is half the reason the defect was invisible, so both tests run the pair.

use axum::http::StatusCode;
use sea_orm::Set;

use crate::common::{register_full, spawn_test_app_with_db};

async fn seed_admin(base: &str, db: &rg_db::DatabaseConnection, name: &str) -> String {
    let (token, user_id) = register_full(base, name, &format!("{name}@example.test")).await;
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, Some(true), None)
        .await
        .expect("promote to instance admin");
    token
}

/// Two entries a day apart, so a correct bound can only select one of them.
async fn seed_entries(db: &rg_db::DatabaseConnection) -> (String, String) {
    let old = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("fixed timestamp")
        .with_timezone(&chrono::Utc);
    let new = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
        .expect("fixed timestamp")
        .with_timezone(&chrono::Utc);
    for (action, created_at) in [("repo.create", old), ("repo.delete", new)] {
        rg_db::ops::audit_log_ops::insert(
            db,
            rg_db::entities::audit_log::ActiveModel {
                user_id: Set(None),
                username: Set(None),
                action: Set(action.to_string()),
                resource_type: Set(None),
                resource_id: Set(None),
                resource_name: Set(None),
                ip_address: Set(None),
                user_agent: Set(None),
                details: Set(None),
                created_at: Set(created_at),
                ..Default::default()
            },
        )
        .await
        .expect("insert audit entry");
    }
    (old.to_rfc3339(), new.to_rfc3339())
}

#[tokio::test]
async fn an_unparseable_audit_time_filter_is_refused_instead_of_dropped() {
    let (base, db) = spawn_test_app_with_db().await;
    let token = seed_admin(&base, &db, "auditfilter").await;
    seed_entries(&db).await;
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/v1/admin/audit/logs");

    // Baseline: with no bounds at all the admin sees everything the instance
    // has recorded — the seeded pair plus whatever registering the admin wrote.
    // This is what the broken parse used to return for a *filtered* request, so
    // the test would be vacuous without it.
    let unfiltered = client
        .get(&endpoint)
        .bearer_auth(&token)
        .send()
        .await
        .expect("list without bounds");
    assert_eq!(unfiltered.status(), StatusCode::OK);
    let unfiltered_total = unfiltered.json::<serde_json::Value>().await.expect("body")["total"]
        .as_u64()
        .expect("total is a number");
    assert!(
        unfiltered_total >= 2,
        "both seeded entries have to be visible, got {unfiltered_total}"
    );

    // A date without a time and an offset — the shape an admin actually types.
    let refused = client
        .get(&endpoint)
        .query(&[("start_time", "2026-08-01")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("list with a malformed bound");
    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "a bound that was sent and did not parse must not be answered with the whole journal"
    );
    let body = refused.text().await.unwrap_or_default();
    assert!(
        body.contains("start_time"),
        "the refusal has to name the field the admin has to fix, got: {body}"
    );

    // The same input on the sibling route, which already behaved: the pair is
    // what keeps the two admin routes from drifting apart again.
    let sibling = client
        .get(format!("{base}/api/v1/admin/login-attempts"))
        .query(&[("end_time", "not-a-timestamp")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("login attempts with a malformed bound");
    assert_eq!(sibling.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn valid_and_absent_audit_time_filters_keep_their_contract() {
    let (base, db) = spawn_test_app_with_db().await;
    let token = seed_admin(&base, &db, "auditkeep").await;
    seed_entries(&db).await;
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/v1/admin/audit/logs");

    // A valid RFC 3339 bound still filters. The upper bound sits between the
    // two seeded entries and before anything the running instance writes, so
    // exactly one row can satisfy it — the property the broken parse destroyed.
    let filtered = client
        .get(&endpoint)
        .query(&[("end_time", "2026-03-01T00:00:00Z")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("list with a valid bound");
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered: serde_json::Value = filtered.json().await.expect("body");
    assert_eq!(filtered["total"], 1);
    assert_eq!(filtered["logs"][0]["action"], "repo.create");

    // A window that cannot contain anything is a request to fix, not an empty
    // list to puzzle over — the sibling route has always said so.
    let reversed = client
        .get(&endpoint)
        .query(&[
            ("start_time", "2026-06-01T00:00:00Z"),
            ("end_time", "2026-01-01T00:00:00Z"),
        ])
        .bearer_auth(&token)
        .send()
        .await
        .expect("list with a reversed window");
    assert_eq!(reversed.status(), StatusCode::BAD_REQUEST);
}
