//! Measured cost of the session revocation gate.
//!
//! The gate spends one primary-key read per authenticated request. This is the
//! measurement behind that decision: run it, read the numbers, and if the
//! lookup ever stops being noise next to the request it guards, that is the
//! moment to reach for a cache — not before.
//!
//! `cargo test --release -j 6 -p rg-http --test integration session_gate_cost -- --ignored --nocapture`
//!
//! Reading of 2026-07-27, SQLite, release: lookup 31.8µs, `GET /users/me`
//! 122.0µs, `GET /health` 54.6µs — 26% of the *lightest* authenticated
//! endpoint, which is the worst case by construction (`/users/me` does little
//! more than the same lookup again). Run it in release: a debug build inflates
//! the lookup ~18× and answers a different question.

use std::time::Instant;

use crate::common::{register_full, spawn_test_app_with_db};

const ROUNDS: u32 = 300;

#[tokio::test]
#[ignore = "measurement, not an assertion — run explicitly"]
async fn session_gate_lookup_cost() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "gatecost", "gatecost@example.com").await;
    let client = reqwest::Client::new();
    let me = format!("{}/api/v1/users/me", base);
    let health = format!("{}/health", base);

    // Warm the connection pool and the route so the first request's setup does
    // not land on whichever measurement runs first.
    for _ in 0..20 {
        client.get(&me).bearer_auth(&jwt).send().await.unwrap();
        client.get(&health).send().await.unwrap();
        rg_db::ops::user_ops::find_by_id(&db, user_id)
            .await
            .unwrap();
    }

    let t = Instant::now();
    for _ in 0..ROUNDS {
        rg_db::ops::user_ops::find_by_id(&db, user_id)
            .await
            .unwrap();
    }
    let lookup = t.elapsed() / ROUNDS;

    let t = Instant::now();
    for _ in 0..ROUNDS {
        let r = client.get(&me).bearer_auth(&jwt).send().await.unwrap();
        assert_eq!(r.status(), 200);
    }
    let authenticated = t.elapsed() / ROUNDS;

    let t = Instant::now();
    for _ in 0..ROUNDS {
        client.get(&health).send().await.unwrap();
    }
    let anonymous = t.elapsed() / ROUNDS;

    println!("\n── session gate cost ({ROUNDS} rounds each) ──");
    println!("  gate lookup (find_by_id)      {lookup:>10.1?}");
    println!("  GET /api/v1/users/me          {authenticated:>10.1?}");
    println!("  GET /health (no session)      {anonymous:>10.1?}");
    println!(
        "  gate share of a hot request   {:>9.2}%\n",
        lookup.as_secs_f64() / authenticated.as_secs_f64() * 100.0
    );
}
