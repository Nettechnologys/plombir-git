//! One pass over every route whose gate is an *anchored* extractor.
//!
//! `cross_repo_id_scope_sweep_tests` walks the routes that name a repository in
//! their path and then act on an instance-wide id. Its selector is `{owner}` —
//! and the subject of the sweep is wider than its selector, which is the gap
//! this file closes. `/api/v1/artifacts/{id}` carries exactly the same subject:
//! an instance-wide primary key that reaches a row in somebody's private
//! repository. It names no repository at all, so `repo_tail` returns `None` and
//! every loop over there drops it with a silent `continue`.
//!
//! The persona sweep does not reach them either, and says so: all three artifact
//! rows sit in its `NO_FIXTURE` list, because that fixture builds no pipeline
//! and so has no artifact to address. So until this file, the only thing driving
//! an anchored route was `artifact_file_tests` — one handwritten test, for the
//! one anchor that exists today. That is coverage of an *instance*, not of a
//! *class*, and `api::repo_access::RepoAnchor` is written as a mechanism: the
//! second anchor gets no such test, and nothing goes red.
//!
//! This is the same failure the id-scope sweep already had once. Its selector
//! was `strip_prefix("/api/v1/repos/{owner}/{name}")`, so the
//! `/api/v1/ai/repos/{owner}/{name}/…` group went unswept — one abstraction step
//! up, same silence. `RepoAnchor` is the next step: the repository is named by a
//! *row* instead of by the path.
//!
//! # The shape
//!
//! The population is not a list of routes. It is read out of the tree:
//! [`anchored_aliases`] finds every `pub type X = AnchoredRead|AnchoredWrite<A>`
//! and every handler whose signature takes one is a route this sweep must drive.
//! A hardcoded list of paths is precisely what rotted in the id-scope sweep, and
//! a new anchor has to be *seeded* here or *signed off* in [`NOT_PROBED`] —
//! either way somebody has to have thought about it.
//!
//! Each route is then driven four times, and any two of the requests differ in
//! exactly one place:
//!
//! - **the probe** — an outsider with a perfectly valid session, the victim's
//!   real id. Owed a masked refusal: `401` or `404`, and specifically not `403`.
//! - **the reference** — the same outsider, the same route, [`ABSENT_ID`]. Owed
//!   the same answer as the probe, *body included*.
//! - **the anonymous pair** — both of those again with no token at all. The two
//!   have to match each other; which code they match on is the anchor's own
//!   business (`AnchoredWrite` answers `401` before it resolves anything, which
//!   is why that `401` is not an oracle, and `AnchoredRead` masks even that).
//! - **the baseline** — the owner, the same real id. Owed anything but a denial.
//!
//! The baseline is not decoration. A fixture whose artifact never uploaded would
//! answer `404` to everybody, and a wall of `404`s reads as a passing security
//! test. Here it reads as a dead fixture and fails the run.
//!
//! Neither is the reference. `403` for a real id and `404` for an absent one is
//! the pair that makes `{id}` an enumeration oracle over every row on the
//! instance — and so is `404 artifact expired` against `404 artifact not found`,
//! one level lower, where no assertion about a status can see it. Both were live
//! defects of the one anchor that exists (`card_1419723e0606`), which is the
//! whole reason the comparison is a *pair* and not a constant: the green run
//! that hid them consisted of two individually true assertions.
//!
//! # What it does not do
//!
//! It sends no request bodies. Every anchored route today is a `GET` or a
//! `DELETE`, and a `POST`/`PATCH` anchor would answer `400`/`422` from its
//! deserializer before the gate decided anything — which this sweep reports as a
//! route it cannot judge rather than passing over, so the body support arrives
//! with the route that needs it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use reqwest::{Client, StatusCode};
use rg_http::route_table::RouteFact;

use crate::common::answer::Answer;
use crate::common::source_scan::{
    anchored_aliases, functions, param_base_types, relative, rust_files, signature_params,
    src_root, Anchor,
};
use crate::common::{register_full, spawn_test_app_with_routes_and_db};

const OWNER: &str = "anchorowner";
const OUTSIDER: &str = "anchoroutsider";
/// The owner's private repository — the one every seeded row lives in.
const VAULT: &str = "anchorvault";

/// An id no row has ever carried — the reference every probe is measured
/// against.
const ABSENT_ID: i64 = 999_999;

/// Anchors this sweep deliberately does not drive, each with the reason.
///
/// Keyed on the alias, checked both ways: an entry whose anchor this sweep has
/// started driving fails the run, so a signed-off anchor cannot leave a standing
/// exemption behind once somebody seeds it.
const NOT_PROBED: &[(&str, &str)] = &[];

// ── The fixture ────────────────────────────────────────────────────────────

/// The rows this sweep seeded in the owner's private repository, one per
/// [`RepoAnchor`] implementor it knows how to build.
struct Seeded {
    artifact: i64,
}

/// What this sweep can do about one anchor.
enum Coverage {
    /// A row of this anchor's kind exists in the private repository; here is the
    /// instance-wide id that addresses it.
    Probe(i64),
    /// Nobody decided. Fails the run — see [`NOT_PROBED`].
    Unclassified,
}

/// The seeded row for one anchor, keyed on the [`RepoAnchor`] implementor rather
/// than on the alias: `ArtifactRead` and `ArtifactWrite` are two levels of
/// access to the same row, and seeding it twice would only give the two probes
/// different ids.
fn coverage(target: &str, seeded: &Seeded) -> Coverage {
    match target {
        "Artifact" => Coverage::Probe(seeded.artifact),
        _ => Coverage::Unclassified,
    }
}

struct Fixture {
    base: String,
    client: Client,
    owner_token: String,
    outsider_token: String,
}

async fn create_private_repo(fx: &Fixture, name: &str) -> i64 {
    let response = fx
        .client
        .post(format!("{}/api/v1/repos", fx.base))
        .bearer_auth(&fx.owner_token)
        .json(&serde_json::json!({ "name": name, "is_private": true }))
        .send()
        .await
        .expect("create repository");
    assert_eq!(
        response.status(),
        201,
        "the fixture repository was not created"
    );
    response
        .json::<serde_json::Value>()
        .await
        .expect("repo json")["id"]
        .as_i64()
        .expect("repo id")
}

/// One artifact, uploaded through the runner route so its bytes are on disk and
/// the download route has something to serve.
async fn seed_artifact(fx: &Fixture, db: &rg_db::DatabaseConnection, repo: i64) -> i64 {
    let runner = rg_db::ops::runner_ops::register_runner(db, "anchor-runner", "", None, None, None)
        .await
        .expect("register runner");
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .expect("create pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "unit", "echo ok", None, None, None, None, None, false, None, None, None,
    )
    .await
    .expect("create job");
    rg_db::ops::pipeline_ops::assign_job(db, job.id, runner.id)
        .await
        .expect("assign job");

    let response = fx
        .client
        .post(format!(
            "{}/api/v1/runners/{}/jobs/{}/artifacts",
            fx.base, runner.id, job.id
        ))
        .bearer_auth(&runner.token)
        .header("x-artifact-name", "report.txt")
        .body("artifact bytes")
        .send()
        .await
        .expect("upload artifact");
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert_eq!(status, 201, "the fixture artifact was not uploaded: {body}");
    serde_json::from_str::<serde_json::Value>(&body).expect("upload json")["id"]
        .as_i64()
        .expect("artifact id")
}

// ── Reading the population ─────────────────────────────────────────────────

/// One handler whose signature takes an anchored extractor.
struct AnchoredHandler {
    /// The file it is declared in, relative to `src/` — `api/artifacts.rs`.
    file: String,
    /// The function name — `get_artifact`.
    name: String,
    /// The alias its signature names — `ArtifactRead`.
    alias: String,
}

/// Every handler in the tree that takes one of `aliases`.
///
/// Read from the source rather than from the route table because the alias is
/// only visible in a signature: `RouteFact` records the handler's `type_name`
/// and its declared `Access`, and `RepoRead` is what an anchored route declares
/// too — the route table cannot tell an anchored gate from a path-based one.
fn anchored_handlers(aliases: &BTreeSet<&str>) -> Vec<AnchoredHandler> {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    files.sort();
    let mut out = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("read source file");
        for function in functions(&text) {
            let Some(params) = signature_params(&text, &function.name) else {
                continue;
            };
            for base in param_base_types(&params) {
                if let Some(alias) = aliases.get(base) {
                    out.push(AnchoredHandler {
                        file: relative(file),
                        name: function.name.clone(),
                        alias: (*alias).to_string(),
                    });
                    break;
                }
            }
        }
    }
    out
}

/// `api/artifacts.rs` + `get_artifact` ⇒ `rg_http::api::artifacts::get_artifact`,
/// the spelling `RouteFact::handler` carries.
fn handler_type_name(file: &str, name: &str) -> String {
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    let module = stem.strip_suffix("/mod").unwrap_or(stem);
    format!("rg_http::{}::{name}", module.replace('/', "::"))
}

/// Fill an anchored route's path with one id.
///
/// A path with anything else in it returns `None`: the anchor resolves its
/// repository from the id, so a second placeholder is a locator this sweep would
/// have to seed, and guessing at it is how a probe comes to prove nothing.
fn fill(path: &str, id: i64) -> Option<String> {
    let open = path.find('{')?;
    let close = path[open..].find('}')? + open;
    if path[close + 1..].contains('{') {
        return None;
    }
    Some(format!("{}{id}{}", &path[..open], &path[close + 1..]))
}

/// One section of a failure report: the lines, newline-terminated, or nothing
/// at all when there are none.
///
/// `join("\n")` alone runs the last line of one list into the first of the next
/// as soon as two of them are non-empty, which is precisely when a reader most
/// needs to tell the two apart.
fn block(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

async fn drive(fx: &Fixture, fact: &RouteFact, url: &str, token: Option<&str>) -> Answer {
    let url = format!("{}{url}", fx.base);
    let mut request = match fact.method {
        "GET" => fx.client.get(url),
        "HEAD" => fx.client.head(url),
        "POST" => fx.client.post(url),
        "PUT" => fx.client.put(url),
        "PATCH" => fx.client.patch(url),
        "DELETE" => fx.client.delete(url),
        other => panic!("route table produced an unroutable method {other}"),
    };
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    Answer::of(request.send().await.expect("anchored sweep request")).await
}

/// The probe's predicate: refused *without* being told the row exists — `401` or
/// `404`, and specifically **not** `403`.
///
/// `403` is the one answer this sweep's subject matter forbids, and the anchored
/// extractors say so in their own doc comments: the caller supplied an opaque
/// integer, so any answer other than the one an absent id gets confirms the
/// integer hit a row. `401` is admissible and is not a leak — a gate that
/// authenticates before it resolves anything answers the same way either way,
/// which is exactly the order `AnchoredWrite` keeps.
fn masked(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 404)
}

/// The baseline's predicate: any denial at all. Deliberately wider than
/// [`masked`] — a `403` to the owner on the owner's own row means the fixture is
/// dead and the refusals above proved nothing.
fn refused(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 403 | 404)
}

/// A body complaint is not a verdict on the id: the handler rejected the request
/// before it looked anything up.
fn inconclusive(status: StatusCode) -> bool {
    matches!(status.as_u16(), 400 | 409 | 415 | 422)
}

// ── The sweep ──────────────────────────────────────────────────────────────

/// One route to drive: where it is, and the id it addresses.
struct Probe<'a> {
    fact: &'a RouteFact,
    alias: String,
    url: String,
    absent: String,
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn no_anchored_route_confirms_a_row_of_a_private_repository() {
    let (base, facts, db) = spawn_test_app_with_routes_and_db().await;
    let (owner_token, _owner_id) =
        register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    let (outsider_token, _outsider_id) =
        register_full(&base, OUTSIDER, &format!("{OUTSIDER}@example.com")).await;
    let fx = Fixture {
        base,
        client: Client::builder().build().expect("http client"),
        owner_token,
        outsider_token,
    };
    let repo = create_private_repo(&fx, VAULT).await;
    let seeded = Seeded {
        artifact: seed_artifact(&fx, &db, repo).await,
    };

    assert!(
        !facts.is_empty(),
        "the route table came back empty — the sweep is not testing anything"
    );

    // ── The population, read out of the tree ───────────────────────────────
    let anchors = anchored_aliases();
    assert!(
        anchors.len() >= 2,
        "no `AnchoredRead`/`AnchoredWrite` aliases were found in the tree. Either the anchored \
         extractors are gone — in which case delete this sweep — or the census is broken, and a \
         broken census makes this whole file a green no-op"
    );
    let by_alias: BTreeMap<&str, &Anchor> = anchors
        .iter()
        .map(|anchor| (anchor.alias.as_str(), anchor))
        .collect();
    let aliases: BTreeSet<&str> = by_alias.keys().copied().collect();

    // ── The routes those aliases gate ──────────────────────────────────────
    let mut probes: Vec<Probe> = Vec::new();
    let mut unclassified: Vec<String> = Vec::new();
    let mut unrouted: Vec<String> = Vec::new();
    for handler in anchored_handlers(&aliases) {
        let anchor = by_alias
            .get(handler.alias.as_str())
            .expect("every handler's alias came from the census");
        let type_name = handler_type_name(&handler.file, &handler.name);
        let mut routed = false;
        for fact in facts.iter().filter(|fact| fact.handler == type_name) {
            routed = true;
            let Coverage::Probe(id) = coverage(&anchor.target, &seeded) else {
                unclassified.push(format!(
                    "  {} — gated by `{}`, whose anchor `{}` ({}) this sweep cannot address. \
                     Seed a row of it in the fixture and give `coverage` a line, or sign the \
                     anchor off in NOT_PROBED with the reason it cannot be probed",
                    fact.label(),
                    handler.alias,
                    anchor.target,
                    anchor.file,
                ));
                continue;
            };
            let (Some(url), Some(absent)) = (fill(&fact.path, id), fill(&fact.path, ABSENT_ID))
            else {
                unclassified.push(format!(
                    "  {} — gated by `{}`, and its path carries something besides the anchored \
                     id. Teach `fill` to seed that locator; a placeholder filled by guesswork is \
                     a probe that proves nothing",
                    fact.label(),
                    handler.alias,
                ));
                continue;
            };
            probes.push(Probe {
                fact,
                alias: handler.alias.clone(),
                url,
                absent,
            });
        }
        if !routed {
            unrouted.push(format!(
                "  {}::{} takes `{}` and no route in the table names it",
                handler.file, handler.name, handler.alias,
            ));
        }
    }

    // Every anchor is driven or signed off. This is the guard the file exists
    // for: the next `pub type … = AnchoredRead<…>` is written, hung on a route,
    // and lands here rather than in the silence the id-scope sweep left it.
    let driven: BTreeSet<String> = probes.iter().map(|probe| probe.alias.clone()).collect();
    let mut unswept: Vec<String> = Vec::new();
    for anchor in &anchors {
        if driven.contains(anchor.alias.as_str())
            || NOT_PROBED
                .iter()
                .any(|(entry, _)| *entry == anchor.alias.as_str())
        {
            continue;
        }
        unswept.push(format!(
            "  `{}` ({}) is declared and this sweep drives no route behind it",
            anchor.alias, anchor.file,
        ));
    }
    // …and a sign-off that has stopped being one has to go, the way every other
    // quarantine list in this directory is checked in reverse.
    let mut healed: Vec<String> = Vec::new();
    for (alias, reason) in NOT_PROBED {
        if driven.contains(*alias) {
            healed.push(format!(
                "  `{alias}` is being driven now — drop it from NOT_PROBED (was: {reason})"
            ));
        } else if !aliases.contains(*alias) {
            healed.push(format!(
                "  NOT_PROBED names `{alias}`, which is no longer an anchored alias — drop it \
                 (was: {reason})"
            ));
        }
    }

    assert!(
        unclassified.is_empty() && unrouted.is_empty() && unswept.is_empty() && healed.is_empty(),
        "the anchored extractors and the routes this sweep drives have come apart: {} route(s) \
         nothing can address, {} handler(s) on no route, {} anchor(s) driven by nothing, {} stale \
         sign-off(s).\nAn anchored route is invisible to `cross_repo_id_scope_sweep_tests` (its \
         path names no repository) and to `route_access_sweep_tests` (no fixture builds the row), \
         so this sweep is the only pass that drives one at all.\n{}{}{}{}",
        unclassified.len(),
        unrouted.len(),
        unswept.len(),
        healed.len(),
        block(&unclassified),
        block(&unrouted),
        block(&unswept),
        block(&healed),
    );
    assert!(
        !probes.is_empty(),
        "no anchored route is being driven — the census found aliases and the mapping to routes \
         dropped every one of them"
    );

    // The same population, read off the route table instead of out of the
    // source, and the two have to agree. A repository-scoped route names its
    // repository one of exactly two ways: in the path, or through a row — so a
    // declared repository level with no `{owner}` in the path *is* an anchored
    // route, whatever a signature happens to say.
    //
    // Without this the file has the very blind spot it was written to close.
    // `anchored_handlers` finds a route by reading its handler's signature, and
    // a signature the reader cannot parse drops that one route while its alias
    // stays covered by a sibling route — no assertion above would notice. Two
    // independent selectors cannot both go quiet on the same day.
    let mut unseen: Vec<String> = Vec::new();
    for fact in &facts {
        let names_repository = fact.path.contains("{owner}")
            && (fact.path.contains("{name}") || fact.path.contains("{repo}"));
        if !fact.access.is_repo_scoped() || names_repository {
            continue;
        }
        if !probes
            .iter()
            .any(|probe| probe.fact.label() == fact.label())
        {
            unseen.push(format!(
                "  {} declares {:?} and its path names no repository, so it can only be resolving \
                 one from a row — and this sweep is not driving it. Either its gate is an anchored \
                 extractor the census should have found (say why the signature reader missed it), \
                 or the route declares a repository level it has no repository to apply it to, \
                 which is the worse of the two findings.",
                fact.label(),
                fact.access,
            ));
        }
    }
    assert!(
        unseen.is_empty(),
        "{} route(s) declare a repository access level, name no repository in their path, and are \
         driven by nothing here.\n{}",
        unseen.len(),
        block(&unseen),
    );

    // The destructive rows last, deepest path first, so a probe is never driven
    // against a row an earlier baseline has already deleted.
    probes.sort_by_key(|probe| {
        let destructive = probe.fact.method == "DELETE";
        let depth = if destructive {
            usize::MAX - probe.fact.path.matches('/').count()
        } else {
            0
        };
        (
            destructive,
            depth,
            probe.fact.path.clone(),
            probe.fact.method,
        )
    });

    let mut leaks: Vec<String> = Vec::new();
    let mut oracles: Vec<String> = Vec::new();
    // How many pairs of each anchor were compared with a body on both sides.
    let mut paired: BTreeMap<String, usize> = BTreeMap::new();

    // ── Pass one: an outsider with a valid session, against a real row ─────
    for probe in &probes {
        let label = probe.fact.label();
        let real = drive(&fx, probe.fact, &probe.url, Some(&fx.outsider_token)).await;
        let absent = drive(&fx, probe.fact, &probe.absent, Some(&fx.outsider_token)).await;

        if real.status == StatusCode::FORBIDDEN {
            leaks.push(format!(
                "  {label}\n      answered 403 for a row in {OWNER}/{VAULT}, a private \
                 repository. The refusal confirmed the id exists, which is the one thing a masked \
                 denial may not do: the caller supplied an opaque integer, so walking it \
                 enumerates every row of this kind on the instance.\n      body: {}",
                real.excerpt()
            ));
        } else if inconclusive(real.status) {
            leaks.push(format!(
                "  {label}\n      answered {} — the request never reached the row, so nothing was \
                 proven. This sweep sends no request bodies (no anchored route needed one yet); \
                 give it the body this route wants.\n      body: {}",
                real.status,
                real.excerpt()
            ));
        } else if !masked(real.status) {
            leaks.push(format!(
                "  {label}\n      answered {} for a row in {OWNER}/{VAULT} to a caller with no \
                 rights there at all. The anchor resolved the repository and the gate behind it \
                 did not turn him away.\n      body: {}",
                real.status,
                real.excerpt()
            ));
        } else if real.shape() != absent.shape() {
            oracles.push(format!(
                "  {label}\n      refused a real row and an id that never existed, but not with \
                 the same answer — so the pair tells the caller which of the two he hit, and \
                 walking the id space enumerates the instance.\n      real   ({}): {}\n      \
                 absent ({}): {}",
                real.status,
                real.excerpt(),
                absent.status,
                absent.excerpt(),
            ));
        } else if real.speaks() && absent.speaks() {
            *paired.entry(probe.alias.clone()).or_default() += 1;
        }

        // The same pair with no credential at all. Which code the two agree on
        // is the anchor's business — `AnchoredWrite` answers `401` before it
        // resolves anything, and that `401` is not an oracle precisely because
        // it arrives first — but they have to agree: an `AnchoredRead` whose
        // `401` came *after* the lookup told an anonymous caller, with no
        // account at all, which ids are real (card_1419723e0606).
        let anon_real = drive(&fx, probe.fact, &probe.url, None).await;
        let anon_absent = drive(&fx, probe.fact, &probe.absent, None).await;
        if anon_real.shape() != anon_absent.shape() {
            oracles.push(format!(
                "  {label}\n      an anonymous caller is answered differently for a real row and \
                 for an id that never existed, so the route enumerates private rows to callers \
                 with no account at all.\n      real   ({}): {}\n      absent ({}): {}",
                anon_real.status,
                anon_real.excerpt(),
                anon_absent.status,
                anon_absent.excerpt(),
            ));
        }
    }

    // ── Pass two: the owner, on his own row ────────────────────────────────
    let mut dead: Vec<String> = Vec::new();
    for probe in &probes {
        let baseline = drive(&fx, probe.fact, &probe.url, Some(&fx.owner_token)).await;
        if refused(baseline.status) || baseline.status.is_server_error() {
            dead.push(format!(
                "  {}\n      the owner is answered {} on his own row, so the refusals above \
                 describe a broken fixture rather than a gate. Either the row was never seeded, \
                 or an earlier probe destroyed it.\n      body: {}",
                probe.fact.label(),
                baseline.status,
                baseline.excerpt(),
            ));
        }
    }

    assert!(
        leaks.is_empty() && oracles.is_empty() && dead.is_empty(),
        "anchored id scope: {} route(s) let an outsider reach a private row, {} told a real id \
         apart from an absent one, {} dead baseline(s), out of {} driven.\n{}{}{}",
        leaks.len(),
        oracles.len(),
        dead.len(),
        probes.len(),
        block(&leaks),
        block(&oracles),
        block(&dead),
    );

    // Not a coverage counter — a non-vacuity one, and tied to the population
    // rather than to a number that rots. Every assertion above is satisfied by
    // two empty bodies, so an anchor whose routes stopped answering with a body
    // — or whose pair stopped being compared at all — would keep the whole file
    // green while asserting nothing. That is exactly how the status half of the
    // id-scope sweep went vacuous in `card_c46c354ec3ae`.
    let mut silent: Vec<String> = Vec::new();
    for alias in &driven {
        if paired.get(alias.as_str()).copied().unwrap_or_default() == 0 {
            silent.push(format!(
                "  `{alias}` — every pair compared was empty on one side, so only the status was \
                 asserted"
            ));
        }
    }
    assert!(
        silent.is_empty(),
        "{} of {} driven anchor(s) had no masked refusal compared against an absent id's body and \
         all. The oracle-in-the-body half of this sweep is only asserted where that pair \
         holds.\n{}",
        silent.len(),
        driven.len(),
        block(&silent),
    );
}
