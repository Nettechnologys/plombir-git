//! Source guard: `audit_log` rows are built in exactly one module.
//!
//! The workspace used to have four byte-identical copies of `record_audit`, one
//! each in `api::{admin, orgs, users, repos}`, every one of them assembling its
//! own `audit_log::ActiveModel`. They never drifted in mechanics. They drifted
//! in *meaning*: the actor column ended up holding a real username, the numeric
//! `claims.sub`, an empty string, and — in `orgs.rs`, with a comment admitting
//! it — the name of the organization being acted on. `/admin/audit` renders that
//! column as `{username} (#{user_id})`, so one list showed `alice (#7)`,
//! `7 (#7)`, a blank actor, and `acme-corp (#3)`
//! (card_fcc07f8d1505, card_51f6f3a99003).
//!
//! Four copies produced four conventions. The fifth module would have produced a
//! fifth. So the rule is mechanical: `audit_log::ActiveModel` may be constructed
//! only under `rg-core/src/audit/`, and this test is what says so — the defect
//! it guards is a *convention* nobody is obliged to read, and no request can
//! exercise a rule that only exists in a comment.
//!
//! Deliberately a grep and not a compile-time property: sea-orm's `ActiveModel`
//! is generated `pub` for every entity, so there is nothing to make private.
//! Tests are out of scope — a fixture seeding audit rows is a fixture, not a
//! writer, and demanding they route through the writer would only make them
//! lie about how rows arrive.
//!
//! The actor's *value* is held by the type system rather than by this file:
//! `rg_core::audit::AuditActor` has no constructor that takes a name, so an org
//! name or an id cannot reach the column even from inside the one writer. This
//! guard keeps that single door from being bypassed; `AuditActor` keeps what
//! goes through it honest.

use std::fs;
use std::path::{Path, PathBuf};

/// The one directory allowed to build an `audit_log` row.
///
/// A directory rather than a file because the archiver legitimately rebuilds
/// rows it previously archived off — restoring a row it wrote earlier is the
/// same concern, and it sits beside the writer.
const WRITER_DIR: &str = "rg-core/src/audit/";

/// `crates/` — the parent of this crate's directory.
fn workspace_crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rg-http lives under crates/")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read directory") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Every `src/` file of every crate, as `<crate>/src/<path>` strings.
fn workspace_sources() -> Vec<(String, String)> {
    let crates = workspace_crates();
    let mut sources = Vec::new();
    for entry in fs::read_dir(&crates).expect("read crates/") {
        let krate = entry.expect("dir entry").path();
        let src = krate.join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for file in files {
            let relative = file
                .strip_prefix(&crates)
                .expect("file under crates/")
                .to_string_lossy()
                .replace('\\', "/");
            sources.push((relative, fs::read_to_string(&file).expect("read source")));
        }
    }
    sources
}

/// The rule itself.
#[test]
fn only_the_audit_module_builds_an_audit_log_row() {
    let mut offenders = Vec::new();
    let mut writers = 0usize;

    for (path, source) in workspace_sources() {
        // The entity definition is where `ActiveModel` comes *from*, not a use
        // of it.
        if path.contains("/src/entities/") {
            continue;
        }
        for (number, line) in source.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if !line.contains("audit_log::ActiveModel") {
                continue;
            }
            if path.starts_with(WRITER_DIR) {
                writers += 1;
            } else {
                offenders.push(format!("{path}:{}", number + 1));
            }
        }
    }

    assert!(
        writers >= 1,
        "the scan found no `audit_log::ActiveModel` under {WRITER_DIR} at all — it stopped \
         matching the source layout and is no longer checking anything"
    );
    assert!(
        offenders.is_empty(),
        "an `audit_log` row may only be built in {WRITER_DIR}, through \
         `rg_core::audit::record`, which is what keeps one meaning in the actor column. \
         Four private copies of that construction are how the column came to hold usernames, \
         ids, empty strings and organization names at once. Build these through the writer: \
         {offenders:#?}"
    );
}

/// The other half: no module may grow its own `record_audit` again.
///
/// Distinct from the rule above rather than implied by it — a fresh copy that
/// called `rg_core::audit::record` internally would pass the first check while
/// re-introducing exactly the per-module wrapper whose divergence is the defect.
#[test]
fn no_module_keeps_a_private_audit_writer_of_its_own() {
    let offenders: Vec<String> = workspace_sources()
        .into_iter()
        .flat_map(|(path, source)| {
            source
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains("fn record_audit"))
                .map(|(number, _)| format!("{path}:{}", number + 1))
                .collect::<Vec<_>>()
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "a per-module `record_audit` is what this phase removed — call \
         `rg_core::audit::record` with a `rg_core::audit::AuditActor` directly: {offenders:#?}"
    );
}
