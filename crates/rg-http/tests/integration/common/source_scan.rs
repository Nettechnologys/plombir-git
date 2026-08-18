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

fn blank_range(masked: &mut [u8], start: usize, end: usize) {
    for byte in &mut masked[start..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

fn starts_rust_token(bytes: &[u8], at: usize) -> bool {
    at == 0
        || !bytes[at - 1].is_ascii_alphanumeric() && bytes[at - 1] != b'_' && bytes[at - 1] < 0x80
}

fn char_literal_end(text: &str, quote: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = quote + 1;
    let next = *bytes.get(at)?;

    if next == b'\\' {
        at += 1;
        match *bytes.get(at)? {
            b'x' => at += 3,
            b'u' if bytes.get(at + 1) == Some(&b'{') => {
                let close = bytes[at + 2..].iter().position(|byte| *byte == b'}')?;
                at += close + 3;
            }
            b'\n' | b'\r' => return None,
            _ => at += 1,
        }
    } else {
        let ch = text.get(at..)?.chars().next()?;
        if matches!(ch, '\n' | '\r' | '\'') {
            return None;
        }
        at += ch.len_utf8();
    }

    (bytes.get(at) == Some(&b'\'')).then_some(at + 1)
}

/// `text` with every Rust comment and literal blanked out, byte-for-byte.
///
/// Delimiters in comments, normal/byte/C strings, raw strings and character
/// literals must not take part in a source guard's structural scan. Every
/// non-newline byte in those ranges becomes one ASCII space, so offsets and
/// line numbers in this view still address the original UTF-8 source.
#[allow(dead_code)]
pub fn rust_code_only(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut masked = bytes.to_vec();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            let end = bytes[i..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |relative| i + relative);
            blank_range(&mut masked, i, end);
            i = end;
            continue;
        }

        if bytes[i..].starts_with(b"/*") {
            let mut depth = 1usize;
            let mut end = i + 2;
            while end < bytes.len() && depth > 0 {
                if bytes[end..].starts_with(b"/*") {
                    depth += 1;
                    end += 2;
                } else if bytes[end..].starts_with(b"*/") {
                    depth -= 1;
                    end += 2;
                } else {
                    end += 1;
                }
            }
            blank_range(&mut masked, i, end);
            i = end;
            continue;
        }

        let starts_token = starts_rust_token(bytes, i);
        let raw_prefix = if starts_token && bytes[i] == b'r' {
            Some(1usize)
        } else if starts_token && matches!(bytes[i], b'b' | b'c') && bytes.get(i + 1) == Some(&b'r')
        {
            Some(2)
        } else {
            None
        };
        if let Some(prefix_len) = raw_prefix {
            let mut hashes = 0usize;
            while bytes.get(i + prefix_len + hashes) == Some(&b'#') {
                hashes += 1;
            }
            if bytes.get(i + prefix_len + hashes) == Some(&b'"') {
                let mut end = i + prefix_len + hashes + 1;
                while end < bytes.len() {
                    if bytes[end] == b'"'
                        && end + 1 + hashes <= bytes.len()
                        && bytes[end + 1..end + 1 + hashes]
                            .iter()
                            .all(|byte| *byte == b'#')
                    {
                        end += hashes + 1;
                        break;
                    }
                    end += 1;
                }
                blank_range(&mut masked, i, end);
                i = end;
                continue;
            }
        }

        let string_prefix = if bytes[i] == b'"' {
            Some(0usize)
        } else if starts_token && matches!(bytes[i], b'b' | b'c') && bytes.get(i + 1) == Some(&b'"')
        {
            Some(1)
        } else {
            None
        };
        if let Some(prefix_len) = string_prefix {
            let mut end = i + prefix_len + 1;
            while end < bytes.len() {
                match bytes[end] {
                    b'\\' => end = (end + 2).min(bytes.len()),
                    b'"' => {
                        end += 1;
                        break;
                    }
                    _ => end += 1,
                }
            }
            blank_range(&mut masked, i, end);
            i = end;
            continue;
        }

        let quote = if bytes[i] == b'\'' {
            Some(i)
        } else if starts_token && bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'\'') {
            Some(i + 1)
        } else {
            None
        };
        if let Some(quote) = quote {
            if let Some(end) = char_literal_end(text, quote) {
                blank_range(&mut masked, i, end);
                i = end;
                continue;
            }
        }

        i += 1;
    }

    String::from_utf8(masked).expect("blanking UTF-8 bytes with ASCII preserves UTF-8")
}

#[test]
fn rust_code_only_is_byte_aligned_and_ignores_literal_delimiters() {
    let source = r###"fn live(id: i64) {
    let _raw = r##"// ), ({ café"##; rg_db::load(id);
    let _bytes = b'}';
    let _c = c"/* ( */";
    /* nested { /* ) */ } */
}
"###;

    let masked = rust_code_only(source);
    assert_eq!(masked.len(), source.len());
    assert_eq!(masked.matches('\n').count(), source.matches('\n').count());
    assert_eq!(
        masked.find("rg_db::load(id)"),
        source.find("rg_db::load(id)")
    );
    assert!(masked.contains("fn live(id: i64)"));
    assert!(!masked.contains("café"));
    assert!(!masked.contains("nested"));
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
/// non-code Rust text.
#[allow(dead_code)]
pub fn calls(body: &str, name: &str) -> bool {
    calls_in_code(&rust_code_only(body), name)
}

fn calls_in_code(code_only: &str, name: &str) -> bool {
    code_only.lines().any(|line| {
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

/// An unqualified call to a function declared in the same source file.
///
/// [`calls`] deliberately accepts qualified terminal-gate calls such as
/// `repo_access::check_read(...)`. The local call graph has a narrower rule: a
/// `service::list_versions()` must not become an edge to an unrelated top-level
/// `list_versions` handler merely because the names collide. Keep walking after
/// a qualified collision because an unqualified call may follow on the line.
fn calls_local(body: &str, name: &str) -> bool {
    calls_local_in_code(&rust_code_only(body), name)
}

fn calls_local_in_code(code_only: &str, name: &str) -> bool {
    code_only.lines().any(|line| {
        let code = line.trim_start();
        if code.starts_with("//") || code.starts_with("use ") {
            return false;
        }
        if code.contains(&format!("fn {name}(")) {
            return false;
        }
        let needle = format!("{name}(");
        let mut cursor = 0;
        while let Some(relative) = line[cursor..].find(&needle) {
            let at = cursor + relative;
            let prefix = &line[..at];
            let touches_identifier = prefix.chars().next_back().is_some_and(is_ident_char);
            let qualified = prefix.trim_end().ends_with("::") || prefix.trim_end().ends_with('.');
            if !touches_identifier && !qualified {
                return true;
            }
            cursor = at + needle.len();
        }
        false
    })
}

#[test]
fn qualified_same_name_call_is_not_a_local_edge() {
    let source = r#"pub async fn handler() {
    service::list_versions();
}
fn list_versions() {
    package_error_response();
}
fn package_error_response() {
}
"#;

    assert_eq!(
        reachable_within_module(source, "handler"),
        Some(vec!["handler".to_string()])
    );
    assert_eq!(
        reaches_any(source, "handler", &["package_error_response"]),
        Some(false)
    );
    assert!(calls("service::gate();", "gate"));
    assert!(!calls_local("service::gate();", "gate"));
    assert!(calls_local("service::gate(); gate();", "gate"));
}

#[test]
fn call_scans_ignore_non_code_decoys_and_keep_live_calls() {
    const SAMPLE: &str = r####"pub async fn handler() {
    // check_read_for(); decoy_helper();
    /* package_error_response(); decoy_helper(); */
    let _normal = "check_read_for(); decoy_helper();";
    let _raw = r#"package_error_response(); decoy_helper();"#;
    let _bytes = b"check_read_for(); decoy_helper();";
    let _raw_bytes = br#"package_error_response(); decoy_helper();"#;
    repo_access::live_gate();
    live_helper();
}
fn live_helper() {
}
fn decoy_helper() {
    package_error_response();
}
"####;

    let handler = functions(SAMPLE)
        .into_iter()
        .find(|function| function.name == "handler")
        .expect("fixture handler");

    for decoy in ["check_read_for", "package_error_response", "decoy_helper"] {
        assert!(
            !calls(&handler.body, decoy),
            "literal/comment decoy: {decoy}"
        );
    }
    assert!(calls(&handler.body, "live_gate"));
    assert!(!calls_local(&handler.body, "live_gate"));
    assert!(calls_local(&handler.body, "live_helper"));
    assert_eq!(
        reachable_within_module(SAMPLE, "handler"),
        Some(vec!["handler".to_string(), "live_helper".to_string()])
    );
    assert_eq!(
        reaches_any(SAMPLE, "handler", &["package_error_response"]),
        Some(false)
    );
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
        let code_only = rust_code_only(body);
        for candidate in bodies.keys() {
            if candidate != &name
                && !seen.contains(candidate)
                && calls_local_in_code(&code_only, candidate)
            {
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
        bodies.get(fname).is_some_and(|body| {
            let code_only = rust_code_only(body);
            names.iter().any(|name| calls_in_code(&code_only, name))
        })
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

/// The top-level arguments of a call, with structure read from a byte-aligned
/// code-only view and values sliced from the original source.
///
/// `code` is expected to come from [`rust_code_only`]. Keeping the two views
/// separate means delimiters inside any Rust literal cannot close the call,
/// while callers still receive paths, attributes and other literal-bearing
/// argument text exactly as it was written.
#[allow(dead_code)]
pub fn call_args_from_code(src: &str, code: &str, open: usize) -> Option<Vec<String>> {
    if src.len() != code.len() || code.as_bytes().get(open) != Some(&b'(') {
        return None;
    }

    let bytes = code.as_bytes();
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = open + 1;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1)?;
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

struct AnchoredAliasDeclaration<'a> {
    alias: String,
    kind: AnchorKind,
    target: String,
    line: usize,
    original: &'a str,
}

/// Anchored aliases declared by executable Rust in `text`.
///
/// The code-only view decides whether a line is a declaration; the byte-aligned
/// original stays beside it for diagnostics and fixtures.
fn anchored_alias_declarations(text: &str) -> Vec<AnchoredAliasDeclaration<'_>> {
    let code_only = rust_code_only(text);
    code_only
        .lines()
        .zip(text.lines())
        .enumerate()
        .filter_map(|(n, (code, original))| {
            let (alias, target) = code
                .trim_start()
                .strip_prefix("pub type ")?
                .split_once('=')?;
            let target = target.trim();
            let kind = match leading_ident(target) {
                "AnchoredRead" => AnchorKind::Read,
                "AnchoredWrite" => AnchorKind::Write,
                _ => return None,
            };
            Some(AnchoredAliasDeclaration {
                alias: alias.trim().to_string(),
                kind,
                target: anchor_target(target),
                line: n + 1,
                original,
            })
        })
        .collect()
}

#[test]
fn anchored_alias_declarations_ignore_non_code_decoys_and_keep_original_lines() {
    const SAMPLE: &str = r####"// pub type Line = AnchoredRead<Line>;
/*
pub type Block = AnchoredWrite<Block>;
*/
const NORMAL: &str = "
pub type Normal = AnchoredRead<Normal>;
";
const RAW: &str = r#"
pub type Raw = AnchoredWrite<Raw>;
"#;
const BYTES: &[u8] = b"
pub type Bytes = AnchoredRead<Bytes>;
";
const RAW_BYTES: &[u8] = br#"
pub type RawBytes = AnchoredWrite<RawBytes>;
"#;
pub type LiveRead = AnchoredRead<crate::rows::LiveReadRow>;
pub type LiveWrite = AnchoredWrite<LiveWriteRow>;
"####;

    let declarations = anchored_alias_declarations(SAMPLE);
    let actual = declarations
        .iter()
        .map(|declaration| {
            (
                declaration.alias.as_str(),
                declaration.kind,
                declaration.target.as_str(),
                declaration.line,
                declaration.original,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        vec![
            (
                "LiveRead",
                AnchorKind::Read,
                "LiveReadRow",
                17,
                "pub type LiveRead = AnchoredRead<crate::rows::LiveReadRow>;",
            ),
            (
                "LiveWrite",
                AnchorKind::Write,
                "LiveWriteRow",
                18,
                "pub type LiveWrite = AnchoredWrite<LiveWriteRow>;",
            ),
        ]
    );
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
        for declaration in anchored_alias_declarations(&text) {
            out.push(Anchor {
                alias: declaration.alias,
                kind: declaration.kind,
                target: declaration.target,
                file: relative(file),
            });
        }
    }
    out
}

/// One handler whose signature takes an anchored extractor.
#[allow(dead_code)]
pub struct AnchoredHandler {
    /// The file it is declared in, relative to `src/` — `api/artifacts.rs`.
    pub file: String,
    /// The function name — `get_artifact`.
    pub name: String,
    /// The alias its signature names — `ArtifactRead`.
    pub alias: String,
}

/// Every handler in the tree that takes one of `aliases`.
///
/// Read from the source rather than from the route table because the alias is
/// only visible in a signature: `RouteFact` records the handler's `type_name`
/// and its declared `Access`, and `RepoRead` is what an anchored route declares
/// too — the route table cannot tell an anchored gate from a path-based one.
#[allow(dead_code)]
pub fn anchored_handlers(aliases: &BTreeSet<&str>) -> Vec<AnchoredHandler> {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    files.sort();
    let mut out = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("read source file");
        for function in functions(&text) {
            let Some(params) = signature_params(&text, &function.name) else {
                continue;
            };
            for base in param_base_types(&params) {
                if let Some(alias) = aliases.get(base) {
                    out.push(AnchoredHandler {
                        file: relative(file),
                        name: function.name.clone(),
                        alias: (*alias).to_string(),
                    });
                    break;
                }
            }
        }
    }
    out
}

/// `api/artifacts.rs` + `get_artifact` ⇒ `rg_http::api::artifacts::get_artifact`,
/// the spelling `RouteFact::handler` carries.
#[allow(dead_code)]
pub fn handler_type_name(file: &str, name: &str) -> String {
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    let module = stem.strip_suffix("/mod").unwrap_or(stem);
    format!("rg_http::{}::{name}", module.replace('/', "::"))
}

/// Every handler in the tree gated by an anchored extractor, mapped to the
/// [`RepoAnchor`](../../src/api/repo_access.rs) implementor its gate resolves the
/// repository through: `rg_http::api::artifacts::get_artifact` ⇒ `Artifact`.
///
/// The census in one call, keyed the way `RouteFact::handler` spells a handler —
/// for the callers that only need to ask a route "are you anchored, and what row
/// addresses you?". `route_access_sweep_tests` judges an anchored row by a
/// different predicate than a path-based one, and the two passes have to be
/// counting the same routes or the weaker of them silently becomes the ceiling.
#[allow(dead_code)]
pub fn anchored_handler_targets() -> BTreeMap<String, String> {
    let anchors = anchored_aliases();
    let by_alias: BTreeMap<&str, &Anchor> = anchors
        .iter()
        .map(|anchor| (anchor.alias.as_str(), anchor))
        .collect();
    let aliases: BTreeSet<&str> = by_alias.keys().copied().collect();
    anchored_handlers(&aliases)
        .iter()
        .filter_map(|handler| {
            let anchor = by_alias.get(handler.alias.as_str())?;
            Some((
                handler_type_name(&handler.file, &handler.name),
                anchor.target.clone(),
            ))
        })
        .collect()
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
