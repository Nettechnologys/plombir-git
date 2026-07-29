//! Source guard: a handler that is handed a *global* id must anchor it.
//!
//! `api/releases.rs` addresses releases and assets by instance-wide primary
//! keys while the permission gate in its signature only ever proves something
//! about `{owner}/{name}`. The file says so in its own header and re-anchors
//! every id through `release_in_repo` / `asset_in_repo` — but "says so" is the
//! whole problem: the next handler is one forgotten call away from acting on an
//! id that points into somebody else's repository, and the service functions it
//! would call (`get_release`, `delete_asset`, `sign_asset_attestation`, …) take
//! that id without a repository beside it, so nothing downstream can object.
//!
//! `cross_repo_id_scope_sweep_tests` drives the routes that exist today and
//! would catch such a handler once it is wired into the router. This guard
//! catches it one step earlier and for a different reason: it reads the *shape*
//! of the code, so a handler that anchors nothing fails the build even before
//! anyone decides which HTTP verb to hang it on. The failure being guarded here
//! is a line that was not written, and no request can exercise that.
//!
//! Deliberately narrow. The other files that carry an `*_in_repo` helper
//! (`boards.rs`, `webhooks.rs`, `time_tracking.rs`, `issues.rs`) mix three
//! different anchoring shapes — a `*_for_repo` service call that scopes itself,
//! a nested id checked against its parent rather than against the repository,
//! and an id that arrives in the request body — and a rule stretched over all
//! of them would be a rule with four exceptions. `ANCHORED` grows one audited
//! entry at a time.

use std::fs;
use std::path::{Path, PathBuf};

/// Files whose handlers take a global id in the path, and what anchors it.
///
/// Read as: inside this file, a handler that destructures a path parameter
/// named `param` must call `anchor` before it does anything with it.
const ANCHORED: &[(&str, &[(&str, &str)])] = &[(
    "api/releases.rs",
    &[
        // `get_release` / `update_release` / `delete_release` spell the release
        // id `id`; the asset routes carry the release as `release_id`.
        ("id", "release_in_repo"),
        ("release_id", "release_in_repo"),
        ("asset_id", "asset_in_repo"),
    ],
)];

/// The `rg_core::release::service` functions that take a release or asset id
/// with no repository beside it, and the one file allowed to call them.
///
/// The guard above only reads the files in [`ANCHORED`], so on its own it says
/// nothing about a *second* module deciding to reach a release directly —
/// which is the same hole one file over. These are the primitives the anchoring
/// convention exists for, so they are barred everywhere else in `rg-http`, the
/// way `authz_extractor_guard` bars the raw `require_*` gates: an id-taking
/// call outside the file that anchors ids is the defect, wherever it appears.
///
/// `create_release` and `list_releases` are absent on purpose — both take the
/// repository id itself and are scoped by their own signature.
const RELEASE_PRIMITIVES: &[&str] = &[
    "get_release",
    "update_release",
    "delete_release",
    "get_asset",
    "list_assets",
    "upload_asset",
    "download_asset",
    "delete_asset",
    "sign_asset_attestation",
    "get_asset_attestation",
    "verify_asset_attestation",
];

/// The file that owns the release/asset anchoring helpers.
const RELEASE_API: &str = "api/releases.rs";

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// One `pub async fn` and everything up to the next one.
struct Handler {
    name: String,
    line: usize,
    body: String,
}

fn handlers(text: &str) -> Vec<Handler> {
    let mut out: Vec<Handler> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if let Some(rest) = line.strip_prefix("pub async fn ") {
            let name = rest
                .split(['(', '<', ' '])
                .next()
                .unwrap_or_default()
                .to_string();
            out.push(Handler {
                name,
                line: n + 1,
                body: String::new(),
            });
        }
        if let Some(current) = out.last_mut() {
            current.body.push_str(line);
            current.body.push('\n');
        }
    }
    out
}

/// The identifiers a handler destructures out of `Path((…))`.
///
/// Only the destructuring pattern, not the type: `Path<(String, String, i64)>`
/// names no parameter, and reading it would only add ways to match by accident.
fn path_params(body: &str) -> Vec<String> {
    let Some(open) = body.find("Path((") else {
        return Vec::new();
    };
    let after = &body[open + "Path((".len()..];
    let Some(close) = after.find("))") else {
        return Vec::new();
    };
    after[..close]
        .split(',')
        .map(|part| part.trim().trim_start_matches("mut ").to_string())
        .filter(|part| !part.is_empty() && part != "_")
        .collect()
}

/// A call to `name(`, ignoring its own definition and comment lines.
fn calls(body: &str, name: &str) -> bool {
    body.lines().any(|line| {
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

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[test]
fn handlers_holding_a_global_id_anchor_it_to_the_authorized_repository() {
    let mut offenders = Vec::new();

    for (rel, rules) in ANCHORED {
        let path = src_root().join(rel);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("ANCHORED names {rel} but it cannot be read: {e}"));

        let found = handlers(&text);
        assert!(
            !found.is_empty(),
            "no `pub async fn` handler found in {rel} — the guard is reading the wrong shape"
        );

        for handler in &found {
            let params = path_params(&handler.body);
            for (param, anchor) in *rules {
                if params.iter().any(|p| p == param) && !calls(&handler.body, anchor) {
                    offenders.push(format!(
                        "  {rel}:{} — {}() takes `{param}` and never calls {anchor}()",
                        handler.line, handler.name
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a handler acted on an instance-wide id without anchoring it to the repository its \
         access gate actually checked.\n\
         Call the file's `*_in_repo` helper first and act on the model it hands back — the \
         repository in the path only proves the caller may open *a* repository, not that the \
         id points into it. A mismatch answers 404, never 403.\n{}",
        offenders.join("\n")
    );
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

#[test]
fn the_unscoped_release_primitives_are_only_reachable_from_the_file_that_anchors_ids() {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    assert!(
        files.len() > 20,
        "src tree looks empty — the guard is not running"
    );

    let mut offenders = Vec::new();
    for file in &files {
        let rel = relative(file);
        if rel == RELEASE_API {
            continue;
        }
        let text = fs::read_to_string(file).expect("read source file");
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if !line.contains("release::service::") {
                continue;
            }
            // An import would let the qualified path — the thing this guard
            // reads — disappear from the call site entirely.
            if code.starts_with("use ") {
                offenders.push(format!("  {rel}:{} — {}", n + 1, code.trim()));
                continue;
            }
            for name in RELEASE_PRIMITIVES {
                if line.contains(&format!("release::service::{name}(")) {
                    offenders.push(format!("  {rel}:{} — {}", n + 1, code.trim()));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a release or asset was reached by its global id from outside {RELEASE_API}.\n\
         Those service functions take an id with no repository beside them, so the caller — not \
         the callee — is what keeps them inside the right repository, and {RELEASE_API} is where \
         that is done (`release_in_repo` / `asset_in_repo`). Anchor the id there and pass the \
         resolved model on, or give the service function a `repo_id` of its own and check it \
         inside.\n{}",
        offenders.join("\n")
    );
}

/// The guard is worth nothing if its rules match nothing — a renamed path
/// parameter would otherwise turn it green and silent in the same commit.
#[test]
fn every_anchoring_rule_still_matches_a_handler() {
    for (rel, rules) in ANCHORED {
        let path = src_root().join(rel);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("ANCHORED names {rel} but it cannot be read: {e}"));
        let found = handlers(&text);

        for (param, anchor) in *rules {
            assert!(
                text.contains(&format!("async fn {anchor}(")),
                "{rel} is guarded against {anchor}() but does not define it — the rule guards \
                 nothing"
            );
            let matched = found
                .iter()
                .filter(|h| path_params(&h.body).iter().any(|p| p == param))
                .count();
            assert!(
                matched > 0,
                "no handler in {rel} destructures `{param}` — the rule pairing it with {anchor} \
                 matches nothing and would stay green through any rename"
            );
        }
    }
}

/// Same reason, for the barred primitives: a name that no longer exists guards
/// nothing, and a rename that silently empties the list is exactly how a
/// source-grep test turns into decoration.
#[test]
fn every_barred_release_primitive_still_exists() {
    let service = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rg-core/src/release/service.rs")
        .canonicalize()
        .expect("rg-core release service");
    let text = fs::read_to_string(&service).expect("read rg-core release service");

    for name in RELEASE_PRIMITIVES {
        assert!(
            text.contains(&format!("pub async fn {name}(")),
            "RELEASE_PRIMITIVES names `{name}`, but rg-core no longer exposes it — drop the \
             entry or follow the rename, or the guard quietly stops covering it"
        );
    }
}
