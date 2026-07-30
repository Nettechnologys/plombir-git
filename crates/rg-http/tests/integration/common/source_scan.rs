//! Reading `rg-http`'s own source the way the guard tests need it.
//!
//! Three of the guards in this directory are greps over `src/` rather than
//! requests against a running server, for the same reason: the defect each one
//! looks for is a line of code that was *not* written, and no request can
//! exercise a missing call. That makes "which functions does this file
//! declare, and what does each of them call" a shared question, and it was on
//! its way to being answered three times — `global_id_anchor_guard` had grown
//! the careful version, `route_gate_rank_guard` a second one shaped around
//! signatures, and the handler-reachability check in `foreign_gate_guard`
//! would have been the third copy.
//!
//! The subtleties are worth keeping in one place, because both of them were
//! bugs first:
//!
//! * A function body ends at the `}` in **column 0**, not at the next
//!   `pub async fn`. The old form swept up whatever sat between two handlers —
//!   a private helper, the next handler's `#[utoipa::path]` block, its
//!   signature — which made a handler answerable for calls it does not make.
//!   rustfmt puts the closing brace of a top-level item in column 0 and nothing
//!   inside a body there, so that brace is the exact end.
//! * A call is a call at an identifier boundary. `check_read` is a prefix of
//!   `check_read_with_ci`, and a doc comment mentioning either is not a call at
//!   all.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// `crates/rg-http/src` — the tree every source guard walks.
#[allow(dead_code)]
pub fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// `crates/` — the parent of this crate's directory, and the tree a guard has
/// to walk when the rule it enforces is not confined to `rg-http`.
///
/// A `pub` function in `rg-core` is reachable from every crate that can depend
/// on it, so a guard that stops at [`src_root`] is narrower than its own rule:
/// `fork_repo` held a second copy of the repository read rule one crate over
/// for exactly as long as the predicate guard scanned only this crate.
#[allow(dead_code)]
pub fn workspace_crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rg-http lives under crates/")
        .to_path_buf()
}

/// Every `.rs` file under `dir`, recursively.
#[allow(dead_code)]
pub fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A path under [`src_root`], spelled the way the sign-off lists spell it:
/// `api/lfs.rs`, forward slashes on every platform.
#[allow(dead_code)]
pub fn relative(path: &Path) -> String {
    path.strip_prefix(src_root())
        .expect("file under src/")
        .to_string_lossy()
        .replace('\\', "/")
}

/// A path under [`workspace_crates`], spelled the way the workspace-wide
/// sign-off lists spell it: `rg-core/src/release/service.rs`. The crate name
/// is part of it, because out there the crate is the first thing you need to
/// know about an offending line.
#[allow(dead_code)]
pub fn crate_relative(path: &Path) -> String {
    path.strip_prefix(workspace_crates())
        .expect("file under crates/")
        .to_string_lossy()
        .replace('\\', "/")
}

#[allow(dead_code)]
pub fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One `fn` declared at column 0, ending at the `}` that closes it.
#[allow(dead_code)]
pub struct Function {
    pub name: String,
    pub line: usize,
    pub body: String,
    /// Whether the visibility makes it reachable by the router — `pub` or
    /// `pub(crate)`, and `async`. Nothing stops the router from taking a
    /// `pub(crate) async fn`, so a census that only read `pub async fn` would
    /// let a route hide behind the narrower visibility.
    pub is_handler: bool,
}

fn declared_fn(line: &str) -> Option<(String, bool)> {
    for (prefix, is_handler) in [
        ("pub async fn ", true),
        ("pub(crate) async fn ", true),
        ("async fn ", false),
        ("pub(crate) fn ", false),
        ("pub fn ", false),
        ("fn ", false),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let name = rest
                .split(['(', '<', ' '])
                .next()
                .unwrap_or_default()
                .to_string();
            return Some((name, is_handler));
        }
    }
    None
}

/// Every top-level function in `text`, in source order.
#[allow(dead_code)]
pub fn functions(text: &str) -> Vec<Function> {
    let mut out: Vec<Function> = Vec::new();
    let mut open: Option<usize> = None;
    for (n, line) in text.lines().enumerate() {
        if open.is_none() {
            if let Some((name, is_handler)) = declared_fn(line) {
                out.push(Function {
                    name,
                    line: n + 1,
                    body: String::new(),
                    is_handler,
                });
                open = Some(out.len() - 1);
            }
        }
        if let Some(index) = open {
            out[index].body.push_str(line);
            out[index].body.push('\n');
            if line == "}" {
                open = None;
            }
        }
    }
    out
}

/// The subset of [`functions`] the router could be handed.
#[allow(dead_code)]
pub fn handlers(text: &str) -> Vec<Function> {
    functions(text)
        .into_iter()
        .filter(|f| f.is_handler)
        .collect()
}

/// A call to `name(` in `body`, ignoring its own definition, `use` lines and
/// comments.
#[allow(dead_code)]
pub fn calls(body: &str, name: &str) -> bool {
    body.lines().any(|line| {
        let code = line.trim_start();
        if code.starts_with("//") || code.starts_with("use ") {
            return false;
        }
        if code.contains(&format!("fn {name}(")) {
            return false;
        }
        line.find(&format!("{name}("))
            .is_some_and(|at| !line[..at].chars().next_back().is_some_and(is_ident_char))
    })
}

/// Everything `start` reaches inside its own module, itself included.
///
/// Only functions declared in the same file are followed: a call that leaves
/// the module is a *name* to this scan, matched by [`calls`], not an edge to
/// walk. That is the boundary the guards need — "does this handler reach the
/// gate, directly or through the helpers of its own file" — and it is why a
/// handler cannot pass by delegating to a helper that gates nothing.
///
/// `None` when `start` is not declared in `text` at all, which every caller has
/// to treat as a failure rather than an empty answer: a handler the scan cannot
/// find is a handler nothing was proven about.
#[allow(dead_code)]
pub fn reachable_within_module(text: &str, start: &str) -> Option<Vec<String>> {
    let bodies: BTreeMap<String, String> = functions(text)
        .into_iter()
        .map(|f| (f.name, f.body))
        .collect();
    if !bodies.contains_key(start) {
        return None;
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue = vec![start.to_string()];
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(body) = bodies.get(&name) else {
            continue;
        };
        for candidate in bodies.keys() {
            if candidate != &name && !seen.contains(candidate) && calls(body, candidate) {
                queue.push(candidate.clone());
            }
        }
    }
    Some(seen.into_iter().collect())
}

/// Whether anything `start` reaches inside its module calls one of `names`.
///
/// `None` carries the same meaning as in [`reachable_within_module`]: the
/// function is not declared in this file.
#[allow(dead_code)]
pub fn reaches_any(text: &str, start: &str, names: &[&str]) -> Option<bool> {
    let reachable = reachable_within_module(text, start)?;
    let bodies: BTreeMap<String, String> = functions(text)
        .into_iter()
        .map(|f| (f.name, f.body))
        .collect();
    Some(reachable.iter().any(|fname| {
        bodies
            .get(fname)
            .is_some_and(|body| names.iter().any(|name| calls(body, name)))
    }))
}

// ── Reading a signature ────────────────────────────────────────────────────
//
// "What does this handler take?" is the question two guards ask of a signature:
// `route_gate_rank_guard` compares the rung a route declares against the
// extractor its handler holds, and `anchored_scope_sweep_tests` selects the
// routes to drive by the same reading. The subtleties below were bugs in the
// first copy, which is reason enough for there not to be a second one.

/// The identifier `text` starts with — `Foreign(Handler { … })` ⇒ `Foreign`.
#[allow(dead_code)]
pub fn leading_ident(text: &str) -> &str {
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    &text[..end]
}

/// The top-level arguments of the call whose `(` sits at byte offset `open`, or
/// `None` when the parentheses never balance.
#[allow(dead_code)]
pub fn call_args(src: &str, open: usize) -> Option<Vec<String>> {
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

/// The top-level parameters of `fn name` in `text`, or `None` when it is not
/// declared there.
///
/// Anchored on column 0: rustfmt puts a top-level item there and nothing inside
/// a function body, so an inner helper or a closure cannot be mistaken for the
/// handler.
///
/// The parameter list is located from the *name*, not from the first `(` on the
/// line. `pub(crate) async fn openapi_handler(_: AuthUser)` has a paren three
/// characters in, and reading from there parses `(crate)` as the signature — so
/// every `pub(crate)` handler read as taking one parameter called `crate` and
/// therefore no gate at all. The failure direction was safe (such a handler is
/// reported as ungated, never as gated), but it made the level unstatable: a
/// route could not declare `User` over a `pub(crate)` handler however correct
/// that handler was.
#[allow(dead_code)]
pub fn signature_params(text: &str, name: &str) -> Option<Vec<String>> {
    let mut offset = 0usize;
    for line in text.lines() {
        let params_at = [
            "pub async fn ",
            "pub(crate) async fn ",
            "async fn ",
            "pub fn ",
            "pub(crate) fn ",
            "fn ",
        ]
        .iter()
        .find_map(|prefix| {
            let tail = line.strip_prefix(prefix)?.strip_prefix(name)?;
            // A generic list may sit between the name and the parameters:
            // `fn handler<T>(…)`.
            let open = match tail.as_bytes().first()? {
                b'(' => 0,
                b'<' => tail.find('(')?,
                _ => return None,
            };
            Some(prefix.len() + name.len() + open)
        });
        if let Some(open) = params_at {
            return call_args(text, offset + open);
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
#[allow(dead_code)]
pub fn param_type(param: &str) -> Option<&str> {
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

/// The extractor type names a signature carries, reduced to the base name the
/// guard tables are keyed on.
///
/// `AuthUser(user_id): AuthUser`, `OrgAdmin { org, .. }: OrgAdmin` and
/// `admin: crate::api::admin::InstanceAdmin` all yield the bare type name: the
/// path prefix is dropped, and so is anything a generic or a pattern adds.
#[allow(dead_code)]
pub fn param_base_types(params: &[String]) -> Vec<&str> {
    params
        .iter()
        .filter_map(|param| param_type(param))
        .map(|ty| leading_ident(ty.rsplit("::").next().unwrap_or(ty).trim()))
        .collect()
}

// ── The anchored extractors ────────────────────────────────────────────────

/// Which of the anchored extractor pair an alias names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum AnchorKind {
    /// `AnchoredRead<_>` — the row of a public repository stays readable
    /// without a token.
    Read,
    /// `AnchoredWrite<_>` — a session first, then visibility, then permission.
    Write,
}

/// One `pub type ArtifactRead = AnchoredRead<Artifact>;` found in the tree.
#[allow(dead_code)]
pub struct Anchor {
    /// The alias a handler's signature names — `ArtifactRead`.
    pub alias: String,
    pub kind: AnchorKind,
    /// The [`RepoAnchor`](../../src/api/repo_access.rs) implementor the alias is
    /// built over — `Artifact`. Two aliases of one anchor address the same row,
    /// which is why a fixture is keyed on this rather than on the alias.
    pub target: String,
    /// The file the alias is declared in, relative to `src/`.
    pub file: String,
}

/// Every anchored extractor alias declared in the tree.
///
/// The anchored extractors are the shape a route takes when its path names no
/// repository — `/artifacts/{id}` resolves one out of the artifact — and each
/// anchor declares its own alias pair beside it. Reading them out of the tree
/// rather than listing them in a guard means a new anchor is understood the day
/// it is written, and both guards that key on them are counting the same
/// population.
#[allow(dead_code)]
pub fn anchored_aliases() -> Vec<Anchor> {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    files.sort();
    let mut out = Vec::new();
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
            let target = target.trim();
            let kind = match leading_ident(target) {
                "AnchoredRead" => AnchorKind::Read,
                "AnchoredWrite" => AnchorKind::Write,
                _ => continue,
            };
            out.push(Anchor {
                alias: alias.trim().to_string(),
                kind,
                target: anchor_target(target),
                file: relative(file),
            });
        }
    }
    out
}

/// `AnchoredRead<crate::api::artifacts::Artifact>;` ⇒ `Artifact`.
///
/// The generic argument, stripped of any module path it is spelled with, so the
/// anchor is named the same way wherever the alias happens to live.
fn anchor_target(target: &str) -> String {
    let inner = target
        .split_once('<')
        .and_then(|(_, rest)| rest.rsplit_once('>'))
        .map_or(target, |(inner, _)| inner)
        .trim();
    let bare = inner.rsplit("::").next().unwrap_or(inner).trim();
    leading_ident(bare).to_string()
}
