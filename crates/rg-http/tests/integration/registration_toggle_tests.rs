//! `[auth].registration` — the switch that closes self-service sign-up.
//!
//! Before it existed, `POST /users/register` was unconditional and the only
//! thing between a publicly reachable instance and an account farm was
//! `[rate_limit].auth_max` — a throttle (ten accounts a minute, forever), not a
//! refusal. These tests pin the three behaviours the switch is bought for: a
//! closed instance refuses, it refuses *before* writing anything, and it can
//! still be initialised.

use rg_core::user::registration::RegistrationMode;

use crate::common::{spawn_test_app_with_overrides, StateOverrides};

fn closed_instance() -> StateOverrides {
    StateOverrides {
        registration: Some(RegistrationMode::Closed),
        ..Default::default()
    }
}

async fn post_register(base: &str, username: &str, email: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/users/register"))
        .json(&serde_json::json!({
            "username": username,
            "email": email,
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("request")
}

/// The headline behaviour: an instance that already has an account refuses the
/// next one, and refuses it with a status a caller can act on.
#[tokio::test]
async fn a_closed_instance_refuses_registration_and_writes_no_row() {
    let (base, db) = spawn_test_app_with_overrides(closed_instance()).await;

    // The bootstrap account — the only one a closed instance admits.
    let first = post_register(&base, "founder", "founder@example.com").await;
    assert_eq!(
        first.status(),
        201,
        "an empty closed instance must be initialisable"
    );

    let refused = post_register(&base, "stranger", "stranger@example.com").await;
    assert_eq!(
        refused.status(),
        403,
        "a closed instance must refuse, not throttle"
    );
    let body: serde_json::Value = refused.json().await.expect("error envelope");
    assert_eq!(body["error"]["code"], "FORBIDDEN", "envelope: {body}");

    // The refusal is a refusal, not a failed insert: nothing landed.
    assert!(
        rg_db::ops::user_ops::find_by_username(&db, "stranger")
            .await
            .expect("query users")
            .is_none(),
        "the refused registration must not leave a row behind"
    );
    assert_eq!(
        rg_db::ops::user_ops::count_all(&db)
            .await
            .expect("count users"),
        1,
        "only the bootstrap account exists"
    );
}

/// The bootstrap window closes behind the first account even when the accounts
/// arrive together — the count check and the insert are not atomic on their own.
#[tokio::test]
async fn concurrent_bootstrap_attempts_yield_exactly_one_account() {
    let (base, db) = spawn_test_app_with_overrides(closed_instance()).await;

    let attempts = (0..6).map(|n| {
        let base = base.clone();
        async move {
            post_register(
                &base,
                &format!("racer{n}"),
                &format!("racer{n}@example.com"),
            )
            .await
        }
    });
    let statuses: Vec<_> = futures::future::join_all(attempts)
        .await
        .into_iter()
        .map(|resp| resp.status().as_u16())
        .collect();

    assert_eq!(
        statuses.iter().filter(|status| **status == 201).count(),
        1,
        "the bootstrap window must admit exactly one account, got {statuses:?}"
    );
    assert_eq!(
        rg_db::ops::user_ops::count_all(&db)
            .await
            .expect("count users"),
        1,
        "…and the database must agree"
    );
}

/// The default is unchanged behaviour: an instance that never configures the
/// key keeps its sign-up page.
#[tokio::test]
async fn an_open_instance_still_registers_freely() {
    let (base, db) = spawn_test_app_with_overrides(StateOverrides::default()).await;

    for n in 0..3 {
        let resp = post_register(&base, &format!("open{n}"), &format!("open{n}@example.com")).await;
        assert_eq!(resp.status(), 201, "open registration must keep working");
    }

    assert_eq!(
        rg_db::ops::user_ops::count_all(&db)
            .await
            .expect("count users"),
        3
    );
}
