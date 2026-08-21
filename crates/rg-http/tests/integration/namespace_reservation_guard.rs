//! The reserved-name list is the route tree, not a copy of it (card_e3f6f110a622).
//!
//! An owner is the first segment of a URL, and so is every page and endpoint
//! this application answers for itself. `rg_core::namespace::RESERVED_SEGMENTS`
//! is what `validate_username` refuses on that ground — for accounts, for
//! organizations, and on the SSO and LDAP paths that generate a name with no
//! human in the loop.
//!
//! A list like that is worth exactly as much as whatever keeps it in step with
//! the tree, and nothing else can: `rg-core` cannot see this crate's router and
//! neither crate can see `web/src/routes/`. So the comparison lives here, where
//! both halves are reachable, and it is an equality rather than a subset —
//! in both directions, because both directions are real defects:
//!
//! - a page added tomorrow and not reserved is the original bug returning, this
//!   time for whoever registers that name next;
//! - a name reserved with nothing behind it is a name taken away from a person
//!   for no reason, and it is how a derived list rots into a hand-kept ban list.
//!
//! ## What is not on the list, and why that is not a hole
//!
//! A segment no username could ever equal is left off, because
//! `validate_username` would refuse the name before it ever reached the
//! reservation check and a rule that never fires only makes the list harder to
//! read. Today that is `/v2` — the OCI registry root, two characters, under the
//! three-character minimum. The filter below is the same predicate, so the
//! exemption cannot silently widen: a future `/v2-beta` is long enough to be a
//! username and would be required on the list like anything else.

use std::collections::BTreeSet;

use crate::common::source_scan::workspace_crates;
use crate::common::{build_test_app_state, register_full, setup_test_db, spawn_test_app};

/// `web/src/routes`, reached from this crate's manifest directory.
fn spa_routes_dir() -> std::path::PathBuf {
    workspace_crates()
        .parent()
        .expect("crates/ lives under the workspace root")
        .join("web/src/routes")
}

/// Whether `segment` could be an account name at all.
///
/// Asked of the real validator rather than of a copy of its rules: the
/// exemption is "the reservation would never be reached", and
/// `validate_username_shape` is exactly the half of the rule that decides that
/// — it is the four checks a name faces before the list is ever consulted.
fn could_be_an_account_name(segment: &str) -> bool {
    rg_core::user::service::validate_username_shape(segment).is_ok()
}

/// The fragment `validate_username` puts in the refusal it raises for a
/// reserved name. Spelled out rather than imported, because a refusal that
/// stopped saying this is a change this guard has to notice.
const RESERVED_REFUSAL: &str = "is reserved for a page of this application";

/// The first segment of every route this build mounts.
///
/// Read from the `RouteFact` table the router itself produced, so this is the
/// server's real answer to "which top-level paths do I claim", not a reading of
/// `routes.rs`. A segment that is a path parameter belongs to no fixed name and
/// is skipped.
async fn mounted_first_segments() -> BTreeSet<String> {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create the test repo root");
    let state = build_test_app_state(db, repo_root);
    let (_router, facts) = rg_http::create_router_for_test_with_routes(state);
    assert!(!facts.is_empty(), "the route table is empty");
    facts
        .iter()
        .filter_map(|fact| fact.path.split('/').nth(1))
        .filter(|segment| !segment.is_empty() && !segment.starts_with('{'))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// The top-level static routes of the SPA.
///
/// A directory whose name starts with `[` is a parameter route — `[owner]` is
/// the one this whole rule exists to protect — and the `+layout` / `+page`
/// files are the root page itself, not a segment.
fn spa_first_segments() -> BTreeSet<String> {
    let dir = spa_routes_dir();
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    let segments: BTreeSet<String> = entries
        .map(|entry| entry.expect("a directory entry"))
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase())
        .filter(|name| !name.starts_with('[') && !name.starts_with('+'))
        .collect();
    assert!(
        segments.contains("dashboard"),
        "the SPA route scan found no `dashboard` — it is reading the wrong directory: {}",
        dir.display()
    );
    segments
}

#[tokio::test]
async fn every_reserved_name_is_a_segment_this_application_answers_for() {
    let claimed: BTreeSet<String> = mounted_first_segments()
        .await
        .into_iter()
        .chain(spa_first_segments())
        .filter(|segment| could_be_an_account_name(segment))
        .collect();
    let reserved: BTreeSet<String> = rg_core::namespace::RESERVED_SEGMENTS
        .iter()
        .map(|segment| (*segment).to_string())
        .collect();

    let unreserved: Vec<&String> = claimed.difference(&reserved).collect();
    assert!(
        unreserved.is_empty(),
        "these first path segments are answered by this application and are still free to \
         register as an account name — the account would be created and its profile would be \
         unreachable: {unreserved:?}"
    );

    let unclaimed: Vec<&String> = reserved.difference(&claimed).collect();
    assert!(
        unclaimed.is_empty(),
        "these names are refused as account names and nothing answers for them any more — a \
         reservation with nothing behind it takes a name away for no reason: {unclaimed:?}"
    );
}

/// The other half of the claim: the refusal actually happens, everywhere the
/// one validator is the door.
#[test]
fn a_reserved_name_is_refused_and_a_name_that_merely_resembles_one_is_not() {
    for segment in rg_core::namespace::RESERVED_SEGMENTS {
        let error = rg_core::validate_username(segment)
            .expect_err("a reserved segment is not a valid account name");
        let message = error.to_string();
        assert!(
            message.contains(segment) && message.contains(RESERVED_REFUSAL),
            "the refusal did not name what was rejected, or did not say why: {message}"
        );
    }
    assert!(rg_core::validate_username("explorer").is_ok());
    assert!(rg_core::validate_username("admins").is_ok());
}

/// The refusal as a client meets it: registration.
///
/// `validate_username` is one function and the unit assertion above proves it
/// refuses — but the route is what an operator actually gets turned away by,
/// and it is the route that used to answer `201` and produce an account with
/// no page. The name is one a person would plausibly want: `explore` is a word
/// before it is a route.
#[tokio::test]
async fn registering_a_reserved_name_is_refused_and_a_neighbouring_name_is_not() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let register = |username: &str, email: &str| {
        client
            .post(format!("{base}/api/v1/users/register"))
            .json(&serde_json::json!({
                "username": username,
                "email": email,
                "password": "Reserved-Name-Probe-1"
            }))
            .send()
    };

    let refused = register("explore", "explore@example.com")
        .await
        .expect("register a reserved name");
    assert_eq!(
        refused.status(),
        400,
        "a name the application answers for must not become an account"
    );
    let body: serde_json::Value = refused.json().await.expect("a JSON error");
    let message = body["error"]["message"]
        .as_str()
        .expect("the refusal carries a message");
    assert!(
        message.contains("explore") && message.contains(RESERVED_REFUSAL),
        "the refusal did not say which name was rejected or why: {message}"
    );

    let free = register("explorer", "explorer@example.com")
        .await
        .expect("register a free name");
    let status = free.status();
    let body = free.text().await.unwrap_or_default();
    assert_eq!(
        status, 201,
        "the rule is the whole segment: a name that merely starts with one is free: {body}"
    );
}

/// The same door, from the other side of it: an organization name goes through
/// `validate_username` too (`rg_core::org`), and an organization is addressed
/// by the very same first segment.
#[tokio::test]
async fn creating_an_organization_under_a_reserved_name_is_refused() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(&base, "reserved-org-owner", "reserved-org@example.com").await;

    let refused = client
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "name": "settings", "visibility": "public" }))
        .send()
        .await
        .expect("create an organization under a reserved name");
    assert_eq!(refused.status(), 400);
    let body: serde_json::Value = refused.json().await.expect("a JSON error");
    let message = body["error"]["message"]
        .as_str()
        .expect("the refusal carries a message");
    assert!(
        message.contains("settings") && message.contains(RESERVED_REFUSAL),
        "the refusal did not say which name was rejected or why: {message}"
    );

    assert_eq!(
        client
            .post(format!("{base}/api/v1/orgs"))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "name": "settlers", "visibility": "public" }))
            .send()
            .await
            .expect("create an organization under a free name")
            .status(),
        201
    );
}
