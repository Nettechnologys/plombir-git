//! Opening a Git repository is a decision Plombir Git makes, not the host
//! (card_aec3bc83e6ea, phase "Host Git Config Must Not Steer the Server").
//!
//! `gix::open` applies `gix::open::Permissions::secure()`, and "secure" there
//! is `config: all`, `env: all`, `attributes: all` — every repository opened
//! that way reads `/etc/gitconfig`, the `~/.gitconfig` of whichever account
//! the server process runs under, that process's own `GIT_*` variables, and
//! the system and global `gitattributes`. That is the right default for a
//! person's own checkout and the wrong one for a server: an operator who set
//! `merge.renames = false` for their own convenience would be steering what
//! Plombir Git does inside *other people's* repositories, and two instances on
//! differently configured hosts would answer the same request differently
//! without either of them saying so.
//!
//! `rg_git::repository::open` is the one way in. It was introduced for the
//! merge path (card_318ec3e56901) while thirty-one other call sites still
//! opened bare, which is the state this guard exists to stop returning to: a
//! single point is worth exactly as much as whatever keeps the next call site
//! from going around it.
//!
//! ## Why the whole family, and not just `gix::open`
//!
//! The rule is about *permissions*, so it has to name every constructor that
//! picks them silently. `gix::discover` walks upward and then opens with the
//! same `secure()` set; `gix::init` and `gix::init_bare` create and then open
//! it; `ThreadSafeRepository` has its own copy of each. Watching one spelling
//! would leave five ways to reintroduce the defect while the guard stayed
//! green — and `init_bare`, which every fixture in the workspace uses, is
//! exactly the one a future production path would reach for first.
//!
//! `gix::open_opts` is on the list too, even though it is the primitive that
//! takes the permissions as an argument and cannot pick up the host's on its
//! own. The rule this guard enforces is not only "isolated permissions" but
//! "one place decides": a second `open_opts` a crate over is a second
//! independent answer to the same question, and the way back to thirty-nine of
//! them is one correct-looking copy at a time. The single point is exempt
//! because it *is* that place.
//!
//! ## What this guard cannot see
//!
//! A `git` *subprocess* reads the host's configuration through its own
//! mechanism, and none of these names appear on that path. That half of the
//! class is tracked separately — the disarming environment in
//! `rg_git::credentials` is what covers it, and where it does not reach is
//! recorded on its own cards.

use std::fs;

use crate::common::source_scan::{
    crate_relative, is_ident_char, production_rust_code_only, rust_files, workspace_crates,
};

/// The single point, spelled as a path under `crates/`.
const SINGLE_POINT: &str = "rg-git/src/repository.rs";

/// Every `gix` constructor that settles repository permissions — the ones that
/// pick the host's silently, plus the one that would state a second set.
///
/// Spelled as full paths rather than bare function names, so a local binding
/// or a method of the same name on some other type is not mistaken for one of
/// them.
const HOST_PERMISSION_OPENERS: &[&str] = &[
    "gix::open",
    "gix::open_opts",
    "gix::discover",
    "gix::discover_opts",
    "gix::init",
    "gix::init_bare",
    "gix::ThreadSafeRepository::open",
    "gix::ThreadSafeRepository::open_opts",
    "gix::ThreadSafeRepository::discover",
];

/// How the rest of the workspace is expected to reach the single point.
///
/// Two spellings for one function: `rg-git` reaches its own module through
/// `crate::`, everyone else through the crate name.
const SINGLE_POINT_CALLS: &[&str] = &["rg_git::repository::open", "crate::repository::open"];

/// Every `src/` file of every crate, as `(<crate>/src/<path>, contents)`.
fn workspace_sources() -> Vec<(String, String)> {
    let crates = workspace_crates();
    let mut sources = Vec::new();
    for entry in fs::read_dir(&crates).expect("read crates/") {
        let src = entry.expect("dir entry").path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for file in files {
            sources.push((
                crate_relative(&file),
                fs::read_to_string(&file).expect("read source"),
            ));
        }
    }
    sources
}

/// The 1-based lines on which production Rust in `code` calls `name`.
///
/// `code` is already a production code-only view, so a doc comment naming
/// `gix::open` — this file's own prose, and several explanatory comments left
/// beside the converted call sites — cannot be read as a call. A name counts
/// only when it ends on an identifier boundary and the next non-space
/// character opens an argument list: `gix::open_opts(` is a different function
/// and must not be read as `gix::open`.
fn call_lines(code: &str, name: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    for (at, _) in code.match_indices(name) {
        let after = at + name.len();
        if code[after..]
            .chars()
            .next()
            .is_some_and(|next| is_ident_char(next) || next == ':')
        {
            continue;
        }
        if code[after..].trim_start().starts_with('(') {
            lines.push(code[..at].bytes().filter(|byte| *byte == b'\n').count() + 1);
        }
    }
    lines
}

#[test]
fn no_production_code_opens_a_repository_on_the_host_s_terms() {
    let sources = workspace_sources();
    assert!(
        sources.len() > 100,
        "the census read {} workspace source files — it is not walking `crates/*/src` any more",
        sources.len()
    );

    let mut offenders = Vec::new();
    for (path, source) in &sources {
        if path == SINGLE_POINT {
            continue;
        }
        let code = production_rust_code_only(source);
        for opener in HOST_PERMISSION_OPENERS {
            for line in call_lines(&code, opener) {
                offenders.push(format!("{path}:{line} calls `{opener}`"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "production code opens a Git repository with the host's `Permissions::secure()` — \
         /etc/gitconfig, the server account's ~/.gitconfig, the process's GIT_* and the \
         system gitattributes all steer what Plombir Git does inside other people's \
         repositories. Open through `rg_git::repository::open` instead (card_aec3bc83e6ea):\n  {}",
        offenders.join("\n  ")
    );
}

/// The absence above is only worth something if the reader can still see a
/// call at all.
///
/// A census that stopped finding `gix::open` because its matcher broke, or
/// because it walked an empty tree, reports the same clean result as a
/// workspace that genuinely has none. So both halves are pinned: the single
/// point still holds the one allowed construction, and the crates that were
/// converted still reach it.
#[test]
fn the_single_point_is_the_one_place_that_states_the_permissions() {
    let sources = workspace_sources();

    let (_, single_point) = sources
        .iter()
        .find(|(path, _)| path == SINGLE_POINT)
        .unwrap_or_else(|| panic!("`{SINGLE_POINT}` is missing — the single point moved"));
    let code = production_rust_code_only(single_point);

    assert_eq!(
        call_lines(&code, "gix::open_opts").len(),
        1,
        "`{SINGLE_POINT}` no longer opens the repository itself — either the single point \
         moved, or this census has stopped reading it"
    );
    assert!(
        code.contains("gix::open::Options::isolated()"),
        "`{SINGLE_POINT}` opens with permissions other than `Options::isolated()`, so the \
         host is back in the loop even though every call site goes through one function"
    );

    for owner in ["rg-http", "rg-core", "rg-ci", "rg-git"] {
        let reached: usize = sources
            .iter()
            .filter(|(path, _)| path.starts_with(&format!("{owner}/src/")))
            .map(|(_, source)| {
                let code = production_rust_code_only(source);
                SINGLE_POINT_CALLS
                    .iter()
                    .map(|call| call_lines(&code, call).len())
                    .sum::<usize>()
            })
            .sum();
        assert!(
            reached > 0,
            "no production file in `{owner}` opens a repository through the single point any \
             more — either the crate stopped opening repositories, or it went back around \
             `rg_git::repository::open`"
        );
    }
}
