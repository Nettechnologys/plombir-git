//! Source guard: the level a route *declares* is at least the level its handler
//! *takes*.
//!
//! `route_table::Access` is the contract — "this is what a caller needs" — and
//! the extractor in the handler's signature is the code that enforces it.
//! Nothing held the two together. A route could declare `RepoWrite` and take
//! `RepoRead`, and the only pass that would notice is
//! `route_access_sweep_tests`, which notices it *indirectly*: the weaker gate
//! admits a caller the declared level owes a denial, the body extractor behind
//! it runs, and that caller is told its JSON is malformed (`400`/`415`/`422`)
//! instead of being turned away.
//!
//! That oracle only exists when the request **has a body**. `DELETE` and `GET`
//! carry none, no deserializer stands between the gate and the handler, and a
//! weak extractor produces no symptom at all — the handler's own first line
//! looks the caller up by hand and answers correctly, by the author's care
//! rather than by the gate. Six such handlers were found by reading
//! (`card_d3695d1dbe1b`), four more after them; not one could have been found by
//! driving the server.
//!
//! So this is a static pass over the two places the promise is written down: the
//! `(Access, path, handler)` rows of `routes.rs`, and the parameter list of each
//! handler they name. It is a grep for the same reason
//! [`crate::authz_extractor_guard`] is — the defect is a line of code that was
//! *not* written, and no request can exercise that.
//!
//! # What it does not claim
//!
//! Only the *declared ≥ taken* direction is a hole. A handler whose extractor is
//! stronger than its row admits fewer callers than advertised, and the persona
//! sweep does fail that: the owner is owed `Expect::Allowed` and gets a denial.
//! And a rung is not the whole rule — a handler may legitimately widen access
//! for one specific person, which is what every entry in [`SIGNED_OFF`] is.
//!
//! The scope is the whole table, in two passes, because the comparison differs
//! by family. The repository rungs form a scale and are compared as one:
//! [`no_handler_takes_a_weaker_gate_than_its_route_declares`]. `Access::User`,
//! `OrgRead`, `OrgAdmin` and `InstanceAdmin` lie on no shared scale — `OrgRead`
//! admits an anonymous caller to a public organization, `User` proves a session
//! and nothing about any organization — so each names the *set* of extractors
//! that answers it in [`NON_REPO`], checked by
//! [`no_non_repository_row_leaves_its_gate_to_the_handler_body`].
//!
//! That second pass is `card_34fd642a7538`, and it was worth writing rather than
//! recording as out of scope: 18 rows declared a non-repository level while
//! taking nothing but `HeaderMap`, and each of them answered correctly only
//! because its author had not forgotten the prologue. `api::orgs::update_org`
//! was the sharp edge — `OrgAdmin` in the table, `Json<UpdateOrgRequest>` in the
//! signature, and the gate three statements into a body the deserializer reached
//! first.
//!
//! Both passes leave alone `Public` and `PublicFiltered`, which promise nothing a
//! signature could be held to, and `Foreign`, which is
//! `foreign_gate_guard`'s. That those three are the *only* levels neither pass
//! compares is itself asserted, so a new `Access` variant cannot quietly become a
//! family both of them skip — which is exactly what these 18 rows were.

use std::collections::BTreeMap;
use std::fs;

use crate::common::source_scan::{
    anchored_aliases, call_args_from_code, leading_ident, param_base_types,
    production_rust_code_only, relative, rust_files, signature_params, src_root, AnchorKind,
};

/// The repository access levels, weakest first.
///
/// Both sides of the comparison map onto this: `Access::RepoWrite` in the route
/// table and `RepoWrite` in a signature are the same rung. That is the whole
/// point — the enum and the extractor family were written to mirror each other,
/// and nothing checked that a given route used the same rung twice.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Rank {
    /// `RepoRead` — a public repository is readable anonymously.
    Read,
    /// `RepoAuthRead` — readable, but a session is required even so.
    AuthRead,
    /// `RepoWrite`.
    Write,
    /// `RepoAdmin`.
    Admin,
    /// `RepoOwner` — disposing of the repository itself.
    Owner,
}

/// The `Access` variants this guard compares, and the rung each one is.
///
/// Every other variant (`Public`, `User`, `OrgAdmin`, `Foreign`, …) is out of
/// scope: it says nothing about *this* repository, so there is no extractor rung
/// to hold it to.
const DECLARED: &[(&str, Rank)] = &[
    ("RepoRead", Rank::Read),
    ("RepoAuthRead", Rank::AuthRead),
    ("RepoWrite", Rank::Write),
    ("RepoAdmin", Rank::Admin),
    ("RepoOwner", Rank::Owner),
];

/// The extractor types a handler can take, and the rung each one carries.
///
/// `CiRead<_>` is [`Rank::Read`]: it is `RepoRead` plus a second key — a CI job
/// token scoped to this repository — so it admits everyone `RepoRead` admits.
/// The anchored pair is deliberately absent; its aliases are read out of the
/// tree by [`anchored_ranks`], because each anchor declares its own.
const TAKEN: &[(&str, Rank)] = &[
    ("RepoRead", Rank::Read),
    ("CiRead", Rank::Read),
    ("RepoAuthRead", Rank::AuthRead),
    ("RepoWrite", Rank::Write),
    ("RepoAdmin", Rank::Admin),
    ("RepoOwner", Rank::Owner),
];

/// The levels that are *not* about a repository, and every extractor type that
/// proves one.
///
/// A second table rather than more rungs on [`Rank`], because these levels do
/// not lie on that scale and do not lie on a shared one of their own.
/// `Access::OrgRead` admits an anonymous caller to a *public* organization,
/// exactly as `RepoRead` does for a public repository — so it is not "`User`
/// plus an organization", and a handler behind it may hold no session at all.
/// `Access::User` is the opposite shape: a session, and nothing about any
/// organization. Ordering the two would invent a relation the code does not
/// have. What each level does have is a *set* of extractors that answer it, and
/// that is what this states.
///
/// The sets are closed by hand, and every entry is a claim about the extractor's
/// own body rather than about its name. `OrgAdmin` requires a session and
/// resolves the organization before it answers, so it proves `User` and
/// `OrgRead` as well as itself. `RepoAuthRead` and every rung above it require a
/// session, so each proves `User`. `RepoRead` and `CiRead` do not — a public
/// repository is anonymously readable — and are deliberately absent, as is
/// `OrgRead` from the `User` row for the same reason.
///
/// `NamespaceCreate<_>` / `NamespaceWrite<_>` are on the `User` row because each
/// resolves an `actor_id` before it answers: `POST /repos` and `POST /imports`
/// declare `User` and are gated by one of them rather than by a prologue. They
/// are also why this is a set membership and not a name comparison — a level can
/// be proven by more than the extractor that shares its name.
/// `SessionUser` also proves `User` and further refuses delegated PATs on
/// routes that issue new credentials; `SudoUser` is `SessionUser` plus a
/// recent password re-proof (`POST /users/me/sudo`) on the routes that mint
/// credentials outliving the session.
const NON_REPO: &[(&str, &[&str])] = &[
    (
        "User",
        &[
            "AuthUser",
            "SessionUser",
            "SudoUser",
            "OrgAdmin",
            "InstanceAdmin",
            "RepoAuthRead",
            "RepoWrite",
            "RepoAdmin",
            "RepoOwner",
            "NamespaceCreate",
            "NamespaceWrite",
        ],
    ),
    ("OrgRead", &["OrgRead", "OrgAdmin"]),
    ("OrgAdmin", &["OrgAdmin"]),
    ("InstanceAdmin", &["InstanceAdmin"]),
];

/// Handlers that deliberately take a weaker extractor than their row declares:
/// `(handler path, declared level, the floor its signature must still hold, why)`.
///
/// The floor is what keeps this list from being the hole it is meant to
/// document. An exemption spelled "this handler may be weaker than its row"
/// exempts it from *everything* — drop `RepoAuthRead` to `RepoRead` on a signed
/// off handler and nothing here would say a word, which is precisely the class
/// this file exists to catch. Naming the rung instead narrows the sign-off to
/// the one step that was actually argued for: below it the handler is an
/// offender again, like any other.
///
/// Checked in both directions, the way `EXTRACTOR_BEFORE_GATE` is in the access
/// sweep — an entry that stops describing a real widening fails the run, so the
/// list cannot rot into a blanket allowance for a handler somebody later
/// weakened for an entirely different reason.
///
/// All fifteen name a row-specific exception to the ordinary `RepoWrite`
/// contract. The row declares what an *arbitrary* caller needs — that is what a
/// contract states, and what the persona sweep measures a stranger against.
/// The handler takes `RepoAuthRead`, the floor for everybody, then resolves the
/// remaining rule through `repo_access`: an author widening or write access to
/// the PR source repository. The decision remains in the gate module, not in a
/// second copy of the rule, and no weaker caller gets in.
const SIGNED_OFF: &[(&str, &str, Rank, &str)] = &[
    (
        "api::issues::update_issue",
        "RepoWrite",
        Rank::AuthRead,
        "an issue's own author may edit the title and body of their own issue without write \
         access; a caller who is not the author, or who touches labels / assignee / milestone, \
         is held to `may_write`",
    ),
    (
        "api::attachments::create_issue_attachment",
        "RepoWrite",
        Rank::AuthRead,
        "an issue's own author may upload an attachment; anybody else is held to \
         `may_write` in `attachments::create`",
    ),
    (
        "api::attachments::create_issue_comment_attachment",
        "RepoWrite",
        Rank::AuthRead,
        "an issue comment's own author may upload an attachment; anybody else is held to \
         `may_write` in `attachments::create`",
    ),
    (
        "api::attachments::create_pull_request_attachment",
        "RepoWrite",
        Rank::AuthRead,
        "a pull request's own author may upload an attachment; anybody else is held to \
         `may_write` in `attachments::create`",
    ),
    (
        "api::attachments::create_review_comment_attachment",
        "RepoWrite",
        Rank::AuthRead,
        "a review comment's own author may upload an attachment; anybody else is held to \
         `may_write` in `attachments::create`",
    ),
    (
        "api::attachments::delete_issue_attachment",
        "RepoWrite",
        Rank::AuthRead,
        "whoever uploaded the attachment may delete it; anybody else is held to `may_write` in \
         `attachments::delete` (card_d3695d1dbe1b)",
    ),
    (
        "api::attachments::delete_issue_comment_attachment",
        "RepoWrite",
        Rank::AuthRead,
        "same widening as `delete_issue_attachment` — the four share `attachments::delete`",
    ),
    (
        "api::attachments::delete_pull_request_attachment",
        "RepoWrite",
        Rank::AuthRead,
        "same widening as `delete_issue_attachment` — the four share `attachments::delete`",
    ),
    (
        "api::attachments::delete_review_comment_attachment",
        "RepoWrite",
        Rank::AuthRead,
        "same widening as `delete_issue_attachment` — the four share `attachments::delete`",
    ),
    (
        "api::pulls::update_pr",
        "RepoWrite",
        Rank::AuthRead,
        "the PR author may edit their own PR; anybody else is held to `may_write`",
    ),
    (
        "api::reviews::remove_requested_reviewer",
        "RepoWrite",
        Rank::AuthRead,
        "the PR author may manage its reviewers; anybody else is held to `may_write` by \
         `require_pr_manager`",
    ),
    (
        "api::reviews::apply_review_suggestion",
        "RepoWrite",
        Rank::AuthRead,
        "applying a suggestion mutates the PR head, so `require_suggestion_source` holds the \
         caller to write access on that source repository",
    ),
    (
        "api::reviews::set_thread_resolution",
        "RepoWrite",
        Rank::AuthRead,
        "the thread author or PR author may resolve a thread; any other caller is held to \
         `may_write`",
    ),
    (
        "api::reviews::apply_review_suggestions",
        "RepoWrite",
        Rank::AuthRead,
        "applying suggestions mutates the PR head, so `require_suggestion_source` holds the \
         caller to write access on that source repository, which can differ from the URL repository",
    ),
    (
        "api::issues::edit_comment",
        "RepoAdmin",
        Rank::AuthRead,
        "a comment's own author may edit it; anybody else is held to repository administration \
         by `issue::moderation` through `repo_access::administers` (card_60961272e1ba)",
    ),
    (
        "api::issues::delete_comment",
        "RepoAdmin",
        Rank::AuthRead,
        "same widening as `edit_comment` — a comment is its author's or an administrator's",
    ),
    (
        "api::reviews::edit_review_comment",
        "RepoAdmin",
        Rank::AuthRead,
        "same widening as `issues::edit_comment`, for a review comment",
    ),
    (
        "api::reviews::delete_review_comment",
        "RepoAdmin",
        Rank::AuthRead,
        "same widening as `issues::delete_comment`, for a review comment",
    ),
    (
        "api::reviews::request_reviewer",
        "RepoWrite",
        Rank::AuthRead,
        "the PR author may manage its reviewers; any other caller is held to `may_write` by \
         `require_pr_manager`",
    ),
];

/// Lower bound on the rows a healthy parse yields; the table holds ~330.
///
/// A parser that silently understands nothing turns this guard into a green
/// no-op, which is worse than a red one — nobody investigates a passing test.
const MIN_ROWS: usize = 300;

fn read(rel: &str) -> String {
    let path = src_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// ── Reading the route table ────────────────────────────────────────────────

/// One `(access, handler)` registration parsed out of `routes.rs`.
///
/// The path is recorded when it is a literal and left `None` when it is not: the
/// Maven and Cargo-sparse families register one row per depth in a
/// `for path in CONST_ARRAY` loop, so their URL is an identifier at the call
/// site. That is a blind spot for a check keyed on URLs and none at all for this
/// one — the pair being compared is the level and the handler, and both are
/// spelled out in every registration.
struct Row {
    line: usize,
    method: &'static str,
    path: Option<String>,
    access: String,
    handler: String,
}

impl Row {
    /// `PATCH /repos/{owner}/{name}/issues/{number}`, or the handler when the
    /// row's URL is not a literal.
    fn label(&self) -> String {
        match &self.path {
            Some(path) => format!("{} {path}", self.method),
            None => format!("{} <generated> → {}", self.method, self.handler),
        }
    }
}

/// What one pass over `routes.rs` found.
struct Parsed {
    rows: Vec<Row>,
    /// Call sites that look like a registration — three or more arguments, an
    /// access level first — whose handler argument this parser cannot read. Not
    /// skipped quietly: a row the parser drops is a row this guard reports as
    /// fine without ever having looked at it.
    unreadable: Vec<String>,
}

/// Whether `text` is a path expression naming a free function —
/// `api::issues::update_issue`. This is what separates a route registration from
/// any other three-argument `.get(…)` in the file.
fn is_handler_path(text: &str) -> bool {
    let path = text.strip_prefix("crate::").unwrap_or(text);
    let segments: Vec<&str> = path.split("::").collect();
    segments.len() >= 2
        && segments.iter().all(|segment| {
            segment
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                && segment.chars().all(|c| c.is_alphanumeric() || c == '_')
        })
}

/// `const GIT_HTTP: Access = Foreign(Handler { … });` ⇒ `GIT_HTTP` → `Foreign`.
///
/// The `Foreign` sign-offs are spelled once at the top of `routes.rs` and used
/// by name at the call sites, so the first argument of a registration is not
/// always an `Access` variant. Resolving them here is what lets
/// [`the_route_table_parser_reads_every_registration`] insist that every
/// registration names a level this guard recognises.
fn access_constants(src: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in src.lines() {
        let Some((name, value)) = line
            .trim_start()
            .strip_prefix("const ")
            .and_then(|rest| rest.split_once(':'))
        else {
            continue;
        };
        let Some(value) = value
            .trim()
            .strip_prefix("Access")
            .and_then(|rest| rest.trim().strip_prefix('='))
        else {
            continue;
        };
        out.insert(
            name.trim().to_string(),
            leading_ident(value.trim()).to_string(),
        );
    }
    out
}

/// Every route registration in `routes.rs`.
///
/// A registration is a `RouteTable` builder call — `get` / `head` / `post` /
/// `put` / `patch` / `delete`, with or without the `_with` layer argument —
/// whose third argument is a handler path. The access level is the first
/// argument, resolved through the `Foreign` constants.
///
/// Registrations written inside a `#[cfg(test)]` item are not rows of the
/// served table and are not read: a fixture mounting `RepoWrite` on a stub
/// handler would be compared against a signature nothing serves, and — the
/// worse half — would count towards `MIN_ROWS` and towards
/// "no route declares `{name}` any more", both of which are floors that exist
/// to notice the real table shrinking. `route_access_sweep_tests` reached the
/// same conclusion the hard way, from a test-only `Router::new().route(…)` in
/// `api/packages.rs` (card_5b5f4d203378).
fn parse_routes() -> Parsed {
    parse_routes_from(&read("routes.rs"))
}

fn parse_routes_from(src: &str) -> Parsed {
    let code = production_rust_code_only(src);
    let constants = access_constants(&code);
    let mut rows = Vec::new();
    let mut unreadable = Vec::new();

    for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"] {
        for suffix in ["(", "_with("] {
            let needle = format!(".{}{suffix}", method.to_lowercase());
            let mut from = 0;
            while let Some(found) = code[from..].find(&needle) {
                let at = from + found;
                let open = at + needle.len() - 1;
                from = open + 1;
                let Some(args) = call_args_from_code(src, &code, open) else {
                    continue;
                };
                if args.len() < 3 {
                    continue;
                }
                let raw = leading_ident(args[0].trim());
                let handler: String = args[2].split_whitespace().collect();
                let line = code[..at].lines().count();
                if !is_handler_path(&handler) {
                    // An access level in front of an argument this cannot read
                    // is a registration in a spelling the parser has not been
                    // taught. Report it rather than drop it.
                    if raw.starts_with(char::is_uppercase) {
                        unreadable.push(format!(
                            "  routes.rs:{line} — .{}{suffix}{raw}, …, {}",
                            method.to_lowercase(),
                            args[2]
                        ));
                    }
                    continue;
                }
                rows.push(Row {
                    line,
                    method,
                    path: args[1]
                        .strip_prefix('"')
                        .and_then(|p| p.strip_suffix('"'))
                        .map(str::to_string),
                    access: constants
                        .get(raw)
                        .cloned()
                        .unwrap_or_else(|| raw.to_string()),
                    handler: handler
                        .strip_prefix("crate::")
                        .unwrap_or(&handler)
                        .to_string(),
                });
            }
        }
    }
    rows.sort_by_key(|row| row.line);
    Parsed { rows, unreadable }
}

#[test]
fn route_parser_keeps_a_delimiter_shaped_raw_argument() {
    const SOURCE: &str = r####"
fn routes() {
    RouteTable::new("/api/v1").get_with(
        RepoWrite.with_note(r#"{"label": "reader,) //"}"#),
        "/repos/{owner}/{name}/raw-fixture",
        crate::api::fixtures::show,
        &wrap,
    );
}
"####;

    let Parsed { rows, unreadable } = parse_routes_from(SOURCE);
    assert!(unreadable.is_empty(), "{unreadable:?}");
    assert_eq!(
        rows.len(),
        1,
        "expected the fixture route, got {}",
        rows.len()
    );

    let row = &rows[0];
    assert_eq!(row.method, "GET");
    assert_eq!(
        row.path.as_deref(),
        Some("/repos/{owner}/{name}/raw-fixture")
    );
    assert_eq!(row.access, "RepoWrite");
    assert_eq!(row.handler, "api::fixtures::show");
}

/// A registration inside a `#[cfg(test)]` item is not a row of the served
/// table, and one written after that item still is.
#[test]
fn route_parser_reads_only_the_served_registrations() {
    const SOURCE: &str = r####"
#[cfg(test)]
mod tests {
    fn scaffold() {
        let brace_in_a_literal = "}";
        RouteTable::new("/api/v1").get(
            RepoWrite,
            "/repos/{owner}/{name}/fixture-only",
            crate::api::fixtures::stub,
        );
    }
}

fn routes() {
    RouteTable::new("/api/v1").get(
        RepoRead,
        "/repos/{owner}/{name}/served",
        crate::api::fixtures::show,
    );
}
"####;

    let Parsed { rows, unreadable } = parse_routes_from(SOURCE);
    assert!(unreadable.is_empty(), "{unreadable:?}");
    assert_eq!(
        rows.iter()
            .map(|row| row.path.as_deref().unwrap_or("<unread>"))
            .collect::<Vec<_>>(),
        ["/repos/{owner}/{name}/served"],
        "a route mounted by a fixture is compared against a signature nothing \
         serves, and counts towards the floors that watch the real table shrink"
    );
    assert_eq!(rows[0].access, "RepoRead");
}

// ── Reading a handler's signature ──────────────────────────────────────────

/// `api::issues::update_issue` → (`api/issues.rs`, `update_issue`).
fn handler_location(handler: &str) -> (String, String) {
    let mut segments: Vec<&str> = handler.split("::").collect();
    let name = segments.pop().expect("a handler path names a function");
    (format!("{}.rs", segments.join("/")), name.to_string())
}

/// The rung each anchored alias in the tree carries.
///
/// The population is read by [`anchored_aliases`], which
/// `anchored_scope_sweep_tests` drives the same routes off — one census, so the
/// guard that checks an anchored route's rung and the sweep that probes it
/// cannot come to disagree about which anchors exist. All this adds is the
/// mapping onto [`Rank`], which is this file's own scale.
fn anchored_ranks() -> BTreeMap<String, Rank> {
    anchored_aliases()
        .into_iter()
        .map(|anchor| {
            let rank = match anchor.kind {
                AnchorKind::Read => Rank::Read,
                AnchorKind::Write => Rank::Write,
            };
            (anchor.alias, rank)
        })
        .collect()
}

/// The strongest repository gate these parameters carry, or `None` for a
/// signature that carries none.
///
/// Strongest rather than first: a handler taking two gates sits behind both, so
/// the level it actually enforces is the higher one.
fn taken_rank(params: &[String], anchored: &BTreeMap<String, Rank>) -> Option<Rank> {
    param_base_types(params)
        .into_iter()
        .filter_map(|base| {
            TAKEN
                .iter()
                .find(|(name, _)| *name == base)
                .map(|(_, rank)| *rank)
                .or_else(|| anchored.get(base).copied())
        })
        .max()
}

// ── The guard ──────────────────────────────────────────────────────────────

/// Every repository-scoped row, with the rung it declares and the rung its
/// handler takes.
fn ranked_rows() -> Vec<(Row, Rank, Option<Rank>)> {
    let anchored = anchored_ranks();
    assert!(
        anchored.len() >= 2,
        "no `AnchoredRead`/`AnchoredWrite` aliases found — the alias scan is broken, and every \
         anchored handler is about to be reported as ungated"
    );

    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    let mut out = Vec::new();
    for row in parse_routes().rows {
        let Some((_, declared)) = DECLARED.iter().find(|(name, _)| *name == row.access) else {
            continue;
        };
        let (file, name) = handler_location(&row.handler);
        let text = sources.entry(file.clone()).or_insert_with(|| read(&file));
        let params = signature_params(text, &name).unwrap_or_else(|| {
            panic!(
                "{} names `{}`, and `{name}` is not a top-level fn in {file} — the signature \
                 reader is blind, not the route",
                row.label(),
                row.handler
            )
        });
        let taken = taken_rank(&params, &anchored);
        out.push((row, *declared, taken));
    }
    out
}

/// The one that matters: a row cannot promise more than its handler enforces.
#[test]
fn no_handler_takes_a_weaker_gate_than_its_route_declares() {
    let mut offenders = Vec::new();
    let mut checked = 0usize;

    for (row, declared, taken) in ranked_rows() {
        checked += 1;
        // A sign-off lowers the bar to the rung it names; it does not remove it.
        let (owed, signed_off) = match signed_off_floor(&row) {
            Some(floor) => (floor, true),
            None => (declared, false),
        };
        match taken {
            Some(taken) if taken >= owed => {}
            Some(taken) if signed_off => offenders.push(format!(
                "  routes.rs:{} — {} declares {}, is signed off down to {owed:?}, and `{}` now \
                 takes {taken:?}",
                row.line,
                row.label(),
                row.access,
                row.handler
            )),
            Some(taken) => offenders.push(format!(
                "  routes.rs:{} — {} declares {} but `{}` takes {taken:?}",
                row.line,
                row.label(),
                row.access,
                row.handler
            )),
            None => offenders.push(format!(
                "  routes.rs:{} — {} declares {} and `{}` takes no repository gate at all",
                row.line,
                row.label(),
                row.access,
                row.handler
            )),
        }
    }

    assert!(
        checked > 150,
        "only {checked} repository-scoped row(s) compared — the guard is not running"
    );
    assert!(
        offenders.is_empty(),
        "a route declares a level its handler does not take.\n\
         The `Access` in the route table is the contract; the extractor in the signature is what \
         enforces it. Where the signature is weaker the route is gated by whatever the handler \
         body happens to remember to do — and for a `GET` or a `DELETE` there is no body \
         extractor behind it to make the mismatch visible, so nothing fails. Raise the extractor \
         to the declared level, or, if the handler really does widen access for one specific \
         person, sign it off in SIGNED_OFF with the reason.\n{}",
        offenders.join("\n")
    );
}

/// The same promise on the rows whose level is not about a repository.
///
/// Split from the pass above because the comparison is a different one, not
/// because the defect is: `Access::User` / `OrgRead` / `OrgAdmin` /
/// `InstanceAdmin` have extractors of their own, and a row that declares one of
/// them while its handler takes only `HeaderMap` is gated by whatever the body
/// remembers to do — the identical hole, on the identical `GET` and `DELETE`
/// rows where no body extractor exists to make it visible.
///
/// There is no rung arithmetic here, so there is no floor to sign off to and no
/// [`SIGNED_OFF`] equivalent. A row either carries an extractor that proves its
/// level or it does not.
#[test]
fn no_non_repository_row_leaves_its_gate_to_the_handler_body() {
    let mut offenders = Vec::new();
    let mut checked: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();

    for row in parse_routes().rows {
        let Some((level, accepted)) = NON_REPO.iter().find(|(name, _)| *name == row.access) else {
            continue;
        };
        *checked.entry(level).or_default() += 1;

        let (file, name) = handler_location(&row.handler);
        let text = sources.entry(file.clone()).or_insert_with(|| read(&file));
        let params = signature_params(text, &name).unwrap_or_else(|| {
            panic!(
                "{} names `{}`, and `{name}` is not a top-level fn in {file} — the signature \
                 reader is blind, not the route",
                row.label(),
                row.handler
            )
        });

        let taken = param_base_types(&params);
        if !taken.iter().any(|ty| accepted.contains(ty)) {
            offenders.push(format!(
                "  routes.rs:{} — {} declares {level} and `{}` takes none of {accepted:?}",
                row.line,
                row.label(),
                row.handler
            ));
        }
    }

    for (level, _) in NON_REPO {
        assert!(
            checked.get(level).copied().unwrap_or(0) > 0,
            "no route declares `{level}` any more — either the level is dead or the parse is wrong"
        );
    }
    let total: usize = checked.values().sum();
    assert!(
        total > 60,
        "only {total} non-repository row(s) compared ({checked:?}) — the guard is not running"
    );
    assert!(
        offenders.is_empty(),
        "a route declares a non-repository level its handler does not take.\n\
         The `Access` in the route table is the contract; the extractor in the signature is what \
         enforces it. Where the signature carries no such extractor the route is gated by whatever \
         the handler body happens to remember to do — and on a `GET` or a `DELETE` there is no \
         body extractor behind it to turn the omission into a visible symptom, so nothing fails. \
         Take the gate as a handler argument, ahead of `Path<_>` / `Query<_>`, instead of writing \
         it out in the body.\n{}",
        offenders.join("\n")
    );
}

/// And the other direction: a sign-off that no longer describes a real widening
/// has to go.
///
/// Without this the list is a one-way ratchet — an entry outlives the handler it
/// was written for, and the next author to weaken that gate finds it already
/// exempt.
#[test]
fn every_sign_off_still_describes_a_live_widening() {
    let ranked = ranked_rows();
    for (handler, access, floor, reason) in SIGNED_OFF {
        assert!(
            !reason.trim().is_empty(),
            "{handler} is signed off without a reason"
        );
        let rows: Vec<&(Row, Rank, Option<Rank>)> = ranked
            .iter()
            .filter(|(row, _, _)| row.handler == *handler && row.access == *access)
            .collect();
        assert!(
            !rows.is_empty(),
            "SIGNED_OFF names `{handler}` at {access}, and no route declares that — drop the entry"
        );
        for (row, declared, taken) in rows {
            assert!(
                floor < declared,
                "SIGNED_OFF lowers `{handler}` on {} to {floor:?}, which is not below the {} the \
                 row declares — the entry buys nothing, drop it",
                row.label(),
                row.access
            );
            assert!(
                taken.is_some_and(|taken| taken < *declared),
                "SIGNED_OFF names `{handler}` on {}, but its signature is no longer weaker than \
                 the row — drop the entry, the guard covers it now",
                row.label()
            );
        }
    }
}

/// The rung a signed-off row still has to hold, if it is signed off at all.
fn signed_off_floor(row: &Row) -> Option<Rank> {
    SIGNED_OFF
        .iter()
        .find(|(handler, access, _, _)| *handler == row.handler && *access == row.access)
        .map(|(_, _, floor, _)| *floor)
}

/// The parse has to see the whole table.
///
/// A guard that reads a subset of the rows is green on the ones it skipped, and
/// nothing about a green run says which those were. So this asserts the three
/// things that would make the pass above vacuous: that no registration was
/// dropped as unreadable, that the row count is plausible, and that every row
/// names an access level this file understands — a new `Access` variant, or a
/// `Foreign` constant the resolver missed, is a reason to fail rather than to
/// quietly skip.
#[test]
fn the_route_table_parser_reads_every_registration() {
    let known = access_variants();
    let Parsed { rows, unreadable } = parse_routes();

    // Reading one file is only a complete parse while one file is where routes
    // are registered. `RouteTable` is `pub(crate)`, so that is checkable here
    // rather than assumed: a second builder site elsewhere in the crate would be
    // a whole family of routes this guard never looks at.
    //
    // Comments come off first, for the same reason they do in the parse above —
    // and here the direction is the other one: a doc comment that *names* the
    // builder to explain where a URL comes from is prose, not a registration.
    // Scanning the raw text made this fail on `openapi.rs` for a sentence about
    // `RouteTable::new("/api/v1")`, which teaches whoever hits it to write a
    // vaguer comment rather than to move a route. `#[cfg(test)]` items come off
    // with them: a fixture that builds a table to drive one handler registers
    // nothing a client can reach, and telling its author to "move the route"
    // has no move to make.
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    let builders: Vec<String> = files
        .iter()
        .filter(|file| {
            production_rust_code_only(&fs::read_to_string(file).expect("read source file"))
                .contains("RouteTable")
        })
        .map(|file| relative(file))
        .filter(|rel| rel != "routes.rs" && rel != "route_table.rs")
        .collect();
    assert!(
        builders.is_empty(),
        "routes are registered outside routes.rs, and this guard only reads routes.rs: {}",
        builders.join(", ")
    );

    assert!(
        unreadable.is_empty(),
        "a route registration is written in a form this parser cannot read.\n\
         Each line below names an access level and then something the handler reader does not \
         recognise as a path to a function. Teach `parse_routes` the spelling — dropping the row \
         means this guard passes it without ever comparing it.\n{}",
        unreadable.join("\n")
    );
    assert!(
        rows.len() >= MIN_ROWS,
        "the route table parser understood only {} registrations (expected at least {MIN_ROWS}) — \
         the builder form in routes.rs probably changed, and this guard is reading a subset",
        rows.len()
    );

    let unknown: Vec<String> = rows
        .iter()
        .filter(|row| !known.iter().any(|name| name == &row.access))
        .map(|row| {
            format!(
                "  routes.rs:{} — {} declares `{}`",
                row.line,
                row.label(),
                row.access
            )
        })
        .collect();
    assert!(
        unknown.is_empty(),
        "a route declares an access level that is not an `Access` variant.\n\
         Either the level is spelled through a constant this file's resolver does not read, or \
         `Access` gained a variant — in both cases the rows below are being skipped silently, \
         which is the one thing this guard must not do.\n{}",
        unknown.join("\n")
    );

    for name in DECLARED
        .iter()
        .map(|(name, _)| name)
        .chain(NON_REPO.iter().map(|(name, _)| name))
    {
        assert!(
            known.iter().any(|variant| variant == name),
            "`Access::{name}` is compared by this guard but no longer exists in route_table.rs"
        );
        assert!(
            rows.iter().any(|row| row.access == *name),
            "no route declares `{name}` any more — either the rung is dead or the parse is wrong"
        );
    }

    // The two tables have to cover the table between them, minus the levels
    // that name no gate at all. A variant that is in neither is a family of
    // rows both passes skip in silence — which is how `User` / `OrgRead` /
    // `OrgAdmin` stayed uncompared while this file read as covering the table.
    let uncompared: Vec<&String> = known
        .iter()
        .filter(|variant| {
            !matches!(variant.as_str(), "Public" | "PublicFiltered" | "Foreign")
                && !DECLARED.iter().any(|(name, _)| name == variant)
                && !NON_REPO.iter().any(|(name, _)| name == variant)
        })
        .collect();
    assert!(
        uncompared.is_empty(),
        "`Access` has level(s) neither pass compares: {uncompared:?}.\n\
         `Public` / `PublicFiltered` promise nothing to hold a signature to, and `Foreign` is \
         `foreign_gate_guard`'s to check. Anything else needs an entry in `DECLARED` (if it is a \
         repository rung) or in `NON_REPO` (with the extractors that prove it) — otherwise its \
         rows are skipped here without a word."
    );
}

/// The `Access` variant names, read out of `route_table.rs`.
///
/// Derived rather than listed, so a variant added there cannot become a level
/// this guard silently ignores.
fn access_variants() -> Vec<String> {
    let src = production_rust_code_only(&read("route_table.rs"));
    let body = src
        .split_once("pub enum Access {")
        .expect("route_table.rs declares `pub enum Access`")
        .1
        .split_once("\n}")
        .expect("the enum closes at column 0")
        .0;
    let names: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(char::is_uppercase))
        .map(|line| leading_ident(line).to_string())
        .collect();
    assert!(
        names.len() >= 10,
        "only {} `Access` variant(s) read out of route_table.rs — the enum reader is broken",
        names.len()
    );
    names
}
