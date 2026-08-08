//! The published document has to state, per operation, what it demands of a
//! caller — and state the same thing the router does.
//!
//! It stated nothing at all for its whole life. `utoipa` derives no
//! authentication on its own, so `components` carried only `schemas`, no
//! operation declared `security`, and all 287 of them read as anonymous
//! (card_018b2dd39652). Two things followed: Swagger UI had no "Authorize"
//! button, so `/api-docs/` answered 401 to every "Try it out" and looked broken;
//! and `scripts/openapi-interface-smoke.mjs`, which decides from the document
//! whether to send a token, sent none — 155 of its 287 replayed operations
//! stopped at the 401 wall, proving only that the wall stands.
//!
//! The fix derives `security` from `Access`, the level the `RouteTable` row
//! already declares, so there is no second declaration to keep in step. This
//! guard is what holds the derivation to that promise, and it drives the two
//! ends against each other:
//!
//!   * the document is the one the **server serves**, fetched over HTTP, not a
//!     locally rebuilt copy — so a spec built without its facts, or a route that
//!     stops carrying it, fails here;
//!   * the levels are the facts of the **same build** that produced the router,
//!     so "the table says `User`" is not a reading of `routes.rs` by a regex.
//!
//! Both directions are failures. An operation documented as anonymous over a
//! gated route tells a client it needs no token; an operation documented as
//! gated over a public route tells it to go and find one it does not need.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::common::{register_user, spawn_test_app_with_routes};
use rg_http::openapi::{API_SERVER_PREFIX, FOREIGN_SCHEME, SESSION_SCHEME};
use rg_http::route_table::Access;

/// The verbs a `PathItem` can carry. Anything else under a path — `parameters`,
/// `summary` — is not an operation.
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// A floor under the population, so the pass cannot go quietly vacuous if the
/// document ever fails to parse into operations.
const MIN_OPERATIONS: usize = 250;

/// Documented operations that no route mounts, and therefore have no access
/// level to derive anything from.
///
/// An entry here is a known defect under a card, not a design decision — the
/// document advertises a door that answers 404, and the derivation publishes it
/// as requiring a session because that is the safe direction to be wrong in. It
/// is a ratchet, not a dumping ground: an entry whose route gets mounted fails
/// below, so the exemption cannot outlive its reason. The same row is tracked
/// on the other side by `scripts/openapi-route-coverage-contract-check.mjs`.
const UNMOUNTED: [(&str, &str); 0] = [];

/// What the document must say for a route declaring `access`.
///
/// `None` means "no `security` key at all", which is what a client reads as
/// "call this with nothing". This is the whole rule, written once: the
/// assertions below compare against it rather than restating it per family.
fn expected_security(access: Access) -> Option<Value> {
    match access {
        // `PublicFiltered` is included deliberately: it filters its *answer* by
        // who asks, but it demands nothing of the caller.
        a if a.is_public() => None,
        // Anonymous for a public repository or organization, not for a private
        // one. The empty requirement next to the named one is how OpenAPI
        // spells "optional".
        Access::RepoRead | Access::OrgRead => Some(json!([{}, {SESSION_SCHEME: []}])),
        Access::Foreign(_) => Some(json!([{FOREIGN_SCHEME: []}])),
        _ => Some(json!([{SESSION_SCHEME: []}])),
    }
}

#[tokio::test]
async fn every_documented_operation_declares_the_security_its_route_requires() {
    let (base, facts) = spawn_test_app_with_routes().await;
    let jwt = register_user(&base, "specreader", "specreader@example.com", "Qz7$wRtm").await;

    let doc: Value = reqwest::Client::new()
        .get(format!("{}/api-docs/openapi.json", base))
        .bearer_auth(&jwt)
        .send()
        .await
        .expect("fetch the published spec")
        .json()
        .await
        .expect("the published spec is JSON");

    // ── The two static halves ─────────────────────────────────────────────
    //
    // Without the schemes, a per-operation requirement names something the
    // document never defines — Swagger UI shows no "Authorize" and a generated
    // client has nowhere to put a token, which is the visible half of the
    // defect this file guards.
    let schemes = doc
        .pointer("/components/securitySchemes")
        .and_then(Value::as_object)
        .unwrap_or_else(|| {
            panic!(
                "the published document declares no components.securitySchemes — \
                 `modifiers(&SecurityAddon)` is gone from `#[openapi(...)]` in openapi.rs"
            )
        });
    for name in [SESSION_SCHEME, FOREIGN_SCHEME] {
        let scheme = schemes
            .get(name)
            .unwrap_or_else(|| panic!("securitySchemes declares no `{name}`"));
        assert_eq!(
            scheme.get("type").and_then(Value::as_str),
            Some("http"),
            "`{name}` is not an HTTP scheme: {scheme}"
        );
        assert_eq!(
            scheme.get("scheme").and_then(Value::as_str),
            Some("bearer"),
            "`{name}` is not a bearer scheme: {scheme}"
        );
    }

    // The document's own statement of where its paths live. If this and
    // `API_SERVER_PREFIX` ever diverge, every lookup below misses and the
    // unresolved list — not a silent zero — is what fails.
    assert_eq!(
        doc.pointer("/servers/0/url").and_then(Value::as_str),
        Some(API_SERVER_PREFIX),
        "the document's first server URL is not the prefix the security derivation resolves \
         annotations against"
    );

    // ── The derived half ──────────────────────────────────────────────────

    let by_route: BTreeMap<(String, String), Access> = facts
        .iter()
        .map(|fact| ((fact.path.clone(), fact.method.to_string()), fact.access))
        .collect();

    let paths = doc
        .get("paths")
        .and_then(Value::as_object)
        .expect("the document has a paths object");

    let mut unresolved: Vec<String> = Vec::new();
    let mut mismatched: Vec<String> = Vec::new();
    let mut families: BTreeSet<&'static str> = BTreeSet::new();
    let mut operations = 0usize;
    let mut declaring = 0usize;
    let mut gated_routes = 0usize;

    for (path, item) in paths {
        let mounted = format!("{API_SERVER_PREFIX}{path}");
        for method in METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };
            operations += 1;
            let label = format!("{} {path}", method.to_uppercase());

            let Some(access) = by_route
                .get(&(mounted.clone(), method.to_uppercase()))
                .copied()
            else {
                unresolved.push(label);
                continue;
            };

            families.insert(match access {
                a if a.is_public() => "public",
                Access::RepoRead | Access::OrgRead => "optional",
                Access::Foreign(_) => "foreign",
                _ => "required",
            });

            let expected = expected_security(access);
            if !access.is_public() {
                gated_routes += 1;
            }
            let actual = operation.get("security").cloned();
            if actual.is_some() {
                declaring += 1;
            }
            if actual != expected {
                mismatched.push(format!(
                    "  {label} — the route declares {access:?}, so the document owes {}, but it \
                     publishes {}",
                    expected.map_or("no security".to_string(), |v| v.to_string()),
                    actual.map_or("no security".to_string(), |v| v.to_string()),
                ));
            }
        }
    }

    let exempt: BTreeMap<&str, &str> = UNMOUNTED.iter().copied().collect();
    let surprises: Vec<&String> = unresolved
        .iter()
        .filter(|label| !exempt.contains_key(label.as_str()))
        .collect();
    assert!(
        surprises.is_empty(),
        "{} documented operation(s) name a URL no route table row matches, so nothing states what \
         they demand of a caller and the document publishes a guess:\n  {}",
        surprises.len(),
        surprises
            .iter()
            .map(|label| label.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
    // The other direction of the same ratchet: an exemption that no longer
    // applies is a lie about the state of the code, and a silent one.
    let stale: Vec<String> = exempt
        .iter()
        .filter(|(label, _)| !unresolved.iter().any(|found| found == *label))
        .map(|(label, reason)| format!("  {label} — {reason}"))
        .collect();
    assert!(
        stale.is_empty(),
        "UNMOUNTED names {} operation(s) that ARE mounted now — drop the entr{} so the exemption \
         cannot outlive its reason:\n{}",
        stale.len(),
        if stale.len() == 1 { "y" } else { "ies" },
        stale.join("\n")
    );
    assert!(
        mismatched.is_empty(),
        "the published document and the route table disagree about {} operation(s):\n{}",
        mismatched.len(),
        mismatched.join("\n")
    );

    // Counting the two sides separately, and only then comparing them, is what
    // makes this more than a restatement of the loop above: it fails if an
    // operation is stamped without a route, or a gated route left unstamped.
    assert_eq!(
        declaring, gated_routes,
        "{declaring} operation(s) declare security but {gated_routes} of the routes behind them \
         are mounted at a non-public access level"
    );

    // Anti-vacuity. A pass over an empty document, or over one where every
    // route collapsed into a single family, would agree with itself perfectly.
    assert!(
        operations >= MIN_OPERATIONS,
        "only {operations} operations parsed out of the published document (expected at least \
         {MIN_OPERATIONS}) — this pass is reading a subset"
    );
    assert!(
        declaring > 0,
        "no operation declares security — the derivation ran and stamped nothing"
    );
    assert_eq!(
        families,
        BTreeSet::from(["foreign", "optional", "public", "required"]),
        "the document no longer covers all four access families, so the families missing here are \
         unchecked by this pass"
    );
}
