//! Source guard: the repository access gate stays a *type*, not a call.
//!
//! Every repository-scoped REST handler declares its access level in its
//! signature (`RepoRead` / `RepoAuthRead` / `RepoWrite` / `RepoAdmin` /
//! `CiRead<_>`, and `AnchoredRead<_>` / `AnchoredWrite<_>` where the path names
//! no repository). The `require_*` functions those extractors are built on live
//! in `api::repo_access` and are meant to be called from nowhere else — the
//! moment a handler calls one directly, the gate is a convention again and the
//! next handler can simply forget it.
//!
//! This test is deliberately a grep over the tree rather than a runtime check:
//! the failure it guards against is a line of code that was *not* written, and
//! no request can exercise that.
//!
//! Note the *instance*-admin gate is called `require_instance_admin` and lives
//! in `api::admin`. It used to be a second `require_admin`, which made this
//! guard ambiguous and — more to the point — had already been copied verbatim
//! into `api::runners`, the exact duplication this phase exists to remove.
//!
//! It is deliberately absent from [`GATES`] below: `card_cd6f512e2e52` made it
//! module-private, so `api::admin::InstanceAdmin` is the only way to reach it
//! from anywhere else and the compiler enforces what this grep would only
//! notice. The repository gates cannot follow suit — `api::repo_access` has to
//! export `check_read_for` / `check_write_for` to the transports that resolve
//! their own caller, and those live in other modules of this crate — which is
//! why the grep is still the mechanism *inside* `rg-http`.
//!
//! They do stop one step short of public, though, and that step is the whole
//! answer to the workspace question: every name in [`GATES`] is `pub(crate)`,
//! so a crate that depends on `rg-http` cannot call one however it spells the
//! path. `pub mod api;` / `pub mod repo_access;` make the *module* reachable
//! and the functions in it are not — a distinction worth writing down, because
//! reading only the module declarations says the opposite.
//!
//! So the rule has two halves and a test each.
//! [`the_repository_gates_are_crate_private`] pins the compiler's half, which
//! is the half actually holding today; without it, one word in a signature
//! retires the guarantee silently.
//! [`the_repository_gates_are_not_called_from_the_other_crates_either`] is the
//! grep behind it, and it is deliberately kept even though it cannot fire while
//! the first one passes: the day a gate is widened on purpose, the call sites
//! are what is left to answer for, and that is not the day to start writing the
//! scan.

use std::fs;
use std::path::{Path, PathBuf};

/// The gate functions that must not be called outside `api::repo_access`.
///
/// `check_read_for` / `check_write_for` / `check_admin_for` are in here for a
/// different reason than the rest: they take the actor as an *argument*, so a
/// REST handler calling one would be free to pass whichever user id it happened
/// to have in scope. They exist for the transports that genuinely resolve their
/// own caller — see `TRANSPORTS` below — and a handler that can take an
/// extractor must. All three are listed even though only the first two are
/// reached from outside `api::repo_access` today: the omission of the third was
/// the same defect as the one `PREDICATES` had — a family member left out of the
/// name list, so the first handler to call it would have been the guard's blind
/// spot rather than its failure.
///
/// `require_namespace_write` / `require_namespace_create` guard the routes whose
/// target is named by the request *body* (`POST /imports`, `POST /repos`, the
/// destination of a transfer). Their extractors — `NamespaceWrite` /
/// `NamespaceCreate` — read the payload themselves, so a handler that took the
/// body as a plain `Json<_>` and called the gate afterwards would be back to
/// remembering the check by hand.
///
/// `check_read` / `check_read_with_ci` are the header-reading half of the same
/// pair, and they were the loophole this list left open: they take a repository
/// the caller resolved itself, so `resolve_repo` followed by `check_read` is a
/// complete gate written by hand — no `require_*` in it for the grep to find,
/// and no `can_*_repo` for the predicate guard below. `api::artifacts` was
/// living in exactly that blind spot with a prologue of its own for four routes
/// (card_1ec383429aea); when the path names no repository, the shape to take is
/// `AnchoredRead` / `AnchoredWrite`, which resolves and then asks the gate here.
///
/// A name added here has to be `pub(crate) async fn` in [`GATES_HOME`] — see
/// [`the_repository_gates_are_crate_private`], which is both the visibility pin
/// and the check that this list still names something. A rename that empties it
/// turns every guard over it green in the same commit.
const GATES: &[&str] = &[
    "require_read",
    "require_read_with_ci",
    "require_authenticated_read",
    "require_write",
    "require_admin",
    "require_owner",
    "require_namespace_write",
    "require_namespace_create",
    "check_read",
    "check_read_with_ci",
    "check_read_for",
    "check_write_for",
    "check_admin_for",
];

/// Files that legitimately hold their own gate, with the reason.
///
/// These are not REST handlers on `/api/v1/repos/{owner}/{name}` and cannot use
/// the extractors: they speak a different protocol with its own credential
/// rules, so their gate is a separate mechanism on purpose.
const SIGNED_OFF: &[(&str, &str)] = &[
    (
        "api/repo_access.rs",
        "the gate itself: the require_* implementations and the extractors over them",
    ),
    (
        "git_http.rs",
        "git-over-HTTP: PAT / HTTP-Basic credentials and a `check_git_access` gate of its own",
    ),
    (
        "oci.rs",
        "OCI registry: OCI-scoped bearer tokens and the registry's own error envelope",
    ),
    (
        "api/lfs.rs",
        "Git LFS batch protocol: its own error envelope, gated per-operation",
    ),
    (
        "ws.rs",
        "job-log WebSocket: the token arrives in `Sec-WebSocket-Protocol` or `?token=`, not in \
         `Authorization`, so a headers-based gate does not reach it",
    ),
];

/// The non-REST transports, and what each of them is allowed to decide.
///
/// Being signed off above means "you resolve your own caller", not "you write
/// your own permission rule". Each of these used to do both: LFS turned an
/// anonymous caller away through its credential helper rather than through a
/// gate, the registry ran `can_read_repo` with a sentinel user id that exists in
/// no database, and the job-log socket had its own copy of the read check. The
/// permission predicates below are therefore off-limits here — the decision has
/// to come from `api::repo_access`, whatever the credential looked like.
const TRANSPORTS: &[&str] = &["oci.rs", "api/lfs.rs", "ws.rs"];

/// The raw permission predicates. Calling one is deciding access.
///
/// These are enforced across the whole tree, not just the transports below.
/// `require_*` was only ever half the gate: a handler that never called one and
/// wrote `can_write_repo(...)` with its own `Ok(false) => forbidden(...)` arm
/// was invisible to this guard — the same rule in a second dialect, free to
/// drift from the first. 32 call sites were living in that blind spot.
///
/// The decision now comes from `api::repo_access` in one of three shapes: the
/// extractor in a handler's signature, `check_*_for` for a transport that
/// resolves its own caller, or `may_read` / `may_write` / `may_admin` for a
/// handler that needs access as a *question* — "is this caller also a writer?",
/// on top of a gate it has already passed.
///
/// `can_read` / `can_write` are the same three predicates in a fourth dialect:
/// they take `(owner, name)` strings instead of a repository model, resolve the
/// name themselves, and hand off to the `*_repo` pair. Listing only the pair let
/// a handler write `rg_core::repo::service::can_read(&state.db, &owner, &name,
/// uid)` with its own `Ok(false) => forbidden(…)` arm and stay green on every
/// guard in this file at once — no `require_*` to find, no `check_read` to find,
/// and no `can_*_repo` either. The rule is the rule whichever argument it takes.
const PREDICATES: &[&str] = &[
    "can_read_repo",
    "can_write_repo",
    "can_admin_repo",
    "can_read",
    "can_write",
];

/// The files that may ask a predicate directly, with the reason.
///
/// `api::repo_access` is the gate itself — `check_*_for`, `may_*` and the
/// `require_*` implementations are built on these predicates, so barring it
/// would bar the one implementation everything else is meant to route through.
///
/// `git_http.rs` is the exception the fourth dialect exists for, and it is
/// signed off rather than migrated: `check_git_access` holds `owner` / `repo` as
/// strings from the URL, not a resolved model, and it needs to tell "no such
/// repository" apart from "the lookup failed" — `can_read` / `can_write` mark
/// the first with a typed `rg_core::error::NotFound`, and answering `404` to a
/// database outage is what that distinction was added to stop (a clone gives up
/// on a 404, and CI / mirrors do not retry one). `check_read_for` flattens both
/// into a single refusal, so routing through it would put the bug back.
///
/// Note this list is *not* [`SIGNED_OFF`]: being a protocol with its own
/// credentials buys you the right to resolve your own caller, not the right to
/// write the permission rule. `oci.rs`, `api/lfs.rs` and `ws.rs` are signed off
/// there and deliberately absent here — see [`TRANSPORTS`].
const PREDICATE_SIGNED_OFF: &[(&str, &str)] = &[
    (
        "api/repo_access.rs",
        "the gate itself: check_*_for / may_* / require_* are built on these predicates",
    ),
    (
        "git_http.rs",
        "git-over-HTTP: has owner/name strings rather than a model, and needs the typed \
         `NotFound` that `can_read` / `can_write` carry to answer 404 instead of 5xx",
    ),
];

/// The organization-membership predicate — the third dialect, and the one both
/// guards above were blind to.
///
/// `create_repo` decided "may I put a repository in this organization" in its own
/// body: `get_org_by_name` followed by `is_org_member`, with an `Ok(true) =>` arm
/// and a `_ =>` that swallowed the `Err` — so a database outage answered `403 you
/// are not a member of this organization`, and an organization that did not exist
/// answered `404`, telling an outsider the account is real. The rule already had
/// an implementation in `require_namespace_create`, which is what the extractors
/// use; the copy was invisible here because it named neither a `require_*` gate
/// nor a `can_*_repo` predicate (card_1e1ed1ee06f1).
///
/// Only `get_org_by_name`'s companion is barred, not `get_org_by_name` itself:
/// resolving a name is not deciding anything, and half the tree legitimately
/// does it. Deciding *membership* is the part that has to come from a gate.
///
/// `is_org_member` was the whole list, and one name is not the rule. The same
/// question is answered a second way — `find_org_member` followed by a look at
/// `member.role` — and that is the dialect the code actually prefers:
/// `api::orgs::is_org_admin` asks it, and so do
/// `rg_core::repo::service::can_write_repo` / `can_admin_repo`. A handler that
/// wrote the pair out by hand named no barred function at all, which is the
/// same omission `can_read` / `can_write` were two lists up.
///
/// The team pair is the same rule one level further down: when the role
/// comparison says "ordinary member", `is_member_of_write_team` /
/// `is_member_of_admin_team` are what it falls through to, so they are the
/// third way to spell "may this member write here".
///
/// `list_org_members` and `is_team_member` are absent on purpose.
/// `list_org_members` answers about the organization rather than about the
/// caller — it *is* the member list, which `api::orgs` serves behind
/// `require_org_visible`. `is_team_member` names a team, and a team on its own
/// grants nothing: the permission sits on the team row, which is why the two
/// `is_member_of_*_team` predicates join it before the answer means anything.
const ORG_MEMBERSHIP: &[&str] = &[
    "is_org_member",
    "find_org_member",
    "is_member_of_write_team",
    "is_member_of_admin_team",
];

/// The two files that own an organization rule, and may therefore ask about
/// membership directly.
///
/// `api::repo_access` decides which namespace a repository may be created in;
/// `api::orgs` decides who may read and administer the organization itself
/// (`require_org_visible` / `require_org_admin`, both of which already classify
/// a failed lookup as ours rather than as a refusal). Every other file has to
/// take the answer from one of them.
const ORG_GATE_OWNERS: &[&str] = &["api/repo_access.rs", "api/orgs.rs"];

/// The file that *defines* the membership predicates.
///
/// Nothing in it calls them — every one is a query of its own — so it needs no
/// exemption today, and it deliberately does not get one in advance. The data
/// layer holds no policy, so the first op that reuses a sibling internally is
/// the moment to decide whether that reuse is a lookup or a rule; a sign-off
/// written now would answer that question for whoever writes it.
const MEMBERSHIP_OPS: &str = "rg-db/src/ops/org_ops.rs";

/// The file outside `rg-http` that wraps the membership predicates.
///
/// `rg_core::org::is_org_member` / `find_org_member` are one-line pass-throughs
/// to [`MEMBERSHIP_OPS`], and they are what `api::orgs` calls. Signing the file
/// off wholesale is exactly the mistake [`PREDICATE_HOME`] exists to avoid — it
/// is a service module, and the next function in it to decide membership would
/// inherit the exemption, the way `fork_repo` did with the read rule. So the
/// rule here is the same shape and just as narrow: inside this file, only a
/// function delegating to its own namesake may ask.
const MEMBERSHIP_WRAPPER_HOME: &str = "rg-core/src/org/mod.rs";

/// Files outside `rg-http` that legitimately ask about organization membership.
///
/// Empty, and worth keeping empty: the two files that ask today are the
/// predicate home and the wrapper home above, and both are scanned by a rule
/// narrower than a sign-off. This list is where the next exception goes, with
/// its reason, the way `rg-ssh` is carried in [`WORKSPACE_SIGNED_OFF`].
const ORG_MEMBERSHIP_SIGNED_OFF: &[(&str, &str)] = &[];

/// The file that *defines* the permission predicates.
///
/// Every guard above stops at the edge of this crate, and that is exactly where
/// the last copy of the read rule was found: `rg_core::repo::service::fork_repo`
/// answered "may this caller read the source?" itself, with its own
/// `can_read_repo` + `forbidden(...)` arm, while the route table declared
/// `RepoAuthRead` and the handler took no gate at all (card_b38bfb0f2b40).
///
/// A blanket sign-off for the defining file is what let that sit there, so this
/// one is narrower: inside it, only the `can_*` family may ask. A service
/// function that decides access is the defect, whichever crate it lives in.
const PREDICATE_HOME: &str = "rg-core/src/repo/service.rs";

/// Files outside `rg-http` that legitimately ask a permission predicate, with
/// the reason.
///
/// None of them gates *the caller*: the first two filter a list of **other**
/// people against the repository, which is the `may_read` question this crate
/// answers with `repo_access::may_read`. `rg-ssh` is a transport in the sense
/// of `TRANSPORTS` above — it resolves its own caller from the public key — and
/// it cannot route through `check_read_for`, because `api::repo_access` lives
/// in `rg-http` and `rg-ssh` does not (and should not) depend on it. That the
/// rule therefore has a second entry point is a known cost, not an oversight:
/// it still calls the one implementation in `rg-core`, and writes no arm of its
/// own beyond `allowed`/`insufficient repository permission`.
const WORKSPACE_SIGNED_OFF: &[(&str, &str)] = &[
    (
        "rg-core/src/notification/mod.rs",
        "filters watchers before delivery — the subject is each recipient, not the caller",
    ),
    (
        "rg-core/src/review/codeowners.rs",
        "filters CODEOWNERS candidates — the subject is each reviewer, not the caller",
    ),
    (
        "rg-ssh/src/lib.rs",
        "SSH transport: resolves its caller from the public key, and does not depend on `rg-http` \
         at all — the dependency graph, not the visibility of one module, is what puts \
         `api::repo_access` out of its reach",
    ),
];

/// The file that *defines* the repository gates, spelled workspace-relative.
///
/// The crate name is part of it because the question asked of this file —
/// "can anything outside `rg-http` call one of these?" — is a question about
/// the crate boundary, so it is read through [`workspace_crates`] the way
/// [`MEMBERSHIP_OPS`] is, not through [`src_root`].
const GATES_HOME: &str = "rg-http/src/api/repo_access.rs";

/// Files outside `rg-http` that legitimately call a repository gate, with the
/// reason.
///
/// Empty, and it cannot be otherwise while
/// [`the_repository_gates_are_crate_private`] passes: `pub(crate)` means an
/// entry here would name a file that does not compile. It is written out rather
/// than left absent for the same reason the release-primitive guard next door
/// carries an empty list — an absent list is indistinguishable from a rule
/// nobody extended, and this is the line the first legitimate exception has to
/// be argued on.
const GATES_WORKSPACE_SIGNED_OFF: &[(&str, &str)] = &[];

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// `crates/` — the parent of this crate's directory.
fn workspace_crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rg-http lives under crates/")
        .to_path_buf()
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

fn relative(path: &Path) -> String {
    path.strip_prefix(src_root())
        .expect("file under src/")
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every call to one of `predicates` outside the files that own the rule.
///
/// Shared by the predicate guards below so "which files may decide this" is the
/// only thing that differs between them, and adding a predicate cannot
/// accidentally come with a laxer scan.
fn predicate_offenders(predicates: &[&str], owners: &[&str]) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    assert!(
        files.len() > 20,
        "src tree looks empty — guard is not running"
    );
    for owner in owners {
        assert!(
            src_root().join(owner).exists(),
            "{owner} owns a rule in this guard but that file is gone — fix the list"
        );
    }

    let mut offenders = Vec::new();
    for file in &files {
        let rel = relative(file);
        if owners.contains(&rel.as_str()) {
            continue;
        }
        let text = fs::read_to_string(file).expect("read source file");
        for (n, line) in text.lines().enumerate() {
            for predicate in predicates {
                if calls_gate(line, predicate) {
                    offenders.push(format!("  {rel}:{} — {}", n + 1, line.trim()));
                }
            }
        }
    }
    offenders
}

/// A call to `name(` that is not a definition, a doc reference or a comment.
fn calls_gate(line: &str, name: &str) -> bool {
    let code = line.trim_start();
    if code.starts_with("//") || code.starts_with("use ") {
        return false;
    }
    // `pub(crate) async fn require_read(` is the definition, not a call.
    if code.contains(&format!("fn {name}(")) {
        return false;
    }
    let Some(at) = line.find(&format!("{name}(")) else {
        return false;
    };
    // `require_read` is a prefix of `require_read_with_ci`; only match on a
    // real identifier boundary so the shorter name does not swallow the longer.
    !line[..at]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

#[test]
fn repository_gates_are_only_reachable_through_the_extractors() {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    assert!(
        files.len() > 20,
        "src tree looks empty — guard is not running"
    );

    let mut offenders = Vec::new();
    for file in &files {
        let rel = relative(file);
        if SIGNED_OFF.iter().any(|(allowed, _)| rel == *allowed) {
            continue;
        }
        let text = fs::read_to_string(file).expect("read source file");
        for (n, line) in text.lines().enumerate() {
            for gate in GATES {
                if calls_gate(line, gate) {
                    offenders.push(format!("  {rel}:{} — {}", n + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "repository access gate called directly instead of taken as a handler argument.\n\
         Use RepoRead / RepoAuthRead / RepoWrite / RepoAdmin / RepoOwner / CiRead<_> from \
         `crate::api::repo_access`, so the compiler carries the gate instead of the author \
         remembering it. If this really is a different protocol with its own credentials, add \
         the file to SIGNED_OFF in this test with the reason.\n{}",
        offenders.join("\n")
    );
}

/// The other dialect: a handler that never called `require_*` and wrote the
/// decision itself, straight off `rg_core::repo::service::can_*_repo`.
///
/// The guard above cannot see that — there is no gate call in it to find. So
/// the predicates are barred everywhere outside the gate module: a handler
/// takes an extractor, a transport calls `check_*_for`, and a handler that
/// needs the weaker *question* ("is this caller also a writer?") calls
/// `may_read` / `may_write` / `may_admin`. All three live in one file, so the
/// rule has one implementation no matter which shape asks for it.
#[test]
fn the_permission_predicates_are_only_reachable_through_the_gate_module() {
    let owners: Vec<&str> = PREDICATE_SIGNED_OFF.iter().map(|(rel, _)| *rel).collect();
    let offenders = predicate_offenders(PREDICATES, &owners);

    assert!(
        offenders.is_empty(),
        "a repository permission was decided outside `api::repo_access`.\n\
         Take an extractor (RepoRead / RepoAuthRead / RepoWrite / RepoAdmin / RepoOwner / \
         CiRead<_>) when the answer gates the whole handler; call \
         `repo_access::may_read` / `may_write` / `may_admin` when you need it as a question on \
         top of a gate you already passed; call `check_read_for` / `check_write_for` when the \
         protocol resolves its own caller. Writing the rule again with `can_read_repo` — or with \
         the name-resolving `can_read` / `can_write` pair, which is the same rule with a lookup \
         in front — is how the copies this phase exists to remove got made. If the answer really \
         has to come from the predicate itself, add the file to PREDICATE_SIGNED_OFF with the \
         reason, the way `git_http.rs` is.\n{}",
        offenders.join("\n")
    );
}

/// The third dialect: the rule written out of `is_org_member` instead of out of
/// a gate.
///
/// Neither guard above could see `create_repo`'s copy — it called no `require_*`
/// and no `can_*_repo`, so "may I create a repository in this organization"
/// existed twice, and the copy answered a failed lookup with `403` and an
/// unknown organization with `404`. A membership question therefore has to come
/// from one of the two files that own an organization rule.
#[test]
fn the_org_membership_predicate_is_only_reachable_through_a_gate_module() {
    let offenders = predicate_offenders(ORG_MEMBERSHIP, ORG_GATE_OWNERS);

    assert!(
        offenders.is_empty(),
        "organization membership was decided outside a gate module.\n\
         For \"may this caller put a repository here\", take `NamespaceCreate` / `NamespaceWrite` \
         from `crate::api::repo_access` — it answers the namespace question and hands back the \
         organization id it resolved. For \"may this caller see or administer the organization\", \
         use the `OrgRead` / `OrgAdmin` extractors in `api::orgs`. Asking any of \
         {ORG_MEMBERSHIP:?} here writes the rule a second time — whether it is spelled \
         `is_org_member` or as `find_org_member` plus a look at `member.role` — and the second \
         copy is where the swallowed `Err` and the `404` existence oracle came from.\n{}",
        offenders.join("\n")
    );
}

/// A transport may resolve its own caller; it may not decide what that caller
/// is allowed to do.
#[test]
fn the_other_protocols_take_the_decision_from_the_shared_gate() {
    let mut offenders = Vec::new();
    for transport in TRANSPORTS {
        let path = src_root().join(transport);
        assert!(
            path.exists(),
            "TRANSPORTS names {transport} but that file is gone — drop the entry"
        );
        let text = fs::read_to_string(&path).expect("read source file");
        for (n, line) in text.lines().enumerate() {
            for predicate in PREDICATES {
                if calls_gate(line, predicate) {
                    offenders.push(format!("  {transport}:{} — {}", n + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a transport decided repository access itself instead of asking the shared gate.\n\
         Resolve the actor however this protocol carries it, then call \
         `repo_access::check_read_for` / `check_write_for` with it, so \"who is allowed\" has one \
         implementation and \"who is calling\" stays the transport's own business.\n{}",
        offenders.join("\n")
    );
}

/// The extractors are worth nothing if nothing uses them — that is exactly how
/// `AuthUser` sat dead in the tree while 100+ handlers hand-rolled their gate.
#[test]
fn the_extractors_are_actually_used() {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);

    let mut uses = 0;
    for file in &files {
        if relative(file) == "api/repo_access.rs" {
            continue;
        }
        let text = fs::read_to_string(file).expect("read source file");
        for line in text.lines() {
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("use ") {
                continue;
            }
            for ty in [
                "RepoRead",
                "RepoAuthRead",
                "RepoWrite",
                "RepoAdmin",
                "RepoOwner",
                "CiRead",
                // The anchored pair, taken by the routes whose path names no
                // repository — they are extractors like the rest, and a
                // migration that dropped them back into handler bodies has to
                // show up in this count too.
                "ArtifactRead",
                "ArtifactWrite",
            ] {
                if code.contains(&format!("{ty} {{")) || code.contains(&format!(": {ty},")) {
                    uses += 1;
                }
            }
        }
    }

    assert!(
        uses > 100,
        "only {uses} handler(s) take a repository access extractor — the migration regressed"
    );
}

/// The name of the top-level `fn` this line opens, if it opens one.
///
/// Deliberately blind to anything indented: the file this is used on is a flat
/// list of free functions, and an indented `fn` inside one of them is a closure
/// or a nested helper that belongs to whatever encloses it.
fn top_level_fn_name(line: &str) -> Option<&str> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let at = line.find("fn ")?;
    // Only the usual item prefixes may sit in front of it, so a `fn` inside a
    // string literal or a trailing comment cannot rename the enclosing item.
    if !line[..at].split_whitespace().all(|word| {
        matches!(word, "pub" | "async" | "unsafe" | "const" | "extern") || word.starts_with("pub(")
    }) {
        return None;
    }
    let rest = &line[at + "fn ".len()..];
    let end = rest.find(|c: char| !c.is_alphanumeric() && c != '_')?;
    Some(&rest[..end])
}

/// Calls to `names` inside the file that owns them, outside the family that is
/// allowed to ask.
///
/// The definitions themselves are not calls (`calls_gate` skips the `fn name(`
/// line). `may_ask` receives the enclosing top-level function and the name it
/// calls, and decides whether that pairing is the family asking itself: for
/// [`PREDICATE_HOME`] the family is `can_*`, for [`MEMBERSHIP_WRAPPER_HOME`] it
/// is a wrapper delegating to its own namesake. Everything else in such a file
/// is a service function, and a service function deciding the caller's access
/// is the defect this guards.
///
/// `min_family_calls` is a self-test rather than a rule about the code: if the
/// family stops being seen asking at all, the enclosing-function tracking has
/// broken and an empty offender list proves nothing.
fn home_offenders(
    home: &str,
    names: &[&str],
    may_ask: impl Fn(&str, &str) -> bool,
    min_family_calls: usize,
) -> Vec<String> {
    let path = workspace_crates().join(home);
    assert!(
        path.exists(),
        "{home} is named as a rule's home in this guard but that file is gone — fix the constant"
    );
    let text = fs::read_to_string(&path).expect("read source file");
    let lines: Vec<&str> = text.lines().collect();

    let mut enclosing = "<file scope>";
    let mut family_calls = 0usize;
    let mut offenders = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        // The unit tests at the file's tail walk the permission matrix the
        // predicates implement — the one place where calling them *is* the
        // point. Anchored on the `#[cfg(test)] mod …` pair rather than on the
        // attribute alone, so a `#[cfg(test)]` helper earlier in the file
        // cannot silently end the scan.
        if line.trim_start() == "#[cfg(test)]"
            && lines[n + 1..]
                .iter()
                .take(2)
                .any(|next| next.trim_start().starts_with("mod "))
        {
            break;
        }
        if let Some(name) = top_level_fn_name(line) {
            enclosing = name;
        }
        for predicate in names {
            if calls_gate(line, predicate) {
                if may_ask(enclosing, predicate) {
                    family_calls += 1;
                } else {
                    offenders.push(format!(
                        "  {home}:{} — {} (in `{enclosing}`)",
                        n + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        family_calls >= min_family_calls,
        "only {family_calls} call(s) of {names:?} attributed to the family that owns them in \
         {home}, expected at least {min_family_calls} — the enclosing-function tracking is \
         broken, not the file"
    );
    offenders
}

/// Every call to one of `names` in the `rg-*` crates other than `rg-http`,
/// together with the number of files walked.
///
/// `homes` are the files scanned by [`home_offenders`] under a narrower rule
/// than a sign-off; skipping them here is what keeps that rule the only one
/// that applies to them. `signed_off` is the ordinary exception list.
///
/// The count comes back with the offenders because an empty result means two
/// very different things — nobody decides the rule out here, or the walk never
/// ran — and only the caller can assert which one it got.
fn other_crate_offenders(
    names: &[&str],
    homes: &[&str],
    signed_off: &[(&str, &str)],
) -> (Vec<String>, usize) {
    let crates_dir = workspace_crates();
    let mut offenders = Vec::new();
    let mut scanned = 0usize;

    for entry in fs::read_dir(&crates_dir).expect("read crates dir") {
        let krate = entry.expect("dir entry").path();
        // `rg-http` is the subject of the crate-scoped guards above, which scan
        // it against a stricter owner list.
        if krate.file_name().is_some_and(|name| name == "rg-http") {
            continue;
        }
        let src = krate.join("src");
        if !src.is_dir() {
            continue;
        }

        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for file in &files {
            let rel = file
                .strip_prefix(&crates_dir)
                .expect("file under crates/")
                .to_string_lossy()
                .replace('\\', "/");
            scanned += 1;
            if homes.contains(&rel.as_str())
                || signed_off.iter().any(|(allowed, _)| rel == *allowed)
            {
                continue;
            }
            let text = fs::read_to_string(file).expect("read source file");
            for (n, line) in text.lines().enumerate() {
                for name in names {
                    if calls_gate(line, name) {
                        offenders.push(format!("  {rel}:{} — {}", n + 1, line.trim()));
                    }
                }
            }
        }
    }

    (offenders, scanned)
}

/// The fourth dialect: the rule written in a crate the guards above cannot see.
///
/// Every scan in this file stops at `crates/rg-http/src`, and the read rule had
/// a live second implementation just outside it — in `fork_repo`, one call
/// below a route that declared `RepoAuthRead`. So the predicates are barred
/// across the whole workspace, not just this crate: a `rg-*` crate that decides
/// repository access is writing a copy of the gate wherever it sits.
#[test]
fn the_permission_predicates_are_not_decided_in_the_other_crates_either() {
    let mut offenders = home_offenders(
        PREDICATE_HOME,
        PREDICATES,
        // `can_read` / `can_write` resolve a name and hand off to the
        // model-taking predicate: the family asking itself.
        |enclosing, _| enclosing.starts_with("can_"),
        2,
    );
    let (mut elsewhere, scanned) =
        other_crate_offenders(PREDICATES, &[PREDICATE_HOME], WORKSPACE_SIGNED_OFF);
    offenders.append(&mut elsewhere);

    assert!(
        scanned > 50,
        "only {scanned} file(s) scanned outside rg-http — the guard is not running"
    );
    assert!(
        offenders.is_empty(),
        "a repository permission was decided outside `rg-http`'s gate module.\n\
         A crate that serves HTTP takes the decision from an extractor; a crate that does not \
         has to be signed off in WORKSPACE_SIGNED_OFF with the reason, the way `rg-ssh` is. \
         Deciding it in a service function is how `fork_repo` came to hold a copy of the read \
         rule that no guard in `rg-http` could see.\n{}",
        offenders.join("\n")
    );
}

/// Membership, in the crates the org guard used to stop short of.
///
/// `the_org_membership_predicate_is_only_reachable_through_a_gate_module` walks
/// `crates/rg-http/src` and nothing else, while [`PREDICATES`] had already been
/// widened to the whole workspace — after `fork_repo` was found holding a copy
/// of the read rule one crate over. Membership is reachable from exactly the
/// same place: `rg_core::org::is_org_member` is a live `pub` function, so a
/// service function in `rg-core` answering "is this caller in the organization"
/// would have passed every guard in this file at once. The scan is the rule's
/// reach, not the crate the guard happens to live in.
#[test]
fn the_org_membership_predicate_is_not_decided_in_the_other_crates_either() {
    // Org membership is one of the terms of the repository rule, so the `can_*`
    // family asking it in the predicate home is that rule's implementation, not
    // a second copy of it.
    let mut offenders = home_offenders(
        PREDICATE_HOME,
        ORG_MEMBERSHIP,
        |enclosing, _| enclosing.starts_with("can_"),
        3,
    );
    // In the wrapper home, only a pass-through to its own namesake may ask.
    offenders.extend(home_offenders(
        MEMBERSHIP_WRAPPER_HOME,
        ORG_MEMBERSHIP,
        |enclosing, name| enclosing == name,
        2,
    ));
    let (mut elsewhere, scanned) = other_crate_offenders(
        ORG_MEMBERSHIP,
        &[PREDICATE_HOME, MEMBERSHIP_WRAPPER_HOME],
        ORG_MEMBERSHIP_SIGNED_OFF,
    );
    offenders.append(&mut elsewhere);

    assert!(
        scanned > 50,
        "only {scanned} file(s) scanned outside rg-http — the guard is not running"
    );
    assert!(
        offenders.is_empty(),
        "organization membership was decided outside a gate module.\n\
         The answer has to come from `api::repo_access` (`NamespaceCreate` / `NamespaceWrite`) \
         or from `api::orgs` (`OrgRead` / `OrgAdmin`), whichever crate the caller sits in. A \
         crate that cannot reach either — none can today — has to be signed off in \
         ORG_MEMBERSHIP_SIGNED_OFF with the reason, the way `rg-ssh` is for the repository \
         predicates. Asking here writes the rule a second time in a crate no guard in \
         `rg-http` can see, which is how the read rule came to live in `fork_repo`.\n{}",
        offenders.join("\n")
    );
}

/// The compiler's half of the gate rule: `pub(crate)`, never `pub`.
///
/// `repository_gates_are_only_reachable_through_the_extractors` walks
/// `crates/rg-http/src` and stops, and every sibling rule that stopped at that
/// edge turned out to be narrower than itself — `PREDICATES`, `ORG_MEMBERSHIP`
/// and the release primitives each had to be widened after a copy was found one
/// crate over. This list is the one that does *not* need widening for that
/// reason, and it is worth being precise about why: not because the module is
/// unreachable — `pub mod api;` and `pub mod repo_access;` make it perfectly
/// reachable, and `rg-cli` already depends on `rg-http` — but because every
/// function in it is `pub(crate)`. The gate is barred outside this crate by the
/// compiler, which is a stronger guarantee than any grep, and it was resting on
/// nothing but the absence of a reason to change it.
///
/// One word in one signature retires it. That is what this test costs to keep
/// and what it buys.
///
/// It doubles as the liveness check the barred lists all need: a rename that
/// empties [`GATES`] turns
/// `repository_gates_are_only_reachable_through_the_extractors` green and
/// silent in the same commit, exactly as
/// [`every_membership_predicate_still_exists`] guards against next door.
#[test]
fn the_repository_gates_are_crate_private() {
    let path = workspace_crates().join(GATES_HOME);
    let text = fs::read_to_string(&path).expect("read the gate module");

    for gate in GATES {
        assert!(
            !text.contains(&format!("pub async fn {gate}(")),
            "`{gate}` is `pub` in {GATES_HOME}, so every crate that depends on `rg-http` can now \
             call the gate directly — `resolve_repo` plus `{gate}` is a complete gate written by \
             hand, and no guard in this file walks the crate that would write it. Narrow it back \
             to `pub(crate)`. If the wider visibility is genuinely wanted, say why here and lean \
             on `the_repository_gates_are_not_called_from_the_other_crates_either` for the call \
             sites — that scan exists for this day."
        );
        assert!(
            text.contains(&format!("pub(crate) async fn {gate}(")),
            "GATES names `{gate}`, but {GATES_HOME} no longer defines it as \
             `pub(crate) async fn` — follow the rename or drop the entry, or every guard over \
             this list quietly stops covering it"
        );
    }
}

/// The gates, in the crates no scan in this file reaches.
///
/// This one cannot fail while [`the_repository_gates_are_crate_private`] holds,
/// and that is the point of writing it now rather than later: it is the second
/// half of a rule whose first half is one word wide. The three sibling lists
/// were each widened *after* a live copy of the rule was found outside
/// `rg-http` — `fork_repo` for the read predicate, `create_repo` for
/// membership. Here there is no copy to find, because the compiler never let
/// one be written; if that ever changes deliberately, the scan is already in
/// place and the exception has a list to be argued into.
#[test]
fn the_repository_gates_are_not_called_from_the_other_crates_either() {
    // No home to exempt: [`GATES_HOME`] lives inside `rg-http`, which
    // `other_crate_offenders` skips wholesale. The predicate guards pass one
    // because their rule is defined in `rg-core`, out in the scanned tree.
    let (offenders, scanned) = other_crate_offenders(GATES, &[], GATES_WORKSPACE_SIGNED_OFF);

    assert!(
        scanned > 50,
        "only {scanned} file(s) scanned outside rg-http — the guard is not running"
    );
    assert!(
        offenders.is_empty(),
        "a repository access gate was called from outside `rg-http`.\n\
         A crate that serves HTTP takes the decision from an extractor (RepoRead / RepoAuthRead \
         / RepoWrite / RepoAdmin / RepoOwner / CiRead<_>); a crate speaking another protocol \
         resolves its own caller and asks `check_read_for` / `check_write_for` — from inside \
         `rg-http`, where those live. Assembling the gate out here instead — `resolve_repo` \
         followed by `check_read` — is a second implementation in a crate the guards above \
         cannot see, which is how `fork_repo` came to hold a copy of the read rule. If a crate \
         really has to call one, sign it off in GATES_WORKSPACE_SIGNED_OFF with the reason.\n{}",
        offenders.join("\n")
    );
}

/// The barred names have to keep naming something.
///
/// A rename that empties the list turns every guard over it green and silent in
/// the same commit — the failure mode a source grep is most exposed to, and the
/// reason the release-primitive guard next door carries the same check.
#[test]
fn every_membership_predicate_still_exists() {
    let path = workspace_crates().join(MEMBERSHIP_OPS);
    let text = fs::read_to_string(&path).expect("read rg-db org ops");
    for name in ORG_MEMBERSHIP {
        assert!(
            text.contains(&format!("pub async fn {name}(")),
            "ORG_MEMBERSHIP names `{name}`, but {MEMBERSHIP_OPS} no longer defines it — follow \
             the rename or drop the entry, or the guard quietly stops covering it"
        );
    }
}

/// An org gate owner that stopped asking holds a blanket exemption, not a
/// reason — the same rot [`predicate_sign_offs_still_ask_the_predicate`] catches
/// on the repository side.
#[test]
fn the_org_gate_owners_still_ask_about_membership() {
    for owner in ORG_GATE_OWNERS {
        let path = src_root().join(owner);
        assert!(
            path.exists(),
            "ORG_GATE_OWNERS names {owner} but that file is gone — fix the list"
        );
        let text = fs::read_to_string(&path).expect("read source file");
        let asks = text
            .lines()
            .any(|line| ORG_MEMBERSHIP.iter().any(|name| calls_gate(line, name)));
        assert!(
            asks,
            "{owner} may decide organization membership directly, but asks none of \
             {ORG_MEMBERSHIP:?} any more. The entry has stopped describing the file and is now \
             an allowance over whatever it does instead — drop it from ORG_GATE_OWNERS."
        );
    }
}

/// A predicate sign-off has to keep being true in both directions.
///
/// The forward half is the guard above: the listed file may decide access. The
/// backward half is here, and it is the half that rots silently — the entry
/// stops describing a file that asks the rule and becomes a standing exemption
/// for whatever that file does next. `git_http.rs` is signed off *because* it
/// calls `can_read` / `can_write` for the `NotFound` distinction; a `git_http.rs`
/// that no longer calls them has no claim on the exception, and the next hand-
/// rolled gate written there would be invisible.
#[test]
fn predicate_sign_offs_still_ask_the_predicate() {
    for (rel, reason) in PREDICATE_SIGNED_OFF {
        let path = src_root().join(rel);
        assert!(
            path.exists(),
            "PREDICATE_SIGNED_OFF names {rel} ({reason}) but that file is gone — drop the entry"
        );
        assert!(!reason.is_empty(), "{rel} is signed off without a reason");

        let text = fs::read_to_string(&path).expect("read source file");
        let asks = text
            .lines()
            .any(|line| PREDICATES.iter().any(|p| calls_gate(line, p)));
        assert!(
            asks,
            "{rel} is signed off to decide repository access directly ({reason}) but calls none \
             of {PREDICATES:?} any more. The reason no longer describes the file, so the entry is \
             a blanket allowance over whatever it does instead — drop it."
        );
    }
}

/// Every signed-off exception must name a file that still exists, so the list
/// cannot quietly turn into a blanket allowance as files are renamed.
#[test]
fn signed_off_exceptions_are_live() {
    for (rel, reason) in SIGNED_OFF {
        assert!(
            src_root().join(rel).exists(),
            "SIGNED_OFF names {rel} ({reason}) but that file is gone — drop the entry"
        );
        assert!(!reason.is_empty(), "{rel} is signed off without a reason");
    }
    for (rel, reason) in WORKSPACE_SIGNED_OFF {
        assert!(
            workspace_crates().join(rel).exists(),
            "WORKSPACE_SIGNED_OFF names {rel} ({reason}) but that file is gone — drop the entry"
        );
        assert!(!reason.is_empty(), "{rel} is signed off without a reason");
    }
    // Empty today. Listed anyway so the first entry added here is held to the
    // same two conditions as the rest, rather than to none.
    for (rel, reason) in ORG_MEMBERSHIP_SIGNED_OFF {
        assert!(
            workspace_crates().join(rel).exists(),
            "ORG_MEMBERSHIP_SIGNED_OFF names {rel} ({reason}) but that file is gone — drop the \
             entry"
        );
        assert!(!reason.is_empty(), "{rel} is signed off without a reason");
    }
    // Empty for a stronger reason than the list above — `pub(crate)` makes an
    // entry here uncompilable — but held to the same conditions, because the
    // day it stops being empty is the day the visibility changed.
    for (rel, reason) in GATES_WORKSPACE_SIGNED_OFF {
        assert!(
            workspace_crates().join(rel).exists(),
            "GATES_WORKSPACE_SIGNED_OFF names {rel} ({reason}) but that file is gone — drop the \
             entry"
        );
        assert!(!reason.is_empty(), "{rel} is signed off without a reason");
    }
}
