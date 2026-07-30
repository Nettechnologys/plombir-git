//! One pass over every `Access::Foreign` route that addresses a row by an
//! instance-wide id.
//!
//! `Foreign` is the level the persona sweep does not drive. The reason is
//! honest — `route_access_sweep_tests` holds no runner token, no OCI bearer
//! token, no git credentials, so it cannot speak those transports at all — and
//! the exemption it buys is written as `Access::Foreign(_) => Expect::Unchecked`,
//! which is one line and covers *everything*.
//!
//! That is the gap this file closes, and it is a gap about the *question*, not
//! about the credential. "May this caller in?" needs a foreign credential to
//! ask. "Does this route tell a real id apart from one nothing ever carried?"
//! does not: both probes are driven by the same person, holding the same
//! credential, and the answer is a comparison between two of that person's own
//! replies. `Expect::Unchecked` was never a licence to skip it, and skipping it
//! is how `/api/v1/ws/job/{job_id}` answered `403 access denied` for a job in a
//! stranger's private repository and `404 job not found` for an id that never
//! existed — the fourth axis of one defect (`card_6b2cadf41876`), and the first
//! reached over a transport rather than over REST.
//!
//! # The population
//!
//! Read off the route table, not listed here: every `Foreign` row, and within it
//! every path placeholder named `id` or `*_id`. That is what an instance-wide
//! primary key looks like in this tree — an opaque integer the caller supplies,
//! naming a row whose repository the path does not. A `{owner}`/`{repo}` pair is
//! not one of those and is not swept here: the caller named the repository, so a
//! refusal by name teaches them nothing they did not already type.
//!
//! Each (route, placeholder) pair must be *driven* or *signed off* in
//! [`NOT_PROBED`] with a reason, and the sign-off list is checked in reverse the
//! way every quarantine list in this directory is. So the next transport with an
//! opaque id lands here rather than in the silence this one did.
//!
//! # The shape of one probe
//!
//! Four requests, and any two of them differ in exactly one place:
//!
//! - **the probe** — the transport's own credential, legitimately held by
//!   somebody else, against a real row of that kind;
//! - **the reference** — the same caller, the same route, [`ABSENT_ID`]. Owed
//!   the same reply as the probe, body and all;
//! - **the anonymous pair** — both again with no credential. Which status the
//!   two agree on is the transport's business; that they agree is not.
//! - **the baseline** — one per credential family, at the end: the runner that
//!   *does* own the job starts it, and the user who *does* own the repository
//!   opens the log socket. Without them a wall of `404`s attests to a correct
//!   scope and to a dead fixture equally well.
//!
//! The verdict is the pair, not a status whitelist, and that distinction is
//! load-bearing here in a way it is not in the sibling sweeps. `authenticate_runner`
//! answers `403 token does not match runner ID` to a runner that names *any*
//! other id — one that exists and one that does not alike — so its `403` is not
//! an oracle at all, while a `403` from the job-log socket was one. Only the
//! comparison can tell those two apart.

use std::collections::{BTreeMap, BTreeSet};

use reqwest::{Client, StatusCode};
use rg_http::route_table::{Access, RouteFact};

use crate::common::answer::Answer;
use crate::common::{register_full, spawn_test_app_with_routes_and_db};

const OWNER: &str = "foreignowner";
const OUTSIDER: &str = "foreignoutsider";
/// The owner's private repository — where the seeded pipeline lives, so that a
/// job id the outsider walks names a row they may not know about.
const VAULT: &str = "foreignvault";
/// The cache key the seeded job declares, so the cache routes have one to name.
const CACHE_KEY: &str = "foreign-scope-key";

/// An id no row of any kind on this instance has ever carried.
const ABSENT_ID: i64 = 999_999;

/// (route label + placeholder) pairs this sweep deliberately does not drive.
///
/// Checked both ways: an entry that has started being driven fails the run, and
/// so does one naming a route the table no longer has. Empty, and meant to stay
/// that way — an entry here is a Foreign route whose opaque id nothing compares.
const NOT_PROBED: &[(&str, &str)] = &[];

// ── The fixture ────────────────────────────────────────────────────────────

/// One registered runner: what it is called in a path, and what it authenticates
/// with.
struct Runner {
    id: i64,
    token: String,
}

struct Fixture {
    base: String,
    client: Client,
    /// The runner the seeded job is assigned to, and the owner of the baseline.
    mine: Runner,
    /// A second, perfectly legitimate runner — the persona that walks the ids.
    stranger: Runner,
    /// A job of `mine`, in a private repository of `OWNER`.
    job_id: i64,
    /// The account that owns that repository — the WebSocket baseline.
    owner_token: String,
    /// A perfectly valid session belonging to nobody in particular.
    outsider_token: String,
}

async fn seed(
    fx_base: &str,
    client: &Client,
    owner_token: &str,
    db: &rg_db::DatabaseConnection,
) -> (Runner, Runner, i64) {
    let created = client
        .post(format!("{fx_base}/api/v1/repos"))
        .bearer_auth(owner_token)
        .json(&serde_json::json!({ "name": VAULT, "is_private": true }))
        .send()
        .await
        .expect("create repository");
    assert_eq!(
        created.status(),
        201,
        "the fixture repository was not created"
    );
    let repo_id = created
        .json::<serde_json::Value>()
        .await
        .expect("repo json")["id"]
        .as_i64()
        .expect("repo id");

    let mine = rg_db::ops::runner_ops::register_runner(db, "foreign-mine", "[]", None, None, None)
        .await
        .expect("register the owning runner");
    let stranger =
        rg_db::ops::runner_ops::register_runner(db, "foreign-stranger", "[]", None, None, None)
            .await
            .expect("register the walking runner");
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
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
        db,
        stage.id,
        "scoped",
        "true",
        None,
        None,
        None,
        Some(CACHE_KEY),
        Some(r#"["target"]"#),
        false,
        None,
        None,
        None,
    )
    .await
    .expect("create job");
    rg_db::ops::pipeline_ops::assign_job(db, job.id, mine.id)
        .await
        .expect("assign the job to its runner");

    (
        Runner {
            id: mine.id,
            token: mine.token,
        },
        Runner {
            id: stranger.id,
            token: stranger.token,
        },
        job.id,
    )
}

// ── Reading the population ─────────────────────────────────────────────────

/// The placeholders of `path` that carry an instance-wide id.
///
/// `id` and `*_id` — the spelling this tree uses for a primary key handed to a
/// caller. `{owner}`, `{repo}`, `{digest}`, `{uuid}` and the rest are locators
/// the caller already knows or already supplied, and are not this sweep's
/// subject.
fn id_params(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|at| at + open) else {
            break;
        };
        let raw = &rest[open + 1..close];
        let name = raw.strip_prefix('*').unwrap_or(raw);
        if name == "id" || name.ends_with("_id") {
            out.push(name.to_string());
        }
        rest = &rest[close + 1..];
    }
    out
}

/// Fill a path from `value`, or `None` if it names a placeholder the caller has
/// no value for — a locator filled by guesswork is a probe that proves nothing.
fn fill(path: &str, value: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..].find('}')? + open;
        let raw = &rest[open + 1..close];
        let name = raw.strip_prefix('*').unwrap_or(raw);
        out.push_str(&value(name)?);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Which credential a probe carries, and how it reaches the route.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
enum Driver {
    /// A runner token over plain HTTP.
    RunnerToken,
    /// A user session offered as the `bearer.<jwt>` subprotocol of a WebSocket
    /// handshake — the only way a browser can authenticate an upgrade.
    WsSession,
}

impl Driver {
    fn label(self) -> &'static str {
        match self {
            Self::RunnerToken => "runner token",
            Self::WsSession => "ws session",
        }
    }
}

/// One (route, placeholder) pair, and the two URLs that differ only in it.
struct Probe<'a> {
    fact: &'a RouteFact,
    subject: String,
    driver: Driver,
    real: String,
    absent: String,
}

impl Probe<'_> {
    fn label(&self) -> String {
        format!("{} [{{{}}}]", self.fact.label(), self.subject)
    }
}

/// How this sweep drives one (route, placeholder) pair, or `None` when nothing
/// here knows how — which is a failure, not a skip.
fn plan<'a>(fact: &'a RouteFact, subject: &str, fx: &Fixture) -> Option<Probe<'a>> {
    let (driver, real_id) = if fact.path.starts_with("/api/v1/runners/{id}/") {
        match subject {
            // The persona is another registered runner, so the row it must not
            // be able to confirm is the *other* runner's own id.
            "id" => (Driver::RunnerToken, fx.mine.id),
            "job_id" => (Driver::RunnerToken, fx.job_id),
            _ => return None,
        }
    } else if fact.path == "/api/v1/ws/job/{job_id}" && subject == "job_id" {
        (Driver::WsSession, fx.job_id)
    } else {
        return None;
    };

    // Everything that is *not* the subject is pinned to a value that lets the
    // request reach the decision: the stranger's own runner id, so
    // `authenticate_runner` admits it and the question about `{job_id}` is
    // actually asked.
    let others = |name: &str| -> Option<String> {
        match name {
            "id" => Some(fx.stranger.id.to_string()),
            "job_id" => Some(fx.job_id.to_string()),
            _ => None,
        }
    };
    let with = |id: i64| {
        fill(&fact.path, &|name: &str| {
            if name == subject {
                Some(id.to_string())
            } else {
                others(name)
            }
        })
    };

    Some(Probe {
        fact,
        subject: subject.to_string(),
        driver,
        real: with(real_id)?,
        absent: with(ABSENT_ID)?,
    })
}

// ── Driving one probe ──────────────────────────────────────────────────────

/// A body complaint is not a verdict on the id: the request never reached the
/// decision, so a matching pair of them would prove nothing.
fn inconclusive(status: StatusCode) -> bool {
    matches!(status.as_u16(), 400 | 409 | 415 | 422)
}

async fn drive_http(fx: &Fixture, fact: &RouteFact, url: &str, token: Option<&str>) -> Answer {
    let full = format!("{}{url}", fx.base);
    let mut request = match fact.method {
        "GET" => fx.client.get(full),
        "HEAD" => fx.client.head(full),
        "POST" => fx.client.post(full),
        "PUT" => fx.client.put(full),
        "PATCH" => fx.client.patch(full),
        "DELETE" => fx.client.delete(full),
        other => panic!("route table produced an unroutable method {other}"),
    };
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    // What each route wants before it will look at anything. Without these the
    // reply is a `400` about the request rather than an answer about the id —
    // which this sweep reports rather than passes over, so a route that grows a
    // new requirement is a failure here and not a silent pair of 400s.
    if url.ends_with("/cache") {
        request = request.header("x-cache-key", CACHE_KEY);
    }
    if url.ends_with("/finish") {
        request = request.json(&serde_json::json!({"status": "success", "exit_code": 0}));
    }
    if url.ends_with("/artifacts") {
        request = request
            .header("x-artifact-name", "a.txt")
            .header("x-artifact-path", "a.txt")
            .body("artifact-bytes");
    }
    Answer::of(request.send().await.expect("foreign-scope probe")).await
}

async fn drive(fx: &Fixture, probe: &Probe<'_>, url: &str, credentialed: bool) -> Answer {
    match probe.driver {
        Driver::RunnerToken => {
            let token = credentialed.then_some(fx.stranger.token.as_str());
            drive_http(fx, probe.fact, url, token).await
        }
        Driver::WsSession => {
            let token = credentialed.then_some(fx.outsider_token.as_str());
            crate::common::ws::refusal(&fx.base, url, token).await
        }
    }
}

/// One section of a failure report, newline-terminated, or nothing at all.
fn block(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

// ── The sweep ──────────────────────────────────────────────────────────────

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn no_foreign_route_tells_a_real_id_from_an_absent_one() {
    let (base, facts, db) = spawn_test_app_with_routes_and_db().await;
    let (owner_token, _owner_id) =
        register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    let (outsider_token, _outsider_id) =
        register_full(&base, OUTSIDER, &format!("{OUTSIDER}@example.com")).await;
    let client = Client::builder().build().expect("http client");
    let (mine, stranger, job_id) = seed(&base, &client, &owner_token, &db).await;
    let fx = Fixture {
        base,
        client,
        mine,
        stranger,
        job_id,
        owner_token,
        outsider_token,
    };

    assert!(
        !facts.is_empty(),
        "the route table came back empty — the sweep is not testing anything"
    );

    // ── The population ─────────────────────────────────────────────────────
    let mut probes: Vec<Probe> = Vec::new();
    let mut unclassified: Vec<String> = Vec::new();
    let mut signed_off: BTreeSet<String> = BTreeSet::new();
    for fact in &facts {
        if !matches!(fact.access, Access::Foreign(_)) {
            continue;
        }
        for subject in id_params(&fact.path) {
            let key = format!("{} [{{{subject}}}]", fact.label());
            if let Some((entry, _)) = NOT_PROBED.iter().find(|(entry, _)| *entry == key) {
                signed_off.insert((*entry).to_string());
                continue;
            }
            match plan(fact, &subject, &fx) {
                Some(probe) => probes.push(probe),
                None => unclassified.push(format!(
                    "  {key}\n      declared {:?} and carries an instance-wide id this sweep \
                     cannot address. Teach `plan` how to hold that transport's credential and \
                     what a real row of this kind is, or sign the pair off in NOT_PROBED with \
                     the reason — an opaque id nothing compares is the oracle this file exists \
                     for.",
                    fact.access,
                )),
            }
        }
    }

    // A sign-off that has stopped being one has to go, the way every other
    // quarantine list in this directory is checked in reverse.
    let mut healed: Vec<String> = Vec::new();
    for (entry, reason) in NOT_PROBED {
        if !signed_off.contains(*entry) {
            healed.push(format!(
                "  NOT_PROBED names '{entry}', which is not a Foreign route with that \
                 placeholder any more — drop it (was: {reason})"
            ));
        }
    }

    assert!(
        unclassified.is_empty() && healed.is_empty(),
        "{} Foreign route/id pair(s) nothing drives, {} stale sign-off(s).\n{}{}",
        unclassified.len(),
        healed.len(),
        block(&unclassified),
        block(&healed),
    );

    // Not a coverage number that rots into a floor nobody revisits: it is the
    // statement that the *selector* still selects. Relabel the runner routes or
    // rename `{job_id}` and every assertion below would go quiet with nothing
    // red — which is exactly how `Expect::Unchecked` came to cover this.
    assert!(
        probes.len() >= 10,
        "only {} Foreign route/id pair(s) were found — the selector, not the server, is what \
         changed, and this sweep is now proving nothing",
        probes.len()
    );
    let drivers: BTreeSet<Driver> = probes.iter().map(|probe| probe.driver).collect();
    assert!(
        drivers.len() >= 2,
        "every pair found is driven by {:?} alone — a sweep over one transport is the \
         instance-level test this file replaced",
        drivers
    );

    // ── Pass one: the transport's own credential, held by somebody else ─────
    let mut oracles: Vec<String> = Vec::new();
    let mut unreached: Vec<String> = Vec::new();
    // How many pairs of each driver were compared with a body on both sides.
    let mut spoke: BTreeMap<Driver, usize> = BTreeMap::new();

    for probe in &probes {
        let label = probe.label();
        let real = drive(&fx, probe, &probe.real, true).await;
        let absent = drive(&fx, probe, &probe.absent, true).await;

        if inconclusive(real.status) || real.status.is_server_error() {
            unreached.push(format!(
                "  {label}\n      answered {} for a real row, so the request never reached a \
                 decision about the id and the comparison below proves nothing. Give `drive_http` \
                 whatever this route wants.\n      body: {}",
                real.status,
                real.excerpt(),
            ));
        } else if real.shape() != absent.shape() {
            oracles.push(format!(
                "  {label}\n      a caller holding a {} is answered differently for a real row \
                 and for an id nothing ever carried, so walking the id space enumerates every row \
                 of this kind on the instance — private repositories included.\n      real   \
                 ({}): {}\n      absent ({}): {}",
                probe.driver.label(),
                real.status,
                real.excerpt(),
                absent.status,
                absent.excerpt(),
            ));
        } else if real.speaks() && absent.speaks() {
            *spoke.entry(probe.driver).or_default() += 1;
        }

        // The same pair with no credential at all. Which status the two agree on
        // is the transport's own business — a gate that authenticates before it
        // resolves anything answers the same way either way, and that is the
        // order to keep — but they have to agree, or the route enumerates rows
        // to callers with no account at all.
        let anon_real = drive(&fx, probe, &probe.real, false).await;
        let anon_absent = drive(&fx, probe, &probe.absent, false).await;
        if anon_real.shape() != anon_absent.shape() {
            oracles.push(format!(
                "  {label}\n      an anonymous caller is answered differently for a real row and \
                 for an id nothing ever carried.\n      real   ({}): {}\n      absent ({}): {}",
                anon_real.status,
                anon_real.excerpt(),
                anon_absent.status,
                anon_absent.excerpt(),
            ));
        }
    }

    // ── Pass two: the baselines, one per credential family ─────────────────
    //
    // Last, because the runner one starts the job it drives.
    let mut dead: Vec<String> = Vec::new();

    let started = drive_http(
        &fx,
        facts
            .iter()
            .find(|fact| fact.label() == "POST /api/v1/runners/{id}/jobs/{job_id}/start")
            .expect("the runner start route is in the table"),
        &format!("/api/v1/runners/{}/jobs/{}/start", fx.mine.id, fx.job_id),
        Some(&fx.mine.token),
    )
    .await;
    if started.status != StatusCode::OK {
        dead.push(format!(
            "  the runner the job is assigned to cannot start it ({}), so every refusal above is \
             equally good evidence of a dead fixture.\n      body: {}",
            started.status,
            started.excerpt(),
        ));
    }

    match tokio_tungstenite::connect_async(crate::common::ws::handshake_request(
        &fx.base,
        &format!("/api/v1/ws/job/{}", fx.job_id),
        Some(&fx.owner_token),
    ))
    .await
    {
        Ok((mut socket, response)) => {
            if response.status() != 101 {
                dead.push(format!(
                    "  the owner of the repository was answered {} on their own job's log \
                     socket",
                    response.status()
                ));
            }
            socket
                .close(None)
                .await
                .expect("the baseline socket closes cleanly");
        }
        Err(error) => dead.push(format!(
            "  the owner of the repository cannot open their own job's log socket ({error:?}), so \
             the refusals above describe a broken fixture rather than a gate"
        )),
    }

    assert!(
        oracles.is_empty() && unreached.is_empty() && dead.is_empty(),
        "foreign id scope: {} route/id pair(s) tell a real id from an absent one, {} that never \
         reached a decision, {} dead baseline(s), out of {} driven.\nA `Foreign` route is exempt \
         from the persona sweep because nothing there holds its credential — not because the id \
         it takes is nobody's business.\n{}{}{}",
        oracles.len(),
        unreached.len(),
        dead.len(),
        probes.len(),
        block(&oracles),
        block(&unreached),
        block(&dead),
    );

    // Every assertion above is satisfied by two empty bodies, so a family whose
    // routes stopped answering with one would keep this file green while
    // asserting only the status — the half the oracle moves out of.
    let silent: Vec<String> = drivers
        .iter()
        .filter(|driver| spoke.get(driver).copied().unwrap_or_default() == 0)
        .map(|driver| {
            format!(
                "  {} — every pair compared was empty on one side, so only the status was \
                 asserted",
                driver.label()
            )
        })
        .collect();
    assert!(
        silent.is_empty(),
        "{} of {} credential famil(ies) had no refusal compared against an absent id's body and \
         all.\n{}",
        silent.len(),
        drivers.len(),
        block(&silent),
    );
}
