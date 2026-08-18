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
//!
//! Tests are out of scope — a fixture seeding audit rows is a fixture, not a
//! writer, and demanding they route through the writer would only make them lie
//! about how rows arrive. Both censuses below therefore read
//! [`production_rust_code_only`]: comments, literals and complete
//! `#[cfg(test)]` items are blanked, byte-for-byte, so the line numbers they
//! report still address the file as written. Reading the raw text instead was
//! wrong in both directions, and the quieter direction was live
//! (card_dfd5da074447): `archiver.rs` builds two `audit_log::ActiveModel` rows
//! inside its own test module, which is two thirds of what held the liveness
//! floor below up — delete the one production construction in `audit.rs` and
//! the floor would have stayed green over a guard that had stopped watching
//! anything. The louder direction is a false red: the old scan skipped a `//`
//! line and nothing else, so any block comment or fixture literal spelling
//! either name — in any of the twenty decoy fixtures this tree already carries
//! — would have named an offender that does not exist.
//!
//! The actor's *value* is held by the type system rather than by this file:
//! `rg_core::audit::AuditActor` has no constructor that takes a name, so an org
//! name or an id cannot reach the column even from inside the one writer. This
//! guard keeps that single door from being bypassed; `AuditActor` keeps what
//! goes through it honest.

use std::fs;

use crate::common::source_scan::{
    crate_relative, declarations, production_rust_code_only, rust_files, workspace_crates,
};

/// The one directory allowed to build an `audit_log` row.
///
/// A directory rather than a file because the archiver legitimately rebuilds
/// rows it previously archived off — restoring a row it wrote earlier is the
/// same concern, and it sits beside the writer.
const WRITER_DIR: &str = "rg-core/src/audit/";

/// The construction the rule is about, as it is spelled in code.
const CONSTRUCTION: &str = "audit_log::ActiveModel";

/// The name every one of the four private copies went by.
const WRITER_FN: &str = "record_audit";

/// Every `src/` file of every crate, as `<crate>/src/<path>` strings.
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

/// The 1-based lines on which production Rust in `source` names the `audit_log`
/// row type at all — a construction, an import, a type annotation.
///
/// Deliberately wider than [`audit_row_construction_lines`], because this is
/// the half that names offenders: a module outside the writer has no business
/// naming the type in any position. An `ActiveModel` import in `api/orgs.rs` is
/// a private copy being prepared, and reading it as harmless would let the next
/// one land one line at a time.
fn audit_row_mention_lines(source: &str) -> Vec<usize> {
    production_rust_code_only(source)
        .lines()
        .enumerate()
        .filter(|(_, code)| code.contains(CONSTRUCTION))
        .map(|(number, _)| number + 1)
        .collect()
}

/// The 1-based lines on which production Rust in `source` actually *builds* an
/// `audit_log` row — the type followed by the brace of its struct literal.
///
/// The narrower half, and it is what the liveness floor is allowed to rest on.
/// A mention cannot answer "is a row still built here": renaming the one
/// construction in `audit.rs` to `AuditRow` through an aliased import left the
/// old spelling on the `use` line, and a floor counting mentions stayed green
/// over a writer that no longer contained the construction it was watching.
fn audit_row_construction_lines(source: &str) -> Vec<usize> {
    production_rust_code_only(source)
        .lines()
        .enumerate()
        .filter(|(_, code)| {
            code.match_indices(CONSTRUCTION).any(|(at, _)| {
                code[at + CONSTRUCTION.len()..]
                    .trim_start()
                    .starts_with('{')
            })
        })
        .map(|(number, _)| number + 1)
        .collect()
}

/// The 1-based lines on which production Rust in `source` declares an audit
/// writer of its own.
///
/// A declaration, not a substring: `contains("fn record_audit")` matches a doc
/// comment describing the copies this phase removed and a fixture quoting one
/// just as readily as a real function. The prefix match is deliberate — the
/// defect is a per-module wrapper, and `record_audit_entry` would be the same
/// wrapper under a name the exact spelling would miss.
fn private_audit_writer_lines(source: &str) -> Vec<usize> {
    declarations(&production_rust_code_only(source))
        .into_iter()
        .filter(|declared| declared.name.starts_with(WRITER_FN))
        .map(|declared| declared.line)
        .collect()
}

/// The rule itself.
#[test]
fn only_the_audit_module_builds_an_audit_log_row() {
    let mut offenders = Vec::new();
    let mut writers = Vec::new();
    let mut scanned = 0usize;

    for (path, source) in workspace_sources() {
        // The entity definition is where `ActiveModel` comes *from*, not a use
        // of it.
        if path.contains("/src/entities/") {
            continue;
        }
        scanned += 1;
        if path.starts_with(WRITER_DIR) {
            writers.extend(
                audit_row_construction_lines(&source)
                    .into_iter()
                    .map(|number| format!("{path}:{number}")),
            );
        } else {
            offenders.extend(
                audit_row_mention_lines(&source)
                    .into_iter()
                    .map(|number| format!("{path}:{number}")),
            );
        }
    }

    // An empty offender list means one of two things — nobody builds a row
    // outside the writer, or the walk never ran — and only this tells them
    // apart.
    assert!(
        scanned > 50,
        "only {scanned} file(s) scanned — the guard is not running"
    );
    assert!(
        !writers.is_empty(),
        "the scan found no production `{CONSTRUCTION}` construction — the type followed by the \
         `{{` of its struct literal — anywhere under {WRITER_DIR}. The row is built somewhere \
         this guard cannot see, and until it can it is checking nothing. An import or a type \
         annotation deliberately does not count here: leaving the old spelling on a `use` line \
         is exactly how the construction left `audit.rs` while the floor stayed green"
    );
    assert!(
        offenders.is_empty(),
        "the `audit_log` row type is named outside {WRITER_DIR}, where a row may only be \
         built — through `rg_core::audit::record`, which is what keeps one meaning in the actor \
         column. Four private copies of that construction are how the column came to hold usernames, \
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
    let mut offenders = Vec::new();
    let mut scanned = 0usize;

    for (path, source) in workspace_sources() {
        scanned += 1;
        offenders.extend(
            private_audit_writer_lines(&source)
                .into_iter()
                .map(|number| format!("{path}:{number}")),
        );
    }

    // Nothing in the tree declares a `record_audit` any more, so this check has
    // no positive control of its own: an empty offender list is the state it is
    // defending, and a walk that read no files would produce the same list.
    assert!(
        scanned > 50,
        "only {scanned} file(s) scanned — the guard is not running"
    );
    assert!(
        offenders.is_empty(),
        "a per-module `{WRITER_FN}` is what this phase removed — call \
         `rg_core::audit::record` with a `rg_core::audit::AuditActor` directly: {offenders:#?}"
    );
}

/// A fixture the censuses have to survive, because the tree already carries its
/// shapes.
///
/// Every decoy below is text a raw-line scan reads as code: a block comment, a
/// normal / raw / byte literal, a multi-line raw fixture whose own lines start
/// in column 0, and an inline `#[cfg(test)]` module holding both a writer
/// declaration and a row construction. The live lines sit *after* the decoys,
/// so a scan that stops at the first match cannot pass either.
#[test]
fn the_censuses_read_production_code_and_not_prose_data_or_fixtures() {
    const SAMPLE: &str = r####"
// let row = audit_log::ActiveModel { ..Default::default() };
/* async fn record_audit(actor: i64) {} builds an audit_log::ActiveModel */
const NORMAL: &str = "audit_log::ActiveModel";
const RAW: &str = r#"async fn record_audit(actor: i64)"#;
const BYTES: &[u8] = b"audit_log::ActiveModel";
const FIXTURE: &str = r#"
async fn record_audit(actor: i64) {}
let row = audit_log::ActiveModel { ..Default::default() };
"#;

#[cfg(test)]
mod tests {
    async fn record_audit(actor: i64) {}

    fn seed() {
        let _fixture_row = audit_log::ActiveModel {
            ..Default::default()
        };
    }
}

fn live_writer() {
    let _row = audit_log::ActiveModel {
        ..Default::default()
    };
}

pub(crate) async fn record_audit_entry(actor: i64) {}

use rg_db::entities::audit_log::ActiveModel as AuditRow;
"####;

    let live_construction = SAMPLE
        .lines()
        .position(|line| line.contains("let _row = audit_log::ActiveModel {"))
        .map(|n| n + 1)
        .expect("the sample builds one production row");
    let live_writer = SAMPLE
        .lines()
        .position(|line| line.contains("pub(crate) async fn record_audit_entry"))
        .map(|n| n + 1)
        .expect("the sample declares one production writer");

    // What the raw-line scans this replaced would have counted, and why each
    // half of the old form was wrong: seven `CONSTRUCTION` lines where one is
    // code, four `fn record_audit` lines where one is a declaration.
    assert!(
        SAMPLE
            .lines()
            .filter(|line| line.contains(CONSTRUCTION))
            .count()
            > 2,
        "the fixture lost its non-code decoys and can no longer fail on them"
    );
    assert!(
        SAMPLE
            .lines()
            .filter(|line| line.contains(&format!("fn {WRITER_FN}")))
            .count()
            > 2,
        "the fixture lost its writer decoys and can no longer fail on them"
    );

    let aliased_import = SAMPLE
        .lines()
        .position(|line| line.contains("as AuditRow;"))
        .map(|n| n + 1)
        .expect("the sample imports the row type under an alias");

    assert_eq!(
        audit_row_construction_lines(SAMPLE),
        vec![live_construction],
        "a comment, a literal, a `#[cfg(test)]` fixture or a bare import is being counted as a \
         row construction — or the live one after them was lost"
    );
    assert_eq!(
        audit_row_mention_lines(SAMPLE),
        vec![live_construction, aliased_import],
        "the wider census is what names offenders, and an import naming the row type outside \
         the writer is one — a private copy takes two lines to land"
    );
    assert_eq!(
        private_audit_writer_lines(SAMPLE),
        vec![live_writer],
        "a comment, a literal or a `#[cfg(test)]` fixture is being counted as a private \
         audit writer — or the live declaration after them was lost"
    );
}
