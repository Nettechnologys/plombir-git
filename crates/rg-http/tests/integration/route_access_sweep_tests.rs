//! One pass per persona over *every* route the server declares.
//!
//! The router used to be unenumerable: 320 method/path pairs registered by a
//! builder chain that nothing could read back, so "did we forget one?" was a
//! question only a pair of eyes could answer — and every hole this phase closed
//! was found by reading, one module at a time.
//!
//! `rg_http::route_table` fixed the enumeration half: a route can only be
//! registered through a call that also states its [`Access`] level, and the
//! build hands the resulting `(method, path, access)` rows out. This test is
//! the other half. It takes those rows and, for each one, asks the running
//! server the same question three times:
//!
//! - **anonymous** — no credentials at all;
//! - **outsider** — a perfectly valid session belonging to somebody else;
//! - **owner** — the account that owns the repository in the path.
//!
//! Repository-scoped routes are asked twice over, against a *private* and a
//! *public* repository, because that is where the levels differ: `RepoRead` is
//! open to anonymous callers on a public repository and closed on a private
//! one, while `RepoWrite` is closed on both.
//!
//! The owner pass is the baseline the other two need. A route that answers
//! "denied" to everyone proves nothing — the fixture could simply be broken —
//! so the same run checks that the owner is *not* denied, and re-checks the
//! owner's session at the end of the pass so a dead fixture is reported as a
//! dead fixture instead of as a passing security test.
//!
//! # What the three passes cannot see
//!
//! Both blind spots are here rather than in a card, because a reader deciding
//! whether a green run means anything needs them in front of them:
//!
//! - **Both public levels are owed `Expect::Allowed`**, and everything short of
//!   a denial satisfies that — so a public row cannot fail the persona passes
//!   whatever it answers. For static content that is correct; for the handful
//!   of rows that are *self-filtering* data gates it would be vacuous, and
//!   those declare [`Access::PublicFiltered`] instead. The persona passes still
//!   cannot fail them, but [`the_self_filtering_routes_show_public_and_hide_private`]
//!   holds each one to a promise a generic pass cannot state: answer an
//!   anonymous caller with real data, and never with data that caller may not
//!   see. [`no_public_route_names_the_private_repo`] stays as the broad net
//!   under *every* public row, including the ones nobody thought were gates.
//! - **A gate that resolves the right repository and then acts on a global
//!   `id`** passes here, because the gate did answer. That is a second
//!   mechanism, not a hole in this one, and it has its own pass:
//!   `cross_repo_id_scope_sweep_tests` drives every repository-scoped route
//!   that carries an instance-wide id against a repository the id does not
//!   belong to.

use std::collections::{BTreeSet, HashMap};

use reqwest::{Client, StatusCode};
use rg_http::route_table::{Access, RouteFact};

use crate::common::{create_issue, register_user, spawn_test_app_with_routes};

const PW: &str = "Qz7$wRtm";

const OWNER: &str = "sweepowner";
const OUTSIDER: &str = "sweepoutsider";
const PRIVATE_REPO: &str = "sweepprivate";
const PUBLIC_REPO: &str = "sweeppublic";
const ORG: &str = "sweeporg";

// ── The exceptions, spelled out ────────────────────────────────────────────
//
// The card that asked for this sweep asked for its exceptions to be explicit
// and signed, and these three lists are that. Nothing is skipped implicitly:
// a route absent from all of them is driven and asserted.
//
// The *protocol* exceptions are not listed here at all — a route whose
// credentials are not a ForgeKeep session declares `Access::Foreign` with its
// reason in the route table itself, and the sweep skips it on that basis.

/// Routes the fixture cannot reach the gate of, each with the reason.
const NO_FIXTURE: &[(&str, &str)] = &[
    (
        "GET /api/v1/artifacts/{id}",
        "the repository is resolved from the artifact id, and this fixture builds no \
         pipeline — every persona is answered 404 before any gate runs",
    ),
    (
        "GET /api/v1/artifacts/{id}/download",
        "same as GET /api/v1/artifacts/{id}",
    ),
    (
        "DELETE /api/v1/artifacts/{id}",
        "same as GET /api/v1/artifacts/{id}",
    ),
    (
        "GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts",
        "`require_pipeline_read` resolves the pipeline before the repository, and this \
         fixture runs no pipeline — 404 for every persona",
    ),
    (
        "DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}",
        "the gate resolves the pull request first, and seeding one needs commits on two \
         branches — out of reach of this fixture, so every persona is answered 404",
    ),
    (
        "DELETE /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}",
        "same as DELETE .../pulls/{number}/assets/{attachment_id}: no pull request, so no \
         review comment to hang an attachment on",
    ),
    (
        "GET /metrics",
        "the test harness never installs the Prometheus registry, so this answers 503 to \
         everyone; the route carries no gate of its own to check",
    ),
];

/// Routes that fall over (5xx) *after* letting the right caller through: the
/// access decision is correct, the handler behind it is not.
///
/// Keyed by route rather than by persona, because a handler that panics on the
/// owner would panic on anyone the gate admits. Each entry is a filed defect;
/// the list is checked both ways, so a route that stops falling over has to be
/// removed from it.
const FALLS_OVER: &[(&str, &str)] = &[
    (
        "GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list",
        "'package type not enabled for this repo' is a bare `anyhow!`, so a repository \
         without that registry answers 500 instead of 404 — see card_6db8f22d6b61",
    ),
    (
        "GET /api/v1/repos/{owner}/{name}/packages/npm/list",
        "same bare `anyhow!` as .../packages/{pkg_type}/list",
    ),
    (
        "GET /api/v1/repos/{owner}/{name}/packages/nuget/query",
        "same bare `anyhow!` as .../packages/{pkg_type}/list",
    ),
    (
        "GET /api/v1/repos/{owner}/{name}/packages/helm/index.yaml",
        "same bare `anyhow!` as .../packages/{pkg_type}/list",
    ),
    (
        "DELETE /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}",
        "same bare `anyhow!` as .../packages/{pkg_type}/list",
    ),
];

/// Routes whose successful effect breaks the fixture for everything after them.
///
/// They are still driven and still asserted; the ordering only makes sure the
/// rest of the pass is not asked to authenticate with a session this route just
/// revoked, or to read a repository this route just deleted. Order matters —
/// entries run in the order listed, after every other route in the pass.
const RUN_LAST: &[(&str, &str)] = &[
    (
        "POST /api/v1/repos/{owner}/{name}/transfer",
        "moves the repository out from under the fixture",
    ),
    (
        "DELETE /api/v1/repos/{owner}/{name}",
        "deletes the repository the rest of the pass is scoped to",
    ),
    (
        "POST /api/v1/users/logout",
        "revokes the session the rest of the pass authenticates with",
    ),
];

/// Routes that answer a *body* complaint before they answer the access
/// question: `400`, `415` or `422` where a denial was owed.
///
/// The cause is always the same shape — the handler takes `Json<_>` or
/// `Query<_>` and decides who is asking further down in its own body, so axum
/// runs the deserializer first and an anonymous caller is told its JSON is
/// malformed instead of being turned away. That is the "second dialect" of the
/// gate this phase is retiring (`card_f037c6e2e1f5` for the repository-scoped
/// ones, `card_533a5ddff9d3` for the rest).
///
/// **These routes are not proven closed.** The sweep proves only that the body
/// is rejected first; with a well-formed body the gate might or might not be
/// there. That is precisely why they are named here rather than passed over
/// quietly — the list is a work queue, and it shrinks as handlers take their
/// access level as an argument instead of fetching it mid-body.
const EXTRACTOR_BEFORE_GATE: &[&str] = &[
    // The twelve non-repository entries are gone: `card_533a5ddff9d3` moved
    // personal tokens, SSH keys, MFA enable/disable, repository and
    // organization creation, imports, the org membership and team surface and
    // the two SSO provider routes onto `AuthUser` / `OrgAdmin` /
    // `InstanceAdmin`. The gate is an argument on each of them now, so it runs
    // before axum reads the body.
    //
    // Repository-scoped, gate written by hand inside the handler. Five entries
    // left this list when `card_f037c6e2e1f5` moved milestones, labels, mirrors,
    // commit statuses and repository transfer onto `RepoWrite` / `RepoOwner`:
    // the gate is now an argument, so it runs before the body is read.
    "POST /api/v1/repos/{owner}/{name}/issues",
    "POST /api/v1/repos/{owner}/{name}/issues/{number}/comments",
    "POST /api/v1/repos/{owner}/{name}/issues/{number}/assets",
    "POST /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets",
    "POST /api/v1/repos/{owner}/{name}/pulls/{number}/assets",
    "POST /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets",
    // `GET /api/v1/ai/repos/{owner}/{name}/search/code` left this list with
    // `card_cd6f512e2e52`. It was never a *body* entry: its gate was a
    // `RepoRead` argument all along, declared after `Query<SearchCodeQuery>`,
    // whose required `q` answered first. Moving the gate ahead of the query is
    // the whole fix — see
    // [`no_route_answers_a_query_complaint_before_its_gate`], which is now the
    // pass that would notice.
];

/// Routes that answer a *query* complaint before they answer the access
/// question — the same defect as [`EXTRACTOR_BEFORE_GATE`], reached through
/// `Query<_>` instead of `Json<_>`.
///
/// Empty, and meant to stay that way: the list exists so that a route which
/// regresses can be quarantined with a reason instead of the pass being
/// switched off. See [`no_route_answers_a_query_complaint_before_its_gate`].
const QUERY_BEFORE_GATE: &[&str] = &[];

/// A query string no handler can deserialize, and no gate needs to.
///
/// Every name in it is a parameter this tree really declares as a number or a
/// bool, so a handler that reads its query before it decides who is asking is
/// guaranteed to trip on it. Names a handler does not declare are ignored by
/// serde, so a route taking none of them is unaffected — which is the point:
/// the pass separates "reads the query first" from "answers the question
/// first", and asserts nothing else.
const HOSTILE_QUERY: &str = "?page=zz&per_page=zz&page_size=zz&limit=zz&offset=zz\
                             &unread_only=zz&success=zz&user_id=zz";

/// Mismatches that are real defects, already filed, and not fixed yet.
///
/// An entry is `(persona scope METHOD /path, why)`. The test fails if a route
/// not in this list misbehaves — and it *also* fails if a route in this list
/// starts behaving, so the list cannot quietly rot into a blanket allowance.
const KNOWN_GAPS: &[(&str, &str)] = &[];

// ── Personas ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Persona {
    Anonymous,
    Outsider,
    Owner,
}

impl Persona {
    fn label(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::Outsider => "outsider",
            Self::Owner => "owner",
        }
    }
}

/// Which repository a repository-scoped route is pointed at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    /// Not repository-scoped — asked once.
    Global,
    PrivateRepo,
    PublicRepo,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::PrivateRepo => "private-repo",
            Self::PublicRepo => "public-repo",
        }
    }
}

/// What the declared level owes this persona.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    /// The gate must not turn this caller away.
    Allowed,
    /// The gate must turn this caller away — `401` or `403`.
    Denied,
    /// The gate must turn this caller away *without* confirming the resource
    /// exists — `401`, `403` or `404`. Used where masking is the deliberate
    /// design, as it is for a private organization.
    Hidden,
    /// Not asserted; the caller holds the reason.
    Unchecked,
}

/// The heart of the sweep: what each declared level owes each persona.
///
/// `Denied` is checked as a family (`401` *or* `403`) rather than as an exact
/// code. Which of the two is right is a separate question — a private repo owes
/// `401` to an anonymous caller and `403` to an authenticated outsider, and
/// `repo_read_gate_tests` pins exactly that. Here the question is only whether
/// the gate answered at all.
fn expectation(access: Access, persona: Persona, scope: Scope) -> Expect {
    use Persona::{Anonymous, Outsider, Owner};

    match access {
        // Neither public level can be failed here — see the module note and
        // `the_self_filtering_routes_show_public_and_hide_private`, which is
        // where `PublicFiltered` is actually held to something.
        Access::Public | Access::PublicFiltered => Expect::Allowed,
        Access::User => match persona {
            Anonymous => Expect::Denied,
            Outsider | Owner => Expect::Allowed,
        },
        // A public repository is readable by anyone, a private one is not.
        Access::RepoRead => match (persona, scope) {
            (_, Scope::PublicRepo) | (Owner, _) => Expect::Allowed,
            _ => Expect::Denied,
        },
        // Readable, but only with a session: anonymous is out even on a public
        // repository.
        Access::RepoAuthRead => match (persona, scope) {
            (Anonymous, _) => Expect::Denied,
            (Outsider, Scope::PublicRepo) | (Owner, _) => Expect::Allowed,
            _ => Expect::Denied,
        },
        // Nobody but the repository's own people, public or not.
        Access::RepoWrite | Access::RepoAdmin | Access::RepoOwner => match persona {
            Owner => Expect::Allowed,
            _ => Expect::Denied,
        },
        // The fixture organization is private and the outsider is not in it, so
        // both of the other personas are owed a denial — a masked one, since
        // `require_org_visible` answers 404 on purpose.
        Access::OrgRead | Access::OrgAdmin => match persona {
            Owner => Expect::Allowed,
            _ => Expect::Hidden,
        },
        // Neither fixture account is an instance administrator, so all three
        // personas are owed a denial — including the owner, which is what makes
        // this row an assertion rather than a tautology.
        Access::InstanceAdmin => Expect::Denied,
        // Signed off in the route table itself: a runner token, an OCI bearer
        // token, git credentials, a CI job token, a WebSocket ticket. The sweep
        // holds none of those.
        Access::Foreign(_) => Expect::Unchecked,
    }
}

/// How a probe came out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Match,
    Mismatch,
    /// A body or query extractor answered before the gate could — see
    /// [`EXTRACTOR_BEFORE_GATE`].
    GateNotReached,
    /// The route did not decide anything; it fell over. Always a failure,
    /// whatever the persona was owed.
    ServerError,
}

fn judge(expect: Expect, status: StatusCode) -> Outcome {
    // `501` is the one 5xx that is an answer rather than a failure: a route
    // that is honestly not implemented yet still made an access decision to get
    // there.
    if status.is_server_error() && status != StatusCode::NOT_IMPLEMENTED {
        return Outcome::ServerError;
    }
    let denied = matches!(status.as_u16(), 401 | 403);
    let hidden = denied || status == StatusCode::NOT_FOUND;
    let satisfied = match expect {
        Expect::Allowed => !denied,
        Expect::Denied => denied,
        Expect::Hidden => hidden,
        Expect::Unchecked => true,
    };
    if satisfied {
        return Outcome::Match;
    }
    // Owed a denial and told the body is malformed: the gate never ran.
    if matches!(expect, Expect::Denied | Expect::Hidden)
        && matches!(status.as_u16(), 400 | 415 | 422)
    {
        return Outcome::GateNotReached;
    }
    Outcome::Mismatch
}

// ── Fixture ────────────────────────────────────────────────────────────────

struct Fixture {
    base: String,
    client: Client,
    owner_token: String,
    outsider_token: String,
}

impl Fixture {
    fn token(&self, persona: Persona) -> Option<&str> {
        match persona {
            Persona::Anonymous => None,
            Persona::Outsider => Some(&self.outsider_token),
            Persona::Owner => Some(&self.owner_token),
        }
    }

    /// Is this persona's session still the session we think it is?
    async fn session_alive(&self, persona: Persona) -> bool {
        match self.token(persona) {
            None => true,
            Some(token) => {
                self.client
                    .get(format!("{}/api/v1/users/me", self.base))
                    .bearer_auth(token)
                    .send()
                    .await
                    .expect("users/me")
                    .status()
                    == StatusCode::OK
            }
        }
    }

    /// `GET <path>` with the given session, or with none at all.
    async fn get_as(&self, token: Option<&str>, path: &str) -> (StatusCode, String) {
        let mut req = self.client.get(format!("{}{path}", self.base));
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await.expect("filtered-route probe");
        let status = resp.status();
        (status, resp.text().await.unwrap_or_default())
    }

    async fn repo_readable_by_owner(&self, repo: &str) -> StatusCode {
        self.client
            .get(format!("{}/api/v1/repos/{OWNER}/{repo}", self.base))
            .bearer_auth(&self.owner_token)
            .send()
            .await
            .expect("repo read")
            .status()
    }
}

async fn create_repo(fx: &Fixture, name: &str, private: bool) {
    let resp = fx
        .client
        .post(format!("{}/api/v1/repos", fx.base))
        .bearer_auth(&fx.owner_token)
        .json(&serde_json::json!({"name": name, "is_private": private}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "fixture: creating repo '{name}' failed");
}

/// A *private* organization owned by the fixture owner, with one team in it.
///
/// Private on purpose: a public organization is anonymously readable by design,
/// which would make every `OrgRead` row of the matrix vacuous.
async fn create_org_with_team(fx: &Fixture) {
    let resp = fx
        .client
        .post(format!("{}/api/v1/orgs", fx.base))
        .bearer_auth(&fx.owner_token)
        .json(&serde_json::json!({
            "name": ORG,
            "display_name": "Sweep Org",
            "visibility": "private",
        }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "fixture: creating org '{ORG}' failed: {}",
        resp.status()
    );

    let resp = fx
        .client
        .post(format!("{}/api/v1/orgs/{ORG}/teams", fx.base))
        .bearer_auth(&fx.owner_token)
        .json(&serde_json::json!({"name": "sweepteam", "permission": "read"}))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "fixture: creating a team in '{ORG}' failed: {}",
        resp.status()
    );
}

/// What the fixture actually seeded in one repository, for the placeholders a
/// constant cannot fill.
struct RepoSeed {
    name: String,
    /// Id of the issue comment seeded in this repository. Comment ids are
    /// handed out instance-wide, so the public repository's comment is not
    /// number 1 — and the handlers refuse a comment belonging to another
    /// repository, which is exactly the cross-repo scoping earlier cards fixed.
    comment_id: String,
}

// ── Path filling ───────────────────────────────────────────────────────────

/// Whether a route is scoped to a repository by its path.
fn is_repo_path(path: &str) -> bool {
    path.contains("{owner}") && (path.contains("{name}") || path.contains("{repo}"))
}

/// Turn a route pattern into a concrete URL for this fixture.
///
/// Placeholders naming a resource the fixture does not create are filled with
/// `1`, which is deliberate: every repository gate resolves the repository from
/// `{owner}`/`{name}` and answers before the handler looks the inner id up, so
/// a non-existent milestone or webhook id cannot change the verdict. A route
/// where it *does* change the verdict is a route whose gate runs too late —
/// which is one of the things this sweep is for.
fn fill(path: &str, repo: &RepoSeed) -> String {
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
            "name" if is_org_route => ORG,
            "name" | "repo" => &repo.name,
            // Comments are numbered instance-wide while the handlers scope them
            // to the repository in the path, so this one has to come from the
            // seed rather than be guessed.
            "comment_id" => &repo.comment_id,
            "username" => OUTSIDER,
            "path" | "file" => "README.md",
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
            // Numeric ids, and anything new that has not been thought about:
            // `1` parses as every id type in the tree, and the fixture seeds
            // id 1 for the resources whose gate is resolved through them.
            _ => "1",
        });
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

// ── Driving one route ──────────────────────────────────────────────────────

async fn probe(
    fx: &Fixture,
    fact: &RouteFact,
    persona: Persona,
    repo: &RepoSeed,
) -> (StatusCode, String) {
    let url = format!("{}{}", fx.base, fill(&fact.path, repo));
    let mut req = match fact.method {
        "GET" => fx.client.get(url),
        "HEAD" => fx.client.head(url),
        "POST" => fx.client.post(url),
        "PUT" => fx.client.put(url),
        "PATCH" => fx.client.patch(url),
        "DELETE" => fx.client.delete(url),
        other => panic!("route table produced an unroutable method {other}"),
    };
    if let Some(token) = fx.token(persona) {
        req = req.bearer_auth(token);
    }
    if matches!(fact.method, "POST" | "PUT" | "PATCH") {
        // An empty JSON object. Whether it satisfies the handler's schema is
        // beside the point: the access gate is a `FromRequestParts` extractor,
        // so it runs *before* the body is looked at. A route that answers a
        // body-shape complaint to an anonymous caller has its gate in the wrong
        // place — see `EXTRACTOR_BEFORE_GATE`.
        req = req.json(&serde_json::json!({}));
    }
    let resp = req.send().await.expect("sweep request");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    (status, body.chars().take(160).collect())
}

/// `owner private-repo GET /api/v1/repos/{owner}/{name}` — how a single
/// (persona, scope, route) cell is named in the exception lists and the report.
fn cell(persona: Persona, scope: Scope, fact: &RouteFact) -> String {
    format!("{} {} {}", persona.label(), scope.label(), fact.label())
}

fn listed(list: &[(&str, &str)], label: &str) -> bool {
    list.iter().any(|(entry, _)| *entry == label)
}

// ── The sweep ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_route_answers_its_declared_access_level() {
    let (base, facts) = spawn_test_app_with_routes().await;
    // No cookie jar: every persona is identified by the bearer token this test
    // attaches, never by a `Set-Cookie` a previous request happened to leave
    // behind.
    let client = Client::builder().build().expect("http client");

    let owner_token = register_user(&base, OWNER, &format!("{OWNER}@example.com"), PW).await;
    let outsider_token =
        register_user(&base, OUTSIDER, &format!("{OUTSIDER}@example.com"), PW).await;
    let fx = Fixture {
        base,
        client,
        owner_token,
        outsider_token,
    };
    create_repo(&fx, PRIVATE_REPO, true).await;
    create_repo(&fx, PUBLIC_REPO, false).await;
    create_org_with_team(&fx).await;
    // An issue and a comment in each repository, so the routes that resolve one
    // of those before the repository are asked a real question rather than
    // answered 404. Issue *numbers* restart per repository, comment *ids* do
    // not — hence the seed.
    let mut seeds = Vec::new();
    for repo in [PRIVATE_REPO, PUBLIC_REPO] {
        create_issue(&fx.base, &fx.owner_token, OWNER, repo, "sweep").await;
        let resp = fx
            .client
            .post(format!(
                "{}/api/v1/repos/{OWNER}/{repo}/issues/1/comments",
                fx.base
            ))
            .bearer_auth(&fx.owner_token)
            .json(&serde_json::json!({"body": "sweep"}))
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_success(),
            "fixture: commenting on {repo}#1 failed: {}",
            resp.status()
        );
        let comment: serde_json::Value = resp.json().await.unwrap();
        seeds.push(RepoSeed {
            name: repo.to_string(),
            comment_id: comment["id"].as_i64().expect("comment id").to_string(),
        });
    }
    let (private_seed, public_seed) = (&seeds[0], &seeds[1]);

    assert!(
        !facts.is_empty(),
        "the route table came back empty — the sweep is not testing anything"
    );

    // A typo in an exception list would silently widen the sweep's blind spot,
    // so the lists are checked against the table before anything is driven.
    let labels: BTreeSet<String> = facts.iter().map(RouteFact::label).collect();
    for (label, reason) in NO_FIXTURE.iter().chain(RUN_LAST).chain(FALLS_OVER) {
        assert!(
            labels.contains(*label),
            "exception list names '{label}' ({reason}), which is not a route any more — drop it"
        );
    }
    for label in EXTRACTOR_BEFORE_GATE {
        assert!(
            labels.contains(*label),
            "EXTRACTOR_BEFORE_GATE names '{label}', which is not a route any more — drop it"
        );
    }
    // A `KNOWN_GAPS` key is `persona scope METHOD /path`; the route it names is
    // the last two words.
    for (key, reason) in KNOWN_GAPS {
        let route: Vec<&str> = key.split_whitespace().collect();
        let route = route[route.len().saturating_sub(2)..].join(" ");
        assert!(
            labels.contains(&route),
            "KNOWN_GAPS names '{key}' ({reason}), which is not a route any more — drop it"
        );
    }

    // The destructive routes go last, in the order they are listed; DELETE goes
    // after everything else within the main group, so a destructive call is
    // never the reason a later route in the same pass answers differently.
    let rank = |fact: &RouteFact| {
        let label = fact.label();
        RUN_LAST
            .iter()
            .position(|(entry, _)| *entry == label)
            .map_or(0, |index| index + 1)
    };
    let mut ordered: Vec<&RouteFact> = facts
        .iter()
        .filter(|fact| !listed(NO_FIXTURE, &fact.label()))
        .collect();
    ordered.sort_by_key(|fact| (rank(fact), rank(fact) == 0 && fact.method == "DELETE"));
    let deferred_from = ordered.partition_point(|fact| rank(fact) == 0);

    let mut report = String::new();
    let mut failures: BTreeSet<String> = BTreeSet::new();
    let mut gate_not_reached: BTreeSet<String> = BTreeSet::new();
    let mut fell_over: BTreeSet<String> = BTreeSet::new();

    for persona in [Persona::Anonymous, Persona::Outsider, Persona::Owner] {
        // The baseline, taken before the pass rather than inferred after it:
        // the owner can read their own private repository right now.
        if persona == Persona::Owner {
            let status = fx.repo_readable_by_owner(PRIVATE_REPO).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "fixture is dead before the owner pass: the owner cannot read their own \
                 private repository ({status}) — every denial below would be meaningless"
            );
        }

        for (index, fact) in ordered.iter().enumerate() {
            // The session check sits between the two groups: everything after
            // it is known to destroy what it touches.
            if index == deferred_from {
                assert!(
                    fx.session_alive(persona).await,
                    "the {} session died during its own pass — the verdicts above describe a \
                     broken fixture, not the access gates",
                    persona.label()
                );
            }

            let scopes: &[Scope] = if is_repo_path(&fact.path) && fact.access.is_repo_scoped() {
                &[Scope::PrivateRepo, Scope::PublicRepo]
            } else {
                &[Scope::Global]
            };

            for &scope in scopes {
                let expect = expectation(fact.access, persona, scope);
                if expect == Expect::Unchecked {
                    continue;
                }
                let repo = match scope {
                    Scope::PublicRepo => public_seed,
                    _ => private_seed,
                };
                let (status, body) = probe(&fx, fact, persona, repo).await;
                let outcome = judge(expect, status);
                if outcome == Outcome::Match {
                    continue;
                }

                let label = fact.label();
                let key = cell(persona, scope, fact);
                let note = match outcome {
                    Outcome::GateNotReached if EXTRACTOR_BEFORE_GATE.contains(&label.as_str()) => {
                        gate_not_reached.insert(label.clone());
                        continue;
                    }
                    Outcome::GateNotReached => {
                        gate_not_reached.insert(label.clone());
                        failures.insert(key.clone());
                        "  [body rejected before the gate — add to EXTRACTOR_BEFORE_GATE only \
                         with a reason]"
                    }
                    Outcome::ServerError if listed(FALLS_OVER, &label) => {
                        fell_over.insert(label.clone());
                        continue;
                    }
                    Outcome::ServerError => {
                        failures.insert(key.clone());
                        "  [the route fell over instead of deciding]"
                    }
                    _ if listed(KNOWN_GAPS, &key) => "  [known gap]",
                    _ => {
                        failures.insert(key.clone());
                        ""
                    }
                };
                report.push_str(&format!(
                    "  {key}\n      declared {:?}, expected {expect:?}, got {status}{note}\n\
                     \x20     body: {body}\n",
                    fact.access,
                ));
            }
        }
    }

    // A quarantined route that has started behaving must leave its list, or the
    // list slowly becomes a blanket allowance.
    let mut healed: Vec<String> = Vec::new();
    for label in EXTRACTOR_BEFORE_GATE {
        if !gate_not_reached.contains(*label) {
            healed.push(format!(
                "  {label} now decides before it reads the body — drop it from \
                 EXTRACTOR_BEFORE_GATE"
            ));
        }
    }
    for (label, reason) in FALLS_OVER {
        if !fell_over.contains(*label) {
            healed.push(format!(
                "  {label} no longer falls over — drop it from FALLS_OVER (was: {reason})"
            ));
        }
    }
    for (key, reason) in KNOWN_GAPS {
        if !report.contains(key) {
            healed.push(format!(
                "  {key} now behaves — drop it from KNOWN_GAPS (was: {reason})"
            ));
        }
    }

    assert!(
        failures.is_empty() && healed.is_empty(),
        "route access sweep: {} undeclared mismatch(es), {} stale quarantine entr(ies).\n\
         Every route below answered something other than what its `Access` level in \
         `rg_http::route_table` promises. Either the gate is wrong, or the declaration is.\n\
         {report}{}",
        failures.len(),
        healed.len(),
        healed.join("\n"),
    );
}

/// A hostile query string must not overtake the gate.
///
/// `Query<_>` is a `FromRequestParts` like `Json<_>` is a `FromRequest`, and
/// axum runs a handler's arguments left to right — so a gate written as the
/// first statement of the function *body* still runs after the query has been
/// deserialized. `GET /api/v1/admin/users?per_page=abc` answered an anonymous
/// caller `400`, with the serde error naming the parameter and its type,
/// instead of `403`.
///
/// [`every_route_answers_its_declared_access_level`] cannot see this, and not
/// by oversight: its `probe` sends a body only on `POST`/`PUT`/`PATCH` and a
/// query string never, so every route answers on its defaults and the gate
/// gets there in time. This pass drives the same table with a query that
/// cannot deserialize as anything and asks the one question that survives the
/// missing fixture — did the route still *decide*?
///
/// Deliberately narrow. It seeds nothing, so a route that resolves a
/// non-existent repository first answers `404` and that is accepted; what is
/// not accepted is `400`/`415`/`422`, which is a route telling a caller it was
/// never going to admit what its parameters are called.
#[tokio::test]
async fn no_route_answers_a_query_complaint_before_its_gate() {
    let (base, facts) = spawn_test_app_with_routes().await;
    let client = Client::builder().build().expect("http client");
    // Nothing is seeded: every probe here is anonymous, and the question is
    // only which *kind* of answer comes back.
    let seed = RepoSeed {
        name: "nosuchrepo".to_string(),
        comment_id: "1".to_string(),
    };

    let labels: BTreeSet<String> = facts.iter().map(RouteFact::label).collect();
    for label in QUERY_BEFORE_GATE {
        assert!(
            labels.contains(*label),
            "QUERY_BEFORE_GATE names '{label}', which is not a route any more — drop it"
        );
    }

    let mut probed = 0usize;
    let mut offenders: Vec<String> = Vec::new();
    let mut quarantined: BTreeSet<String> = BTreeSet::new();

    for fact in &facts {
        let label = fact.label();
        if listed(NO_FIXTURE, &label) {
            continue;
        }
        // An anonymous caller is owed the same thing whatever the scope;
        // `PrivateRepo` is simply the one where that is also true of the
        // repository levels.
        let expect = expectation(fact.access, Persona::Anonymous, Scope::PrivateRepo);
        if !matches!(expect, Expect::Denied | Expect::Hidden) {
            continue;
        }

        let url = format!("{base}{}{HOSTILE_QUERY}", fill(&fact.path, &seed));
        let mut req = match fact.method {
            "GET" => client.get(url),
            "HEAD" => client.head(url),
            "POST" => client.post(url),
            "PUT" => client.put(url),
            "PATCH" => client.patch(url),
            "DELETE" => client.delete(url),
            other => panic!("route table produced an unroutable method {other}"),
        };
        if matches!(fact.method, "POST" | "PUT" | "PATCH") {
            req = req.json(&serde_json::json!({}));
        }
        let status = req.send().await.expect("hostile-query probe").status();
        probed += 1;

        if !matches!(status.as_u16(), 400 | 415 | 422) {
            continue;
        }
        // A route already quarantined for reading its *body* first will
        // complain about that body here too; it is the same defect and the
        // main sweep is where its list is kept honest.
        if EXTRACTOR_BEFORE_GATE.contains(&label.as_str()) {
            continue;
        }
        if QUERY_BEFORE_GATE.contains(&label.as_str()) {
            quarantined.insert(label);
            continue;
        }
        offenders.push(format!(
            "  {label} — declared {:?}, anonymous got {status}",
            fact.access
        ));
    }

    assert!(
        probed > 50,
        "only {probed} gated route(s) were driven — the filter is wrong, not the server"
    );

    let healed: Vec<String> = QUERY_BEFORE_GATE
        .iter()
        .filter(|label| !quarantined.contains(**label))
        .map(|label| {
            format!(
                "  {label} now decides before it reads the query — drop it from QUERY_BEFORE_GATE"
            )
        })
        .collect();

    assert!(
        offenders.is_empty() && healed.is_empty(),
        "{} route(s) answered a query complaint to a caller they owed a denial, and {} \
         stale quarantine entr(ies).\nThe gate is inside the handler body, behind a \
         `Query<_>` argument that axum runs first. Take the access level as an argument \
         *before* the query — `InstanceAdmin` / `AuthUser` / the `repo_access` extractors \
         — so the decision happens before anything is parsed.\n{}{}",
        offenders.len(),
        healed.len(),
        offenders.join("\n"),
        healed.join("\n"),
    );
}

/// Files allowed to call `Router::route` directly, with the reason.
const ROUTE_CALL_SIGNED_OFF: &[(&str, &str)] = &[
    (
        "route_table.rs",
        "the table itself: the one place a route is registered, and it takes an `Access`",
    ),
    (
        "security.rs",
        "a two-route scaffold inside `#[cfg(test)]`, to drive the header middleware",
    ),
    (
        "rate_limit.rs",
        "a one-route scaffold inside `#[cfg(test)]`, to drive the limiter middleware",
    ),
];

/// A route may only be born through [`RouteTable`], which cannot register one
/// without an access level.
///
/// `RouteTable` has no `route()`, so the compiler already stops the ordinary
/// mistake — but nothing stops somebody reaching past it for a bare
/// `Router::new().route(...)` and merging that in. This guard closes the gap:
/// it is a grep because the failure it guards against is a line of code that
/// was *not* written, and no request can exercise that.
#[test]
fn no_route_is_registered_outside_the_table() {
    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(
        files.len() > 20,
        "src tree looks empty — guard is not running"
    );

    let mut offenders = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&src)
            .expect("file under src/")
            .to_string_lossy()
            .replace('\\', "/");
        if ROUTE_CALL_SIGNED_OFF
            .iter()
            .any(|(allowed, _)| rel == *allowed)
        {
            continue;
        }
        let text = std::fs::read_to_string(file).expect("read source file");
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains(".route(") || code.contains(".route_service(") {
                offenders.push(format!("  {rel}:{} — {}", n + 1, code));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a route was registered straight on an `axum::Router`, so nothing declares who may \
         call it. Register it through `RouteTable` (`crate::route_table`) instead — its \
         `get`/`post`/… all take an `Access` level, which is what the route-access sweep \
         walks. If this really is test scaffolding rather than a served route, add the file \
         to ROUTE_CALL_SIGNED_OFF with the reason.\n{}",
        offenders.join("\n")
    );
}

/// The sweep is worthless if it walks a handful of routes, so the shape of the
/// table is pinned separately from its contents.
#[tokio::test]
async fn the_route_table_covers_the_whole_server() {
    let (_base, facts) = spawn_test_app_with_routes().await;

    assert!(
        facts.len() > 300,
        "the route table declares only {} routes — the server has ~320, so something is \
         registering routes outside the table",
        facts.len()
    );

    // Nothing may be declared twice with two different levels: that is a route
    // whose access level depends on which registration you happen to read.
    let mut seen: HashMap<String, Access> = HashMap::new();
    for fact in &facts {
        if let Some(previous) = seen.insert(fact.label(), fact.access) {
            assert_eq!(
                previous,
                fact.access,
                "{} is declared twice with different access levels",
                fact.label()
            );
        }
    }

    // Every `Foreign` sign-off must say something.
    for fact in &facts {
        if let Access::Foreign(reason) = fact.access {
            assert!(
                !reason.trim().is_empty(),
                "{} is signed off as Foreign without a reason",
                fact.label()
            );
        }
    }
}

/// No route a caller can reach *without a ForgeKeep session* names a private
/// repository to an anonymous caller.
///
/// The broad net under both public levels — and over the `Foreign` rows too,
/// which is the same population by a different route: git-over-HTTP, the OCI
/// registry and the LFS batch endpoint carry their own credentials, so a caller
/// with none at all reaches them exactly as it reaches a public one. They have
/// their own gates and their own tests; this is only the net that would notice
/// if one of them started naming a repository it should not.
///
/// It exists because `expectation` owes a public route `Expect::Allowed` and
/// every non-denial satisfies that, so the persona passes cannot fail a public
/// row however it answers. This one can: seed a private repository, drive every
/// such `GET`/`HEAD` with no credentials, fail if its name comes back. What it
/// catches is a leak through a route nobody wrote a bespoke test for —
/// including one added tomorrow, since it walks the table rather than a list.
///
/// It is deliberately weaker than a bespoke test and should not be mistaken for
/// one. A route that answers `400` for want of a query parameter is not proven
/// to filter anything; it is only proven not to have leaked *here*. That is
/// exactly the excuse [`the_self_filtering_routes_show_public_and_hide_private`]
/// takes away from the rows where filtering is the point — this stays the net
/// under the rest, where nobody expected a gate at all.
#[tokio::test]
async fn no_public_route_names_the_private_repo() {
    let (base, facts) = spawn_test_app_with_routes().await;
    let client = Client::builder().build().expect("http client");

    let owner_token = register_user(&base, OWNER, &format!("{OWNER}@example.com"), PW).await;
    let fx = Fixture {
        base,
        client,
        owner_token,
        // Never used: every probe below is anonymous.
        outsider_token: String::new(),
    };
    create_repo(&fx, PRIVATE_REPO, true).await;
    let seed = RepoSeed {
        name: PRIVATE_REPO.to_string(),
        comment_id: "1".to_string(),
    };

    // The owner can see it — otherwise "nobody mentioned it" would just mean
    // the fixture never created anything.
    assert_eq!(
        fx.repo_readable_by_owner(PRIVATE_REPO).await,
        StatusCode::OK,
        "fixture is dead: the owner cannot read their own private repository, so no \
         route could name it and this test would pass vacuously"
    );

    let mut probed = 0usize;
    let mut leaks: Vec<String> = Vec::new();
    let no_session_needed =
        |fact: &&RouteFact| fact.access.is_public() || matches!(fact.access, Access::Foreign(_));

    for fact in facts.iter().filter(no_session_needed) {
        // Only the safe verbs: a public `POST` here is login / register /
        // password reset, and driving those adds accounts and mail instead of
        // reading anything back.
        if !matches!(fact.method, "GET" | "HEAD") {
            continue;
        }
        let url = format!("{}{}", fx.base, fill(&fact.path, &seed));
        let req = if fact.method == "HEAD" {
            fx.client.head(url)
        } else {
            fx.client.get(url)
        };
        let resp = req.send().await.expect("public leak probe");
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        probed += 1;
        if body.contains(PRIVATE_REPO) {
            leaks.push(format!(
                "  {} — anonymous reply ({status}) names '{PRIVATE_REPO}':\n      {}",
                fact.label(),
                body.chars().take(240).collect::<String>(),
            ));
        }
    }

    assert!(
        probed > 5,
        "only {probed} session-less route(s) were probed — the filter is wrong, not the server"
    );
    assert!(
        leaks.is_empty(),
        "{} route(s) reachable without a session handed a private repository to an \
         anonymous caller. Such a route either serves content that does not depend on who \
         asks, or filters by what the caller may see — these did neither.\n{}",
        leaks.len(),
        leaks.join("\n"),
    );
}

// ── The self-filtering rows, held to both halves of their promise ──────────

/// One [`Access::PublicFiltered`] row and the two probes that pin it.
///
/// Two probes rather than one, because a route that answers nothing at all
/// satisfies "did not leak" perfectly. `shows_public` proves the route is
/// alive and serving; `hides_private` proves the filter is what keeps the
/// private repository out of that same answer.
struct FilteredProbe {
    /// `RouteFact::label()` of the row this describes.
    route: &'static str,
    /// Query string (with its `?`, or empty) whose anonymous answer must name
    /// the *public* repository.
    shows_public: String,
    /// Query string whose anonymous answer must not name the *private* one.
    hides_private: String,
    /// Whether the owner is supposed to see the private repository through
    /// this very route. When true, the `hides_private` probe is re-run with
    /// the owner's session and must name it — which is what proves the query
    /// can reach the repository at all, so that "anonymous saw nothing" is the
    /// filter working rather than the probe missing.
    ///
    /// `/repos/explore` is the instance shop window: nobody sees a private
    /// repository there, its owner included, so there is no such control to
    /// run and `shows_public` carries the anti-vacuity weight alone.
    owner_sees_private: bool,
}

fn filtered_probes() -> Vec<FilteredProbe> {
    vec![
        FilteredProbe {
            route: "GET /api/v1/repos/explore",
            shows_public: String::new(),
            hides_private: String::new(),
            owner_sees_private: false,
        },
        FilteredProbe {
            route: "GET /api/v1/repos/{owner}",
            shows_public: String::new(),
            hides_private: String::new(),
            owner_sees_private: true,
        },
        FilteredProbe {
            // The route the old net could not check at all: with no `q` it
            // answers `400`, and a `400` proves nothing about filtering.
            route: "GET /api/v1/search",
            shows_public: format!("?type=repos&q={PUBLIC_REPO}"),
            hides_private: format!("?type=repos&q={PRIVATE_REPO}"),
            owner_sees_private: true,
        },
    ]
}

/// Every self-filtering public route answers an anonymous caller with real
/// data, and never with data that caller may not see.
///
/// This is the half of the `Public` blind spot a generic pass cannot reach.
/// The persona matrix owes these rows `Expect::Allowed`, which any non-denial
/// satisfies, so they cannot fail it however they answer — and they are exactly
/// the rows where the answer *must* depend on who asks: `/repos/explore`,
/// `/repos/{owner}` and `/search` are the shape `card_76e22c3a8364` found a
/// private repository on show through.
///
/// Held to both halves on purpose. "Did not name the private repository" is
/// satisfied by a route that answers `400`, `404` or an empty page — which is
/// how the previous net let `/search` through without ever driving it. So each
/// row also has to *show* the public repository, and where the owner is
/// supposed to see the private one, the same query run with the owner's session
/// has to return it. A filter dropped from any of the three then fails this
/// test by name.
///
/// What it still does not check, plainly: a leak that is not the repository's
/// *name* — an id, a count, a `total` that includes rows the caller cannot see.
/// Those need the per-route tests.
#[tokio::test]
async fn the_self_filtering_routes_show_public_and_hide_private() {
    let (base, facts) = spawn_test_app_with_routes().await;
    let client = Client::builder().build().expect("http client");

    let owner_token = register_user(&base, OWNER, &format!("{OWNER}@example.com"), PW).await;
    let fx = Fixture {
        base,
        client,
        owner_token,
        outsider_token: String::new(),
    };
    create_repo(&fx, PUBLIC_REPO, false).await;
    create_repo(&fx, PRIVATE_REPO, true).await;
    let seed = RepoSeed {
        name: PRIVATE_REPO.to_string(),
        comment_id: "1".to_string(),
    };

    let probes = filtered_probes();

    // Checked both ways, like every other list in this file: a new
    // `PublicFiltered` route with no probe would otherwise be declared and
    // never driven, and a probe left behind after a route is renamed would
    // quietly stop testing anything.
    let declared: BTreeSet<String> = facts
        .iter()
        .filter(|f| f.access == Access::PublicFiltered)
        .map(|f| f.label())
        .collect();
    let probed: BTreeSet<String> = probes.iter().map(|p| p.route.to_string()).collect();
    assert_eq!(
        declared,
        probed,
        "every `Access::PublicFiltered` route needs a probe in `filtered_probes` and every \
         probe needs a route.\n  declared, not probed: {:?}\n  probed, not declared: {:?}",
        declared.difference(&probed).collect::<Vec<_>>(),
        probed.difference(&declared).collect::<Vec<_>>(),
    );
    assert!(
        !probes.is_empty(),
        "no route declares `Access::PublicFiltered` — either the level went unused or the \
         three self-filtering routes were quietly relabelled `Public`, which puts them back \
         in the blind spot this test exists for"
    );

    let path_of = |label: &str| -> String {
        let fact = facts
            .iter()
            .find(|f| f.label() == label)
            .expect("probe names a route in the table");
        fill(&fact.path, &seed)
    };

    let mut failures: Vec<String> = Vec::new();
    for probe in &probes {
        let path = path_of(probe.route);

        // Half one: the route serves. Without this a handler that answered
        // `{"results":[]}` to everything would pass the leak half perfectly.
        let (status, body) = fx
            .get_as(None, &format!("{path}{}", probe.shows_public))
            .await;
        if status != StatusCode::OK || !body.contains(PUBLIC_REPO) {
            failures.push(format!(
                "  {} — anonymous {}{} answered {status} without naming the public \
                 repository, so this route proves nothing about filtering:\n      {}",
                probe.route,
                path,
                probe.shows_public,
                body.chars().take(240).collect::<String>(),
            ));
        }

        // Half two: and it filters.
        let (status, body) = fx
            .get_as(None, &format!("{path}{}", probe.hides_private))
            .await;
        if status != StatusCode::OK {
            failures.push(format!(
                "  {} — anonymous {}{} answered {status}; a route that declines to answer \
                 has not been shown to filter anything",
                probe.route, path, probe.hides_private,
            ));
        } else if body.contains(PRIVATE_REPO) {
            failures.push(format!(
                "  {} — anonymous {}{} names the private repository:\n      {}",
                probe.route,
                path,
                probe.hides_private,
                body.chars().take(240).collect::<String>(),
            ));
        }

        // The control on half two: the query does reach the repository, and it
        // was the filter that held it back.
        if probe.owner_sees_private {
            let (status, body) = fx
                .get_as(
                    Some(&fx.owner_token),
                    &format!("{path}{}", probe.hides_private),
                )
                .await;
            if status != StatusCode::OK || !body.contains(PRIVATE_REPO) {
                failures.push(format!(
                    "  {} — the owner's own {}{} answered {status} without naming their \
                     private repository, so the anonymous probe next to it is vacuous: it \
                     found nothing because the query finds nothing.\n      {}",
                    probe.route,
                    path,
                    probe.hides_private,
                    body.chars().take(240).collect::<String>(),
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} self-filtering public route(s) broke their promise:\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

/// The PAT scope a route demands must follow from the level it declares.
///
/// `pat_auth::required_pat_scope` decides which family a personal access token
/// may enter by reading the *path string* — `/admin…` is `"admin"`, `/users…`
/// is `"user"`, everything else is `"repo"`. It has to work that way: the
/// middleware is an outer layer and runs before axum matches anything, so
/// there is no `MatchedPath` to look the route up by. But the level every one
/// of those routes requires is already declared, in the table this file
/// walks — and until now nothing compared the two statements.
///
/// They have drifted once already, and the drift was patched by hand:
/// `|| path == "/runners/register"` exists because that route needs an
/// instance admin while its path does not begin with `/admin`. The patch is
/// fine; the *generator* is what this test pins. The next administrative route
/// registered outside `/admin` would silently fall through to `"repo"`, which
/// is the scope `create_token` hands out by default.
///
/// Only the instance-admin family is asserted, and that is a deliberate scope
/// rather than a gap. It is the family where a disagreement is a hole instead
/// of an inconvenience, and it is the only one that actually lines up with a
/// level: `POST /repos` and `POST /orgs` declare `User` and ask for `"repo"`,
/// which is right — they create repositories. A blanket level-to-scope rule
/// would be a rule with a list of exceptions, which is what this file already
/// has enough of.
///
/// Both directions, because they catch different mistakes:
///
/// - a route declared `InstanceAdmin` that does not demand `"admin"` is the
///   hole itself;
/// - a route demanding `"admin"` that is *not* declared `InstanceAdmin` is a
///   hand-written exception that has outlived its route — which is exactly how
///   the first one comes back.
///
/// The second direction buys something it was not aimed at.
/// `middleware::maintenance_middleware` exempts `/api/v1/admin/` from read-only
/// maintenance mode by the same kind of prefix test, and *that* one fails open:
/// a non-administrative route registered under `/api/v1/admin/` would keep
/// accepting mutations while the instance is meant to be frozen. Since
/// `required_pat_scope` answers `"admin"` for exactly that prefix, such a route
/// shows up here as an over-scoped row — so the exemption's precondition is
/// pinned too, by a test that is nominally about something else.
#[tokio::test]
async fn every_instance_admin_route_demands_the_admin_pat_scope() {
    let (_base, facts) = spawn_test_app_with_routes().await;
    assert!(
        !facts.is_empty(),
        "the route table came back empty — this test is not checking anything"
    );

    let mut under_scoped: Vec<String> = Vec::new();
    let mut over_scoped: Vec<String> = Vec::new();
    let mut admin_rows = 0usize;

    for fact in &facts {
        let scope = rg_http::pat_auth::required_pat_scope(&fact.path);
        let declared_admin = fact.access == Access::InstanceAdmin;
        let demands_admin = scope == Some("admin");
        if declared_admin {
            admin_rows += 1;
        }
        match (declared_admin, demands_admin) {
            (true, false) => under_scoped.push(format!(
                "  {} — declared InstanceAdmin, but a PAT scoped {:?} reaches it",
                fact.label(),
                scope.unwrap_or("(none)")
            )),
            (false, true) => over_scoped.push(format!(
                "  {} — demands the \"admin\" scope, but declares {:?}",
                fact.label(),
                fact.access
            )),
            _ => {}
        }
    }

    // Without this the whole pass would be vacuously green the day the table
    // stops declaring any administrative route at all.
    assert!(
        admin_rows >= 10,
        "only {admin_rows} route(s) declare InstanceAdmin — the table, not the rule, is what \
         changed, and this pass would now prove nothing"
    );

    assert!(
        under_scoped.is_empty() && over_scoped.is_empty(),
        "`pat_auth::required_pat_scope` and the levels declared in `rg_http::route_table` \
         disagree about {} route(s).\nThe scope decision is made from the path string, so an \
         administrative route registered outside `/admin` falls through to \"repo\" — the \
         scope `create_token` issues by default. Either teach `required_pat_scope` about the \
         route, or fix the level it declares.\n{}{}",
        under_scoped.len() + over_scoped.len(),
        under_scoped.join("\n"),
        over_scoped.join("\n"),
    );
}
