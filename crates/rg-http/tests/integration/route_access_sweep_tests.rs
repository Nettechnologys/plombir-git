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
//! A row owed [`Expect::Hidden`] is asked once more again — against an
//! organization name nothing created, or, on an anchored route, against a row id
//! nothing created — and the two replies are compared whole. That is a different
//! kind of question from the rest of the sweep: everywhere else the oracle is a
//! status, and a mask is a claim about what the caller *learns* — which lives
//! just as much in the message underneath the status as in the status itself
//! (card_81e6649615c1).
//!
//! Which rows those are is the sweep's third axis, and it does not come from the
//! route table. An *anchored* route — one whose path names no repository and
//! resolves it out of the row its id addresses — declares the same `RepoRead` a
//! path-based one declares, so the level cannot tell them apart. The census
//! comes from the handlers' signatures instead, read through the same
//! `common::source_scan` helpers `anchored_scope_sweep_tests` reads, because two
//! passes counting different populations means the weaker one is the real
//! ceiling (card_41ec8f1a29a3).
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
//! - **An [`Access::Foreign`] row is not driven at all**, and that exemption is
//!   about the *credential* — no fixture here holds a runner token or an OCI
//!   bearer token. It is not a statement about the row's conduct, and reading it
//!   as one is how `/api/v1/ws/job/{job_id}` answered `403` for a stranger's
//!   private job and `404` for an id that never existed (card_6b2cadf41876).
//!   `foreign_id_scope_sweep_tests` asks those rows the one question that needs
//!   no foreign credential, because the same caller drives both halves of it:
//!   is a real id tellable apart from an absent one?

use std::collections::{BTreeSet, HashMap};

use reqwest::{Client, StatusCode};
use rg_http::route_table::{Access, RouteFact};

use crate::common::source_scan::anchored_handler_targets;
use crate::common::{
    create_issue, register_user, seed_artifact, spawn_test_app_with_routes,
    spawn_test_app_with_routes_and_db,
};

const PW: &str = "Qz7$wRtm";

const OWNER: &str = "sweepowner";
const OUTSIDER: &str = "sweepoutsider";
const PRIVATE_REPO: &str = "sweepprivate";
const PUBLIC_REPO: &str = "sweeppublic";
const ORG: &str = "sweeporg";

/// An organization name nothing in this fixture ever creates.
///
/// The reference reply the private organization has to be indistinguishable
/// from. [`Expect::Hidden`] is a claim about what a caller *learns*, and a `404`
/// on its own says nothing about that — an organization that does not exist
/// answers `404` too. Only the two replies side by side answer the question,
/// which is why the sweep needs a second name to point at (card_81e6649615c1).
const ABSENT_ORG: &str = "sweepabsentorg";

/// A row id nothing in this fixture ever creates.
///
/// [`ABSENT_ORG`]'s counterpart on the anchored routes. Their paths carry no
/// name to vary — the repository is resolved out of the row the id addresses —
/// so the second question a masked cell is asked varies the *id* instead, and
/// the two replies have to be the same reply.
const ABSENT_ID: &str = "999999";

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
///
/// An entry here is skipped by every persona pass, which makes it the widest
/// exemption the sweep grants — and it used to be the only one of the three
/// lists nothing re-read. `FALLS_OVER` and `EXTRACTOR_BEFORE_GATE` are both
/// checked in reverse (a route that stops misbehaving has to leave the list);
/// this one was only checked for naming a live route, so the *reason* — a
/// checkable claim, "every persona is answered before any gate runs" — was
/// never compared to what the route actually answers.
///
/// [`the_out_of_reach_routes_are_still_out_of_reach`] closes that: it drives
/// every entry with all three personas, in both repository scopes, and fails if
/// any of them gets *in*. That is the rot this list can hide — a route stops
/// being out of reach, keeps its exemption, and nobody is checking who it lets
/// through.
///
/// Writing it also corrected two reasons that were simply untrue: the two
/// `DELETE .../assets/{attachment_id}` rows were signed off as "answered 404 for
/// every persona" while on the private repository they answer `401` to an
/// anonymous caller and `403` to the outsider. They stay on the list — no
/// fixture here can seed a pull request — but on a reason that describes them
/// (card_3cc941a766a0), and one that names *which* persona is the reason.
///
/// That second correction did not go far enough, which is the standing hazard
/// of a reason written as prose: the rewritten text still claimed the public
/// half answered `404` to *everyone*, when an anonymous caller has been getting
/// `401` there all along — first from an `extract_user_id` call at the top of
/// the handler body, now from the `RepoAuthRead` in its signature
/// (card_41d5b5cf0cbb). Nothing in this file compares a reason to a status
/// code, so only a reader re-measuring catches it.
///
/// The three artifact rows used to be here, excused as "this fixture builds no
/// pipeline — every persona is answered 404 before any gate runs". True, and
/// that was the problem: the excuse was also the *only* thing keeping the rows
/// out of a matrix that would have judged them wrong. An anchored route declares
/// `RepoRead` like a path-based one, `expectation` read the level and owed the
/// outsider `Expect::Denied`, and `Denied` accepts `401` **or** `403` — the one
/// answer `AnchoredRead` exists to never send. The row was latent rather than
/// live only because nothing drove it (card_41ec8f1a29a3).
///
/// So the fixture seeds an artifact now and the rows are judged: `Expect::Hidden`
/// for the two personas who cannot read the repository it lives in, paired
/// against [`ABSENT_ID`] the way an organization row is paired against
/// [`ABSENT_ORG`].
const NO_FIXTURE: &[(&str, &str)] = &[
    (
        "DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}",
        "only the *owner* persona is out of reach: seeding a pull request needs commits on \
         two branches, so the row answers 404 for want of one. The gate itself answers \
         everybody else, and the row is listed on measured values rather than a guess — \
         private: 401 anonymous, 403 outsider, 404 owner; public: 401 anonymous, 404 \
         outsider, 404 owner. The anonymous 401 on the *public* repository is what the \
         handler's `RepoAuthRead` says (card_41d5b5cf0cbb); the outsider gets as far as the \
         missing pull request because reading a public repository is his right",
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
const FALLS_OVER: &[(&str, &str)] = &[];

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
///
/// **Empty, and the queue is finished.** Every route that was ever named here
/// now takes its access level as a handler argument. The list stays so that a
/// regression can be quarantined with a reason instead of the pass being
/// switched off — and because the pass checks it in both directions, an entry
/// added without cause fails the run as loudly as a missing one.
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
    //
    // The last six left with `card_d3695d1dbe1b`: issue creation, issue
    // comments and the four attachment uploads declared `RepoAuthRead` in the
    // route table while their handlers took `RepoRead` and looked the session up
    // mid-body. On a *public* repository `RepoRead` admitted the anonymous
    // caller, so the answer came from `Json` / `Multipart` — a 422 or a 415
    // where a 401 was owed. Taking `RepoAuthRead` in the signature makes the
    // declared level the one that actually runs, and it runs first.
    //
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
    /// exists — `401` or `404`, and specifically **not** `403`. Used where
    /// masking is the deliberate design, as it is for a private organization.
    ///
    /// `403` used to be accepted here, which made the whole expectation
    /// vacuous: `403` is the one denial that confirms the resource exists, so
    /// the oracle admitted exactly the answer its own name forbids. That is how
    /// the `OrgAdmin` hole survived a green sweep — `PATCH /orgs/{name}`
    /// answered `403` on a private organization and `404` on an unknown one,
    /// and the sweep called both a match (card_c46c354ec3ae).
    ///
    /// `401` stays admissible and is not a leak: a gate that authenticates
    /// before it resolves anything answers an anonymous caller the same way
    /// whether the organization exists or not.
    ///
    /// Strengthening the *status* predicate was only half of it, and the other
    /// half is why this variant is the one the sweep judges by body as well. A
    /// status is one bit of a reply; the oracle simply moved into the other
    /// half — `require_namespace_create` answered a single `403` to three
    /// refusals with two different messages under it, and no wording of an
    /// `Expect` could have reached that (card_2179245d41db). So a `Hidden` cell
    /// is asked twice: once about the fixture's private organization, once about
    /// [`ABSENT_ORG`], and the two replies have to be the same reply.
    ///
    /// The organization is not the only thing a route can mask. An *anchored*
    /// route masks a row id — the caller supplied an opaque integer and may not
    /// learn whether it hit anything — so its second question varies
    /// [`ABSENT_ID`] instead of the name. Which of the two a cell is paired on
    /// follows from the route rather than from the variant: the path either
    /// names an organization or resolves a repository out of a row.
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
///
/// `anchored` is the third axis, and it is not derivable from the other two.
/// An anchored route declares the same `RepoRead` a path-based one declares —
/// the level says which repository admits the caller, not how the gate found
/// it — so the level alone cannot tell the two shapes apart. It arrives from
/// the same census `anchored_scope_sweep_tests` reads, and it has to: judging an
/// anchored row as `Denied` accepts the `403` that row's own doctrine forbids
/// (card_41ec8f1a29a3).
fn expectation(access: Access, persona: Persona, scope: Scope, anchored: bool) -> Expect {
    use Persona::{Anonymous, Outsider, Owner};

    match access {
        // ── The anchored levels ────────────────────────────────────────────
        //
        // A route whose path names no repository resolves one out of the row
        // its id addresses, and from there every refusal owes the answer an
        // absent id gets. The caller supplied an opaque integer, so `403` — the
        // one denial that confirms the integer hit a row — is precisely what
        // `AnchoredRead` and `AnchoredWrite` refuse to send, and `Denied`
        // accepts `401` *or* `403`.
        //
        // The owner is owed `Allowed` because the row this fixture seeds lives
        // in the owner's own private repository. That is also what fixes
        // `Hidden` for the other two: neither can read that repository, so
        // nothing they are told may set the row apart from an absent id. A row
        // seeded in a repository an outsider *can* read would owe the write
        // half an unmasked `403` instead — `AnchoredWrite`'s second step — so
        // this arm is a claim about the fixture as much as about the level. The
        // fixture half is not left implicit: the sweep reads the seeded artifact
        // back as the owner before the passes start, so a row that never
        // uploaded is reported as a dead fixture rather than as six green masks.
        access if anchored && access.is_repo_scoped() => match persona {
            Owner => Expect::Allowed,
            _ => Expect::Hidden,
        },
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
        //
        // What it buys is *this* pass, and only because a persona here cannot
        // authenticate. It is not a licence to skip questions a persona could
        // ask — `foreign_id_scope_sweep_tests` puts the transport's own
        // credential behind the wheel and compares a real id against an absent
        // one, which is the question this row let through for four axes of the
        // same defect (card_6b2cadf41876).
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
    // Deliberately *not* `denied || 404`: `403` confirms the resource exists,
    // which is the one thing a masked denial may not do.
    let hidden = matches!(status.as_u16(), 401 | 404);
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

/// Create one of the fixture's repositories and return its id — the anchored
/// rows are seeded through the database, which knows a repository by id and not
/// by `{owner}/{name}`.
async fn create_repo(fx: &Fixture, name: &str, private: bool) -> i64 {
    let resp = fx
        .client
        .post(format!("{}/api/v1/repos", fx.base))
        .bearer_auth(&fx.owner_token)
        .json(&serde_json::json!({"name": name, "is_private": private}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "fixture: creating repo '{name}' failed");
    resp.json::<serde_json::Value>().await.expect("repo json")["id"]
        .as_i64()
        .expect("repo id")
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

/// The rows this fixture seeded for the anchored routes, one per
/// [`RepoAnchor`](../../src/api/repo_access.rs) implementor it knows how to
/// build. All of them live in the *private* repository — see the anchored arm of
/// [`expectation`] for why that is load-bearing rather than incidental.
struct Seeded {
    artifact: i64,
}

/// The id addressing the row this fixture seeded for one anchor.
///
/// `None` is not an oversight to paper over with `1`: an anchored route filled
/// with an id that addresses nothing is a probe whose `404` proves the row is
/// masked when it only proves the row is absent. A new anchor is either seeded
/// here or signed off in [`NO_FIXTURE`]; the sweep fails until one of the two
/// has happened. Keyed on the anchor rather than on the alias, because
/// `ArtifactRead` and `ArtifactWrite` address the same row.
fn anchored_id(target: &str, seeded: &Seeded) -> Option<i64> {
    match target {
        "Artifact" => Some(seeded.artifact),
        _ => None,
    }
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
///
/// `org` is a parameter rather than a constant because the paired probe behind
/// [`Expect::Hidden`] needs the same URL twice with one word changed: the
/// fixture's private organization, and [`ABSENT_ORG`].
fn fill(path: &str, repo: &RepoSeed, org: &str) -> String {
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
            "name" if is_org_route => org,
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

/// Fill an anchored route's path with one row id.
///
/// [`fill`] cannot do this, and not by omission: it fills every placeholder it
/// does not recognise with `1`, which is right for the locators a repository
/// gate answers *before* — a milestone, a webhook — and wrong for this one. Here
/// the id is what the gate resolves the repository from, so a guessed value
/// drives the probe at a row that does not exist and the `404` it earns reads
/// as a mask.
///
/// A path carrying anything besides the anchored id returns `None`: the second
/// placeholder is a locator this fixture would have to seed, and the caller
/// signs the route off rather than guessing — the same rule
/// `anchored_scope_sweep_tests` keeps.
fn fill_anchored(path: &str, id: &str) -> Option<String> {
    let open = path.find('{')?;
    let close = path[open..].find('}')? + open;
    if path[close + 1..].contains('{') {
        return None;
    }
    Some(format!("{}{id}{}", &path[..open], &path[close + 1..]))
}

// ── Driving one route ──────────────────────────────────────────────────────

/// One probe's answer: the status, and the body with the volatile part removed.
///
/// The body is kept whole rather than truncated because it is compared, not just
/// printed — see [`Answer::excerpt`] for the reporting half.
struct Answer {
    status: StatusCode,
    /// The reply body, `error.request_id` stripped.
    ///
    /// That field is the one part of an error envelope that differs between any
    /// two requests: `AppError::into_response` leaves it `None` and the tracing
    /// middleware stamps a fresh uuid into it further down the stack, so a raw
    /// comparison of two identical denials is permanently red. Same
    /// normalisation as `create_in_full` in `create_repo_namespace_tests`, which
    /// is where that trap cost an hour.
    ///
    /// A body that is not JSON is kept verbatim: nothing else in the tree
    /// answers a denial that way, and silently accepting one would be the sort
    /// of quiet pass-through this file exists to refuse.
    body: String,
}

impl Answer {
    /// The first 160 characters, for the report.
    fn excerpt(&self) -> String {
        self.body.chars().take(160).collect()
    }
}

fn normalized(body: &str) -> String {
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    if let Some(error) = json.get_mut("error").and_then(|e| e.as_object_mut()) {
        error.remove("request_id");
    }
    json.to_string()
}

async fn probe(
    fx: &Fixture,
    fact: &RouteFact,
    persona: Persona,
    repo: &RepoSeed,
    org: &str,
) -> Answer {
    drive(fx, fact, persona, &fill(&fact.path, repo, org)).await
}

/// [`probe`] with the path already filled — for the anchored routes, whose
/// placeholder [`fill`] cannot fill (see [`fill_anchored`]).
async fn drive(fx: &Fixture, fact: &RouteFact, persona: Persona, path: &str) -> Answer {
    let url = format!("{}{path}", fx.base);
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
    Answer {
        status,
        body: normalized(&body),
    }
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
#[allow(clippy::too_many_lines)]
async fn every_route_answers_its_declared_access_level() {
    // The database handle is here for the anchored rows and nothing else: their
    // repository is resolved out of an artifact, and an artifact needs a runner,
    // a pipeline, a stage and a job under it — a walk this harness exposes no
    // API for.
    let (base, facts, db) = spawn_test_app_with_routes_and_db().await;
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
    let private_repo_id = create_repo(&fx, PRIVATE_REPO, true).await;
    create_repo(&fx, PUBLIC_REPO, false).await;
    create_org_with_team(&fx).await;
    // One artifact in the *private* repository, which is what makes the anchored
    // rows judgeable at all: without it every anchored route answers `404` for
    // want of a row, and a wall of `404`s satisfies `Expect::Hidden` while
    // proving nothing about the gate behind it.
    let seeded = Seeded {
        artifact: seed_artifact(&fx.base, &fx.client, &db, private_repo_id, "sweep-runner").await,
    };
    // The anchored baseline, taken before the passes for the reason the
    // repository one is: a fixture whose artifact never uploaded would answer
    // the owner `404` too, and the six masked cells below would be measuring an
    // absent row rather than a working mask.
    let (status, body) = fx
        .get_as(
            Some(&fx.owner_token),
            &format!("/api/v1/artifacts/{}", seeded.artifact),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "fixture is dead: the owner cannot read the artifact this sweep seeded in their own \
         private repository ({status}) — every anchored mask below would be a 404 for want of a \
         row: {body}"
    );
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
    // The paired-probe half of `Expect::Hidden`: which cells were actually asked
    // the second question, over how many distinct routes, which could not be
    // asked it at all, and which answered the two questions differently.
    let mut paired: BTreeSet<String> = BTreeSet::new();
    let mut paired_routes: BTreeSet<String> = BTreeSet::new();
    let mut unpaired: Vec<String> = Vec::new();
    let mut oracles: Vec<String> = Vec::new();
    // The anchored axis: which cells were judged by the masked predicate rather
    // than by the one their `Access` level alone would have chosen, and which
    // anchored routes this fixture could not address at all.
    let anchors = anchored_handler_targets();
    // The other direction of the same equivalence, and the cheaper half to get
    // wrong: a route that declares a repository level while naming no repository
    // in its path *is* the anchored shape, whether or not anybody took the
    // extractor. One that has the shape and not the gate is back where the
    // artifact routes started — a prologue in the handler body, resolving the
    // row and deciding by hand — and it is invisible to both sweeps at once:
    // `anchored_scope_sweep_tests` reads its population out of the signatures,
    // and the pass below would judge it by `Expect::Denied`.
    let shaped_but_unanchored: Vec<String> = facts
        .iter()
        .filter(|fact| {
            fact.access.is_repo_scoped()
                && !is_repo_path(&fact.path)
                && !anchors.contains_key(fact.handler)
        })
        .map(|fact| format!("  {} — {}", fact.label(), fact.handler))
        .collect();
    assert!(
        shaped_but_unanchored.is_empty(),
        "{} route(s) declare a repository level and name no repository in their path, without \
         taking an anchored extractor.\nThe repository is being resolved somewhere, and where \
         that is a prologue rather than `AnchoredRead`/`AnchoredWrite` the refusal is unmasked \
         by default — a `403` confirming that the caller's opaque id hit a row. Take the \
         extractor, or give the anchor its `RepoAnchor` impl.\n{}",
        shaped_but_unanchored.len(),
        shaped_but_unanchored.join("\n"),
    );
    let mut anchored_cells: BTreeSet<String> = BTreeSet::new();
    let mut anchored_gaps: BTreeSet<String> = BTreeSet::new();

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

            let anchor = anchors.get(fact.handler);
            // The anchored arm of `expectation` only fires on a repository
            // level, and silence is the wrong answer to a row that has an
            // anchored gate and declares something else: it would fall through
            // to a generic arm — `Access::Public` is owed `Expect::Allowed`, and
            // every non-denial satisfies that — with nothing to say it had.
            //
            // Nor is it covered next door. `route_gate_rank_guard` compares a
            // declaration against the extractor in the signature, but through
            // two hand-closed tables: `DECLARED` for the five repository rungs
            // and `NON_REPO` for `User` / `OrgRead` / `OrgAdmin` /
            // `InstanceAdmin`. `Public`, `PublicFiltered` and `Foreign` are in
            // neither, so a row declaring one of those is skipped by both of its
            // passes — and an anchored handler under such a declaration would be
            // gated at runtime and unjudged by every sweep that reads the table.
            // The one anchored level `NON_REPO` does reach is `User`, whose
            // accepted set names no anchored alias; that row fails there.
            if anchor.is_some() && !fact.access.is_repo_scoped() {
                anchored_gaps.insert(format!(
                    "  {}\n      declared {:?} while its handler takes an anchored extractor. \
                     An anchored gate answers a masked refusal; a level that is not about a \
                     repository is judged by a predicate that knows nothing about masking, so \
                     the row would pass on the `403` the anchor exists to never send",
                    fact.label(),
                    fact.access,
                ));
                continue;
            }

            for &scope in scopes {
                let expect = expectation(fact.access, persona, scope, anchor.is_some());
                if expect == Expect::Unchecked {
                    continue;
                }
                let repo = match scope {
                    Scope::PublicRepo => public_seed,
                    _ => private_seed,
                };
                let label = fact.label();
                let key = cell(persona, scope, fact);

                // Where the two questions this cell may be asked differ. A
                // path-based row varies the organization name; an anchored row
                // has no name in its path to vary, so it varies the id its
                // repository is resolved from.
                let (url, twin) = match anchor {
                    None => (
                        fill(&fact.path, repo, ORG),
                        Some(fill(&fact.path, repo, ABSENT_ORG)),
                    ),
                    Some(target) => {
                        let Some(id) = anchored_id(target, &seeded) else {
                            anchored_gaps.insert(format!(
                                "  {label}\n      gated through the `{target}` anchor, and this \
                                 fixture seeds no row of it. Give `anchored_id` a line, or sign \
                                 the route off in NO_FIXTURE — filling the id by guesswork turns \
                                 the 404 of an absent row into a passing mask",
                            ));
                            continue;
                        };
                        match fill_anchored(&fact.path, &id.to_string()) {
                            Some(url) => (url, fill_anchored(&fact.path, ABSENT_ID)),
                            None => {
                                anchored_gaps.insert(format!(
                                    "  {label}\n      gated through the `{target}` anchor, and \
                                     its path carries something besides the anchored id. Seed \
                                     that locator, or sign the route off in NO_FIXTURE",
                                ));
                                continue;
                            }
                        }
                    }
                };
                let answer = drive(&fx, fact, persona, &url).await;

                // The body half of a masked denial. `judge` reads one bit of the
                // reply, and a mask is a claim about the whole of it, so the
                // same request goes out a second time against the twin above —
                // a name nothing created, or an id nothing created: if the two
                // replies differ in any way a caller can see, the difference
                // *is* the existence oracle the `404` was there to close.
                if expect == Expect::Hidden {
                    if anchor.is_some() {
                        anchored_cells.insert(key.clone());
                    }
                    match twin.filter(|twin| *twin != url) {
                        None => unpaired.push(format!(
                            "  {key}\n      declared {:?}, and there is nothing in its path to \
                             vary — no second name and no second id to compare the reply \
                             against, so the mask is still judged by status alone",
                            fact.access,
                        )),
                        Some(twin) => {
                            let absent = drive(&fx, fact, persona, &twin).await;
                            paired.insert(key.clone());
                            paired_routes.insert(label.clone());
                            if (answer.status, &answer.body) != (absent.status, &absent.body) {
                                oracles.push(format!(
                                    "  {key}\n      exists → {} {}\n\
                                     \x20     absent ({twin}) → {} {}",
                                    answer.status,
                                    answer.excerpt(),
                                    absent.status,
                                    absent.excerpt(),
                                ));
                            }
                        }
                    }
                }

                let outcome = judge(expect, answer.status);
                if outcome == Outcome::Match {
                    continue;
                }

                let status = answer.status;
                let body = answer.excerpt();
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

    // Nothing is waved through: a `Hidden` cell the pair cannot be pointed at is
    // a cell whose mask is still taken on the strength of one status code, and
    // it has to say so out loud rather than pass quietly.
    assert!(
        unpaired.is_empty(),
        "{} `Expect::Hidden` cell(s) could not be paired against a second name.\nGive the pass \
         a way to ask the same route about something that does not exist, or take the \
         `Expect::Hidden` off the level — an unpaired mask is the status-only oracle this pass \
         stopped accepting.\n{}",
        unpaired.len(),
        unpaired.join("\n"),
    );
    // And the pair must actually have run. Relabel the organization routes and
    // the whole body half would go quiet without a single assertion firing —
    // which is exactly how the status half went vacuous before it
    // (card_c46c354ec3ae).
    assert!(
        paired.len() >= 20 && paired_routes.len() >= 10,
        "only {} `Expect::Hidden` cell(s) across {} route(s) were asked the paired question — \
         the table, not the gate, is what changed, and the body half of every masking claim is \
         now proving nothing",
        paired.len(),
        paired_routes.len(),
    );
    assert!(
        oracles.is_empty(),
        "{} of {} masked denial(s), over {} route(s), can be told apart from the same request \
         against something that does not exist.\nThe status matches and the reply does \
         not, so the mask is decoration: a caller reads existence off the difference. Both \
         branches owe one reply — `resolve_org` and `require_org_visible` both answer \
         `organization not found` for precisely this reason, and `RepoAnchor::masked` is the \
         one answer an anchored route gives an absent row and a refused one alike.\n{}",
        oracles.len(),
        paired.len(),
        paired_routes.len(),
        oracles.join("\n"),
    );

    // The anchored axis, held to the same two rules as the rest: nothing is
    // waved through, and the axis has to have been exercised.
    assert!(
        anchored_gaps.is_empty(),
        "{} anchored route(s) could not be addressed by this fixture.\nAn anchored route is \
         judged by `Expect::Hidden` — a `403` from it confirms the caller's opaque id hit a row \
         — and that verdict is only worth the row it was measured against.\n{}",
        anchored_gaps.len(),
        anchored_gaps.iter().cloned().collect::<Vec<_>>().join("\n"),
    );
    // Rename the artifact routes, or let the anchored extractors fall out of the
    // signatures the census reads, and every anchored row would quietly go back
    // to being judged by `Expect::Denied` — the predicate that accepts the one
    // answer its own doctrine forbids (card_41ec8f1a29a3). This is what says so.
    assert!(
        anchored_cells.len() >= 4,
        "only {} anchored cell(s) were judged as masked. The anchored census comes from the \
         handlers' signatures, so a rename that hid it would leave every anchored row judged by \
         `Expect::Denied`, which accepts the `403` that confirms a private row exists",
        anchored_cells.len(),
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
    let anchors = anchored_handler_targets();

    for fact in &facts {
        let label = fact.label();
        if listed(NO_FIXTURE, &label) {
            continue;
        }
        // An anonymous caller is owed the same thing whatever the scope;
        // `PrivateRepo` is simply the one where that is also true of the
        // repository levels.
        let expect = expectation(
            fact.access,
            Persona::Anonymous,
            Scope::PrivateRepo,
            anchors.contains_key(fact.handler),
        );
        if !matches!(expect, Expect::Denied | Expect::Hidden) {
            continue;
        }

        let url = format!("{base}{}{HOSTILE_QUERY}", fill(&fact.path, &seed, ORG));
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

/// On what grounds a file is allowed to call `Router::route` directly.
///
/// The reason beside each entry is prose, and prose is not checked. The variant
/// is the part [`every_route_call_sign_off_still_holds`] can read back: it says
/// *which* claim the entry is making, so the claim can be measured against the
/// file instead of trusted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RouteCallSignOff {
    /// The table itself. `.route` here *is* the implementation of the thing
    /// every other module has to go through, so it is production code by
    /// definition and there is nothing further to check.
    TheTable,
    /// Test scaffolding: the calls exist only inside the file's `#[cfg(test)]`
    /// module, so no route the server serves can come from them. Checkable, and
    /// checked — a call that moves out of that module fails the guard.
    TestScaffold,
}

/// Files allowed to call `Router::route` directly, with the grounds and reason.
///
/// Read back by [`every_route_call_sign_off_still_holds`]: the file has to still
/// exist, still carry a route call the entry buys something for, and — for a
/// [`RouteCallSignOff::TestScaffold`] — keep every one of those calls inside its
/// `#[cfg(test)]` module. Without that this was the one exemption list in the
/// authz guards nothing re-read, so a production `.route(...)` added to a signed
/// file would have been exempt for good (card_543ed2598323).
const ROUTE_CALL_SIGNED_OFF: &[(&str, RouteCallSignOff, &str)] = &[
    (
        "route_table.rs",
        RouteCallSignOff::TheTable,
        "the table itself: the one place a route is registered, and it takes an `Access`",
    ),
    (
        "security.rs",
        RouteCallSignOff::TestScaffold,
        "a two-route scaffold inside `#[cfg(test)]`, to drive the header middleware",
    ),
    (
        "rate_limit.rs",
        RouteCallSignOff::TestScaffold,
        "a one-route scaffold inside `#[cfg(test)]`, to drive the limiter middleware",
    ),
];

/// `crates/rg-http/src`, the tree both route-call guards walk.
fn src_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Lines of `text` that mount something on a bare `axum::Router`, as
/// `(1-based line, trimmed line)`.
///
/// Shared by the guard and its reverse check so the two cannot disagree about
/// what counts as registering a route — a list of names that one caller knows
/// and another does not is the defect this phase keeps finding.
///
/// `nest_service` is in here with the two `route*` forms because it mounts a
/// whole tower `Service` at a path, and a `Service` is not built by `.route`
/// calls this guard could catch further down. `fallback_service` is *not*: it
/// claims no path of its own, it answers the ones nothing claimed, and what it
/// answers with is a separate question (card_dd8497e4fd58).
///
/// Comment lines are skipped — `RouteTable`'s own doc comment quotes the chained
/// `.route(path, post(h).layer(l))` form it replaced.
fn route_mount_lines(text: &str) -> Vec<(usize, &str)> {
    text.lines()
        .enumerate()
        .filter_map(|(n, line)| {
            let code = line.trim_start();
            if code.starts_with("//") {
                return None;
            }
            let mounts = code.contains(".route(")
                || code.contains(".route_service(")
                || code.contains(".nest_service(");
            mounts.then_some((n + 1, code))
        })
        .collect()
}

/// The 1-based line the file's `#[cfg(test)] mod …` block starts on.
///
/// Anchored on the `#[cfg(test)]` + `mod` *pair* rather than on the attribute
/// alone, the same way `authz_extractor_guard::predicate_home_offenders` finds
/// this boundary. `rate_limit.rs` is why: it carries three `#[cfg(test)]`
/// helpers a hundred lines above its test module, and anchoring on the attribute
/// would put the boundary at the first of them and wave the rest of the file
/// through.
fn test_module_start(text: &str) -> Option<usize> {
    let lines: Vec<&str> = text.lines().collect();
    lines.iter().enumerate().find_map(|(n, line)| {
        let is_pair = line.trim_start() == "#[cfg(test)]"
            && lines[n + 1..]
                .iter()
                .take(2)
                .any(|next| next.trim_start().starts_with("mod "));
        is_pair.then_some(n + 1)
    })
}

/// Every sign-off still describes the file it names.
///
/// The list is the widest exemption in this file — an entry turns the guard off
/// for a whole source file — and it was the only one nothing read back. Not even
/// "the file exists": a rename left the entry behind as a standing indulgence for
/// whatever took that name next, and the `#[cfg(test)]` claim two of the three
/// entries make was never compared to where the calls actually are.
///
/// So each entry is measured here:
///
/// - the file exists, and the reason is not empty — as
///   `authz_extractor_guard::signed_off_exceptions_are_live` does for its list;
/// - the file still mounts *something* on a bare `Router`, or the entry buys
///   nothing and should go;
/// - a [`RouteCallSignOff::TestScaffold`] keeps every one of those mounts inside
///   its `#[cfg(test)]` module. This is the half that matters: a served
///   `.route(...)` added to `security.rs` or `rate_limit.rs` never reaches a
///   `RouteFact`, so the persona sweep, `foreign_gate_guard` and
///   `route_gate_rank_guard` are all blind to it at once.
#[test]
fn every_route_call_sign_off_still_holds() {
    let src = src_root();
    let mut offenders = Vec::new();

    for (rel, grounds, reason) in ROUTE_CALL_SIGNED_OFF {
        let path = src.join(rel);
        assert!(
            path.exists(),
            "ROUTE_CALL_SIGNED_OFF names {rel} ({reason}) but that file is gone — drop the \
             entry rather than leaving it to exempt whatever takes that name next"
        );
        assert!(
            !reason.trim().is_empty(),
            "{rel} is signed off without a reason"
        );

        let text = std::fs::read_to_string(&path).expect("read signed-off source file");
        let mounts = route_mount_lines(&text);
        assert!(
            !mounts.is_empty(),
            "ROUTE_CALL_SIGNED_OFF exempts {rel} ({reason}) and it registers no route at all \
             any more — the entry buys nothing, drop it"
        );

        if *grounds != RouteCallSignOff::TestScaffold {
            continue;
        }

        let boundary = test_module_start(&text).unwrap_or_else(|| {
            panic!(
                "{rel} is signed off as test scaffolding ({reason}) and has no \
                 `#[cfg(test)] mod` at all — the claim cannot be true, so either the file \
                 changed shape or the grounds are wrong"
            )
        });
        for (line, code) in mounts {
            if line < boundary {
                offenders.push(format!(
                    "  {rel}:{line} — {code}\n      (the `#[cfg(test)] mod` begins at \
                     {rel}:{boundary})"
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a file signed off as test scaffolding registers a route outside its `#[cfg(test)]` \
         module, which means the route is served and nothing declares who may call it. The \
         sign-off does not cover it: register it through `RouteTable` (`crate::route_table`), \
         which cannot take a route without an `Access` level, or move the call into the test \
         module the sign-off describes.\n{}",
        offenders.join("\n")
    );
}

/// A route may only be born through [`RouteTable`], which cannot register one
/// without an access level.
///
/// `RouteTable` has no `route()`, so the compiler already stops the ordinary
/// mistake — but nothing stops somebody reaching past it for a bare
/// `Router::new().route(...)` and merging that in. This guard closes the gap:
/// it is a grep because the failure it guards against is a line of code that
/// was *not* written, and no request can exercise that.
///
/// What counts as registering a route is [`route_mount_lines`], shared with
/// [`every_route_call_sign_off_still_holds`] — which is the other half of this
/// guard, and reads its exemption list back.
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

    let src = src_root();
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
            .any(|(allowed, _, _)| rel == *allowed)
        {
            continue;
        }
        let text = std::fs::read_to_string(file).expect("read source file");
        for (line, code) in route_mount_lines(&text) {
            offenders.push(format!("  {rel}:{line} — {code}"));
        }
    }

    assert!(
        offenders.is_empty(),
        "a route was registered straight on an `axum::Router`, so nothing declares who may \
         call it. Register it through `RouteTable` (`crate::route_table`) instead — its \
         `get`/`post`/… all take an `Access` level, which is what the route-access sweep \
         walks. If this really is test scaffolding rather than a served route, add the file \
         to ROUTE_CALL_SIGNED_OFF with `RouteCallSignOff::TestScaffold` and the reason — and \
         keep the calls inside the `#[cfg(test)]` module, which is the part the sign-off \
         promises and `every_route_call_sign_off_still_holds` measures.\n{}",
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

    // Every `Foreign` sign-off must say something. What it *claims* — a named
    // middleware, or the module that holds the gate — is checked against the
    // route it sits on by `foreign_gate_guard`.
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
        let url = format!("{}{}", fx.base, fill(&fact.path, &seed, ORG));
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
        fill(&fact.path, &seed, ORG)
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

/// No `NO_FIXTURE` route lets anybody in.
///
/// An entry on that list is skipped by every persona pass, which makes it the
/// widest exemption the sweep grants — and it was the only one of the three
/// lists nothing re-read: `FALLS_OVER` and `EXTRACTOR_BEFORE_GATE` both fail on
/// a stale entry, this one only checked that the name still matched a live
/// route (card_3cc941a766a0).
///
/// What can rot here is the precondition: the fixture grows, the route starts
/// resolving something real, and a row that nobody judges starts answering
/// `2xx`. So every entry is driven — three personas, both repository scopes —
/// and a success is the failure: the route has become checkable and owes the
/// sweep an answer instead of an excuse.
///
/// It deliberately does *not* demand a particular non-answer. The rows measure
/// differently by scope — the `pulls/.../assets` pair answers `401`/`403` on the
/// private repository and `404` on the public one — and pinning a single status
/// would only encode the fixture's shape of the day.
#[tokio::test]
async fn the_out_of_reach_routes_are_still_out_of_reach() {
    let (base, facts) = spawn_test_app_with_routes().await;
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
    // The repositories have to be real, or "out of reach" would be true for the
    // wrong reason and this test would pass on a dead fixture.
    for repo in [PRIVATE_REPO, PUBLIC_REPO] {
        assert_eq!(
            fx.repo_readable_by_owner(repo).await,
            StatusCode::OK,
            "fixture is dead: the owner cannot read '{repo}', so every route below would look \
             out of reach whatever it does"
        );
    }
    let seeds = [PRIVATE_REPO, PUBLIC_REPO].map(|repo| RepoSeed {
        name: repo.to_string(),
        // No comment is seeded: these rows resolve an artifact, a pipeline or a
        // pull request first, and none of them gets as far as a comment.
        comment_id: "1".to_string(),
    });

    let mut offenders: Vec<String> = Vec::new();
    let mut probed = 0usize;
    for (label, reason) in NO_FIXTURE {
        let fact = facts
            .iter()
            .find(|fact| fact.label() == *label)
            .unwrap_or_else(|| panic!("NO_FIXTURE names '{label}', which is not a route"));

        for seed in &seeds {
            for persona in [Persona::Anonymous, Persona::Outsider, Persona::Owner] {
                let answer = probe(&fx, fact, persona, seed, ORG).await;
                probed += 1;
                if !answer.status.is_success() {
                    continue;
                }
                offenders.push(format!(
                    "  {} {} {label} answered {} (excused as: {reason})\n    {}",
                    persona.label(),
                    seed.name,
                    answer.status,
                    answer.excerpt(),
                ));
            }
            // Only the repository-scoped rows have anything to say about a
            // second repository; the rest resolve an instance-wide id.
            if !is_repo_path(&fact.path) {
                break;
            }
        }
    }

    assert!(
        probed > 0,
        "NO_FIXTURE is empty — the guard is not guarding"
    );
    assert!(
        offenders.is_empty(),
        "{} NO_FIXTURE row(s) are no longer out of reach.\nThey are skipped by every persona \
         pass, so a route that has started letting somebody in is a route nobody is checking. \
         Drop it from NO_FIXTURE and let the sweep judge it.\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}
