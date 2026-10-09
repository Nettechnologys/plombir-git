//! `[auth].registration` — the switch that closes self-service sign-up.
//!
//! Before it existed, `POST /users/register` was unconditional and the only
//! thing between a publicly reachable instance and an account farm was
//! `[rate_limit].auth_max` — a throttle (ten accounts a minute, forever), not a
//! refusal. These tests pin the three behaviours the switch is bought for: a
//! closed instance refuses, it refuses *before* writing anything, and it can
//! still be initialised.

use rg_core::user::registration::RegistrationMode;
use sea_orm::{EntityTrait, PaginatorTrait};

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

async fn admin_users_status(base: &str, token: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .get(format!("{base}/api/v1/admin/users"))
        .bearer_auth(token)
        .send()
        .await
        .expect("request admin user listing")
        .status()
}

async fn user_count(db: &rg_db::DatabaseConnection) -> u64 {
    rg_db::entities::user::Entity::find()
        .count(db)
        .await
        .expect("count users")
}

/// A clean instance must be operable without an out-of-band SQL update: the
/// account that closes the bootstrap window is its first instance admin. The
/// capability is one-shot — ordinary registrations after it stay ordinary.
#[tokio::test]
async fn the_first_registration_bootstraps_exactly_one_instance_admin() {
    let (base, db) = spawn_test_app_with_overrides(StateOverrides::default()).await;

    let founder = post_register(&base, "founder", "founder@example.com").await;
    assert_eq!(founder.status(), 201);
    let founder: serde_json::Value = founder.json().await.expect("founder response");
    let founder_token = founder["token"].as_str().expect("founder token");
    assert_eq!(
        admin_users_status(&base, founder_token).await,
        reqwest::StatusCode::OK,
        "the first account must be able to operate the instance"
    );

    let member = post_register(&base, "member", "member@example.com").await;
    assert_eq!(member.status(), 201);
    let member: serde_json::Value = member.json().await.expect("member response");
    let member_token = member["token"].as_str().expect("member token");
    assert_eq!(
        admin_users_status(&base, member_token).await,
        reqwest::StatusCode::FORBIDDEN,
        "the bootstrap privilege must not leak into later registrations"
    );

    let founder = rg_db::ops::user_ops::find_by_username(&db, "founder")
        .await
        .expect("query founder")
        .expect("founder exists");
    let member = rg_db::ops::user_ops::find_by_username(&db, "member")
        .await
        .expect("query member")
        .expect("member exists");
    assert!(founder.is_admin, "the bootstrap row must carry admin state");
    assert!(!member.is_admin, "a later row must not carry admin state");
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
        user_count(&db).await,
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
    assert_eq!(user_count(&db).await, 1, "…and the database must agree");
}

/// Open registration admits every valid request, but the instance-admin
/// capability still belongs to exactly one committed first row under a race.
#[tokio::test]
async fn concurrent_open_registrations_yield_exactly_one_instance_admin() {
    let (base, db) = spawn_test_app_with_overrides(StateOverrides::default()).await;

    let attempts = (0..4).map(|n| {
        let base = base.clone();
        async move {
            post_register(
                &base,
                &format!("openracer{n}"),
                &format!("openracer{n}@example.com"),
            )
            .await
        }
    });
    let statuses: Vec<_> = futures::future::join_all(attempts)
        .await
        .into_iter()
        .map(|resp| resp.status().as_u16())
        .collect();
    assert!(
        statuses.iter().all(|status| *status == 201),
        "open registration must admit every valid request, got {statuses:?}"
    );

    let mut admin_count = 0;
    for n in 0..4 {
        let user = rg_db::ops::user_ops::find_by_username(&db, &format!("openracer{n}"))
            .await
            .expect("query registered user")
            .expect("registered user exists");
        admin_count += usize::from(user.is_admin);
    }
    assert_eq!(
        admin_count, 1,
        "the bootstrap capability must be consumed exactly once"
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

    assert_eq!(user_count(&db).await, 3);
}

async fn instance_says_registration_is_open(base: &str) -> bool {
    let info: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/instance"))
        .send()
        .await
        .expect("request instance info")
        .json()
        .await
        .expect("instance info is JSON");
    info["registration_open"]
        .as_bool()
        .expect("instance info names whether registration is open")
}

/// card_e1baa94866ed: the sign-in page offered "Create an account" on an
/// instance that answers every sign-up with 403. `/instance` now says what the
/// register route would decide — including the one account a closed instance
/// still takes, the one that initialises it.
#[tokio::test]
async fn instance_info_says_what_the_register_route_would_decide() {
    let (base, _db) = spawn_test_app_with_overrides(closed_instance()).await;
    assert!(
        instance_says_registration_is_open(&base).await,
        "an empty closed instance still takes its first account"
    );
    assert_eq!(
        post_register(&base, "founder", "founder@example.com")
            .await
            .status(),
        201
    );
    assert!(
        !instance_says_registration_is_open(&base).await,
        "a closed instance with an account advertised sign-up"
    );
    assert_eq!(
        post_register(&base, "late", "late@example.com")
            .await
            .status(),
        403
    );

    let (base, _db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    assert_eq!(
        post_register(&base, "founder", "founder@example.com")
            .await
            .status(),
        201
    );
    assert!(instance_says_registration_is_open(&base).await);
}
