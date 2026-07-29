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
//! # The denominator
//!
//! [`ANCHORED`] used to be the whole guard, and "how many handlers still need
//! covering" was answered by grepping for `*_in_repo`. That grep counted the
//! files that had already adopted the convention — the ones that had not were
//! invisible to it, so the plan measured its own progress against a population
//! that excluded every remaining gap. Five files looked like the whole job; the
//! census below finds [`CENSUS_TOTAL`] handler/parameter pairs.
//!
//! So the population is counted first and classified second:
//! [`every_global_id_a_handler_takes_is_accounted_for`] walks every
//! `pub async fn` under `src/`, takes every path parameter named `id` or
//! `*_id`, and demands that each one be anchored in a way this file can *read*.
//! Five such ways exist, and they are tried in order:
//!
//! 1. **Instance gate.** The signature carries `InstanceAdmin`. Addressing rows
//!    instance-wide is what the route is for, so a global id is not a leak.
//! 2. **Named anchor** ([`ANCHORED`]). The handler calls a helper that re-ties
//!    the id to the gated repository — `release_in_repo`, `assigned_job`,
//!    `authorize_approval`. This is the form the rest of the guard enforces.
//! 3. **Scope comparison.** The body compares a fetched row's `*_id` against
//!    something the gate produced (`v.repo_id == repo.id`,
//!    `token.user_id != user_id`). Correct, but only visible to a reader — see
//!    below.
//! 4. **Scoped call.** Every call that hands the id to `rg_db` / `rg_core` or to
//!    a helper also hands it the gated repository, the authenticated user, or a
//!    row derived from one of those. The callee holds both halves, so it is the
//!    callee's job to refuse a mismatch (`get_protection_for_repo`,
//!    `mark_read_for_user`).
//! 5. **Signed off** ([`SIGNED_OFF`]), with the reason written down — the id is
//!    anchored by something outside the handler body, such as a route-layer
//!    middleware.
//!
//! An id that reaches no call at all is inert: it is echoed back or logged and
//! never addresses a row, so there is nothing to anchor.
//!
//! # Why the anchors are named helpers now
//!
//! Form 3 is the awkward one: a guard that reads source can see a *call* and
//! cannot see a *comparison*, so an inline `v.repo_id == repo.id` is
//! indistinguishable from no anchor at all. Teaching the guard a second kind of
//! rule per inline shape was the expensive option; the cheap one was to give
//! the comparison a name. `tag_protection.rs`, `ci_environments.rs`,
//! `deploy_keys.rs`, `reviews.rs` and `imports.rs` each held between one and
//! three copies of the same match arm, and each now calls one helper —
//! `tag_protection_in_repo`, `environment_in_repo`, `deploy_key_in_repo`,
//! `review_in_pr`, `import_task_of_user`. `issues.rs` and `webhooks.rs` were
//! reduced the same way earlier.
//!
//! Form 3 is still *accepted*, because a single-use comparison that has never
//! been copied is not worth a helper — `ssh_keys.rs` and `users.rs` each have
//! one. It is simply not *demanded* of anything: nothing about it is checkable.
//!
//! # Where the chain bottoms out
//!
//! Anchoring by delegation has to end somewhere, and the last link is always a
//! comparison. [`LEAF_COMPARISONS`] pins the ones that are load-bearing and
//! deletable in a single line: `attachments.rs` resolves a comment id through
//! its *parent* row (`issue.repo_id != repo.id`), so the comment table itself
//! never learns which repository admitted the caller, and dropping either `if`
//! leaves code that compiles, passes every type check, and reads fine.
//!
//! What the tables deliberately do **not** cover is an id that arrives in the
//! request *body* rather than the path: `create_issue` / `update_issue` take
//! `milestone_id` and `assignee_id` that way, `boards.rs` takes `issue_id`, and
//! `reviews.rs` takes `review_id`. The census keys off path parameters, so it
//! is blind to them by construction. They are listed in [`BODY_BORNE_IDS`],
//! which asserts the anchors still exist, and are the reason a body-id rule is
//! a separate piece of work rather than another column.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rg_http::route_table::RUNNER_AUTH_LAYER;

use crate::common::spawn_test_app_with_routes;

/// One rule: a path parameter, and the anchors any of which satisfies it.
type AnchorRule = (&'static str, &'static [&'static str]);

/// One file of [`ANCHORED`]: its path under `src/`, and its rules.
type AnchoredFile = (&'static str, &'static [AnchorRule]);

/// Files whose handlers take a global id in the path, and what anchors it.
///
/// Read as: inside this file, a handler that destructures a path parameter
/// named `param` must call one of `anchors` before it does anything with it.
/// More than one anchor is listed when routes of the same family legitimately
/// enter through different gates — `require_artifact_read` for the ones that
/// only read, `require_artifact_write` for the one that deletes.
const ANCHORED: &[AnchoredFile] = &[
    (
        "api/releases.rs",
        &[
            // `get_release` / `update_release` / `delete_release` spell the release
            // id `id`; the asset routes carry the release as `release_id`.
            ("id", &["release_in_repo"]),
            ("release_id", &["release_in_repo"]),
            ("asset_id", &["asset_in_repo"]),
        ],
    ),
    (
        "api/boards.rs",
        &[
            // The board itself is the only id anchored to the *repository*; the
            // two below it are anchored to their parent, which `board_in_repo`
            // has already placed. `get_board` and friends spell it `id`, the
            // nested routes spell it `board_id`.
            ("id", &["board_in_repo"]),
            ("board_id", &["board_in_repo"]),
            // A column belongs to a board, a card belongs to a column — so the
            // anchor is the parent, not the repository. `card_in_board` walks
            // the card's column through `column_in_board` to get there.
            ("col_id", &["column_in_board"]),
            ("card_id", &["card_in_board"]),
        ],
    ),
    (
        "api/webhooks.rs",
        &[
            ("id", &["webhook_in_repo"]),
            // Chained anchor: `webhook_in_repo` ties the hook to the repository,
            // then the delivery is tied to that hook.
            ("delivery_id", &["delivery_in_webhook"]),
        ],
    ),
    (
        "api/time_tracking.rs",
        &[
            // `number` is an issue number, which is only unique *within* a
            // repository — so it is not a global id, but resolving it against
            // anything other than the gated repository is the same defect. The
            // rule exists to keep the resolution pinned to `repo.id`: two of
            // these handlers used to re-resolve `owner`/`name` themselves.
            ("number", &["issue_in_repo"]),
            // The time-entry id *is* global. Nothing anchors it directly; it is
            // only ever passed to the service beside an issue resolved above,
            // and the service refuses a mismatch. The rule keeps that pairing.
            ("id", &["issue_in_repo"]),
        ],
    ),
    (
        "api/issues.rs",
        &[
            // Only the three `/milestones/{id}` routes destructure `id` here.
            // `number` is deliberately absent: an issue number is scoped to its
            // repository by definition, and the routes carrying one hand
            // `owner`/`name` to a service that resolves them itself.
            ("id", &["milestone_in_repo"]),
        ],
    ),
    (
        "api/tag_protection.rs",
        &[("id", &["tag_protection_in_repo"])],
    ),
    (
        "api/ci_environments.rs",
        &[
            ("id", &["environment_in_repo"]),
            // `approve` takes both halves of a pipeline/job pair. The pipeline
            // is anchored to the repository and the job to the pipeline —
            // through its stage — inside one helper, so the rule names that
            // helper for both parameters rather than splitting the chain.
            ("pipeline_id", &["authorize_approval"]),
            ("job_id", &["authorize_approval"]),
        ],
    ),
    ("api/deploy_keys.rs", &[("id", &["deploy_key_in_repo"])]),
    (
        "api/reviews.rs",
        &[
            // A review id must clear *two* checks — the repository and the pull
            // request in the URL — because `{number}` names a PR within the
            // repository, so anchoring to the repository alone would still let
            // a review of PR #7 be dismissed through the URL of PR #9.
            ("id", &["review_in_pr", "require_suggestion_source"]),
        ],
    ),
    (
        "api/imports.rs",
        &[
            // The owner plays the part the repository plays elsewhere: an
            // import task is scoped to the account that started it.
            ("id", &["import_task_of_user"]),
        ],
    ),
    (
        "api/artifacts.rs",
        &[
            (
                "artifact_id",
                &["require_artifact_read", "require_artifact_write"],
            ),
            ("pipeline_id", &["require_pipeline_read"]),
            // The upload route is a runner route: the job must belong to the
            // runner whose token the middleware already checked.
            ("job_id", &["assigned_job"]),
        ],
    ),
    (
        "api/runners.rs",
        &[
            // `runner_id` is anchored by the route layer — see `SIGNED_OFF`.
            // The job is anchored to the runner here, in the handler. The two
            // cache routes need the repository as well, so they go through the
            // variant that resolves it from the same assignment.
            ("job_id", &["assigned_job", "assigned_job_repo"]),
        ],
    ),
    (
        "ws.rs",
        &[
            // The log socket walks job → stage → pipeline → repository and then
            // runs the same read gate the REST route would. Each step feeds the
            // next its own field, so a dropped link does not compile away
            // quietly — but the gate call is what makes the walk mean anything.
            ("job_id", &["check_read_for"]),
        ],
    ),
];

/// Handlers whose id is anchored by something outside the handler body.
///
/// The reason is the point of the entry: this is the escape hatch, and an
/// escape hatch without a written reason is just an exemption list. Each one
/// names what does the anchoring instead — and for the layer named here, the
/// naming is not the end of it:
/// [`the_runner_sign_off_names_a_layer_the_routes_carry`] holds every route
/// these handlers are on to actually carrying it.
const SIGNED_OFF: &[(&str, &str, &str, &str)] = &[
    (
        "api/runners.rs",
        "*",
        "runner_id",
        "`authenticate_runner` runs as a route layer on every runner route and refuses \
         the request unless the bearer token belongs to the runner named in the path, so \
         the id is already the caller's own by the time the handler is entered — checked \
         against the route table by `the_runner_sign_off_names_a_layer_the_routes_carry`",
    ),
    (
        "api/artifacts.rs",
        "upload_artifact",
        "runner_id",
        "same route layer as the `api/runners.rs` routes — the upload is a runner route \
         that happens to live in this file, and is held to the layer by the same test",
    ),
];

/// Global ids that arrive in a request *body*, and the anchor each one gets.
///
/// The census keys off path parameters, so it is blind to these by
/// construction — nothing in the handler's signature announces them. They are
/// recorded here so the anchors cannot be deleted unnoticed, and so the gap is
/// written down rather than merely known: a handler that reads a new id out of
/// its body is *not* covered by this file.
const BODY_BORNE_IDS: &[(&str, &str, &str)] = &[
    ("api/issues.rs", "milestone_id", "milestone_in_repo"),
    ("api/issues.rs", "assignee_id", "require_assignee_in_repo"),
    ("api/boards.rs", "issue_id", "issue_in_repo"),
    ("api/reviews.rs", "review_id", "review_in_pr"),
];

/// The comparisons every chain of delegation eventually rests on.
///
/// A named anchor can be *called*, and that call is what the census reads. The
/// anchor itself contains a comparison, and a comparison is what no source test
/// can infer — so the ones that are both load-bearing and deletable in a single
/// line are written down here verbatim.
///
/// `attachments.rs` is the reason this table exists. A comment carries no
/// repository of its own, so `resolve` reaches the comment's *parent* — the
/// issue or the pull request — and compares that. Delete either `if` and the
/// file still compiles, every type still lines up, and any repository member
/// can read and delete attachments on any comment on the instance.
const LEAF_COMPARISONS: &[(&str, &str, &[&str])] = &[(
    "api/attachments.rs",
    "resolve",
    &["issue.repo_id != repo.id", "pull.repo_id != repo.id"],
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

/// How many (handler, path parameter) pairs in `src/` name a global id.
///
/// The number is written down so that adding a route which takes one is a
/// deliberate act: the census fails until the new pair is classified *and* this
/// count is updated. It is the denominator the plan for this guard was missing.
const CENSUS_TOTAL: usize = 127;

/// Path parameters that name the gated repository or organisation rather than a
/// row inside it. A call that carries one of these is carrying the scope.
const GATE_IDENTITY_PARAMS: &[&str] = &["owner", "name", "repo", "org", "org_name"];

/// The gate extractors and the model each one binds. Their presence in a
/// signature means that model is already the caller's authorized scope.
const GATE_BINDINGS: &[(&str, &str)] = &[
    ("RepoRead", "repo"),
    ("RepoAuthRead", "repo"),
    ("RepoWrite", "repo"),
    ("RepoAdmin", "repo"),
    ("RepoOwner", "repo"),
    ("OrgAdmin", "org"),
    ("OrgRead", "org"),
];

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// One `fn` declared at column 0, ending at the `}` that closes it.
///
/// The body used to run on until the *next* `pub async fn`, which swept up
/// whatever sat between the two — a private helper, the next handler's
/// `#[utoipa::path]` block, its signature. That made a handler answerable for
/// calls it does not make, and let one that takes no `Path` of its own inherit
/// the next handler's parameters and be judged against rules that were never
/// about it. rustfmt puts the closing brace of a top-level item in column 0 and
/// nothing inside a function body there, so that brace is the exact end.
struct Function {
    name: String,
    line: usize,
    body: String,
    is_handler: bool,
}

fn declared_fn(line: &str) -> Option<(String, bool)> {
    // `pub(crate) async fn` counts as a handler candidate: nothing stops the
    // router from taking one, and a census that only reads `pub async fn`
    // would let a route hide behind the narrower visibility.
    for (prefix, is_handler) in [
        ("pub async fn ", true),
        ("pub(crate) async fn ", true),
        ("async fn ", false),
        ("pub(crate) fn ", false),
        ("pub fn ", false),
        ("fn ", false),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let name = rest
                .split(['(', '<', ' '])
                .next()
                .unwrap_or_default()
                .to_string();
            return Some((name, is_handler));
        }
    }
    None
}

fn functions(text: &str) -> Vec<Function> {
    let mut out: Vec<Function> = Vec::new();
    let mut open: Option<usize> = None;
    for (n, line) in text.lines().enumerate() {
        if open.is_none() {
            if let Some((name, is_handler)) = declared_fn(line) {
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
            if line == "}" {
                open = None;
            }
        }
    }
    out
}

fn handlers(text: &str) -> Vec<Function> {
    functions(text)
        .into_iter()
        .filter(|f| f.is_handler)
        .collect()
}

/// Everything up to the `)` that closes the parameter list.
fn signature(body: &str) -> &str {
    match body.find(") ->") {
        Some(at) => &body[..=at],
        None => body,
    }
}

/// The identifiers a handler destructures out of `Path(…)`.
///
/// Only the destructuring pattern, not the type: `Path<(String, String, i64)>`
/// names no parameter, and reading it would only add ways to match by accident.
/// Both shapes count — the four-element tuple of a nested repository route and
/// the bare `Path(id): Path<i64>` of a top-level one, which the tuple-only
/// version of this used to walk straight past.
fn path_params(sig: &str) -> Vec<String> {
    if let Some(open) = sig.find("Path((") {
        let after = &sig[open + "Path((".len()..];
        if let Some(close) = after.find("))") {
            return after[..close]
                .split(',')
                .map(|part| part.trim().trim_start_matches("mut ").to_string())
                .filter(|part| !part.is_empty() && part != "_")
                .collect();
        }
    }
    if let Some(open) = sig.find("Path(") {
        let after = &sig[open + "Path(".len()..];
        if let Some(close) = after.find(')') {
            let inner = after[..close].trim().trim_start_matches("mut ");
            if !inner.is_empty()
                && inner != "_"
                && inner.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
                return vec![inner.to_string()];
            }
        }
    }
    Vec::new()
}

fn is_global_id(param: &str) -> bool {
    !param.starts_with('_') && (param == "id" || param.ends_with("_id"))
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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

/// The body with every `//` comment blanked out, byte lengths preserved so that
/// offsets found in it still address the original text.
fn code_only(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        match line.find("//") {
            Some(at) => {
                out.push_str(&line[..at]);
                for c in line[at..].chars() {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                }
            }
            None => out.push_str(line),
        }
    }
    out
}

fn mentions(text: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(at) = text[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        let before_ok = !text[..start].chars().next_back().is_some_and(is_ident_char);
        let after_ok = !text[end..].chars().next().is_some_and(is_ident_char);
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

fn mentions_any(text: &str, words: &HashSet<String>) -> bool {
    words.iter().any(|word| mentions(text, word))
}

/// The identifier immediately before `at`, including its `::` path.
fn callee_before(code: &str, at: usize) -> String {
    let head = &code[..at];
    let start = head
        .rfind(|c: char| !(is_ident_char(c) || c == ':'))
        .map(|i| i + 1)
        .unwrap_or(0);
    head[start..].to_string()
}

/// The call that `at` sits inside, as `(callee, arguments)`.
fn enclosing_call(code: &str, at: usize) -> Option<(String, String)> {
    let bytes = code.as_bytes();
    let mut depth = 0usize;
    let mut open = None;
    for i in (0..at).rev() {
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                if depth == 0 {
                    open = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let open = open?;
    let mut depth = 0usize;
    let mut close = None;
    for (i, byte) in bytes.iter().enumerate().skip(open + 1) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                if depth == 0 {
                    close = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let close = close?;
    Some((callee_before(code, open), code[open + 1..close].to_string()))
}

/// Calls that hand the id to the database, to a core service, or to another
/// function of this crate — as opposed to a macro or a response wrapper, which
/// only echo it back.
fn consumer_calls(code: &str, param: &str, local_fns: &HashSet<String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = code[from..].find(param) {
        let start = from + at;
        let end = start + param.len();
        from = end;
        if code[..start].chars().next_back().is_some_and(is_ident_char) {
            continue;
        }
        if code[end..].chars().next().is_some_and(is_ident_char) {
            continue;
        }
        let Some((callee, args)) = enclosing_call(code, start) else {
            continue;
        };
        let is_consumer = callee.starts_with("rg_db::")
            || callee.starts_with("rg_core::")
            || callee.starts_with("crate::")
            || callee.starts_with("super::")
            || local_fns.contains(&callee);
        if is_consumer {
            out.push((callee, args));
        }
    }
    out
}

/// Statements of the form `let <pattern> = <rhs>;`, with the pattern flattened
/// to the identifiers it binds.
fn let_bindings(code: &str) -> Vec<(Vec<String>, String)> {
    let lines: Vec<&str> = code.lines().collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if !trimmed.starts_with("let ") {
            index += 1;
            continue;
        }
        let indent = line.len() - trimmed.len();
        let mut statement = String::new();
        let mut cursor = index;
        while cursor < lines.len() {
            let current = lines[cursor];
            statement.push_str(current);
            statement.push('\n');
            let current_trimmed = current.trim_start();
            let current_indent = current.len() - current_trimmed.len();
            if current_trimmed.ends_with(';') && (cursor == index || current_indent <= indent) {
                break;
            }
            cursor += 1;
        }
        index = cursor + 1;
        let Some(eq) = statement.find('=') else {
            continue;
        };
        let pattern = &statement[4..eq];
        let names: Vec<String> = pattern
            .split(|c: char| !is_ident_char(c))
            .filter(|token| !token.is_empty() && *token != "mut" && *token != "Some")
            .map(str::to_string)
            .collect();
        out.push((names, statement[eq + 1..].to_string()));
    }
    out
}

/// Everything in the handler that already carries the gate's decision: the
/// repository model the extractor produced, the `{owner}/{name}` it validated,
/// the authenticated user — and anything fetched with one of those beside it.
fn scoped_bindings(function: &Function) -> HashSet<String> {
    let sig = signature(&function.body);
    let mut scoped: HashSet<String> = HashSet::new();

    for param in path_params(sig) {
        if GATE_IDENTITY_PARAMS.contains(&param.as_str()) {
            scoped.insert(param);
        }
    }
    for (gate, model) in GATE_BINDINGS {
        if mentions(sig, gate) {
            scoped.insert((*model).to_string());
        }
    }
    for (marker, offset) in [
        ("repo:", 5usize),
        ("org:", 4),
        ("actor_id:", 9),
        ("AuthUser(", 9),
    ] {
        if let Some(at) = sig.find(marker) {
            let rest = sig[at + offset..].trim_start();
            let name: String = rest.chars().take_while(|c| is_ident_char(*c)).collect();
            if !name.is_empty() {
                scoped.insert(name);
            }
        }
    }

    let code = code_only(&function.body[sig.len()..]);
    // Two passes so that a binding introduced late is still available to the
    // `let` above it in nested-match code; a third would buy nothing here.
    for _ in 0..2 {
        for (names, rhs) in let_bindings(&code) {
            let derived = mentions_any(&rhs, &scoped)
                || rhs.contains("authenticated_user_id")
                || rhs.contains("extract_user_id");
            if derived {
                scoped.extend(names);
            }
        }
    }
    scoped
}

/// A comparison of some row's `*_id` against something the gate produced.
fn has_scope_comparison(code: &str, scoped: &HashSet<String>) -> bool {
    for line in code.lines() {
        for operator in ["==", "!="] {
            let Some(at) = line.find(operator) else {
                continue;
            };
            let left: String = line[..at]
                .trim_end()
                .chars()
                .rev()
                .take_while(|c| is_ident_char(*c) || *c == '.')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if !left.contains('.') || !left.ends_with("_id") {
                continue;
            }
            let right: String = line[at + 2..]
                .trim_start()
                .chars()
                .take_while(|c| is_ident_char(*c) || *c == '.')
                .collect();
            let root = right.split('.').next().unwrap_or_default();
            if !root.is_empty() && scoped.contains(root) {
                return true;
            }
        }
    }
    false
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

fn anchors_for(file: &str, param: &str) -> Option<&'static [&'static str]> {
    ANCHORED
        .iter()
        .find(|(rel, _)| *rel == file)
        .and_then(|(_, rules)| {
            rules
                .iter()
                .find(|(name, _)| *name == param)
                .map(|(_, anchors)| *anchors)
        })
}

fn signed_off(file: &str, handler: &str, param: &str) -> bool {
    SIGNED_OFF.iter().any(|(rel, name, rule_param, _)| {
        *rel == file && (*name == "*" || *name == handler) && *rule_param == param
    })
}

#[test]
fn every_global_id_a_handler_takes_is_accounted_for() {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    files.sort();

    let mut population = 0usize;
    let mut offenders = Vec::new();

    for file in &files {
        let rel = relative(file);
        let text = fs::read_to_string(file).expect("read source file");
        let local_fns: HashSet<String> = functions(&text).into_iter().map(|f| f.name).collect();

        for handler in handlers(&text) {
            let sig = signature(&handler.body);
            let params: Vec<String> = path_params(sig)
                .into_iter()
                .filter(|param| is_global_id(param))
                .collect();
            if params.is_empty() {
                continue;
            }
            let scoped = scoped_bindings(&handler);
            let code = code_only(&handler.body[sig.len()..]);

            for param in params {
                population += 1;

                if mentions(sig, "InstanceAdmin") {
                    continue;
                }
                if anchors_for(&rel, &param)
                    .is_some_and(|anchors| anchors.iter().any(|anchor| calls(&code, anchor)))
                {
                    continue;
                }
                if has_scope_comparison(&code, &scoped) {
                    continue;
                }
                let consumers = consumer_calls(&code, &param, &local_fns);
                if !consumers.is_empty()
                    && consumers
                        .iter()
                        .all(|(_, args)| mentions_any(args, &scoped))
                {
                    continue;
                }
                if consumers.is_empty() {
                    continue;
                }
                if signed_off(&rel, &handler.name, &param) {
                    continue;
                }

                let reached = consumers
                    .iter()
                    .map(|(callee, _)| callee.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                offenders.push(format!(
                    "  {rel}:{} — {}() takes `{param}` and hands it to {reached} with nothing \
                     that ties it to the caller's repository or account",
                    handler.line, handler.name
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a handler acted on an instance-wide id without anchoring it to the access its gate \
         actually checked.\n\
         Do one of: call a helper that re-resolves the id inside the gated repository and add \
         it to `ANCHORED`; pass the gated repository (or the authenticated user) into the same \
         call so the callee can refuse a mismatch; or, if something outside the handler already \
         anchors it, say so in `SIGNED_OFF`. A mismatch answers 404, never 403.\n{}",
        offenders.join("\n")
    );

    assert_eq!(
        population, CENSUS_TOTAL,
        "the number of (handler, path parameter) pairs naming a global id changed.\n\
         This is the denominator the anchoring plan is measured against, so it is written down \
         on purpose: check that the new pair is anchored, then update `CENSUS_TOTAL`."
    );
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
            let params = path_params(signature(&handler.body));
            for (param, anchors) in *rules {
                if params.iter().any(|p| p == param)
                    && !anchors.iter().any(|anchor| calls(&handler.body, anchor))
                {
                    offenders.push(format!(
                        "  {rel}:{} — {}() takes `{param}` and never calls {}()",
                        handler.line,
                        handler.name,
                        anchors.join("() / ")
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
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    let defined: HashSet<String> = files
        .iter()
        .flat_map(|file| {
            let text = fs::read_to_string(file).expect("read source file");
            functions(&text).into_iter().map(|f| f.name)
        })
        .collect();

    for (rel, rules) in ANCHORED {
        let path = src_root().join(rel);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("ANCHORED names {rel} but it cannot be read: {e}"));
        let found = handlers(&text);

        for (param, anchors) in *rules {
            for anchor in *anchors {
                assert!(
                    defined.contains(*anchor),
                    "{rel} is guarded against {anchor}() but nothing in src/ defines it — the \
                     rule guards nothing"
                );
            }
            let matched = found
                .iter()
                .filter(|h| path_params(signature(&h.body)).iter().any(|p| p == param))
                .count();
            assert!(
                matched > 0,
                "no handler in {rel} destructures `{param}` — the rule pairing it with {} \
                 matches nothing and would stay green through any rename",
                anchors.join("() / ")
            );
        }
    }
}

/// The runner sign-off says a *route layer* anchors `runner_id`. This asks the
/// route table whether the layer is there.
///
/// Everything else in this file reads source, and source cannot show a
/// middleware: the handlers signed off below take `runner_id` and hand it to
/// `assigned_job` with nothing beside it, which is only safe because
/// `authenticate_runner` already refused any token that does not belong to that
/// runner. Until now the exemption rested on the sentence in [`SIGNED_OFF`]
/// saying so — the same shape of claim that let `POST /runners/register` declare
/// a runner-token middleware it did not carry (card_905f6e81efdd).
///
/// So the claim is checked where it can be: every route whose handler is one of
/// the signed-off ones must have been registered with the runner credential
/// layer. Drop `&runner_auth` from any of them and this fails, naming the route
/// — the id it hands on is global from that moment.
#[tokio::test]
async fn the_runner_sign_off_names_a_layer_the_routes_carry() {
    /// `api/runners.rs` → `rg_http::api::runners::` — the prefix a handler
    /// defined in that file carries in its `type_name`.
    fn module_prefix(rel: &str) -> String {
        let stem = rel.strip_suffix(".rs").unwrap_or(rel);
        format!("rg_http::{}::", stem.replace('/', "::"))
    }

    let mut exempt: HashSet<String> = HashSet::new();
    for (rel, name, param, _) in SIGNED_OFF {
        if *param != "runner_id" {
            continue;
        }
        let text = fs::read_to_string(src_root().join(rel))
            .unwrap_or_else(|e| panic!("SIGNED_OFF names {rel} but it cannot be read: {e}"));
        for handler in handlers(&text) {
            let sig = signature(&handler.body);
            // Same precedence the census uses: an `InstanceAdmin` signature
            // accounts for a global id on its own, so `delete_runner_admin` and
            // its siblings never reach the sign-off and are not claiming the
            // layer. They sit in `api/runners.rs` and are swept up by the
            // blanket `*`, which is the only reason they appear here at all.
            if mentions(sig, "InstanceAdmin") {
                continue;
            }
            if (*name == "*" || handler.name == *name)
                && path_params(sig).iter().any(|p| p == param)
            {
                exempt.insert(format!("{}{}", module_prefix(rel), handler.name));
            }
        }
    }
    assert!(
        !exempt.is_empty(),
        "no handler is signed off for `runner_id` any more — this test now checks nothing, so \
         either the sign-off moved or this test outlived it"
    );

    let (_base, facts) = spawn_test_app_with_routes().await;
    let mut checked = 0;
    let mut offenders = Vec::new();
    for fact in &facts {
        if !exempt.contains(fact.handler) {
            continue;
        }
        checked += 1;
        if fact.credential != Some(RUNNER_AUTH_LAYER) {
            offenders.push(format!(
                "  {} → {} carries {}",
                fact.label(),
                fact.handler,
                match fact.credential {
                    Some(other) => format!("`{other}`"),
                    None => "no credential layer".to_string(),
                }
            ));
        }
    }

    assert!(
        checked > 0,
        "none of the handlers signed off for `runner_id` is on a route — the sign-off describes \
         a route layer, so a handler that reaches no route cannot be relying on one"
    );
    assert!(
        offenders.is_empty(),
        "a handler signed off for `runner_id` is on a route that does not carry `{RUNNER_AUTH_LAYER}`.\n\
         The sign-off is what excuses it from anchoring the id in its own body; without the layer \
         the id is whatever the caller typed, and the job it opens is whoever's.\n{}",
        offenders.join("\n")
    );
}

/// A sign-off names a handler that must still exist, or it is an exemption for
/// something that has been gone for a year.
#[test]
fn every_sign_off_still_names_a_live_handler() {
    for (rel, name, param, reason) in SIGNED_OFF {
        assert!(
            reason.len() > 40,
            "the sign-off for {rel}::{name} `{param}` has no real reason written on it"
        );
        let path = src_root().join(rel);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("SIGNED_OFF names {rel} but it cannot be read: {e}"));
        let matched = handlers(&text)
            .iter()
            .filter(|h| *name == "*" || h.name == *name)
            .filter(|h| path_params(signature(&h.body)).iter().any(|p| p == param))
            .count();
        assert!(
            matched > 0,
            "SIGNED_OFF exempts {rel}::{name} for `{param}`, but no such handler takes that \
             parameter any more — drop the entry rather than leaving a standing exemption"
        );
    }
}

/// The leaf comparisons every chain of delegation rests on. Nothing else in
/// this file can see them: the census reads calls, and these are `if`s.
#[test]
fn the_leaf_anchors_still_compare_what_they_promise() {
    for (rel, function, comparisons) in LEAF_COMPARISONS {
        let path = src_root().join(rel);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("LEAF_COMPARISONS names {rel} but it cannot be read: {e}"));
        let body = functions(&text)
            .into_iter()
            .find(|f| f.name == *function)
            .unwrap_or_else(|| panic!("{rel} no longer defines {function}()"))
            .body;
        let code = code_only(&body);

        for comparison in *comparisons {
            assert!(
                code.contains(comparison),
                "{rel}::{function}() no longer contains `{comparison}`.\n\
                 That comparison is the only thing tying a row addressed by a global id to the \
                 repository the caller was admitted to — it has no parent row of its own to \
                 inherit the scope from, and deleting it leaves code that compiles and reads \
                 fine. If the check moved, move this entry with it; if it was reworded, reword \
                 this entry too."
            );
        }
    }
}

/// The body-borne ids are outside what the census can demand, so the least this
/// file can do is refuse to let their anchors disappear quietly. Checking that
/// the anchor is *defined* and that the field is still read is not the same as
/// checking it is *called* — that is exactly the gap being recorded.
#[test]
fn every_body_borne_id_still_has_its_anchor() {
    for (rel, field, anchor) in BODY_BORNE_IDS {
        let path = src_root().join(rel);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("BODY_BORNE_IDS names {rel} but it cannot be read: {e}"));

        assert!(
            text.contains(&format!("async fn {anchor}(")),
            "{rel} is recorded as anchoring the body field `{field}` with {anchor}(), but does \
             not define it — either the anchor was renamed and the note is stale, or the check \
             is gone"
        );
        assert!(
            text.contains(*field),
            "{rel} no longer mentions `{field}` — drop the BODY_BORNE_IDS entry, or the note \
             claims a gap that closed"
        );
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
