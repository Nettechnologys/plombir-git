//! One pass over every route whose gate settles an *organization* and whose
//! path then carries an instance-wide id.
//!
//! This is the fourth axis of one doctrine, and the last one to get a sweep.
//! `cross_repo_id_scope_sweep_tests` walks the routes that name a repository and
//! then act on a global id; `anchored_scope_sweep_tests` walks the ones that
//! resolve the repository *out of* the row; `user_scoped_id_scope_sweep_tests`
//! walks the ones where being signed in is the whole of the proof. All three ask
//! the same question — the gate proved a *container*, and the id in the path
//! belongs to no container until somebody compares them — and none of them can
//! see this one:
//!
//!     GET /api/v1/orgs/<my-own-org>/teams/<a-team-of-somebody-else's>
//!
//! `{team_id}` is the primary key of the `teams` table, instance-wide.
//! [`Access::OrgRead`] / [`Access::OrgAdmin`] prove something about `{name}` and
//! nothing whatever about that integer, and an attacker who owns *any*
//! organization passes every check on their own path. Only
//! `orgs::resolve_team_in_org`'s `team.org_id == org_id` stands between them and
//! another organization's team — which is a line of code, and the whole of this
//! family's history is lines of code like that one going missing.
//!
//! # Why none of the existing sweeps reaches it
//!
//! Each for its own reason, which is what made the gap invisible:
//!
//! - `cross_repo_id_scope_sweep_tests` selects on a path naming `{owner}` and
//!   on `Access::is_repo_scoped`. An org path carries neither, so every one of
//!   these routes is dropped by a silent `continue`.
//! - `anchored_scope_sweep_tests` keys on the same predicate, one layer down.
//! - `user_scoped_id_scope_sweep_tests` selects `Access::User`, and explicitly
//!   steps over any path that also names a container — this one included, with a
//!   comment saying the other sweeps have it.
//! - `route_access_sweep_tests` does drive these routes, and substitutes the
//!   literal `1` for `{team_id}`. Its question is about the *organization*: is a
//!   private one hidden from an outsider. `Expect::Hidden` there is measured
//!   against `ABSENT_ORG`, so what it pins is the org gate, and a team belonging
//!   to somebody else is not a case it has a name for.
//!
//! So the axis was held by one hand-written test —
//! `org_team_authz_tests::a_team_of_another_org_is_not_reachable_through_my_own_path`
//! — which is three `assert_eq!(status, 404)` over three of the five routes that
//! carry a `{team_id}`. `GET .../teams/{team_id}/members` and
//! `DELETE .../teams/{team_id}/members/{user_id}` were checked by nobody, and
//! nothing compared the refusal against the one an absent id gets, which is
//! where the oracle went twice already in this directory (`card_e672d29ad2fe`
//! on the repository axis, `card_1419723e0606` on the anchored one).
//!
//! # The shape
//!
//! Two accounts, each owning one organization, and only one of them owning a
//! team worth reaching. Every route is driven four times, and any two of the
//! requests differ in exactly one place:
//!
//! - **the probe** — the attacker's own session, the attacker's own
//!   organization in `{name}`, the victim's real `team_id`. Owed a masked
//!   refusal: `401` or `404`, and specifically not `403`.
//! - **the reference** — the same caller, the same path, [`ABSENT_ID`]. Owed the
//!   same answer as the probe, *body included*.
//! - **the reach baseline** — the attacker, his own organization, his *own*
//!   team. Owed a success, and it is the assertion that makes the probe mean
//!   anything: if the attacker were refused here too, every `404` above would be
//!   the org gate talking and the team comparison would be untested.
//! - **the owner baseline** — the victim, his own organization and team. Owed a
//!   success, so a fixture that seeded nothing cannot read as a wall of clean
//!   `404`s.
//!
//! # Two selectors, checked against each other
//!
//! The population is read twice, by readers that fail for different reasons.
//! [`Access::is_org_scoped`] plus a target id in the path is the primary one: it
//! trusts the *declaration* in the route table. The second reads the *code* — a
//! handler whose signature carries `OrgRead` or `OrgAdmin` — and holds every
//! route it finds to being driven here too.
//!
//! One reader is not enough, which the anchored sweep learned by mutation
//! (`card_f87450bec7f6`): coverage dies not from a wrong assertion but from a
//! selector that goes quiet. A route mis-declared `User` over an `OrgAdmin`
//! handler is invisible to the first and caught by the second.
//!
//! # `{user_id}` is an argument, and that is asserted rather than assumed
//!
//! Two of these paths end in `{user_id}`, and it is not a locator: the handlers
//! pass it to `remove_org_member(org.id, user_id)` and
//! `remove_team_member(team.id, user_id)`, which scope the delete by the
//! container the gate already settled. There is no id space to walk.
//!
//! What that leaves is a different question — whether the reply distinguishes a
//! real membership change from a no-op without turning the no-op into an account
//! existence oracle. [`removing_a_member_reports_only_real_membership_changes`]
//! below pins both halves: an existing membership is removed successfully, while
//! a repeat, an existing non-member, and an id that names no account all receive
//! the same `404`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use reqwest::{Client, StatusCode};
use rg_http::route_table::RouteFact;

use crate::common::answer::Answer;
use crate::common::route_path::placeholders;
use crate::common::source_scan::{
    functions, handler_type_name, param_base_types, relative, rust_files, signature_params,
    src_root,
};
use crate::common::{
    register_full, spawn_test_app_with_db, spawn_test_app_with_routes,
};

const VICTIM: &str = "orgscopevictim";
const ATTACKER: &str = "orgscopeattacker";
const VICTIM_ORG: &str = "orgscope-victim-org";
const ATTACKER_ORG: &str = "orgscope-attacker-org";

/// An id no row on the instance has ever carried — the reference every probe is
/// measured against.
///
/// The same value the three sibling sweeps hold, and for the same reason: every
/// primary key here comes out of a sequence starting at 1, so one value this far
/// out stands in for all of them.
const ABSENT_ID: i64 = 999_999;

/// The placeholder that names the organization — the container the gate settles.
const ORG_PLACEHOLDER: &str = "name";

/// What a global-id placeholder on an org-scoped route is.
enum Role {
    /// It addresses a row that belongs to the organization. The gate proved
    /// nothing about it, so this sweep drives it.
    Target,
    /// It is an argument the handler passes along, not a locator — with the
    /// reason, because "not a target" is a claim and a wrong one is a hole.
    Argument(&'static str),
}

/// Every `{*_id}` an org-scoped route may carry, keyed on the segment it hangs
/// off and its name.
///
/// The classification is mandatory, not a filter: a global id that is neither
/// listed here nor a target fails the run. Without that, the next
/// `/orgs/{name}/…/{something_id}` would be dropped by a silent `continue` and
/// this sweep would report full coverage of a population it had quietly shrunk —
/// which is precisely how the hand-written test it replaces came to cover three
/// routes out of five.
///
/// Keyed on the *segment*, not on the whole path, so a second route over the
/// same row is covered the day it is written: `GET`, `DELETE` and both
/// `/members` routes under `teams/{team_id}` are one entry, and a
/// `POST /orgs/{name}/teams/{team_id}/rename` would need none at all. A
/// path-string table is what rotted in the id-scope sweep's first selector.
const ID_ROLE: &[(&str, &str, Role)] = &[
    ("teams", "team_id", Role::Target),
    (
        "members",
        "user_id",
        Role::Argument(
            "names an account, not a row of the organization: `remove_org_member(org.id, \
             user_id)` and `remove_team_member(team.id, user_id)` are both scoped by the \
             container the gate already settled, so there is no id space to walk. Whether the \
             reply confirms only a real membership change while keeping every no-op \
             indistinguishable is asserted in \
             `removing_a_member_reports_only_real_membership_changes` rather than assumed here",
        ),
    ),
];

/// Routes this sweep deliberately does not drive, each with the reason.
///
/// Checked both ways, like every other quarantine list in this directory: an
/// entry whose route this sweep has started driving fails the run, and so does
/// one naming a route that no longer exists. Empty on purpose — the emptiness is
/// the statement.
///
/// An entry excuses a route from being *reported*, never from being *driven*: a
/// signed-off route still reaches `probes` if it can be filled, so a sign-off
/// that has stopped being one is visible. The first version of the user-scoped
/// sweep skipped a signed-off route before it reached that point, which left a
/// standing exemption nothing could ever notice.
const NOT_PROBED: &[(&str, &str)] = &[];

fn signed_off(label: &str) -> bool {
    NOT_PROBED.iter().any(|(entry, _)| *entry == label)
}

/// Whether a placeholder names an instance-wide primary key.
///
/// The same predicate `global_id_anchor_guard` applies to a handler's path
/// parameters, which is what keeps the two files counting one population: the
/// guard demands the id be *anchored*, this sweep demands the anchor's two
/// refusals be indistinguishable.
fn is_global_id(name: &str) -> bool {
    name == "id" || name.ends_with("_id")
}

/// Every global-id placeholder of a path, paired with the segment it hangs off:
/// `/orgs/{name}/teams/{team_id}/members/{user_id}` ⇒
/// `[("teams", "team_id"), ("members", "user_id")]`.
fn global_ids(path: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut previous = "";
    for segment in path.split('/') {
        if let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            if is_global_id(name) {
                out.push((previous, name));
            }
        }
        previous = segment;
    }
    out
}

impl Role {
    /// How a stale entry is quoted back when the route it classified is gone —
    /// so the reason a placeholder was waved past this sweep is deleted with a
    /// reader's eye on it rather than as a line that no longer compiles.
    fn describe(&self) -> &'static str {
        match self {
            Self::Target => "a row this sweep drives",
            Self::Argument(reason) => reason,
        }
    }
}

fn role(resource: &str, name: &str) -> Option<&'static Role> {
    ID_ROLE
        .iter()
        .find(|(segment, id, _)| *segment == resource && *id == name)
        .map(|(_, _, role)| role)
}

/// The row this route addresses: the segment it hangs off and the placeholder
/// that carries it, or `None` when the path names no target at all.
///
/// `/orgs/{name}/members/{user_id}` is that `None` — its only global id is an
/// argument — and so is `/orgs/{name}` itself. Both are the org gate's own
/// subject, which `route_access_sweep_tests` holds to `Expect::Hidden`.
fn target(path: &str) -> Option<(&str, &str)> {
    global_ids(path)
        .into_iter()
        .find(|(resource, name)| matches!(role(resource, name), Some(Role::Target)))
}

/// Fill a route's path: the organization, the row the probe addresses, and the
/// account any trailing argument names.
///
/// `None` when the path carries a placeholder this sweep cannot fill, which the
/// caller reports rather than guesses at — a placeholder filled by guesswork is
/// a probe that proves nothing.
fn fill(path: &str, org: &str, target_id: i64, argument_id: i64) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut previous = "";
    for segment in path.split('/') {
        let filled = match segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            None => segment.to_string(),
            Some(name) if name == ORG_PLACEHOLDER => org.to_string(),
            Some(name) if is_global_id(name) => match role(previous, name)? {
                Role::Target => target_id.to_string(),
                Role::Argument(_) => argument_id.to_string(),
            },
            Some(_) => return None,
        };
        parts.push(filled);
        previous = segment;
    }
    Some(parts.join("/"))
}

/// The request body a route wants before it will look at the id.
///
/// Mandatory for every method that carries one: a selected `POST`/`PUT`/`PATCH`
/// with no entry here fails the run. Without a body, `add_team_member`'s
/// `Json<_>` rejects the request *after* the gate and *before* the handler body,
/// so the reply is a `422` about the request rather than an answer about the
/// team — and a matching pair of those proves nothing at all.
fn body(fact: &RouteFact, account: i64) -> Option<serde_json::Value> {
    match (fact.method, fact.path.as_str()) {
        ("POST", "/api/v1/orgs/{name}/teams/{team_id}/members") => {
            Some(serde_json::json!({ "user_id": account, "role": "member" }))
        }
        _ => None,
    }
}

fn wants_body(fact: &RouteFact) -> bool {
    matches!(fact.method, "POST" | "PUT" | "PATCH")
}

/// Whether driving this route disposes of the row it addresses.
///
/// A `DELETE` whose path *ends* at the target id is the route that consumes it —
/// `DELETE /orgs/{name}/teams/{team_id}` — while
/// `DELETE …/teams/{team_id}/members/{user_id}` acts on something under it and
/// leaves the team standing. The baselines are ordered on this, so a baseline
/// never runs against a row an earlier one has already deleted.
fn consumes_row(fact: &RouteFact, placeholder: &str) -> bool {
    fact.method == "DELETE" && fact.path.ends_with(&format!("{{{placeholder}}}"))
}

/// The probe's predicate: refused *without* being told the row exists — `401` or
/// `404`, and specifically **not** `403`.
///
/// `403` is the one answer this sweep's subject matter forbids, and
/// `resolve_team_in_org` says so itself ("a team belonging elsewhere is a `404`
/// rather than a `403` — a `403` would confirm that the id exists"). `401` is
/// admissible and is not a leak: the org extractors run before any team lookup,
/// so they answer the same way whether the team exists or not.
fn masked(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 404)
}

/// A body complaint is not a verdict on the id: the handler rejected the request
/// before it looked anything up.
fn inconclusive(status: StatusCode) -> bool {
    matches!(status.as_u16(), 400 | 409 | 415 | 422)
}

/// One section of a failure report: the lines, newline-terminated, or nothing at
/// all when there are none.
fn block(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

// ── Reading the population out of the source ───────────────────────────────

/// Every handler in the tree whose signature carries `OrgRead` or `OrgAdmin`, by
/// the `type_name` its route records.
///
/// This is the second selector. Either extractor in a signature means the gate
/// settled an *organization* and nothing else — no repository, no instance role
/// — so a path parameter naming an instance-wide row on such a handler is this
/// sweep's subject whatever level the route happens to declare.
fn org_gated_handlers() -> BTreeSet<String> {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    files.sort();
    let mut out = BTreeSet::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("read source file");
        for function in functions(&text) {
            let Some(params) = signature_params(&text, &function.name) else {
                continue;
            };
            let types = param_base_types(&params);
            if types.contains(&"OrgRead") || types.contains(&"OrgAdmin") {
                out.insert(handler_type_name(&relative(file), &function.name));
            }
        }
    }
    out
}

// ── The fixture ────────────────────────────────────────────────────────────

struct Fixture {
    base: String,
    client: Client,
    victim_token: String,
    victim_id: i64,
    attacker_token: String,
    attacker_id: i64,
}

impl Fixture {
    async fn create_org(&self, token: &str, name: &str) {
        let response = self
            .client
            .post(format!("{}/api/v1/orgs", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "name": name, "visibility": "public" }))
            .send()
            .await
            .expect("create org");
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert_eq!(
            status, 201,
            "the fixture organization {name} was not created: {body}"
        );
    }

    async fn create_team(&self, token: &str, org: &str, team: &str) -> i64 {
        let response = self
            .client
            .post(format!("{}/api/v1/orgs/{org}/teams", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "name": team, "permission": "admin" }))
            .send()
            .await
            .expect("create team");
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert_eq!(
            status, 201,
            "the fixture team {team} was not created: {body}"
        );
        serde_json::from_str::<serde_json::Value>(&body).expect("team response is json")["id"]
            .as_i64()
            .unwrap_or_else(|| panic!("team response carried no id: {body}"))
    }

    async fn drive(
        &self,
        fact: &RouteFact,
        url: &str,
        token: Option<&str>,
        account: i64,
    ) -> Answer {
        let full = format!("{}{url}", self.base);
        let mut request = match fact.method {
            "GET" => self.client.get(full),
            "HEAD" => self.client.head(full),
            "POST" => self.client.post(full),
            "PUT" => self.client.put(full),
            "PATCH" => self.client.patch(full),
            "DELETE" => self.client.delete(full),
            other => panic!("route table produced an unroutable method {other}"),
        };
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body(fact, account) {
            request = request.json(&body);
        }
        Answer::of(request.send().await.expect("org-scope sweep request")).await
    }
}

// ── The sweep ──────────────────────────────────────────────────────────────

/// One route to drive: where it is, which resource it addresses, and the four
/// URLs whose answers are compared.
struct Probe<'a> {
    fact: &'a RouteFact,
    resource: String,
    placeholder: String,
    /// The attacker's org, the victim's row.
    foreign: String,
    /// The attacker's org, an id that never existed.
    absent: String,
    /// The attacker's org, the attacker's own row.
    reach: String,
    /// The victim's org, the victim's own row.
    owned: String,
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn no_org_scoped_route_reaches_another_organizations_row() {
    let (base, facts) = spawn_test_app_with_routes().await;
    let (victim_token, victim_id) =
        register_full(&base, VICTIM, &format!("{VICTIM}@example.com")).await;
    let (attacker_token, attacker_id) =
        register_full(&base, ATTACKER, &format!("{ATTACKER}@example.com")).await;
    let fx = Fixture {
        base,
        client: Client::builder().build().expect("http client"),
        victim_token,
        victim_id,
        attacker_token,
        attacker_id,
    };
    fx.create_org(&fx.victim_token, VICTIM_ORG).await;
    fx.create_org(&fx.attacker_token, ATTACKER_ORG).await;
    let victim_team = fx
        .create_team(&fx.victim_token, VICTIM_ORG, "victim-devs")
        .await;
    let attacker_team = fx
        .create_team(&fx.attacker_token, ATTACKER_ORG, "attacker-devs")
        .await;

    assert!(
        !facts.is_empty(),
        "the route table came back empty — the sweep is not testing anything"
    );

    // ── Every placeholder of an org-scoped route is classified ──────────────
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    for fact in &facts {
        if !fact.access.is_org_scoped() {
            continue;
        }
        for name in placeholders(&fact.path) {
            if name == ORG_PLACEHOLDER {
                continue;
            }
            let classified = is_global_id(name)
                && global_ids(&fact.path)
                    .into_iter()
                    .any(|(resource, found)| found == name && role(resource, found).is_some());
            if !classified {
                unknown.insert(format!("  {{{name}}} in {}", fact.label()));
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "a route gated on an organization names {} placeholder(s) this sweep has never heard of. \
         Each one is either an instance-wide primary key the gate proved nothing about — in which \
         case give ID_ROLE a `Role::Target` line and teach the fixture to seed such a row — or it \
         is an argument the handler passes to something already scoped by the container, in which \
         case add it as a `Role::Argument` with the reason. Left unclassified it would be dropped \
         by a silent `continue`, and this sweep would report full coverage of a population it had \
         shrunk.\n{}",
        unknown.len(),
        unknown.into_iter().collect::<Vec<_>>().join("\n"),
    );

    // ── Selector one: the level the route declares ──────────────────────────
    let mut probes: Vec<Probe> = Vec::new();
    let mut unclassified: Vec<String> = Vec::new();
    for fact in &facts {
        if !fact.access.is_org_scoped() {
            continue;
        }
        let Some((resource, placeholder)) = target(&fact.path) else {
            continue;
        };
        if resource != "teams" {
            if !signed_off(&fact.label()) {
                unclassified.push(format!(
                    "  {} — addresses a `{resource}` row by its instance-wide id and this sweep \
                     owns no such row. Seed one in the fixture and give the probe builder a line, \
                     or sign the route off in NOT_PROBED with the reason it cannot be probed",
                    fact.label(),
                ));
            }
            continue;
        }
        if wants_body(fact) && body(fact, fx.attacker_id).is_none() && !signed_off(&fact.label()) {
            unclassified.push(format!(
                "  {} — carries a request body and `body()` has no entry for it, so the reply \
                 would be a deserializer's complaint rather than an answer about the id. Give it \
                 the body this route wants",
                fact.label(),
            ));
            continue;
        }
        let urls = (
            fill(&fact.path, ATTACKER_ORG, victim_team, fx.attacker_id),
            fill(&fact.path, ATTACKER_ORG, ABSENT_ID, fx.attacker_id),
            fill(&fact.path, ATTACKER_ORG, attacker_team, fx.attacker_id),
            fill(&fact.path, VICTIM_ORG, victim_team, fx.victim_id),
        );
        let (Some(foreign), Some(absent), Some(reach), Some(owned)) = urls else {
            if !signed_off(&fact.label()) {
                unclassified.push(format!(
                    "  {} — carries a placeholder `fill` cannot substitute. Teach it to seed that \
                     locator; a placeholder filled by guesswork is a probe that proves nothing",
                    fact.label(),
                ));
            }
            continue;
        };
        probes.push(Probe {
            fact,
            resource: resource.to_string(),
            placeholder: placeholder.to_string(),
            foreign,
            absent,
            reach,
            owned,
        });
    }

    // ── Selector two: the gate the handler actually holds ───────────────────
    //
    // Read out of the source, and blind to what the route table declares. A
    // route mis-declared `User` — or `Public` — over an `OrgAdmin` handler is
    // invisible to the selector above, whose whole reading is `fact.access`.
    let org_gated = org_gated_handlers();
    assert!(
        org_gated.len() >= 8,
        "only {} handler(s) in the tree read as taking `OrgRead`/`OrgAdmin` — the signature reader \
         is broken, and a broken second selector cannot contradict the first one about anything",
        org_gated.len()
    );
    let driven: BTreeSet<String> = probes.iter().map(|probe| probe.fact.label()).collect();
    let mut unseen: Vec<String> = Vec::new();
    for fact in &facts {
        if !org_gated.contains(fact.handler) || target(&fact.path).is_none() {
            continue;
        }
        if driven.contains(&fact.label()) || signed_off(&fact.label()) {
            continue;
        }
        unseen.push(format!(
            "  {} declares {:?} while its handler ({}) is gated by the organization in the path \
             and the path then carries a row this sweep classifies as a target. The gate therefore \
             proved nothing about that id, and nothing here drives the route — so either the \
             declared level is wrong, or the selector above cannot see the route and needs to be \
             told how.",
            fact.label(),
            fact.access,
            fact.handler,
        ));
    }

    // A sign-off, or a classification, that has stopped being one has to go.
    let mut healed: Vec<String> = Vec::new();
    for (label, reason) in NOT_PROBED {
        if driven.contains(*label) {
            healed.push(format!(
                "  {label} is being driven now — drop it from NOT_PROBED (was: {reason})"
            ));
        } else if !facts.iter().any(|fact| fact.label() == *label) {
            healed.push(format!(
                "  NOT_PROBED names {label}, which is no longer a route — drop it (was: {reason})"
            ));
        }
    }
    for (resource, name, role) in ID_ROLE {
        let still_there = facts.iter().any(|fact| {
            fact.access.is_org_scoped()
                && global_ids(&fact.path)
                    .into_iter()
                    .any(|(segment, found)| segment == *resource && found == *name)
        });
        if !still_there {
            healed.push(format!(
                "  ID_ROLE classifies `{resource}/{{{name}}}`, which no org-scoped route carries \
                 any more — drop the entry (was: {})",
                role.describe()
            ));
        }
    }

    assert!(
        unclassified.is_empty() && unseen.is_empty() && healed.is_empty(),
        "the org-scoped routes and the ones this sweep drives have come apart: {} route(s) nothing \
         can address, {} route(s) gated on an organization that nothing here drives, {} stale \
         entr(ies).\nA route of this shape is invisible to every other id-scope sweep — its path \
         names no repository, no row resolves one, and its level is not `Access::User` — and \
         `route_access_sweep_tests` measures it against `ABSENT_ORG`, which is a statement about \
         the organization and not about the id under it. This sweep is the only pass that \
         asks.\n{}{}{}",
        unclassified.len(),
        unseen.len(),
        healed.len(),
        block(&unclassified),
        block(&unseen),
        block(&healed),
    );
    // Not a coverage target — a collapse detector. The two selectors above are
    // what keeps the population honest; this only catches a filter that selected
    // nothing at all and would otherwise pass every assertion below vacuously.
    assert!(
        probes.len() >= 5,
        "only {} org-scoped route(s) carrying a global id are being driven — the filter is wrong, \
         not the server",
        probes.len()
    );

    // The route that disposes of the row it addresses goes last, so no baseline
    // runs against a team an earlier one has already deleted.
    probes.sort_by_key(|probe| {
        (
            consumes_row(probe.fact, &probe.placeholder),
            probe.fact.path.clone(),
            probe.fact.method,
        )
    });

    let mut leaks: Vec<String> = Vec::new();
    let mut oracles: Vec<String> = Vec::new();
    // How many pairs of each resource were compared with a body on both sides.
    let mut paired: BTreeMap<String, usize> = BTreeMap::new();

    // ── Pass one: the attacker, on his own path, naming somebody else's row ─
    for probe in &probes {
        let label = probe.fact.label();
        let real = fx
            .drive(
                probe.fact,
                &probe.foreign,
                Some(&fx.attacker_token),
                fx.attacker_id,
            )
            .await;
        let absent = fx
            .drive(
                probe.fact,
                &probe.absent,
                Some(&fx.attacker_token),
                fx.attacker_id,
            )
            .await;

        if real.status == StatusCode::FORBIDDEN {
            leaks.push(format!(
                "  {label}\n      answered 403 for a row belonging to {VICTIM_ORG}. The refusal \
                 confirmed the id exists, which is the one thing a masked denial may not do: the \
                 caller supplied an opaque integer under a path they legitimately own, so walking \
                 it enumerates every row of this kind on the instance.\n      body: {}",
                real.excerpt()
            ));
        } else if inconclusive(real.status) {
            leaks.push(format!(
                "  {label}\n      answered {} — the request never reached the row, so nothing was \
                 proven. Give `body()` whatever this route wants.\n      body: {}",
                real.status,
                real.excerpt()
            ));
        } else if !masked(real.status) {
            leaks.push(format!(
                "  {label}\n      answered {} for a row belonging to {VICTIM_ORG} to a caller who \
                 owns none of it. The organization in the path is the whole of what the gate \
                 proved, and the handler never compared the row's organization against it.\n      \
                 body: {}",
                real.status,
                real.excerpt()
            ));
        } else if real.shape() != absent.shape() {
            oracles.push(format!(
                "  {label}\n      refused a row of {VICTIM_ORG}'s and an id that never existed, \
                 but not with the same answer — so the pair tells the caller which of the two he \
                 hit, and walking the id space enumerates every row of this kind on the \
                 instance.\n      real   ({}): {}\n      absent ({}): {}",
                real.status,
                real.excerpt(),
                absent.status,
                absent.excerpt(),
            ));
        } else if real.speaks() && absent.speaks() {
            *paired.entry(probe.resource.clone()).or_default() += 1;
        }

        // The same pair with no credential at all. Which code the two agree on
        // is the gate's business — `OrgAdmin` answers `401` before it resolves
        // anything, and a public organization admits an anonymous `OrgRead` —
        // but they have to agree: an answer that differs after the lookup would
        // tell a caller with no account at all which ids are real.
        let anon_real = fx
            .drive(probe.fact, &probe.foreign, None, fx.attacker_id)
            .await;
        let anon_absent = fx
            .drive(probe.fact, &probe.absent, None, fx.attacker_id)
            .await;
        if anon_real.shape() != anon_absent.shape() {
            oracles.push(format!(
                "  {label}\n      an anonymous caller is answered differently for a real row and \
                 for an id that never existed, so the route enumerates another organization's rows \
                 to callers with no account at all.\n      real   ({}): {}\n      absent ({}): {}",
                anon_real.status,
                anon_real.excerpt(),
                anon_absent.status,
                anon_absent.excerpt(),
            ));
        }
    }

    // ── Pass two: the two baselines ────────────────────────────────────────
    //
    // Both held to a *success*, not merely to "not refused". `masked` accepts
    // `404`, so a fixture that quietly stopped seeding would hand out `404` to
    // everybody and read as a clean bill of health.
    let mut dead: Vec<String> = Vec::new();
    for probe in &probes {
        let reach = fx
            .drive(
                probe.fact,
                &probe.reach,
                Some(&fx.attacker_token),
                fx.attacker_id,
            )
            .await;
        if !reach.status.is_success() {
            dead.push(format!(
                "  {}\n      the attacker is answered {} on his OWN organization's team, so the \
                 org gate is what refused him — and every refusal above may be that gate rather \
                 than the comparison this sweep is about. The probes prove nothing until this one \
                 succeeds.\n      body: {}",
                probe.fact.label(),
                reach.status,
                reach.excerpt(),
            ));
        }
        let owned = fx
            .drive(
                probe.fact,
                &probe.owned,
                Some(&fx.victim_token),
                fx.victim_id,
            )
            .await;
        if !owned.status.is_success() {
            dead.push(format!(
                "  {}\n      the victim is answered {} on his own team, so the refusals above \
                 describe a broken fixture rather than a gate. Either the row was never seeded, or \
                 an earlier baseline in this run consumed it — the probes are ordered so the route \
                 that disposes of the row runs last, and a second such route needs a second \
                 row.\n      body: {}",
                probe.fact.label(),
                owned.status,
                owned.excerpt(),
            ));
        }
    }

    assert!(
        leaks.is_empty() && oracles.is_empty() && dead.is_empty(),
        "org-scoped id scope: {} route(s) let one organization reach another's row, {} told a real \
         id apart from an absent one, {} dead baseline(s), out of {} driven.\n{}{}{}",
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
    // two empty bodies, so a resource whose routes stopped answering with a body
    // — or whose pair stopped being compared at all — would keep the whole file
    // green while asserting nothing. That is exactly how the status half of the
    // id-scope sweep went vacuous in `card_c46c354ec3ae`.
    let mut silent: Vec<String> = Vec::new();
    for probe in &probes {
        if paired.get(&probe.resource).copied().unwrap_or_default() == 0 {
            silent.push(format!(
                "  `{}` — every pair compared was empty on one side, so only the status was \
                 asserted",
                probe.resource
            ));
        }
    }
    silent.sort();
    silent.dedup();
    assert!(
        silent.is_empty(),
        "{} resource(s) had no masked refusal compared against an absent id's body and all. The \
         oracle-in-the-body half of this sweep is only asserted where that pair holds.\n{}",
        silent.len(),
        block(&silent),
    );
}

/// The other half of the `{user_id}` question, asserted rather than reasoned
/// about in a comment.
///
/// [`ID_ROLE`] classifies `{user_id}` as an argument, and the classification is
/// only as good as its reason: the handlers hand it to
/// `remove_org_member(org.id, user_id)` / `remove_team_member(team.id, user_id)`,
/// which scope the delete by the container the gate already settled. That
/// disposes of the *IDOR* — there is no id space to walk — and leaves one thing
/// it says nothing about: whether the reply confirms a real membership change
/// without revealing whether a no-op id belongs to an account.
///
/// A successful removal must answer `200 {"removed": true}`. A repeated removal,
/// an existing account that never joined, and an integer no account carries must
/// all answer the same `404`: the first rule prevents a silent no-op, and the
/// second keeps the account-existence oracle closed.
#[tokio::test]
async fn removing_a_member_reports_only_real_membership_changes() {
    const ORG: &str = "orgscope-member-oracle-org";
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _owner_id) = register_full(
        &base,
        "orgscopememberowner",
        "orgscopememberowner@example.com",
    )
    .await;
    let (_member_token, member_id) = register_full(
        &base,
        "orgscopemembermember",
        "orgscopemembermember@example.com",
    )
    .await;
    let (_stranger_token, stranger_id) = register_full(
        &base,
        "orgscopememberstranger",
        "orgscopememberstranger@example.com",
    )
    .await;
    let fx = Fixture {
        base,
        client: Client::builder().build().expect("http client"),
        victim_token: owner_token.clone(),
        victim_id: member_id,
        attacker_token: owner_token,
        attacker_id: member_id,
    };
    fx.create_org(&fx.attacker_token, ORG).await;
    let team = fx.create_team(&fx.attacker_token, ORG, "oracle-devs").await;

    for (path, body) in [
        (
            format!("/api/v1/orgs/{ORG}/members"),
            serde_json::json!({ "user_id": member_id, "role": "member" }),
        ),
        (
            format!("/api/v1/orgs/{ORG}/teams/{team}/members"),
            serde_json::json!({ "user_id": member_id, "role": "member" }),
        ),
    ] {
        let response = fx
            .client
            .post(format!("{}{path}", fx.base))
            .bearer_auth(&fx.attacker_token)
            .json(&body)
            .send()
            .await
            .expect("seed membership");
        assert_eq!(
            response.status(),
            201,
            "the fixture membership on {path} was not created"
        );
    }

    for (family, base_path) in [
        ("organization", format!("/api/v1/orgs/{ORG}/members")),
        ("team", format!("/api/v1/orgs/{ORG}/teams/{team}/members")),
    ] {
        let removed = fx
            .client
            .delete(format!("{}{base_path}/{member_id}", fx.base))
            .bearer_auth(&fx.attacker_token)
            .send()
            .await
            .expect("remove seeded member");
        assert_eq!(
            removed.status(),
            StatusCode::OK,
            "removing a seeded {family} member must succeed"
        );
        let body: serde_json::Value = removed.json().await.expect("removal response JSON");
        assert_eq!(
            body["removed"], true,
            "a successful {family} removal must confirm the change"
        );

        let mut misses = Vec::new();
        for (who, id) in [
            ("the same member a second time", member_id),
            ("an account that never joined", stranger_id),
            ("an id no account carries", ABSENT_ID),
        ] {
            let response = fx
                .client
                .delete(format!("{}{base_path}/{id}", fx.base))
                .bearer_auth(&fx.attacker_token)
                .send()
                .await
                .expect("remove member");
            let answer = Answer::of(response).await;
            assert_eq!(
                answer.status,
                StatusCode::NOT_FOUND,
                "removing {who} from the {family} must report that no membership changed: {}",
                answer.excerpt()
            );
            misses.push((who, answer));
        }

        let (first_who, first) = &misses[0];
        for (who, answer) in &misses[1..] {
            assert_eq!(
                first.shape(),
                answer.shape(),
                "removing {who} from the {family} is answered differently from removing \
                 {first_who}, so a no-op reports whether the account exists or used to be a \
                 member.\n      {first_who}: {} {}\n      {who}: {} {}",
                first.status,
                first.excerpt(),
                answer.status,
                answer.excerpt(),
            );
        }
    }

    let (_logs, total) = rg_db::ops::audit_log_ops::list_paginated(
        &db,
        0,
        100,
        None,
        Some("org.remove_member"),
        Some("org"),
        None,
        None,
    )
    .await
    .expect("list organization member removal audit events");
    assert_eq!(
        total, 1,
        "only the successful organization-member removal may emit org.remove_member; no-op \
         removals must not create audit history"
    );
}
