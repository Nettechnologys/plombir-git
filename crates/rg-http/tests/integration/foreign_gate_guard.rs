//! card_905f6e81efdd: `Access::Foreign` is the one level the route sweep does
//! not check, so it is the one level a route can hand itself.
//!
//! Every other level is driven by `route_access_sweep_tests` with three
//! personas. `Foreign` is owed `Expect::Unchecked` — the sweep holds no runner
//! token, no OCI bearer token, no git credentials — and behind that exemption
//! there used to be nothing but a sentence of free text. `POST /runners/
//! register` declared `"CI runner: runner token via `authenticate_runner`"`
//! while carrying no such layer; its real gate was an instance-admin session,
//! and nothing compared the two (card_cd6f512e2e52). It was found by reading.
//!
//! Now the sign-off is a claim with a shape — `ForeignGate::Middleware { layer }`
//! or `ForeignGate::Handler { module }` — and this file holds every `Foreign`
//! route to it. Both halves of the claim are recorded by the same statement
//! that registers the route (the layer's name through the credential `Wrap`,
//! the handler's path through `type_name`), so neither can drift away from the
//! route it describes.
//!
//! What this file can no longer be asked is whether the *named* layer does any
//! checking: it reads a string the wrapper carries. That half is closed by the
//! compiler instead — `Wrap::credential` is private, and the only constructors
//! that mint a name (`Wrap::runner_auth`, `Wrap::docs_auth`) build the
//! middleware they name. There is no way to write down `authenticate_runner`
//! and attach a body limit.

use std::path::{Path, PathBuf};

use rg_http::route_table::{Access, ForeignGate, RouteFact};

use crate::common::spawn_test_app_with_routes;

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// `api/lfs.rs` → `rg_http::api::lfs::` — the prefix a handler defined in that
/// file carries in its `type_name`.
fn module_prefix(module_file: &str) -> String {
    let stem = module_file.strip_suffix(".rs").unwrap_or(module_file);
    format!("rg_http::{}::", stem.replace('/', "::"))
}

async fn route_facts() -> Vec<RouteFact> {
    let (_base, facts) = spawn_test_app_with_routes().await;
    assert!(
        facts.len() > 200,
        "route table looks empty ({} rows) — the guard is not running",
        facts.len()
    );
    facts
}

/// A `Foreign` route that claims a middleware must actually carry it.
///
/// This is the assertion `/runners/register` would have failed: the level named
/// `authenticate_runner`, the route was registered without it, and the
/// mismatch was invisible because the layer had no name to compare.
#[tokio::test]
async fn a_middleware_claim_names_a_layer_the_route_carries() {
    let facts = route_facts().await;
    let mut offenders = Vec::new();
    let mut checked = 0;

    for fact in &facts {
        if let Access::Foreign(ForeignGate::Middleware { layer, .. }) = fact.access {
            checked += 1;
            if fact.credential != Some(layer) {
                offenders.push(format!(
                    "  {} claims `{layer}` but carries {}",
                    fact.label(),
                    match fact.credential {
                        Some(other) => format!("`{other}`"),
                        None => "no credential layer".to_string(),
                    }
                ));
            }
        }
    }

    assert!(
        checked > 0,
        "no route declares a middleware-gated Foreign level — the guard stopped guarding"
    );
    assert!(
        offenders.is_empty(),
        "a Foreign route claims a credential middleware it was not registered with.\n\
         Register it through `*_with(..., &<the credential Wrap>)` — `Wrap::runner_auth` and its \
         siblings are the only wrappers that carry a name — or declare the level the route really \
         has: the claim is what buys it `Expect::Unchecked` in the access sweep.\n{}",
        offenders.join("\n")
    );
}

/// And the other direction: a route that carries a credential layer must be the
/// `Foreign` level that names it.
///
/// Without this half the exemption can be taken by omission — attach the runner
/// gate, declare the route `Public`, and the sweep is told to expect an open
/// route while the layer quietly answers `401`. The pair only means something
/// if neither side can move alone.
#[tokio::test]
async fn a_credential_layer_is_declared_by_the_route_that_carries_it() {
    let facts = route_facts().await;
    let mut offenders = Vec::new();

    for fact in &facts {
        let Some(layer) = fact.credential else {
            continue;
        };
        let declared = matches!(
            fact.access,
            Access::Foreign(ForeignGate::Middleware { layer: claimed, .. }) if claimed == layer
        );
        if !declared {
            offenders.push(format!(
                "  {} carries `{layer}` but declares {:?}",
                fact.label(),
                fact.access
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "a route carries a credential middleware its declared access level does not name.\n\
         The level and the layer are one decision; declare \
         `Foreign(ForeignGate::Middleware {{ layer, .. }})` with the same constant the wrapper \
         uses.\n{}",
        offenders.join("\n")
    );
}

/// A `Foreign` route that says its handler holds the gate must have a handler
/// in the module it names — and no credential layer to hide behind.
#[tokio::test]
async fn a_handler_claim_names_the_module_the_handler_lives_in() {
    let facts = route_facts().await;
    let mut offenders = Vec::new();
    let mut checked = 0;

    for fact in &facts {
        let Access::Foreign(ForeignGate::Handler { module, .. }) = fact.access else {
            continue;
        };
        checked += 1;

        if !src_root().join(module).exists() {
            offenders.push(format!(
                "  {} is signed off in `{module}`, which does not exist",
                fact.label()
            ));
            continue;
        }
        let prefix = module_prefix(module);
        if !fact.handler.starts_with(&prefix) {
            offenders.push(format!(
                "  {} is signed off in `{module}` but its handler is `{}`",
                fact.label(),
                fact.handler
            ));
        }
        if let Some(layer) = fact.credential {
            offenders.push(format!(
                "  {} claims its handler gates it, yet carries the `{layer}` layer",
                fact.label()
            ));
        }
    }

    assert!(
        checked > 0,
        "no route declares a handler-gated Foreign level — the guard stopped guarding"
    );
    assert!(
        offenders.is_empty(),
        "a Foreign route is signed off against a module that does not hold its handler.\n\
         `ForeignGate::Handler {{ module }}` is the sign-off for a protocol whose credential the \
         handler checks itself; naming someone else's file is how a route borrows an exemption it \
         was never granted.\n{}",
        offenders.join("\n")
    );
}

/// The prose half still has to say something — it is what a reader gets.
#[tokio::test]
async fn every_foreign_sign_off_says_what_the_mechanism_is() {
    let facts = route_facts().await;

    for fact in &facts {
        if let Access::Foreign(gate) = fact.access {
            assert!(
                !gate.note().trim().is_empty(),
                "{} is signed off as Foreign without a reason",
                fact.label()
            );
        }
    }
}
