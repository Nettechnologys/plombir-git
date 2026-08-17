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
//! checking: it reads a string the wrapper carries. That half rests on
//! `Wrap::credential` being private — the only constructors that mint a name
//! (`Wrap::runner_auth` and its body-limit sibling) build the middleware they
//! name, so `authenticate_runner` cannot be written down next to a body limit.
//! The compiler enforces it and [`the_credential_constructor_is_private`] is
//! what holds the compiler to it, because a guard whose coverage is bought with
//! a compile-time property is one word away from covering nothing
//! (`card_eba65d156601`).
//!
//! Only the *pairing* is closed that way, and it is worth being exact about the
//! rest: that each named constructor attaches the layer it names is semantics
//! no source guard here proves.
//! [`the_credential_name_is_minted_only_by_the_naming_constructors`] takes the
//! cheap half of it — every mint of a name stays inside `impl Wrap<'static>`,
//! where a constructor that names a layer it does not apply sits next to two
//! that do — and a reader is owed the difference.
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
    calls, functions, reachable_within_module, reaches_any, rust_code_only, src_root, Function,
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
            for (n, code, original) in gateless_data_source_hits(&function.body) {
                for source in GATELESS_DATA_SOURCES {
                    if code.contains(source) {
                        offenders.push(format!(
                            "  {} claims to read nothing, yet `{}` touches `{source}` at \
                             {module}:{} — {original}",
                            fact.label(),
                            function.name,
                            function.line + n - 1
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

// ── The half handed to the compiler ────────────────────────────────────────
//
// Everything above reads the route table. What follows reads the source of the
// table itself, because the half this file's header hands to the compiler —
// "the name cannot be claimed by a wrapper that does not do the check" — is one
// word wide and nothing was asking the compiler about it. The sibling shape is
// `authz_extractor_guard::the_repository_gates_are_crate_private` and
// `the_instance_admin_gate_is_module_private`: a guarantee a guard's coverage
// rests on has to be asserted where the guard rests on it.

/// The credential wrapper's home, relative to `crates/rg-http/src/`.
const WRAP_HOME: &str = "route_table.rs";

/// The constructor that pairs a credential *name* with an arbitrary closure.
const WRAP_CONSTRUCTOR: &str = "credential";

/// The wrapper's own declaration — the fields the constructor writes.
const WRAP_STRUCT: &str = "struct Wrap<'a> {";

/// The block of named constructors, each of which applies the layer it names.
/// The only place allowed to call [`WRAP_CONSTRUCTOR`].
const NAMING_IMPL: &str = "impl Wrap<'static> {";

fn wrap_home() -> String {
    fs::read_to_string(src_root().join(WRAP_HOME)).expect("read the route table module")
}

type SourceHit<'a> = (usize, String, &'a str);

/// Lines selected from executable Rust, with the original line retained for
/// diagnostics. The shared view is byte-aligned, so line numbers cannot drift.
fn source_hits<'a>(
    text: &'a str,
    mut matches: impl FnMut(usize, &str) -> bool,
) -> Vec<SourceHit<'a>> {
    rust_code_only(text)
        .lines()
        .zip(text.lines())
        .enumerate()
        .filter_map(|(n, (code, original))| {
            let line = n + 1;
            matches(line, code).then_some((line, code.trim().to_owned(), original.trim()))
        })
        .collect()
}

const GATELESS_DATA_SOURCES: [&str; 4] = ["rg_db::", "rg_core::", "repo_access::", "state.db"];

fn gateless_data_source_hits(text: &str) -> Vec<SourceHit<'_>> {
    source_hits(text, |_, code| {
        let code = code.trim_start();
        !code.starts_with("use ")
            && GATELESS_DATA_SOURCES
                .iter()
                .any(|source| code.contains(source))
    })
}

#[test]
fn gateless_data_source_scan_ignores_non_code_decoys_and_keeps_original_lines() {
    const SAMPLE: &str = r####"
fn sample() {
    // rg_db::ops::load();
    /* rg_core::repo::service::find(); */
    let normal = "repo_access::check_read_for(actor)";
    let raw = r#"state.db"#;
    let bytes = b"rg_db::ops::load()";
    let raw_bytes = br#"rg_core::repo::service::find()"#;
    use rg_db::entities::repository;
    let _ = raw; state.db.ping();
}
"####;

    let hits = gateless_data_source_hits(SAMPLE);
    assert_eq!(
        hits.len(),
        1,
        "unexpected executable data sources: {hits:?}"
    );
    assert_eq!(hits[0].0, 10);
    assert_eq!(hits[0].1, "let _ = raw; state.db.ping();");
    assert_eq!(hits[0].2, "let _ = raw; state.db.ping();");
}

/// The 1-based inclusive line span of the block whose opening line contains
/// `header`, by brace counting over the non-prose lines.
fn block_span(text: &str, header: &str) -> (usize, usize) {
    let code = rust_code_only(text);
    let lines: Vec<&str> = code.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains(header))
        .unwrap_or_else(|| {
            panic!(
                "{WRAP_HOME} no longer declares `{header}`. It was renamed or moved, and both \
                 guards below are then about a shape that is not there — follow the rename here."
            )
        });

    let mut depth = 0usize;
    for (offset, line) in lines[start..].iter().enumerate() {
        depth += line.matches('{').count();
        depth = depth.saturating_sub(line.matches('}').count());
        if depth == 0 {
            return (start + 1, start + offset + 1);
        }
    }
    panic!("`{header}` in {WRAP_HOME} is never closed — the guard cannot tell where it ends");
}

/// `  route_table.rs:257 — fn credential(` for each hit, for a failure a reader
/// can act on without opening the file.
fn listing(hits: &[SourceHit<'_>]) -> String {
    hits.iter()
        .map(|(n, _, original)| format!("  {WRAP_HOME}:{n} — {original}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn credential_definitions(text: &str) -> Vec<SourceHit<'_>> {
    let needle = format!("fn {WRAP_CONSTRUCTOR}(");
    source_hits(text, |_, code| code.contains(&needle))
}

fn credential_fields(text: &str) -> Vec<SourceHit<'_>> {
    let (start, end) = block_span(text, WRAP_STRUCT);
    source_hits(text, |line, code| {
        (start..=end).contains(&line) && code.contains(&format!("{WRAP_CONSTRUCTOR}:"))
    })
}

fn credential_call_sites(text: &str) -> Vec<SourceHit<'_>> {
    let definition = format!("fn {WRAP_CONSTRUCTOR}(");
    source_hits(text, |_, code| {
        code.contains(&format!("{WRAP_CONSTRUCTOR}(")) && !code.contains(&definition)
    })
}

#[test]
fn credential_scanners_ignore_non_code_decoys_and_literal_braces() {
    const SAMPLE: &str = r####"
// fn credential(name: &'static str) {}
/* credential: Option<&'static str>, Self::credential("block", apply); */
const NORMAL: &str = "fn credential(name: &'static str) {}";
const RAW: &str = r#"credential: Option<&'static str>, Self::credential("raw", apply);"#;
const BYTES: &[u8] = b"Self::credential(\"bytes\", apply)";
struct Wrap<'a> {
    #[doc = "}"]
    credential: Option<&'a str>,
}
fn credential(name: &'static str) {}
impl Wrap<'static> {
    #[doc = "}"]
    fn runner_auth() {
        let _ = RAW; Self::credential("live", apply);
    }
}
"####;

    let definitions = credential_definitions(SAMPLE);
    assert_eq!(definitions.len(), 1, "{}", listing(&definitions));
    assert_eq!(definitions[0].2, "fn credential(name: &'static str) {}");

    let fields = credential_fields(SAMPLE);
    assert_eq!(fields.len(), 1, "{}", listing(&fields));
    assert_eq!(fields[0].2, "credential: Option<&'a str>,");

    let calls = credential_call_sites(SAMPLE);
    assert_eq!(calls.len(), 1, "{}", listing(&calls));
    assert_eq!(
        calls[0].2,
        "let _ = RAW; Self::credential(\"live\", apply);"
    );
    let (start, end) = block_span(SAMPLE, NAMING_IMPL);
    assert!((start..=end).contains(&calls[0].0));
}

/// The compiler's half of this file's rule: `Wrap::credential` is private, so a
/// credential *name* and the closure it travels with can only be paired inside
/// `route_table`.
///
/// [`a_middleware_claim_names_a_layer_the_route_carries`] compares two strings
/// and believes the wrapper's. That is sound only while nothing outside this
/// module can mint the string: `routes.rs` already imports both `Wrap` and
/// `RUNNER_AUTH_LAYER`, so one `pub(crate)` on the constructor is the whole
/// distance to
///
/// ```ignore
/// Wrap::credential(RUNNER_AUTH_LAYER, |mr| mr.layer(RequestBodyLimitLayer::new(n)))
/// ```
///
/// — a route declaring `Foreign(ForeignGate::Middleware { layer: RUNNER_AUTH_LAYER })`,
/// green through every check above, with no authentication anywhere on it and
/// `Expect::Unchecked` bought in the access sweep on top. That is the
/// `/runners/register` defect this file was written for (`card_cd6f512e2e52`),
/// re-entered through the guard's own blind spot.
///
/// The struct's field is asserted with it because it is the same pairing one
/// level down: a visible `credential` field is a `Wrap { credential: Some(..),
/// apply: .. }` literal written anywhere in the crate, i.e. the constructor's
/// hole with the constructor left untouched.
///
/// The definition line is read whole rather than a set of widened spellings
/// searched for — `pub`, `pub(crate)`, `pub(super)`, `pub(in crate::…)` do not
/// form a closed list and the last is unguessable, the lesson
/// [`the_instance_admin_gate_is_module_private`] paid for by mutation.
/// Requiring exactly one definition is the liveness half: a rename that leaves
/// this guard matching nothing has to be loud, or it goes green while guarding
/// air.
#[test]
fn the_credential_constructor_is_private() {
    let text = wrap_home();
    let needle = format!("fn {WRAP_CONSTRUCTOR}(");

    let definitions = credential_definitions(&text);

    assert_eq!(
        definitions.len(),
        1,
        "expected exactly one definition of `Wrap::{WRAP_CONSTRUCTOR}` in {WRAP_HOME}, found \
         {}:\n{}\nNone means the constructor was renamed or removed, and this file's header — \
         which tells the reader the compiler closes the \"does the named layer check anything\" \
         half — is then a claim about nothing. More than one means the name can be minted in two \
         places, and only one of them is guarded.",
        definitions.len(),
        listing(&definitions)
    );

    let (_, definition, _) = &definitions[0];
    assert!(
        definition.starts_with(&needle),
        "`Wrap::{WRAP_CONSTRUCTOR}` is no longer private to `route_table`:\n{}\n\
         Any module in this crate can now pair a credential name with a closure that does not \
         check it — `Wrap::{WRAP_CONSTRUCTOR}(RUNNER_AUTH_LAYER, |mr| mr.layer(body_limit))` \
         registers a route that declares `Foreign(ForeignGate::Middleware {{ layer }})`, passes \
         every check in this file, and authenticates nobody. Narrow it back to `fn \
         {WRAP_CONSTRUCTOR}(`. If the wider visibility is genuinely wanted, then the claim this \
         file reads back is no longer worth more than the string it is written in, and the \
         header has to stop promising the compiler closes that half.",
        listing(&definitions)
    );

    let fields = credential_fields(&text);

    assert_eq!(
        fields.len(),
        1,
        "expected exactly one `{WRAP_CONSTRUCTOR}` field in `{WRAP_STRUCT}`, found {}:\n{}\n\
         The field is what the constructor writes; if it moved or was renamed, the constructor \
         above is no longer the thing that mints the name and this guard is watching the wrong \
         door.",
        fields.len(),
        listing(&fields)
    );

    assert!(
        fields[0].1.starts_with(&format!("{WRAP_CONSTRUCTOR}:")),
        "the `{WRAP_CONSTRUCTOR}` field of `Wrap` is no longer private to `route_table`:\n{}\n\
         A visible field is the private constructor's hole with the constructor left alone: \
         `Wrap {{ {WRAP_CONSTRUCTOR}: Some(RUNNER_AUTH_LAYER), apply: .. }}` written in any \
         module of this crate mints the same unbacked claim. Narrow it back.",
        listing(&fields)
    );
}

/// The second half, taken as far as a source guard honestly can: the only
/// callers of `Wrap::credential` are the named constructors that apply the
/// layer they name.
///
/// "Each naming constructor attaches the middleware it names" is semantics and
/// this test does not prove it — `Wrap::runner_auth` swapping
/// `runner_auth_layer` for a body limit stays green here, and only reading it
/// catches that. What this does catch is the drift that needs no lie: a new
/// constructor somewhere else in the module minting a name out of its own
/// argument, which is the general-purpose `credential(name, closure)` the
/// module's doc comment says was deliberately not written. Keeping the call
/// inside `impl Wrap<'static>` keeps every mint of a name next to its
/// neighbours, where the missing layer is visible to a reader.
///
/// Call sites are found by the bare name rather than by a list of spellings —
/// `Self::credential(`, `Wrap::credential(`, an aliased import — for the same
/// reason the sibling guard reads the definition line whole.
#[test]
fn the_credential_name_is_minted_only_by_the_naming_constructors() {
    let text = wrap_home();
    let (start, end) = block_span(&text, NAMING_IMPL);
    let call_sites = credential_call_sites(&text);

    assert!(
        !call_sites.is_empty(),
        "nothing in {WRAP_HOME} calls `Wrap::{WRAP_CONSTRUCTOR}` any more. Either no route \
         carries a credential layer — in which case `ForeignGate::Middleware` claims nothing and \
         `a_middleware_claim_names_a_layer_the_route_carries` is asserting over an empty set — or \
         the name is now minted some other way, which is the thing both of these guards exist to \
         see."
    );

    let strays: Vec<SourceHit<'_>> = call_sites
        .iter()
        .filter(|(n, _, _)| !(start..=end).contains(n))
        .cloned()
        .collect();

    assert!(
        strays.is_empty(),
        "a credential name is minted outside `{NAMING_IMPL}` ({WRAP_HOME}:{start}-{end}):\n{}\n\
         That block is the whole rule the route table's claim rests on: one constructor per \
         layer, each applying the middleware it names, all of them side by side. A constructor \
         elsewhere — worse, one that takes the name as an argument — is the general-purpose \
         `{WRAP_CONSTRUCTOR}(name, closure)` this module refused to write, and it hands the next \
         route a `Foreign` sign-off with nothing behind it. Move it into the block, or say here \
         what now holds a named wrapper to doing the check it names.",
        listing(&strays)
    );
}
