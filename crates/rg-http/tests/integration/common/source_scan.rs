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
