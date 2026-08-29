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
//! A repository access grant is stored in two tables: the normalised
//! `user_grants` rows and `repo_collaborators`. Their write doors are named in
//! [`GRANT_WRITES`], including both creation and permission changes for a
//! collaborator. A hard-coded list of *handlers* would be the thing that goes
//! stale — so the writers are found by starting from those shapes and closing
//! over the call graph backwards through `rg-core` and `rg-db`, hop by hop,
//! until it stops growing. A new op wrapping `user_grants::replace`, or a new
//! service layer above one, joins the census on its own; the handler that then
//! calls it has to journal.
//!
//! Deletes sit outside this rule and are journalled anyway. They are a real
//! access change — removing a protection rule removes the gate in front of a
//! branch — but the row they delete is the rule, not the grant, so they cannot
//! be derived from the two shapes above without naming each one by hand, which
//! is the staleness this file is built to avoid.
//!
//! ## Why every function, and not only the handlers
//!
//! The final pass used to skip anything that was not `is_handler`. That
//! reasoning holds for where the *grant* is written and not for where the
//! *function boundary* falls: a handler is free to delegate the grant to a
//! private helper beside it, and `grant_writers()` closes over `rg-core` and
//! `rg-db` only, so such a helper is in neither population — not a writer, and
//! not read. The grant would have gone out with nothing mechanical behind it.
//!
//! The sibling guard had the identical hole and it was **not** hypothetical
//! there: `find_or_create_sso_user` sat in it, and the create side of an
//! external-identity credential rested on a behavioural test for months
//! (card_6f301a1a0b18). Here it is latent — measured when the filter came off,
//! exactly zero functions under `rg-http/src/api` reach a grant writer outside
//! a handler body.
//!
//! ## How the reach is proven, since the tree cannot prove it
//!
//! That zero is the whole difficulty. The sibling closed its provability with a
//! floor — "at least one credential write is below a handler, or the filter is
//! back" — and that assertion is red here on the first run, because the
//! population it would count is empty. A change nobody can redden is not
//! evidence in this repository, so the reach is proven against a source that is
//! not on disk instead: [`the_scan_reads_past_the_handler_boundary`] hands the
//! same scan a fabricated module whose grant is written by a private helper,
//! and requires it to be reported. Put the `is_handler` filter back and that
//! test goes red on a tree where nothing else would.
//!
//! ## Why the journal call is checked by name
//!
//! `api::access_audit::record_grant` is the one door; `rg_core::audit::record`
//! is the door beneath it that `api::orgs` and `api::repos` use directly. Either
//! satisfies the rule — what it is about is whether a row is written at all.
//! A write may live one local call below the journal when a race-safe helper
//! accepts a success continuation: every production caller of that helper must
//! contain a journal call, so adding a second silent caller turns the guard red.

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
    /// A qualified write-door call such as `user_grants::replace(...)`.
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

/// The four shapes that write a repository access grant, and why each is the
/// grant rather than merely near it:
///
/// - `user_grants::replace` is the only writer of the normalised grant rows the
///   push, tag and approval gates read (`rg-db/src/user_grants.rs`);
/// - a `repo_collaborators` row *is* the collaborator's access, so both building
///   one and changing its `permission` are grants. The update door is named
///   explicitly because it uses a conditional `update_many`, not an
///   `ActiveModel` construction (`card_2c08bac2dc10`);
/// - a `deploy_keys` row with `read_only: false` *is* push access to one
///   repository — `rg-ssh` reads the column and lets `git-receive-pack`
///   through on it — so building one is the grant too (card_2a9beaf7b207).
const GRANT_WRITES: [GrantWrite; 4] = [
    GrantWrite::Call("user_grants::replace"),
    GrantWrite::Construction("repo_collaborator::ActiveModel"),
    GrantWrite::Call("repo_collaborator_ops::update_permission"),
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

/// Every configured write shape still names production code.
///
/// The endpoint floor catches a write path falling out of the derived census
/// today. This per-shape ratchet keeps the same loss visible after unrelated new
/// endpoints have raised the aggregate above that floor: a refactor of one
/// write door must update the matcher deliberately, not leave a stale string
/// that another grant category masks.
#[test]
fn every_grant_write_shape_is_live() {
    let mut sources = Vec::new();
    for source_dir in WRITER_CRATES.into_iter().chain([HANDLER_DIR]) {
        let mut files = Vec::new();
        rust_files(&workspace_crates().join(source_dir), &mut files);
        for file in files {
            let text = fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
            sources.push((crate_relative(&file), text));
        }
    }
    assert!(!sources.is_empty(), "no grant-writing source was read");

    for write in &GRANT_WRITES {
        let locations: Vec<String> = sources
            .iter()
            .flat_map(|(path, text)| {
                functions(text)
                    .into_iter()
                    .filter(|function| write.found_in(&function.body))
                    .map(move |function| format!("{path}:{}::{}", function.line, function.name))
            })
            .collect();
        assert!(
            !locations.is_empty(),
            "no production function still contains `{}`; update GRANT_WRITES with the new write \
             door before this grant category silently leaves the endpoint census",
            write.shape()
        );
    }
}

/// One production function that hands out a grant, and whether it journalled.
struct GrantSite {
    key: String,
    /// Whether the router can reach this function directly. Read by the
    /// fixture below, which exists to prove that `false` is still scanned.
    below_a_handler: bool,
    offence: Option<String>,
}

/// Every grant-writing function in one source.
///
/// Factored out of the test so the same scan can be pointed at a source that
/// never touches the disk — see [`the_scan_reads_past_the_handler_boundary`].
fn grant_sites(path: &str, text: &str, writers: &BTreeSet<String>) -> Vec<GrantSite> {
    let mut sites = Vec::new();
    let functions = functions(text);
    // Every production function, not only the ones the router can reach: which
    // side of a private helper's boundary the grant lands on is a decision
    // about the code, not about whether handing out access needs a journal
    // entry. See the module header for why this is proven by a fixture.
    for function in &functions {
        // Two routes into the census. Collaborator and normalised-grant writes
        // live down in `rg-core`/`rg-db`, so their callers are found by the call
        // graph above; a deploy key is built in `rg-http/src/api` itself, and a
        // census that only followed calls would never see it — which is how the
        // sixth way of handing out repository access sat outside this rule
        // (card_2a9beaf7b207).
        let writer = match writers.iter().find(|writer| calls(&function.body, writer)) {
            Some(writer) => writer.clone(),
            None => {
                let Some(write) = GRANT_WRITES
                    .iter()
                    .find(|write| write.found_in(&function.body))
                else {
                    continue;
                };
                write.shape().to_string()
            }
        };
        let journalled_here = JOURNAL_CALLS
            .iter()
            .any(|journal| calls(&function.body, journal));
        let callers: Vec<_> = functions
            .iter()
            .filter(|caller| calls(&caller.body, &function.name))
            .collect();
        let journalled_by_every_local_caller = !callers.is_empty()
            && callers.iter().all(|caller| {
                JOURNAL_CALLS
                    .iter()
                    .any(|journal| calls(&caller.body, journal))
            });
        let journalled = journalled_here || journalled_by_every_local_caller;
        sites.push(GrantSite {
            key: format!("{path}::{}", function.name),
            below_a_handler: !function.is_handler,
            offence: (!journalled).then(|| {
                format!(
                    "{path}:{} `{}` reaches `{writer}`, which writes a repository access grant, \
                     and neither it nor every local production caller writes to `audit_log`. \
                     Resolve the actor with \
                     `access_audit::grant_actor` *before* the change and call \
                     `access_audit::record_grant` after it — an allow-list goes in whole and by \
                     name, not as the delta this request happened to carry.",
                    function.line, function.name
                )
            }),
        });
    }
    sites
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
        for site in grant_sites(&crate_relative(&file), &text, &writers) {
            granting.push(site.key);
            if let Some(offence) = site.offence {
                offenders.push(offence);
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

/// The reach this file gained, proven against a source that is not in the tree.
///
/// Zero functions under `rg-http/src/api` currently write a grant outside a
/// handler body, so the sibling guard's floor — "at least one, or the filter is
/// back" — cannot be used here: it would be red on a clean tree. What can be
/// asserted is the scan's own behaviour, and that is what this does. The
/// fabricated module below is exactly the shape the filter used to skip: a
/// private helper, one call under a handler, that hands out access and records
/// nothing.
///
/// The writer it calls is taken from [`grant_writers`] at run time rather than
/// written in here, so this fixture cannot go stale against a rename — and it
/// exercises the call-graph branch, which is the one a helper in this crate
/// falls outside of.
#[test]
fn the_scan_reads_past_the_handler_boundary() {
    let writers = grant_writers();
    let writer = writers
        .iter()
        .next()
        .expect("grant_writers() asserts it is non-empty")
        .clone();

    let silent =
        format!("async fn hand_out_access(state: &AppState) {{\n    {writer}(state).await;\n}}\n");
    let sites = grant_sites("fixture.rs", &silent, &writers);
    let [site] = sites.as_slice() else {
        panic!(
            "the scan found {} grant site(s) in a fixture with exactly one; put the `is_handler` \
             filter back and this is what fails — a grant handed out by a private helper is \
             invisible to this rule again",
            sites.len()
        )
    };
    assert!(
        site.below_a_handler,
        "the fixture's helper was read as a handler, so this test would pass with the filter back \
         in place and proves nothing"
    );
    assert!(
        site.offence.is_some(),
        "a helper that hands out access and journals nothing was not reported"
    );

    // The other direction, so the fixture proves the rule rather than proving
    // that everything is reported.
    let journalled = format!(
        "async fn hand_out_access(state: &AppState) {{\n    {writer}(state).await;\n    \
         record_grant(state).await;\n}}\n"
    );
    let sites = grant_sites("fixture.rs", &journalled, &writers);
    let [site] = sites.as_slice() else {
        panic!(
            "the journalling fixture produced {} site(s), not one",
            sites.len()
        )
    };
    assert!(
        site.offence.is_none(),
        "a helper that journals its grant was reported anyway: {:?}",
        site.offence
    );

    // And a helper that hands out nothing is not a site at all, so the scan is
    // matching the grant rather than matching every function it reads.
    let unrelated = "async fn count_something(state: &AppState) {\n    state.tally().await;\n}\n";
    assert!(
        grant_sites("fixture.rs", unrelated, &writers).is_empty(),
        "a function that writes no grant was counted as one"
    );

    // A race-safe helper may publish the journal through a success continuation
    // in its caller. That is sound only while every production caller carries
    // the journal: a new silent caller must not inherit the first caller's proof.
    let delegated = format!(
        "async fn write_grant(state: &AppState) {{\n    {writer}(state).await;\n}}\n\
         pub async fn update(state: &AppState) {{\n    write_grant(state).await;\n    \
         record_grant(state).await;\n}}\n"
    );
    let sites = grant_sites("fixture.rs", &delegated, &writers);
    let [site] = sites.as_slice() else {
        panic!(
            "the delegated fixture produced {} grant sites, not one",
            sites.len()
        )
    };
    assert!(
        site.offence.is_none(),
        "a grant helper whose only production caller journals the successful write was reported"
    );

    let one_silent_caller = format!(
        "{delegated}pub async fn silent_update(state: &AppState) {{\n    \
         write_grant(state).await;\n}}\n"
    );
    let sites = grant_sites("fixture.rs", &one_silent_caller, &writers);
    let [site] = sites.as_slice() else {
        panic!(
            "the mixed delegated fixture produced {} grant sites, not one",
            sites.len()
        )
    };
    assert!(
        site.offence.is_some(),
        "one journalling caller hid a second production caller that publishes the grant silently"
    );
}
