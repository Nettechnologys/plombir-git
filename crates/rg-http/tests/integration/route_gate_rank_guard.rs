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
//! The scope is the **repository** rungs, and that boundary is worth stating
//! plainly, because a guard whose coverage nobody has written down is read as
//! covering the table. `Access::User`, `OrgRead`, `OrgAdmin` and `InstanceAdmin`
//! have extractors of their own (`AuthUser`, `OrgRead` / `OrgAdmin` in
//! `api::orgs`, `InstanceAdmin` in `api::admin`) and the identical defect is
//! open on 18 of those rows today — declared level in the table, gate written
//! out by hand in the body, no symptom on a `GET` or a `DELETE`. That is
//! `card_34fd642a7538`, not something this file quietly passes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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
/// All five are one pattern, and it is worth naming because it is the only
/// legitimate way past this guard. The row declares what an *arbitrary* caller
/// needs — that is what a contract states, and what the persona sweep measures a
/// stranger against. The handler takes `RepoAuthRead`, the floor for everybody,
/// and then widens for one specific person by asking `repo_access::may_write`:
/// the same gate module, as a *question* on top of a gate already passed, not a
/// second copy of the rule. What separates this from the defect is that the
/// widening is a property of the row being acted on — its author — and not of
/// the caller's repository permission, so no weaker caller gets in.
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
];

/// Lower bound on the rows a healthy parse yields; the table holds ~330.
///
/// A parser that silently understands nothing turns this guard into a green
/// no-op, which is worse than a red one — nobody investigates a passing test.
const MIN_ROWS: usize = 300;

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

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

/// Rust line and block comments removed; string literals and the line count kept.
///
/// A commented-out registration has to read as a deleted route. Without this a
/// row that is not in the binary still answers the parse, and the guard reports
/// on code nobody runs.
fn strip_comments(src: &str) -> String {
    // Byte-wise is safe on UTF-8: every byte of a multi-byte sequence is ≥ 0x80,
    // so none of the ASCII delimiters below can match inside one.
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            let mut depth = 1;
            i += 2;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    if bytes[i] == b'\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
            }
        } else if bytes[i] == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                i += 1;
                if bytes[i - 1] == b'"' {
                    break;
                }
            }
            out.push_str(&src[start..i]);
        } else {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
                i += 1;
            }
            out.push_str(&src[start..i]);
        }
    }
    out
}

/// The top-level arguments of the call whose `(` sits at byte offset `open`, or
/// `None` when the parentheses never balance.
fn call_args(src: &str, open: usize) -> Option<Vec<String>> {
    let bytes = src.as_bytes();
    let mut args: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut start = open + 1;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        break;
                    }
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    args.push(src[start..i].trim().to_string());
                    // Rust's trailing comma leaves an empty tail segment.
                    if args.last().is_some_and(String::is_empty) {
                        args.pop();
                    }
                    return Some(args);
                }
            }
            b',' if depth == 1 => {
                args.push(src[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
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

/// The identifier `text` starts with — `Foreign(Handler { … })` ⇒ `Foreign`.
fn leading_ident(text: &str) -> &str {
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    &text[..end]
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
fn parse_routes() -> Parsed {
    let src = strip_comments(&read("routes.rs"));
    let constants = access_constants(&src);
    let mut rows = Vec::new();
    let mut unreadable = Vec::new();

    for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"] {
        for suffix in ["(", "_with("] {
            let needle = format!(".{}{suffix}", method.to_lowercase());
            let mut from = 0;
            while let Some(found) = src[from..].find(&needle) {
                let at = from + found;
                let open = at + needle.len() - 1;
                from = open + 1;
                let Some(args) = call_args(&src, open) else {
                    continue;
                };
                if args.len() < 3 {
                    continue;
                }
                let raw = leading_ident(args[0].trim());
                let handler: String = args[2].split_whitespace().collect();
                let line = src[..at].lines().count();
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

// ── Reading a handler's signature ──────────────────────────────────────────

/// `api::issues::update_issue` → (`api/issues.rs`, `update_issue`).
fn handler_location(handler: &str) -> (String, String) {
    let mut segments: Vec<&str> = handler.split("::").collect();
    let name = segments.pop().expect("a handler path names a function");
    (format!("{}.rs", segments.join("/")), name.to_string())
}

/// The top-level parameters of `fn name` in `text`, or `None` when it is not
/// declared there.
///
/// Anchored on column 0: rustfmt puts a top-level item there and nothing inside
/// a function body, so an inner helper or a closure cannot be mistaken for the
/// handler.
fn signature_params(text: &str, name: &str) -> Option<Vec<String>> {
    let mut offset = 0usize;
    for line in text.lines() {
        let declares = [
            "pub async fn ",
            "pub(crate) async fn ",
            "async fn ",
            "pub fn ",
            "pub(crate) fn ",
            "fn ",
        ]
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix))
        .is_some_and(|rest| {
            rest.strip_prefix(name)
                .is_some_and(|tail| tail.starts_with('(') || tail.starts_with('<'))
        });
        if declares {
            return call_args(text, offset + line.find('(')?);
        }
        offset += line.len() + 1;
    }
    None
}

/// The declared type of one parameter — the text after the `:` that separates
/// pattern from type at nesting depth zero.
///
/// The depth matters: `Path((owner, name, number)): Path<(String, String, i64)>`
/// and `RepoAuthRead { repo, actor_id }: RepoAuthRead` both carry colons inside
/// the *pattern*, and splitting at the first one reads a field name as the type.
/// So does `state: axum::extract::State<AppState>`, whose `::` pairs are not
/// separators at all.
fn param_type(param: &str) -> Option<&str> {
    let bytes = param.as_bytes();
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' => depth -= 1,
            b':' if depth == 0 => {
                if bytes.get(i + 1) == Some(&b':') {
                    i += 2;
                    continue;
                }
                return Some(param[i + 1..].trim());
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `pub type ArtifactRead = AnchoredRead<Artifact>;` across the tree.
///
/// The anchored extractors are the shape a route takes when its path names no
/// repository — `/artifacts/{id}` resolves one out of the artifact — and each
/// anchor declares its own alias pair beside it. Reading them out of the tree
/// rather than listing them here means a new anchor is understood the day it is
/// written; and an alias this misses reads as a handler with no gate at all,
/// which is an offender, so the failure direction is the safe one.
fn anchored_ranks() -> BTreeMap<String, Rank> {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    let mut out = BTreeMap::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("read source file");
        for line in text.lines() {
            let Some((alias, target)) = line
                .trim_start()
                .strip_prefix("pub type ")
                .and_then(|rest| rest.split_once('='))
            else {
                continue;
            };
            let rank = match leading_ident(target.trim()) {
                "AnchoredRead" => Rank::Read,
                "AnchoredWrite" => Rank::Write,
                _ => continue,
            };
            out.insert(alias.trim().to_string(), rank);
        }
    }
    out
}

fn relative(path: &Path) -> String {
    path.strip_prefix(src_root())
        .expect("file under src/")
        .to_string_lossy()
        .replace('\\', "/")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The strongest repository gate these parameters carry, or `None` for a
/// signature that carries none.
///
/// Strongest rather than first: a handler taking two gates sits behind both, so
/// the level it actually enforces is the higher one.
fn taken_rank(params: &[String], anchored: &BTreeMap<String, Rank>) -> Option<Rank> {
    params
        .iter()
        .filter_map(|param| param_type(param))
        .filter_map(|ty| {
            let base = leading_ident(ty.rsplit("::").next().unwrap_or(ty).trim());
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
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    let builders: Vec<String> = files
        .iter()
        .filter(|file| {
            fs::read_to_string(file)
                .expect("read source file")
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

    for (name, _) in DECLARED {
        assert!(
            known.iter().any(|variant| variant == name),
            "`Access::{name}` is compared by this guard but no longer exists in route_table.rs"
        );
        assert!(
            rows.iter().any(|row| row.access == *name),
            "no route declares `{name}` any more — either the rung is dead or the parse is wrong"
        );
    }
}

/// The `Access` variant names, read out of `route_table.rs`.
///
/// Derived rather than listed, so a variant added there cannot become a level
/// this guard silently ignores.
fn access_variants() -> Vec<String> {
    let src = strip_comments(&read("route_table.rs"));
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
