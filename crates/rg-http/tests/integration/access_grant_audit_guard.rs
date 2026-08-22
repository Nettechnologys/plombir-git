//! Source guard: an endpoint that hands out repository access writes a journal
//! entry (card_06393f036456).
//!
//! `audit_log` is what an incident is reconstructed from — the one this phase
//! was raised from was read straight off it (`org.create → team.create →
//! repo.create`, and no `org.add_member`). Five kinds of endpoint grant access
//! to a repository, and between them they wrote nine entries, all nine from
//! `api::orgs`. Adding a collaborator — the commonest grant on any instance —
//! wrote nothing at all, and neither did the push allow-list of a protected
//! branch, a protected tag's, or the approver list of a deployment
//! environment. An owner could put themselves on the exception list of `main`,
//! push, and take themselves off again with no line anywhere saying so.
//!
//! Twelve call sites were added. The sixth kind of endpoint would have been
//! written without one, so the rule is mechanical rather than a convention in a
//! comment — the same reason `audit_writer_guard` exists next door.
//!
//! ## What the census is, and why it is derived rather than listed
//!
//! A repository access grant is stored in exactly two places: the normalised
//! `user_grants` rows, written only through [`GRANT_WRITES`]`[0]`, and the
//! `repo_collaborators` row, whose creation is [`GRANT_WRITES`]`[1]`. A hard-coded
//! list of *handlers* would be the thing that goes stale — so the writers are
//! found by starting from those two shapes and closing over the call graph
//! backwards through `rg-core` and `rg-db`, hop by hop, until it stops growing.
//! A new op wrapping `user_grants::replace`, or a new service layer above one,
//! joins the census on its own; the handler that then calls it has to journal.
//!
//! Deletes sit outside this rule and are journalled anyway. They are a real
//! access change — removing a protection rule removes the gate in front of a
//! branch — but the row they delete is the rule, not the grant, so they cannot
//! be derived from the two shapes above without naming each one by hand, which
//! is the staleness this file is built to avoid.
//!
//! ## Why the journal call is checked by name
//!
//! `api::access_audit::record_grant` is the one door; `rg_core::audit::record`
//! is the door beneath it that `api::orgs` and `api::repos` use directly. Either
//! satisfies the rule — what it is about is whether a row is written at all.

use std::collections::BTreeSet;
use std::fs;

use crate::common::source_scan::{
    calls, crate_relative, functions, production_rust_code_only, rust_files, workspace_crates,
};

/// How a grant write is recognised in source.
///
/// Two matchers rather than one, because the two grants are written in two
/// grammatical shapes and a single scan gets one of them wrong. A call is
/// matched as a call — so an identifier that merely *starts* with the name does
/// not count — while a struct literal has no `(` to anchor on and is matched as
/// a substring of the same comment- and literal-blanked view.
enum GrantWrite {
    /// `user_grants::replace(...)`.
    Call(&'static str),
    /// `repo_collaborator::ActiveModel { ... }`.
    Construction(&'static str),
}

impl GrantWrite {
    fn found_in(&self, body: &str) -> bool {
        match self {
            Self::Call(name) => calls(body, name),
            Self::Construction(shape) => production_rust_code_only(body).contains(shape),
        }
    }

    fn shape(&self) -> &'static str {
        let (Self::Call(shape) | Self::Construction(shape)) = self;
        shape
    }
}

/// The three shapes that write a repository access grant, and why each is the
/// grant rather than merely near it:
///
/// - `user_grants::replace` is the only writer of the normalised grant rows the
///   push, tag and approval gates read (`rg-db/src/user_grants.rs`);
/// - a `repo_collaborators` row *is* the collaborator's access, so building one
///   is the grant;
/// - a `deploy_keys` row with `read_only: false` *is* push access to one
///   repository — `rg-ssh` reads the column and lets `git-receive-pack`
///   through on it — so building one is the grant too (card_2a9beaf7b207).
const GRANT_WRITES: [GrantWrite; 3] = [
    GrantWrite::Call("user_grants::replace"),
    GrantWrite::Construction("repo_collaborator::ActiveModel"),
    GrantWrite::Construction("deploy_key::ActiveModel"),
];

/// Where the writers are looked for: everything the handlers call into.
const WRITER_CRATES: [&str; 2] = ["rg-core/src", "rg-db/src"];

/// Where the handlers are.
const HANDLER_DIR: &str = "rg-http/src/api";

/// Either spelling of "a row was written to `audit_log`".
const JOURNAL_CALLS: [&str; 2] = ["record_grant", "record"];

/// Production functions of `WRITER_CRATES` that write a grant, closed over the
/// call graph until the set stops growing.
fn grant_writers() -> BTreeSet<String> {
    let mut sources: Vec<(String, String)> = Vec::new();
    for crate_dir in WRITER_CRATES {
        let mut files = Vec::new();
        rust_files(&workspace_crates().join(crate_dir), &mut files);
        for file in files {
            let text = fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
            sources.push((crate_relative(&file), text));
        }
    }
    assert!(
        !sources.is_empty(),
        "no source was read out of {WRITER_CRATES:?}, so every verdict below means nothing"
    );

    let mut writers: BTreeSet<String> = BTreeSet::new();
    // Seed: a function that spells one of the two grant-writing shapes itself.
    // The shapes are matched as calls / constructions in production code, so a
    // comment or a fixture literal naming one cannot enrol a function.
    for (_, text) in &sources {
        for function in functions(text) {
            if GRANT_WRITES
                .iter()
                .any(|write| write.found_in(&function.body))
            {
                writers.insert(function.name.clone());
            }
        }
    }
    assert!(
        !writers.is_empty(),
        "no function under {WRITER_CRATES:?} writes {:?} any more — the scan, not the code, is \
         what broke",
        GRANT_WRITES
            .iter()
            .map(GrantWrite::shape)
            .collect::<Vec<_>>()
    );

    // Closure: anything calling a known writer is one too, so a service layer
    // between the handler and the op does not hide the grant from this rule.
    loop {
        let mut grew = false;
        for (_, text) in &sources {
            for function in functions(text) {
                if writers.contains(&function.name) {
                    continue;
                }
                if writers.iter().any(|writer| calls(&function.body, writer)) {
                    writers.insert(function.name.clone());
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    writers
}

#[test]
fn every_endpoint_that_grants_repository_access_writes_a_journal_entry() {
    let writers = grant_writers();

    let mut files = Vec::new();
    rust_files(&workspace_crates().join(HANDLER_DIR), &mut files);
    assert!(
        !files.is_empty(),
        "no handler module was read out of {HANDLER_DIR}"
    );

    let mut granting = Vec::new();
    let mut offenders = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        let path = crate_relative(&file);
        for handler in functions(&text) {
            if !handler.is_handler {
                continue;
            }
            // Two ways to be granting, because the grants are written in two
            // places. The collaborator row and the normalised grant rows are
            // built down in `rg-core`/`rg-db`, so those handlers are found by
            // the call graph above; the deploy key is built by the handler
            // itself, and a census that only followed calls would never see it
            // — which is how the sixth way of handing out repository access sat
            // outside this rule (card_2a9beaf7b207).
            let writer = match writers.iter().find(|writer| calls(&handler.body, writer)) {
                Some(writer) => writer.clone(),
                None => {
                    let Some(write) = GRANT_WRITES
                        .iter()
                        .find(|write| write.found_in(&handler.body))
                    else {
                        continue;
                    };
                    write.shape().to_string()
                }
            };
            granting.push(format!("{path}::{}", handler.name));
            if !JOURNAL_CALLS
                .iter()
                .any(|journal| calls(&handler.body, journal))
            {
                offenders.push(format!(
                    "{path}:{} `{}` reaches `{writer}`, which writes a repository access grant, \
                     and writes nothing to `audit_log`. Resolve the actor with \
                     `access_audit::grant_actor` *before* the change and call \
                     `access_audit::record_grant` after it — an allow-list goes in whole and by \
                     name, not as the delta this request happened to carry.",
                    handler.line, handler.name
                ));
            }
        }
    }

    // Liveness floor. Without it, a scan that stopped matching anything would
    // report a clean tree — which is the failure this file cannot afford, since
    // its whole subject is an absence.
    assert!(
        granting.len() >= 9,
        "only {} endpoint(s) were found to grant repository access ({granting:?}); the census, \
         not the tree, is what changed",
        granting.len()
    );
    assert!(
        offenders.is_empty(),
        "{} endpoint(s) hand out repository access without journalling it:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}
