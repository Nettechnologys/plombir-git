//! Every route, asked the same question twice: once of a healthy server, once
//! of a server whose infrastructure has been taken away underneath it.
//!
//! `route_access_sweep_tests` retired the "did we forget a gate?" question by
//! making the route table enumerable and walking it. This file does the same
//! for the other contract every handler carries and nothing checks: **a failure
//! of ours must not be reported as a fault of the caller's.** That class has
//! been closed by hand four times in this phase — 35 sites in `card_45ae7515860a`,
//! 19 more in `card_a086f3421ee9` — and it regrew each time, because the only
//! thing holding it shut was a convention (`card_bbf156b20bd5`) that lives in a
//! card rather than in a test.
//!
//! # Why this is a differential, not a table of 325 declared outcomes
//!
//! The obvious shape — annotate every route with the infrastructure it depends
//! on, the way it already declares an `Access` level — was rejected. Nearly
//! every row would read "database", the annotation would be written by the same
//! person who wrote the handler and would therefore repeat the same wrong
//! assumption, and 325 of them would be a diff nobody reviews.
//!
//! The differential needs no such declaration and cannot be lied to. A route is
//! driven against a healthy server and against a broken one, and the verdict
//! comes from the *pair*:
//!
//! | healthy | broken  | verdict                                              |
//! |---------|---------|------------------------------------------------------|
//! | 2xx     | 5xx     | correct — the failure is ours and it says so          |
//! | 2xx     | 4xx     | **the defect** — the route worked, the infrastructure |
//! |         |         | died, and the client got the blame                    |
//! | 2xx     | 2xx `[]`| **silent failure** — the answer went empty instead of |
//! |         |         | failing (the `let _ =` class, seen from outside)       |
//! | 2xx     | 2xx     | the route never touched this subsystem                |
//! | not 2xx | *       | no baseline: the fixture never reached the handler    |
//!
//! Three subsystems are taken away, because "infrastructure" is three different
//! things to a forge and a handler can classify one correctly while collapsing
//! another: the **database**, the **blob store**, and the **git tree on disk**.
//! The database gets two passes rather than one — see [`Fault`] for why a total
//! outage cannot reach a handler at all.
//!
//! # What this cannot see
//!
//! Stated here rather than in a card, because a reader deciding what a green run
//! means needs them in front of them:
//!
//! - **A route with no 2xx baseline is not checked at all.** Every write route
//!   is driven with an empty JSON body, so most answer `400` to the healthy
//!   server too and drop out. 99 of 334 routes currently have a baseline, which
//!   is why [`COVERAGE_FLOOR`] exists: the sweep asserts how much of the table it
//!   actually held under the question, so it cannot quietly decay into a test of
//!   eleven endpoints.
//! - **`2xx` healthy and `2xx` broken is read as "did not touch it".** A route
//!   that swallowed the failure and answered a *plausible* body is
//!   indistinguishable from one that never made the call. Only the collapse to
//!   an empty body is caught — which is why `GET .../issue_config` is *not*
//!   caught here even though its handler does map a git failure to `400`: with
//!   the tree gone it returns a default config and `200`, and that is the silent
//!   half of the same defect rather than the loud one.
//! - **Both database passes keep the `users` table alive.** They have to:
//!   `session_standing_middleware` reads it on every authenticated request, so a
//!   total outage answers `503` from the middleware and no handler is reached.
//!   A route whose only database access is that one lookup is therefore not
//!   covered.

use std::collections::{BTreeMap, BTreeSet};

use reqwest::{Client, StatusCode};
use rg_http::route_table::RouteFact;

use crate::common::fault::{
    drop_every_table_except, spawn_test_app_for_fault_sweep, AUTH_TABLES, GATE_TABLES,
};
use crate::common::register_user;

const PW: &str = "Qz7$wRtm";
const OWNER: &str = "faultowner";
const REPO: &str = "faultrepo";

/// The fraction of the route table that must reach a handler with a `2xx` on
/// the healthy pass, or the sweep is not entitled to call itself a sweep.
///
/// A floor rather than an exact count: the point is to fail loudly if a fixture
/// change or a new gate quietly halves the coverage, not to pin a number that
/// every added route perturbs.
const COVERAGE_FLOOR: usize = 60;

// ── The faults ─────────────────────────────────────────────────────────────

/// A piece of infrastructure a pass takes away from the running server.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fault {
    /// Every table but `users` is gone. Reads and writes alike fail at the
    /// statement, which is what a corrupted page or a dropped schema looks
    /// like from a handler's side.
    ///
    /// This lands on the *gate*: a repository-scoped route never reaches its
    /// handler, because `RepoRead` and friends resolve the repository first and
    /// fail there.
    Database,
    /// The same outage, except that authentication and repository resolution
    /// still work — see `GATE_TABLES`.
    ///
    /// This is the pass that reaches handlers, and it exists because the total
    /// outage above cannot: with `repositories` gone, every gate answers before
    /// the handler runs, so a handler that turns a database error into `400` is
    /// shielded from the fault and the sweep would call it correct. Measured,
    /// not assumed — a deliberate collapse planted in `list_issues` was invisible
    /// to the total-outage pass and is caught by this one.
    DatabaseBehindTheGate,
    /// The blob store fails every `put`, `put_file`, `get` and `delete`.
    BlobStore,
    /// The repository's bare git directory has been removed from disk while its
    /// database row stays — the shape a bind-mount that did not come back after
    /// a restart leaves behind.
    GitTree,
}

impl Fault {
    fn label(self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::DatabaseBehindTheGate => "database behind the gate",
            Self::BlobStore => "blob store",
            Self::GitTree => "git tree",
        }
    }
}

// ── The exceptions, spelled out ────────────────────────────────────────────

/// Routes driven on neither pass, each with the reason.
///
/// Kept deliberately short. A route belongs here only when driving it makes the
/// *rest* of the pass meaningless — not merely because it is inconvenient.
const NOT_DRIVEN: &[(&str, &str)] = &[
    (
        "POST /api/v1/users/logout",
        "revokes the session the rest of the pass authenticates with",
    ),
    (
        "DELETE /api/v1/repos/{owner}/{name}",
        "deletes the repository the rest of the pass is scoped to",
    ),
    (
        "POST /api/v1/repos/{owner}/{name}/transfer",
        "moves the repository out from under the fixture",
    ),
    (
        "POST /api/v1/users/mfa/enable",
        "a second factor the rest of the pass has no way to present",
    ),
];

/// Known instances of the class: the route blames the caller for a failure of
/// ours, the defect is filed, and it is not fixed yet.
///
/// An entry is `(fault label METHOD /path, why)`. The sweep fails if a route
/// outside this list misbehaves — and it *also* fails if a route in it starts
/// behaving, so the list cannot rot into a blanket allowance. This is the queue
/// the card asked for: it shrinks as the handlers are fixed, and it is the
/// evidence that the gate sees them rather than being vacuously green.
const KNOWN_GAPS: &[(&str, &str)] = &[
    // card_de1377f8da43 is fixed: the smart protocol used to answer "no such
    // repository" to a database outage and ship `db: find repo by owner and
    // name` in a body nothing sanitizes. `check_git_access` now answers 404
    // only to a typed `rg_core::error::NotFound`; see
    // `git_http_failure_status_tests` for the body half, which this sweep
    // does not look at.
    //
    // card_b013d630a280 — a bare repository that is in the database but gone
    // from disk reads as a deleted repository. Ten of the ten routes that see
    // this fault get it wrong, because they all share one open-the-repository
    // layer that cannot tell "absent" from "unopenable".
    (
        "git tree GET /api/v1/repos/{owner}/{name}/archive/{archive}",
        "card_b013d630a280: missing bare repo -> 404 (`stream_git_archive_with_idle`)",
    ),
    (
        "git tree GET /api/v1/repos/{owner}/{name}/log",
        "card_b013d630a280: missing bare repo -> 404 (`get_log`)",
    ),
    (
        "git tree GET /api/v1/repos/{owner}/{name}/blob/{*path}",
        "card_b013d630a280: missing bare repo -> 404",
    ),
    (
        "git tree GET /api/v1/repos/{owner}/{name}/tree",
        "card_b013d630a280: missing bare repo -> 404",
    ),
    (
        "git tree GET /api/v1/repos/{owner}/{name}/branches",
        "card_b013d630a280: missing bare repo -> 404",
    ),
    (
        "git tree GET /api/v1/repos/{owner}/{name}/tags",
        "card_b013d630a280: missing bare repo -> 404",
    ),
    (
        "git tree POST /git/{owner}/{repo}/git-upload-pack",
        "card_b013d630a280: missing bare repo -> 404",
    ),
    (
        "git tree POST /git/{owner}/{repo}/git-receive-pack",
        "card_b013d630a280: missing bare repo -> 404",
    ),
    (
        "git tree POST /{owner}/{repo}/git-upload-pack",
        "card_b013d630a280: same handler on the root mount",
    ),
    (
        "git tree POST /{owner}/{repo}/git-receive-pack",
        "card_b013d630a280: same handler on the root mount",
    ),
];

fn listed(list: &[(&str, &str)], key: &str) -> bool {
    list.iter().any(|(entry, _)| *entry == key)
}

/// Split a `KNOWN_GAPS` key into `(fault label, route label)`.
///
/// A route label is always the last two words (`METHOD /path`), so everything
/// before them is the fault. Splitting rather than prefix-matching is not
/// fussiness: `"database"` is a prefix of `"database behind the gate"`, so a
/// `starts_with` would let the total-outage pass claim the behind-the-gate
/// pass's entries and report them as healed.
fn gap_parts(key: &str) -> (String, String) {
    let words: Vec<&str> = key.split_whitespace().collect();
    let split = words.len().saturating_sub(2);
    (words[..split].join(" "), words[split..].join(" "))
}

// ── Fixture ────────────────────────────────────────────────────────────────

/// What the fixture seeded, for the placeholders a constant cannot fill.
///
/// Both passes run this same function against their own server, so the healthy
/// and the broken run are asking about the same resources — which is the whole
/// basis of comparing their answers.
struct Seed {
    comment_id: String,
    /// An issue attachment, so the blob-store pass has a route whose bytes live
    /// in the store it is about to take away. Without one, every blob route
    /// answers `404` on the healthy pass too and that whole pass is vacuous.
    attachment_id: String,
}

async fn seed(base: &str, client: &Client) -> (String, Seed) {
    let token = register_user(base, OWNER, &format!("{OWNER}@example.com"), PW).await;

    // `auto_init` matters: without it the bare repository has no commit, and
    // every content/tree/log route answers "empty repository" on the healthy
    // pass too, so the git-tree fault would have nothing to break.
    let resp = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": REPO,
            "is_private": false,
            "auto_init": true,
            "readme": "default",
        }))
        .send()
        .await
        .expect("create repo");
    assert_eq!(
        resp.status(),
        201,
        "fixture: creating '{REPO}' failed: {}",
        resp.status()
    );

    let repo = format!("{base}/api/v1/repos/{OWNER}/{REPO}");

    let resp = client
        .post(format!("{repo}/issues"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "sweep"}))
        .send()
        .await
        .expect("create issue");
    assert!(
        resp.status().is_success(),
        "fixture: creating an issue failed: {}",
        resp.status()
    );

    let resp = client
        .post(format!("{repo}/issues/1/comments"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"body": "sweep"}))
        .send()
        .await
        .expect("create comment");
    assert!(
        resp.status().is_success(),
        "fixture: commenting failed: {}",
        resp.status()
    );
    let comment: serde_json::Value = resp.json().await.expect("comment body");
    let comment_id = comment["id"].as_i64().expect("comment id").to_string();

    // A label and a milestone, so the routes that read one back have something
    // to find rather than answering 404 on both passes.
    for (path, body) in [
        (
            "labels",
            serde_json::json!({"name": "sweep", "color": "#ff0000"}),
        ),
        ("milestones", serde_json::json!({"title": "sweep"})),
    ] {
        let resp = client
            .post(format!("{repo}/{path}"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .expect("seed request");
        assert!(
            resp.status().is_success(),
            "fixture: creating a {path} entry failed: {}",
            resp.status()
        );
    }

    let resp = client
        .post(format!("{repo}/issues/1/assets"))
        .bearer_auth(&token)
        .multipart(
            reqwest::multipart::Form::new().part(
                "attachment",
                reqwest::multipart::Part::bytes(b"sweep attachment".to_vec())
                    .file_name("sweep.txt")
                    .mime_str("text/plain")
                    .expect("mime"),
            ),
        )
        .send()
        .await
        .expect("upload attachment");
    assert_eq!(
        resp.status(),
        201,
        "fixture: uploading an attachment failed: {}",
        resp.status()
    );
    let attachment: serde_json::Value = resp.json().await.expect("attachment body");
    let attachment_id = attachment["id"]
        .as_i64()
        .expect("attachment id")
        .to_string();

    (
        token,
        Seed {
            comment_id,
            attachment_id,
        },
    )
}

// ── Path filling ───────────────────────────────────────────────────────────

/// Turn a route pattern into a concrete URL against the fixture.
///
/// Unknown placeholders become `1`, for the same reason the access sweep does
/// it: the fixture seeds id 1 for the resources it creates, and a route where a
/// wrong inner id changes the verdict is a route that answered before it looked
/// the id up — which shows as "no baseline" here rather than as a false pass.
fn fill(path: &str, seed: &Seed) -> String {
    let is_org_route = path.starts_with("/api/v1/orgs/") || path.starts_with("/api/v1/admin/orgs/");

    let mut out = String::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..].find('}').expect("unclosed path placeholder") + open;
        let raw = &rest[open + 1..close];
        let name = raw.strip_prefix('*').unwrap_or(raw);
        out.push_str(match name {
            "owner" => OWNER,
            "name" if is_org_route => OWNER,
            "name" | "repo" => REPO,
            "comment_id" => &seed.comment_id,
            "attachment_id" => &seed.attachment_id,
            "username" => OWNER,
            "path" | "file" => "README.md",
            "branch" | "ref" | "rev" | "sha" | "commit" => "main",
            "title" => "Home",
            "slug" => "sweep-provider",
            "archive" => "main.zip",
            "secret_name" => "SWEEP_SECRET",
            "pkg_type" => "cargo",
            "pkg" | "pkg_name" | "gem_name" => "sweeppkg",
            "group_id" => "com.example",
            "artifact_id" => "sweep",
            "version" => "1.0.0",
            "reference" => "latest",
            "digest" | "uuid" => {
                "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            }
            _ => "1",
        });
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

// ── Driving one route ──────────────────────────────────────────────────────

/// The answer a single probe got, reduced to what the verdict needs.
#[derive(Clone)]
struct Answer {
    status: StatusCode,
    body: String,
}

impl Answer {
    /// Whether a `2xx` body carries anything at all.
    ///
    /// `[]`, `{}`, `null` and an empty body are all "the answer went away"; a
    /// list endpoint whose table has just been dropped answering `200 []` is the
    /// silent-failure class seen from the client's side.
    fn is_empty_payload(&self) -> bool {
        let trimmed = self.body.trim();
        trimmed.is_empty()
            || matches!(trimmed, "[]" | "{}" | "null")
            || trimmed == r#"{"items":[]}"#
    }
}

async fn probe(client: &Client, base: &str, token: &str, fact: &RouteFact, seed: &Seed) -> Answer {
    let url = format!("{}{}", base, fill(&fact.path, seed));
    let mut req = match fact.method {
        "GET" => client.get(url),
        "HEAD" => client.head(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "PATCH" => client.patch(url),
        "DELETE" => client.delete(url),
        other => panic!("route table produced an unroutable method {other}"),
    };
    req = req.bearer_auth(token);
    if matches!(fact.method, "POST" | "PUT" | "PATCH") {
        req = req.json(&serde_json::json!({}));
    }
    let resp = req.send().await.expect("sweep request");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Answer { status, body }
}

/// Walk the whole table once against one server, in a fixed order.
///
/// The order is the table's own, minus [`NOT_DRIVEN`]. Both passes walk it
/// identically, so a route that mutates state perturbs the healthy and the
/// broken run at the same point — the comparison stays honest even though the
/// server does not stand still.
async fn walk(
    client: &Client,
    base: &str,
    token: &str,
    facts: &[RouteFact],
    seed: &Seed,
) -> BTreeMap<String, Answer> {
    let mut answers = BTreeMap::new();
    for fact in facts {
        let label = fact.label();
        if listed(NOT_DRIVEN, &label) {
            continue;
        }
        answers.insert(label, probe(client, base, token, fact, seed).await);
    }
    answers
}

/// What the pair of answers says about the route.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The route worked, the infrastructure died, the client got the blame.
    Blamed,
    /// The route worked, the infrastructure died, and it answered success with
    /// nothing in it.
    WentSilent,
    /// Either correct, or outside what this pass can see.
    Fine,
}

fn judge(healthy: &Answer, broken: &Answer) -> Verdict {
    // No baseline: the fixture never got this route to do its job, so whatever
    // it answers under the fault says nothing about how it classifies failure.
    if !healthy.status.is_success() {
        return Verdict::Fine;
    }
    if broken.status.is_client_error() {
        return Verdict::Blamed;
    }
    if broken.status.is_success() && broken.is_empty_payload() && !healthy.is_empty_payload() {
        return Verdict::WentSilent;
    }
    Verdict::Fine
}

// ── The sweep ──────────────────────────────────────────────────────────────

async fn sweep(fault: Fault) {
    let client = Client::builder().build().expect("http client");

    // The healthy pass, on its own server. Its answers are the baseline: what
    // this route does when everything underneath it works.
    let healthy_app = spawn_test_app_for_fault_sweep().await;
    let (healthy_token, healthy_seed) = seed(&healthy_app.base, &client).await;
    assert!(
        !healthy_app.facts.is_empty(),
        "the route table came back empty — the sweep is not testing anything"
    );
    // A typo in an exception list would silently widen the blind spot, so both
    // lists are checked against the table before anything is driven. A
    // `KNOWN_GAPS` key is `<fault label> METHOD /path`; the route it names is
    // the last two words.
    let labels: BTreeSet<String> = healthy_app.facts.iter().map(RouteFact::label).collect();
    for (entry, reason) in NOT_DRIVEN {
        assert!(
            labels.contains(*entry),
            "NOT_DRIVEN names '{entry}' ({reason}), which is not a route any more — drop it"
        );
    }
    for (key, reason) in KNOWN_GAPS {
        let (_, route) = gap_parts(key);
        assert!(
            labels.contains(&route),
            "KNOWN_GAPS names '{key}' ({reason}), which is not a route any more — drop it"
        );
    }

    let healthy = walk(
        &client,
        &healthy_app.base,
        &healthy_token,
        &healthy_app.facts,
        &healthy_seed,
    )
    .await;

    // A second server, seeded identically, then broken.
    let broken_app = spawn_test_app_for_fault_sweep().await;
    let (broken_token, broken_seed) = seed(&broken_app.base, &client).await;
    match fault {
        Fault::Database | Fault::DatabaseBehindTheGate => {
            let keep = if fault == Fault::Database {
                AUTH_TABLES
            } else {
                GATE_TABLES
            };
            let dropped = drop_every_table_except(&broken_app.db, keep).await;
            assert!(
                dropped > 40,
                "only {dropped} table(s) were dropped — the database is not broken, so \
                 every verdict below would be meaningless"
            );
        }
        Fault::BlobStore => broken_app.blob_faults.fail_everything(),
        Fault::GitTree => {
            let git_dir = broken_app.repo_root.join(format!("{OWNER}/{REPO}.git"));
            assert!(
                git_dir.is_dir(),
                "fixture: no bare repository at {} — the git-tree fault would break nothing",
                git_dir.display()
            );
            std::fs::remove_dir_all(&git_dir).expect("remove the bare repository");
        }
    }
    let broken = walk(
        &client,
        &broken_app.base,
        &broken_token,
        &broken_app.facts,
        &broken_seed,
    )
    .await;

    // A pass whose baseline is thin proves little, and one whose baseline
    // silently collapsed proves nothing at all.
    let covered = healthy.values().filter(|a| a.status.is_success()).count();
    assert!(
        covered >= COVERAGE_FLOOR,
        "only {covered} route(s) answered 2xx on the healthy pass, below the floor of \
         {COVERAGE_FLOOR}. The {} sweep is testing almost nothing — the fixture broke, \
         not the server.",
        fault.label(),
    );

    // A fault that changed nobody's answer did not arm, and a sweep over a
    // server that is not actually broken is green for the worst possible
    // reason. Counting the routes whose answer *moved* is the cheap proof that
    // the pass measured something — and the list is worth printing, because it
    // is also the honest statement of how much of the table this fault reaches.
    let disturbed: Vec<&String> = healthy
        .iter()
        .filter(|(label, answer)| {
            answer.status.is_success()
                && broken
                    .get(*label)
                    .is_some_and(|other| other.status != answer.status)
        })
        .map(|(label, _)| label)
        .collect();
    eprintln!(
        "[{}] {covered}/{} routes had a baseline, {} of them noticed the fault",
        fault.label(),
        healthy.len(),
        disturbed.len(),
    );
    assert!(
        !disturbed.is_empty(),
        "taking the {} away changed not one answer out of {covered} working routes. \
         The fault did not arm — this pass is green because nothing was broken, not \
         because the handlers are right.",
        fault.label(),
    );

    let mut report = String::new();
    let mut failures: BTreeSet<String> = BTreeSet::new();
    let mut seen_gaps: BTreeSet<String> = BTreeSet::new();

    for (label, healthy_answer) in &healthy {
        let Some(broken_answer) = broken.get(label) else {
            continue;
        };
        let verdict = judge(healthy_answer, broken_answer);
        if verdict == Verdict::Fine {
            continue;
        }
        let key = format!("{} {label}", fault.label());
        let note = match verdict {
            Verdict::Blamed => "the client is told it made a bad request",
            Verdict::WentSilent => "the answer went empty instead of failing",
            Verdict::Fine => unreachable!(),
        };
        if listed(KNOWN_GAPS, &key) {
            seen_gaps.insert(key);
            continue;
        }
        failures.insert(key.clone());
        report.push_str(&format!(
            "  {key}\n      healthy {} -> broken {} — {note}\n      broken body: {}\n",
            healthy_answer.status,
            broken_answer.status,
            broken_answer.body.chars().take(160).collect::<String>(),
        ));
    }

    // A quarantined route that has started behaving must leave the list.
    let mut healed: Vec<String> = Vec::new();
    for (key, reason) in KNOWN_GAPS {
        if gap_parts(key).0 == fault.label() && !seen_gaps.contains(*key) {
            healed.push(format!(
                "  {key} now classifies the failure correctly — drop it from KNOWN_GAPS \
                 (was: {reason})"
            ));
        }
    }

    assert!(
        failures.is_empty() && healed.is_empty(),
        "{} route(s) mis-classified a {} failure, {} stale quarantine entr(ies). \
         ({covered} of {} routes had a working baseline.)\n\
         Each route below answered the healthy server with a 2xx and then, with the {} \
         gone, told the caller the fault was theirs. A 4xx is not retried and does not \
         page anyone: route the error through `AppError::from` and give the service error \
         a type (`rg_core::error::{{InvalidRequest, NotFound, Conflict, Forbidden}}`) so \
         only the caller's own mistakes stay 4xx.\n{report}{}",
        failures.len(),
        fault.label(),
        healed.len(),
        healthy.len(),
        fault.label(),
        healed.join("\n"),
    );
}

/// A dead database must not read as a malformed request.
#[tokio::test]
async fn no_route_blames_the_client_for_a_dead_database() {
    sweep(Fault::Database).await;
}

/// A dead database must not read as a malformed request *to a handler* either —
/// the pass that gets past the access gate.
#[tokio::test]
async fn no_route_blames_the_client_for_a_dead_database_behind_the_gate() {
    sweep(Fault::DatabaseBehindTheGate).await;
}

/// A dead blob store must not read as a malformed request.
#[tokio::test]
async fn no_route_blames_the_client_for_a_dead_blob_store() {
    sweep(Fault::BlobStore).await;
}

/// A missing git tree must not read as a malformed request.
#[tokio::test]
async fn no_route_blames_the_client_for_a_missing_git_tree() {
    sweep(Fault::GitTree).await;
}
