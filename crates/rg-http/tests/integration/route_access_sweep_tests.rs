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
        "POST /api/v1/repos/{owner}/{name}/fork",
        "`fork_repo` canonicalizes the *target* path before the clone creates it \
         (`path_to_git_url` → `fs::canonicalize`), so every fork of every repository is a \
         500 — see card_3b3525983401",
    ),
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
    // Not repository-scoped: the fix is `AuthUser` / an instance-admin
    // extractor, both of which already exist.
    "POST /api/v1/users/tokens",
    "POST /api/v1/users/ssh-keys",
    "POST /api/v1/users/mfa/enable",
    "POST /api/v1/users/mfa/disable",
    "POST /api/v1/repos",
    "POST /api/v1/imports",
    "POST /api/v1/orgs",
    "POST /api/v1/orgs/{name}/members",
    "POST /api/v1/orgs/{name}/teams",
    "POST /api/v1/orgs/{name}/teams/{team_id}/members",
    "POST /api/v1/admin/sso/providers",
    "PATCH /api/v1/admin/sso/providers/{id}",
    // Repository-scoped, gate written by hand inside the handler.
    "POST /api/v1/repos/{owner}/{name}/milestones",
    "POST /api/v1/repos/{owner}/{name}/labels",
    "POST /api/v1/repos/{owner}/{name}/issues",
    "POST /api/v1/repos/{owner}/{name}/issues/{number}/comments",
    "POST /api/v1/repos/{owner}/{name}/issues/{number}/assets",
    "POST /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets",
    "POST /api/v1/repos/{owner}/{name}/pulls/{number}/assets",
    "POST /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets",
    "POST /api/v1/repos/{owner}/{name}/contents/{*path}",
    "DELETE /api/v1/repos/{owner}/{name}/contents/{*path}",
    "POST /api/v1/repos/{owner}/{name}/mirror",
    "POST /api/v1/repos/{owner}/{name}/statuses/{sha}",
    "POST /api/v1/repos/{owner}/{name}/transfer",
    "POST /api/v1/repos/{owner}/{name}/webhooks/external/ci",
    "GET /api/v1/ai/repos/{owner}/{name}/search/code",
];

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
        Access::Public => Expect::Allowed,
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
