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
//! or `ForeignGate::Handler { module, gates }` — and this file holds every
//! `Foreign` route to it. Both halves of the claim are recorded by the same
//! statement that registers the route (the layer's name through the credential
//! `Wrap`, the handler's path through `type_name`), so neither can drift away
//! from the route it describes.
//!
//! `gates` is the second round (`card_2dd7aa380bc9`). The `Handler` claim used
//! to say only *where* the handler lives, and a claim about an address says
//! nothing about conduct: a transport handler whose last gate call was deleted
//! satisfied every check in this tree — no `require_*` to find (it is signed
//! off in `authz_extractor_guard::SIGNED_OFF`), no `can_*_repo` to find
//! (nothing was left), and its file still on disk. So the route also names the
//! function its handler must **reach**, and
//! [`a_handler_claim_reaches_the_gate_it_names`] walks the call graph of that
//! handler's own module to find it.
//!
//! What this file can no longer be asked is whether the *named* layer does any
//! checking: it reads a string the wrapper carries. That half is closed by the
//! compiler instead — `Wrap::credential` is private, and the only constructors
//! that mint a name (`Wrap::runner_auth`, `Wrap::docs_auth`) build the
//! middleware they name. There is no way to write down `authenticate_runner`
//! and attach a body limit.
//!
//! Nor does it claim that the gate it *reaches* decides correctly — that a
//! reached `check_read_for` still asks `rg-core` is
//! `authz_extractor_guard`'s subject, and how it answers is
//! `oci_permission_tests` / `lfs_signed_url_tests` / `git_auth_tests`'. What is
//! closed here is the step none of them could see: the call that is simply not
//! there any more.

use std::fs;

use rg_http::route_table::{Access, ForeignGate, RouteFact};

use crate::common::source_scan::{
    calls, functions, reachable_within_module, reaches_any, src_root, Function,
};
use crate::common::spawn_test_app_with_routes;

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

/// `rg_http::api::lfs::batch` → `batch`, the name the module declares it under.
fn handler_fn(handler: &str) -> &str {
    let path = handler.split('<').next().unwrap_or(handler);
    path.rsplit("::").next().unwrap_or(path)
}

/// Reading the module, not the router: `module` is the file the claim names.
fn module_source(module: &str) -> String {
    fs::read_to_string(src_root().join(module)).expect("read source file")
}

/// The functions that genuinely decide repository access, wherever a named gate
/// eventually lands.
///
/// `check_read_for` / `check_write_for` are `api::repo_access`'s answer for a
/// caller the transport resolved itself; `can_read` / `can_write` and the
/// `*_repo` pair are `rg-core`'s implementation underneath them, which
/// `git_http::check_git_access` calls directly because it has owner/name strings
/// and needs the `NotFound` distinction. A gate that reaches none of these is
/// not a gate.
const TERMINAL_GATES: &[&str] = &[
    "check_read_for",
    "check_write_for",
    "can_read",
    "can_write",
    "can_read_repo",
    "can_write_repo",
];

/// Whether `name`'s own body calls one of `gates`.
///
/// A body that does not, but whose *whole* content is a single call to one
/// helper of the same module, is followed into that helper — `oci::get_manifest`
/// and `head_manifest` are two such delegations onto `get_manifest_impl`, and a
/// one-line forwarder cannot forget a gate that was never written in it. Any
/// other body has to hold the call itself: the moment a handler does two things,
/// "somewhere down there a gate is reachable" stops meaning "this request went
/// through it", which is exactly how the first version of this check passed a
/// `ws_job_log_handler` with its `check_read_for` deleted — the socket's
/// background session re-check still mentioned the name.
///
/// `Err` when the module does not declare the function at all; every caller has
/// to treat that as a failure rather than an empty answer.
fn calls_a_gate(
    fns: &[Function],
    name: &str,
    gates: &[&str],
    depth: usize,
) -> Result<bool, String> {
    let Some(function) = fns.iter().find(|f| f.name == name) else {
        return Err(format!("`{name}` is not declared at the top level"));
    };
    if gates.iter().any(|gate| calls(&function.body, gate)) {
        return Ok(true);
    }
    let delegates: Vec<&Function> = fns
        .iter()
        .filter(|f| f.name != name && calls(&function.body, &f.name))
        .collect();
    match delegates.as_slice() {
        [only] if depth > 0 => {
            let next = only.name.clone();
            calls_a_gate(fns, &next, gates, depth - 1)
        }
        _ => Ok(false),
    }
}

/// And the half the module claim was never able to say: the handler has to
/// **call the gate**, not merely live next to it.
///
/// `a_handler_claim_names_the_module_the_handler_lives_in` above compares two
/// strings — where the handler is, where the sign-off says it is. Nothing on
/// either side of that comparison is affected by what the handler *does*, so a
/// transport handler whose last `check_read_for` was deleted stayed green
/// through every guard in this tree: it calls no `require_*` (it is signed off
/// in `authz_extractor_guard::SIGNED_OFF`), it calls no `can_*_repo` (there is
/// nothing left to call), and its file still exists. The exemption was being
/// granted on the strength of a promise nobody could check, which is the exact
/// shape of the `/runners/register` defect this file was written for
/// (`card_2dd7aa380bc9`).
///
/// So the route names the gate and this reads the handler that serves it. What
/// the named gate then does with the question is
/// [`a_named_gate_still_decides_something`]'s half; the two together cover every
/// step from the route to `rg-core`.
#[tokio::test]
async fn a_handler_calls_the_gate_its_route_names() {
    let facts = route_facts().await;
    let mut offenders = Vec::new();
    let mut checked = 0;

    for fact in &facts {
        let Access::Foreign(ForeignGate::Handler { module, gates, .. }) = fact.access else {
            continue;
        };
        if gates.is_empty() || !src_root().join(module).exists() {
            // A module that is not there is the other test's finding; reporting
            // it twice would only make one defect look like two.
            continue;
        }
        checked += 1;

        let name = handler_fn(fact.handler);
        let fns = functions(&module_source(module));
        match calls_a_gate(&fns, name, gates, 1) {
            Ok(true) => {}
            Ok(false) => offenders.push(format!(
                "  {} is gated by `{}` — `{name}` calls none of them",
                fact.label(),
                gates.join("` / `")
            )),
            Err(why) => offenders.push(format!("  {} — {module}: {why}", fact.label())),
        }
    }

    assert!(
        checked > 0,
        "no Foreign route names a gate for its handler — the guard stopped guarding"
    );
    assert!(
        offenders.is_empty(),
        "a Foreign route's handler does not call the gate its sign-off names.\n\
         `Foreign` is the one level the access sweep cannot drive, so the sign-off is all that \
         stands behind it: `ForeignGate::Handler {{ gates }}` is the promise that this handler \
         asks somebody whether the caller may do this. Call the gate, or change the level — a \
         route whose handler checks nothing is open, whatever the table says.\n{}",
        offenders.join("\n")
    );
}

/// The other end of the same promise: a gate a route names has to still decide
/// something.
///
/// Without this half the check above only moves the question one call deeper —
/// name a helper, keep calling it, and gut the helper. So every named gate that
/// its module declares itself is walked through that module's call graph down to
/// [`TERMINAL_GATES`]. Here reachability *is* the right question: the helper
/// exists to answer "may this caller", and any path from it to the real
/// predicate is that answer being asked for.
#[tokio::test]
async fn a_named_gate_still_decides_something() {
    let facts = route_facts().await;
    let mut offenders = Vec::new();
    let mut checked = 0;

    for fact in &facts {
        let Access::Foreign(ForeignGate::Handler { module, gates, .. }) = fact.access else {
            continue;
        };
        if gates.is_empty() || !src_root().join(module).exists() {
            continue;
        }
        let text = module_source(module);
        let declared: Vec<String> = functions(&text).into_iter().map(|f| f.name).collect();
        for gate in gates {
            // A gate that lives elsewhere — `repo_access::check_read_for`,
            // `jwt::validate_token`, `auth::ci_job_binding` — is not this
            // module's to prove. `authz_extractor_guard` holds the gate module
            // to its own rule, and the protocol tests drive the answer.
            if !declared.iter().any(|name| name == gate) {
                continue;
            }
            checked += 1;
            if !reaches_any(&text, gate, TERMINAL_GATES).unwrap_or(false) {
                offenders.push(format!(
                    "  {} is gated by `{module}::{gate}`, which reaches no permission decision",
                    fact.label()
                ));
            }
        }
    }

    assert!(
        checked > 0,
        "no Foreign route names a gate its own module declares — the guard stopped guarding"
    );
    assert!(
        offenders.is_empty(),
        "a gate a Foreign route names no longer decides anything.\n\
         A transport's own gate is a shape — resolve the caller however the protocol carries it, \
         then ask `repo_access::check_read_for` / `check_write_for`. A helper that asks nobody \
         turns the route's sign-off into a sentence about a function that returns `Ok(())`.\n{}",
        offenders.join("\n")
    );
}

/// The one shape with no gate to reach, held to the opposite promise.
///
/// `gates: &[]` says "there is nothing behind this route" — the registry's
/// `GET /v2/` answers every caller the same `401` challenge, built out of the
/// request's own `Host` header. An exemption spelled that way is only safe
/// while it stays true, and "true" here is checkable in both halves: the path
/// names no repository, and nothing the handler reaches inside its module
/// touches the database or the gate module. The moment such a route starts
/// reading data, it needs a gate like every other one, and this fails until it
/// declares one.
#[tokio::test]
async fn a_gateless_handler_claim_reads_nothing() {
    let facts = route_facts().await;
    let mut offenders = Vec::new();
    let mut checked = 0;

    for fact in &facts {
        let Access::Foreign(ForeignGate::Handler { module, gates, .. }) = fact.access else {
            continue;
        };
        if !gates.is_empty() || !src_root().join(module).exists() {
            continue;
        }
        checked += 1;

        for scoped in ["{owner}", "{repo}", "{name}", "{org}", "{id}"] {
            if fact.path.contains(scoped) {
                offenders.push(format!(
                    "  {} claims to read nothing, yet its path names `{scoped}`",
                    fact.label()
                ));
            }
        }

        let text = module_source(module);
        let name = handler_fn(fact.handler);
        let Some(reachable) = reachable_within_module(&text, name) else {
            offenders.push(format!(
                "  {} names `{name}`, which `{module}` does not declare at the top level",
                fact.label()
            ));
            continue;
        };
        for function in functions(&text) {
            if !reachable.contains(&function.name) {
                continue;
            }
            for (n, line) in function.body.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") || code.starts_with("use ") {
                    continue;
                }
                for source in ["rg_db::", "rg_core::", "repo_access::", "state.db"] {
                    if code.contains(source) {
                        offenders.push(format!(
                            "  {} claims to read nothing, yet `{}` touches `{source}` at \
                             {module}:{}",
                            fact.label(),
                            function.name,
                            function.line + n
                        ));
                    }
                }
            }
        }
    }

    assert!(
        checked > 0,
        "no Foreign route is signed off as gateless — drop this guard or restore the sign-off"
    );
    assert!(
        offenders.is_empty(),
        "a Foreign route signed off with no gate is no longer answering out of nothing.\n\
         `gates: &[]` is for a constant response — a discovery document, a fixed challenge. A \
         route that reads the database has something to protect and owes a gate its sign-off can \
         name.\n{}",
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
