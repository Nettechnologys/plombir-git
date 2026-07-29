//! Source guard: the repository access gate stays a *type*, not a call.
//!
//! Every repository-scoped REST handler declares its access level in its
//! signature (`RepoRead` / `RepoAuthRead` / `RepoWrite` / `RepoAdmin` /
//! `CiRead<_>`). The `require_*` functions those extractors are built on live
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
//! export `check_read_for` / `check_write_for` for the transports that resolve
//! their own caller — which is why the grep is still the mechanism there.

use std::fs;
use std::path::{Path, PathBuf};

/// The gate functions that must not be called outside `api::repo_access`.
///
/// `check_read_for` / `check_write_for` are in here for a different reason than
/// the rest: they take the actor as an *argument*, so a REST handler calling one
/// would be free to pass whichever user id it happened to have in scope. They
/// exist for the transports that genuinely resolve their own caller — see
/// `TRANSPORTS` below — and a handler that can take an extractor must.
const GATES: &[&str] = &[
    "require_read",
    "require_read_with_ci",
    "require_authenticated_read",
    "require_write",
    "require_admin",
    "check_read_for",
    "check_write_for",
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
const PREDICATES: &[&str] = &["can_read_repo", "can_write_repo", "can_admin_repo"];

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
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
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    assert!(
        files.len() > 20,
        "src tree looks empty — guard is not running"
    );

    let mut offenders = Vec::new();
    for file in &files {
        let rel = relative(file);
        if rel == "api/repo_access.rs" {
            continue;
        }
        let text = fs::read_to_string(file).expect("read source file");
        for (n, line) in text.lines().enumerate() {
            for predicate in PREDICATES {
                if calls_gate(line, predicate) {
                    offenders.push(format!("  {rel}:{} — {}", n + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a repository permission was decided outside `api::repo_access`.\n\
         Take an extractor (RepoRead / RepoAuthRead / RepoWrite / RepoAdmin / RepoOwner / \
         CiRead<_>) when the answer gates the whole handler; call \
         `repo_access::may_read` / `may_write` / `may_admin` when you need it as a question on \
         top of a gate you already passed; call `check_read_for` / `check_write_for` when the \
         protocol resolves its own caller. Writing the rule again with `can_*_repo` is how the \
         copies this phase exists to remove got made.\n{}",
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
}
