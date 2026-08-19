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
use std::ops::RangeInclusive;
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

/// The 1-based, inclusive line ranges the file's `#[cfg(test)]` items span.
///
/// Each `#[cfg(test)]` attribute is followed to the end of the item it marks —
/// by counting braces on the code-only view, or to the `;` of an item that has
/// no block — rather than to the end of the file. That difference is the whole
/// point: taking the first `#[cfg(test)] mod` as a boundary and calling the
/// rest of the file test-only holds for `security.rs` and `rate_limit.rs`,
/// whose tests sit at the tail, and is simply false for a file with test
/// modules *between* production items. `api/packages.rs` has three of them with
/// handlers in between, so the old model would have waved a served route
/// through the moment that file was signed off (card_5b5f4d203378).
///
/// Ranging over the attribute rather than over `#[cfg(test)] mod` pairs also
/// makes `rate_limit.rs`'s three `#[cfg(test)]` helpers, a hundred lines above
/// its test module, test scaffolding in their own right instead of a boundary
/// the old model had to be taught to skip.
#[allow(dead_code)]
pub fn test_item_ranges(text: &str) -> Vec<RangeInclusive<usize>> {
    test_item_ranges_in_code(&rust_code_only(text))
}

/// [`test_item_ranges`] over a view [`rust_code_only`] has already produced.
///
/// The two views are byte-aligned, so a range addresses either of them — and
/// [`production_rust_code_only`], which holds the masked view already, would
/// otherwise pay for a second pass over every file it reads.
fn test_item_ranges_in_code(code: &str) -> Vec<RangeInclusive<usize>> {
    let lines: Vec<&str> = code.lines().collect();
    let mut ranges = Vec::new();
    let mut n = 0;

    while n < lines.len() {
        if lines[n].trim() != "#[cfg(test)]" {
            n += 1;
            continue;
        }

        let mut depth = 0usize;
        let mut opened = false;
        let mut end = lines.len() - 1;
        for (k, line) in lines.iter().enumerate().skip(n + 1) {
            for ch in line.chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            if opened && depth == 0 {
                end = k;
                break;
            }
            // `#[cfg(test)] use …;` — an item with no block of its own.
            if !opened && line.trim_end().ends_with(';') {
                end = k;
                break;
            }
        }

        ranges.push(n + 1..=end + 1);
        n = end + 1;
    }

    ranges
}

/// The byte-aligned code-only view with complete `#[cfg(test)]` items blanked.
///
/// The view a *production* census needs. [`rust_code_only`] already keeps a
/// comment or a literal from manufacturing a fact; this also keeps an inline
/// `#[cfg(test)]` fixture from answering for production code. Both directions
/// of that were live: a fixture seeding a row is not a writer, so counting it
/// as one is a false red — and a liveness floor held up by two fixtures stays
/// green after the one production line it was watching is deleted, which is the
/// quieter half (`audit_writer_guard`, card_dfd5da074447).
///
/// Newlines and byte offsets still address `text`.
#[allow(dead_code)]
pub fn production_rust_code_only(text: &str) -> String {
    let code = rust_code_only(text);
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(code.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let mut masked = code.clone().into_bytes();

    for range in test_item_ranges_in_code(&code) {
        let start = line_starts[range.start() - 1];
        let end = line_starts
            .get(*range.end())
            .copied()
            .unwrap_or(masked.len());
        blank_range(&mut masked, start, end);
    }

    String::from_utf8(masked).expect("blanking UTF-8 bytes with ASCII preserves UTF-8")
}

/// The production view blanks a whole test item and nothing around it.
///
/// The sample carries the two shapes that broke the models this replaced: a
/// production item *after* an inline test module (a file-tail exemption would
/// call it test-only) and a brace inside a literal (counting it would close the
/// test module early and leave its scaffolding visible).
#[test]
fn production_view_blanks_complete_test_items_and_stays_byte_aligned() {
    const SAMPLE: &str = r##"fn early_production() {}

#[cfg(test)]
mod early_tests {
    fn scaffold() {
        let quoted = "unbalanced } in a string";
        let _row = audit_log::ActiveModel { ..Default::default() };
    }
}

fn late_production() {
    let _row = audit_log::ActiveModel { ..Default::default() };
}

#[cfg(test)]
use std::fmt::Debug;

fn after_the_bare_test_item() {}
"##;

    let view = production_rust_code_only(SAMPLE);

    assert_eq!(view.len(), SAMPLE.len());
    assert_eq!(view.matches('\n').count(), SAMPLE.matches('\n').count());
    assert_eq!(
        view.find("fn late_production"),
        SAMPLE.find("fn late_production"),
        "the view is no longer byte-aligned with the source it masks"
    );
    assert!(view.contains("fn early_production"));
    assert!(
        view.contains("fn after_the_bare_test_item"),
        "a `#[cfg(test)] use …;` has no block, and taking it as one blanks the \
         production items that follow it"
    );
    assert!(!view.contains("fn scaffold"));
    assert!(!view.contains("early_tests"));
    assert!(!view.contains("std::fmt::Debug"));
    assert_eq!(
        view.matches("audit_log::ActiveModel").count(),
        1,
        "the fixture construction inside `early_tests` is still visible — a \
         production census would count it"
    );
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

/// How a `fn` declaration is spelled, as written.
///
/// `pub` and `pub(crate)` are told apart because guards rest on the difference:
/// a gate the compiler bars outside its crate needs no grep, and the same gate
/// spelled `pub` needs every one of them.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FnVisibility {
    /// No `pub` at all.
    Private,
    /// `pub(crate)`.
    Crate,
    /// Any other restricted form — `pub(super)`, `pub(in path)`.
    Restricted,
    /// Bare `pub`.
    Public,
}

/// One `fn` declaration: what it is called and how it is spelled.
#[allow(dead_code)]
pub struct Declaration {
    pub name: String,
    pub line: usize,
    pub visibility: FnVisibility,
    pub is_async: bool,
}

/// Read a declaration off `code`, which must *begin* with it.
fn parse_declaration(code: &str) -> Option<(String, FnVisibility, bool)> {
    let (visibility, rest) = if let Some(rest) = code.strip_prefix("pub(") {
        let close = rest.find(')')?;
        let visibility = if rest[..close].trim() == "crate" {
            FnVisibility::Crate
        } else {
            FnVisibility::Restricted
        };
        (visibility, rest[close + 1..].trim_start())
    } else if let Some(rest) = code.strip_prefix("pub ") {
        (FnVisibility::Public, rest.trim_start())
    } else {
        (FnVisibility::Private, code)
    };

    let (is_async, rest) = match rest.strip_prefix("async ") {
        Some(rest) => (true, rest.trim_start()),
        None => (false, rest),
    };

    let name = rest.strip_prefix("fn ")?.split(['(', '<', ' ']).next()?;
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), visibility, is_async))
}

fn declared_fn(line: &str) -> Option<(String, bool)> {
    parse_declaration(line)
        .map(|(name, visibility, is_async)| (name, is_async && visibility != FnVisibility::Private))
}

/// Every `fn` declared in `text`, ignoring comments and literals.
///
/// The existence half of a source guard. A barred list that names a function
/// nobody defines any more guards nothing, and `text.contains("pub async fn
/// {name}(")` cannot tell a declaration from a doc comment or a test fixture
/// that quotes one — so the list keeps looking covered across the very rename
/// that emptied it.
///
/// Unlike [`functions`] this does not insist on column 0: a declaration inside
/// an `impl` block is still a declaration. It does insist the line *starts*
/// with it, so a name mentioned mid-expression is not one.
#[allow(dead_code)]
pub fn declarations(text: &str) -> Vec<Declaration> {
    rust_code_only(text)
        .lines()
        .enumerate()
        .filter_map(|(n, line)| {
            parse_declaration(line.trim_start()).map(|(name, visibility, is_async)| Declaration {
                name,
                line: n + 1,
                visibility,
                is_async,
            })
        })
        .collect()
}

/// Whether `text` declares `name` as a `pub async fn` — the spelling every
/// barred-primitive list is written against.
#[allow(dead_code)]
pub fn declares_public_async(text: &str, name: &str) -> bool {
    declarations(text)
        .iter()
        .any(|d| d.name == name && d.is_async && d.visibility == FnVisibility::Public)
}

#[test]
fn declaration_scan_ignores_non_code_mentions_and_keeps_live_declarations() {
    let source = r###"//! `pub async fn delete_asset(` used to be defined here.

/// Superseded by `get_release`; the old spelling was
/// `pub async fn sign_asset_attestation(`.
pub async fn get_release(id: i64) {
    let _fixture = r##"
pub async fn delete_asset(id: i64) {}
"##;
}

pub(crate) async fn check_read(id: i64) {}

async fn require_instance_admin(id: i64) {}

impl Service {
    pub async fn create_release(id: i64) {}
}
"###;

    // What the guards used to grep for is present for all three of the names
    // that no longer exist — which is exactly how the raw-text check stayed
    // green across the rename it was there to catch.
    assert!(source.contains("pub async fn delete_asset("));
    assert!(source.contains("pub async fn sign_asset_attestation("));

    assert!(declares_public_async(source, "get_release"));
    assert!(declares_public_async(source, "create_release"));
    assert!(!declares_public_async(source, "delete_asset"));
    assert!(!declares_public_async(source, "sign_asset_attestation"));

    let declared = declarations(source);
    assert!(declared
        .iter()
        .any(|d| d.name == "check_read" && d.is_async && d.visibility == FnVisibility::Crate));
    assert!(declared.iter().any(|d| d.name == "require_instance_admin"
        && d.is_async
        && d.visibility == FnVisibility::Private));
    assert!(!declared.iter().any(|d| d.name == "delete_asset"));
}

/// Every top-level function in `text`, in source order.
///
/// Boundaries come from the byte-aligned code-only view; each returned body is
/// still copied from the original source so guards can inspect literal values.
#[allow(dead_code)]
pub fn functions(text: &str) -> Vec<Function> {
    let code = rust_code_only(text);
    let mut out: Vec<Function> = Vec::new();
    let mut open: Option<usize> = None;
    for (n, (line, code_line)) in text.lines().zip(code.lines()).enumerate() {
        if open.is_none() {
            if let Some((name, is_handler)) = declared_fn(code_line) {
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
            if code_line == "}" {
                open = None;
            }
        }
    }
    out
}

#[test]
fn function_boundaries_ignore_non_code_decoys_and_keep_original_bodies() {
    const SAMPLE: &str = r#####"pub async fn first() {
    let _normal = "
}
pub async fn normal_decoy() {}
";
    let _raw = r#"
}
pub async fn raw_decoy() {}
"#;
    let _bytes = b"
}
pub async fn byte_decoy() {}
";
    let _raw_bytes = br##"
}
pub async fn raw_byte_decoy() {}
"##;
    /*
}
pub async fn comment_decoy() {}
    */
    live_call("literal contents stay in Function.body");
}
pub(crate) async fn second() {
}
"#####;

    let functions = functions(SAMPLE);
    assert_eq!(
        functions
            .iter()
            .map(|function| function.name.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(functions[0].is_handler);
    assert!(functions[1].is_handler);
    let second_at = SAMPLE.find("pub(crate) async fn second()").unwrap();
    assert_eq!(functions[0].body, SAMPLE[..second_at]);
    assert_eq!(functions[1].body, SAMPLE[second_at..]);
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

/// A production call to `module::name(` in `body`, allowing a longer path
/// before `module` while rejecting identifier-prefix collisions and non-code
/// Rust text.
///
/// The view is [`production_rust_code_only`] rather than [`rust_code_only`]
/// because the question this answers is always "does anything *shipped* still
/// call this": a reverse liveness check that counts a `#[cfg(test)]` fixture
/// keeps a standing exemption green after the last production call site is
/// deleted, which is the quiet half of the same false green a comment or a
/// call-shaped literal produces (card_2624b261cef7).
#[allow(dead_code)]
pub fn production_calls_qualified(body: &str, module: &str, name: &str) -> bool {
    calls_in_code(
        &production_rust_code_only(body),
        &format!("{module}::{name}"),
    )
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
///
/// Both the declaration line and the parameter list are read off the
/// byte-aligned [`rust_code_only`] view, and only the returned argument text is
/// sliced from the original source. A parameter may carry an attribute and an
/// attribute may carry a raw string, so `#[doc = r#"{"label": "reader,)"}"#]`
/// used to close the list at the `,)` *inside the data*: everything behind it
/// left the signature, and the handler was read as taking no gate at all — one
/// consumer then reported a false rank, another dropped the handler from its
/// sweep, both while staying green. Reading structure from the masked view also
/// means a decoy `pub async fn …(` spelled at column 0 inside a raw string is
/// no longer a declaration.
#[allow(dead_code)]
pub fn signature_params(text: &str, name: &str) -> Option<Vec<String>> {
    let code = rust_code_only(text);
    let mut offset = 0usize;
    for line in code.lines() {
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
            return call_args_from_code(text, &code, offset + open);
        }
        offset += line.len() + 1;
    }
    None
}

#[test]
fn signature_reading_survives_delimiter_shaped_parameter_attributes() {
    let source = r###"pub async fn list_assets(
    #[doc = r#"{"label": "reader,)"}"#] user: AuthUser,
    Path(id): Path<i64>,
) -> Json<Value> {
    let _fixture = r##"
pub async fn delete_asset(id: i64) {}
"##;
}
"###;

    // What a quote-only scan trips over, and what a raw-text line scan reads as
    // a second declaration — both are present, which is how the truncated
    // reading stayed green.
    assert!(source.contains(",)"));
    assert!(source.contains("\npub async fn delete_asset(id: i64) {}"));

    let params = signature_params(source, "list_assets").expect("handler is declared here");
    assert_eq!(params.len(), 2);
    assert!(params[0].ends_with("user: AuthUser"));
    assert_eq!(param_type(&params[1]), Some("Path<i64>"));
    assert_eq!(param_base_types(&params), vec!["AuthUser", "Path"]);

    // The declaration inside the raw string is data, not a signature.
    assert!(signature_params(source, "delete_asset").is_none());
}

/// The declared type of one parameter — the text after the `:` that separates
/// pattern from type at nesting depth zero.
///
/// The depth matters: `Path((owner, name, number)): Path<(String, String, i64)>`
/// and `RepoAuthRead { repo, actor_id }: RepoAuthRead` both carry colons inside
/// the *pattern*, and splitting at the first one reads a field name as the type.
/// So does `state: axum::extract::State<AppState>`, whose `::` pairs are not
/// separators at all.
///
/// The depth is counted on the byte-aligned [`rust_code_only`] view for the
/// same reason [`signature_params`] reads the list there: a parameter attribute
/// carries arbitrary data, and `#[doc = r#"reader,)"#] user: AuthUser` leaves
/// the raw count at depth −1 by the time it reaches the separating `:` — so the
/// parameter reports no type at all, and a gated handler reads as ungated.
#[allow(dead_code)]
pub fn param_type(param: &str) -> Option<&str> {
    let code = rust_code_only(param);
    let bytes = code.as_bytes();
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
