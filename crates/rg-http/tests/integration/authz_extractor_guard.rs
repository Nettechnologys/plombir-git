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

use std::fs;
use std::path::{Path, PathBuf};

/// The gate functions that must not be called outside `api::repo_access`.
const GATES: &[&str] = &[
    "require_read",
    "require_read_with_ci",
    "require_authenticated_read",
    "require_write",
    "require_admin",
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
         Use RepoRead / RepoAuthRead / RepoWrite / RepoAdmin / CiRead<_> from \
         `crate::api::repo_access`, so the compiler carries the gate instead of the author \
         remembering it. If this really is a different protocol with its own credentials, add \
         the file to SIGNED_OFF in this test with the reason.\n{}",
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
