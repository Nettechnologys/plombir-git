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
//! `pub async fn` under `src/`, takes every path parameter that names an
//! instance-wide key ([`is_global_id`] — `id` / `*_id`, and since
//! card_1cfb4519ff04 `uuid` / `*_uuid` too), and demands that each one be
//! anchored in a way this file can *read*.
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
//! `api/ci.rs` was the largest of them and the last: *five* handlers compared
//! `pipeline.repo_id` against `repo.id` in three different spellings, so the
//! file held no call for this guard to read and was not in [`ANCHORED`] at all —
//! its `{id}` passed the census on the weak "scope comparison" branch, and a
//! sixth pipeline route could have been added with no anchor and nothing red.
//! `ci::pipeline_in_repo` is that name, and it is the first anchor deliberately
//! **shared across modules**: `api/artifacts.rs` and `api/ci_environments.rs`
//! carried their own copies of the same rule, and the artifacts copy had already
//! drifted — it converted a database failure through `AppError::internal`, which
//! is a flat 500 and skips the classification that answers 503 on an outage.
//! Anchors stay file-local by default, but "the same rule, spelled three times
//! in three modules" is the generator this whole family of defects comes from,
//! so the copies were collapsed rather than kept in step by hand.
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

use rg_http::route_table::RUNNER_AUTH_LAYER;

use crate::common::source_scan::{
    calls, crate_relative, declarations, declares_public_async, functions, handlers, is_ident_char,
    production_calls_qualified, production_rust_code_only, relative, rust_code_only, rust_files,
    src_root, workspace_crates, Function,
};
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
/// re-tie the id through different helpers — a review id is placed by
/// `review_in_pr` on most routes and by `require_suggestion_source` on the one
/// that applies a suggestion.
///
/// A handler that takes the id through an *extractor* leaves this table
/// altogether: `api/artifacts.rs` used to anchor `artifact_id` with a
/// `require_artifact_read` / `require_artifact_write` prologue of its own, and
/// now takes `ArtifactRead` / `ArtifactWrite`, which resolve the repository
/// from the artifact and gate it before the handler is entered
/// (card_1ec383429aea). There is no path parameter left for the census to see,
/// and no way to forget the anchor — which is why the count below went down by
/// three.
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
        "api/ci.rs",
        &[
            // `get_pipeline` / `retry_pipeline` / `cancel_pipeline` spell the
            // pipeline id `id`; the two job routes carry it as `pipeline_id`.
            // One helper answers for both, and it is the same one
            // `api/artifacts.rs` and `api/ci_environments.rs` call.
            ("id", &["pipeline_in_repo"]),
            ("pipeline_id", &["pipeline_in_repo"]),
            // A job belongs to a stage and a stage to a pipeline, so the job's
            // anchor is the pipeline that `pipeline_in_repo` has just placed —
            // the same chaining as `delivery_in_webhook`.
            ("job_id", &["job_belongs_to_pipeline"]),
        ],
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
            // The pipeline id is instance-wide while `RepoRead` only proves
            // something about `{owner}/{name}`, so the listing route re-ties it
            // to the repository the gate admitted. The helper is `api/ci.rs`'s —
            // an anchor is matched by the call, not by where it is defined, and
            // this file used to hold a second copy of that rule.
            ("pipeline_id", &["pipeline_in_repo"]),
            // The upload and staging routes are runner routes: the job must
            // belong to the runner whose token the middleware already checked.
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
        "oci.rs",
        &[
            // `{uuid}` names a blob-upload session, and sessions are found by
            // that uuid alone — the `oci_upload` row carries its repository,
            // but nothing compared the two. `PATCH` rewrote the offset of a
            // stranger's session and `PUT` deleted its row, both from inside a
            // repository the caller legitimately holds `push` on. One helper
            // answers for every upload-session handler.
            ("uuid", &["upload_in_repo"]),
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
    (
        "api/artifacts.rs",
        "stage_artifact",
        "runner_id",
        "same route layer again — staging is the byte half of the upload above, mounted \
         on the same runner path and held to the layer by the same test",
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
/// `orgs.rs` is the second, and it is here for the same reason one axis over.
/// `resolve_team_in_org` passes the census on the *scoped call* branch — the
/// handlers hand it `org.id` beside the `team_id`, which is all a source reader
/// can see — so what the guard actually checks is the shape of the call site,
/// and the guard files were the only thing that had ever looked at the callee.
/// Delete the `if` inside it and every call site still reads correctly, every
/// type still lines up, and any organization admin can reach every team on the
/// instance through their own path.
const LEAF_COMPARISONS: &[(&str, &str, &[&str])] = &[
    (
        "api/attachments.rs",
        "resolve",
        &["issue.repo_id != repo.id", "pull.repo_id != repo.id"],
    ),
    (
        "api/orgs.rs",
        "resolve_team_in_org",
        &["team.org_id == org_id"],
    ),
    (
        "oci.rs",
        "upload_in_repo",
        &["upload.oci_repository_id == oci_repo.id"],
    ),
];

/// The `rg_core::release::service` functions that take a release or asset id
/// with no repository beside it, and the one file allowed to call them.
///
/// The guard above only reads the files in [`ANCHORED`], so on its own it says
/// nothing about a *second* module deciding to reach a release directly —
/// which is the same hole one file over. These are the primitives the anchoring
/// convention exists for, so they are barred everywhere else in the
/// **workspace**, the way `authz_extractor_guard` bars the raw `require_*`
/// gates: an id-taking call outside the file that anchors ids is the defect,
/// wherever it appears. They are `pub` in `rg_core::release::service`, so
/// "wherever" is every crate that can depend on `rg-core`, not just this one —
/// see [`the_unscoped_release_primitives_are_not_reached_from_the_other_crates_either`].
///
/// `create_release` and `list_releases` are absent on purpose — both take the
/// repository id itself and are scoped by their own signature. That is not a
/// detail to lose in a later edit: the importer in `rg-core` calls
/// `create_release` twice, and it is the deliberate absence of that name here
/// that keeps those calls legitimate rather than signed off.
const RELEASE_PRIMITIVES: &[&str] = &[
    "get_release",
    "update_release",
    "delete_release",
    "get_asset",
    "list_assets",
    "upload_asset_from_file",
    "download_asset",
    "delete_asset",
    "sign_asset_attestation",
    "get_asset_attestation",
    "verify_asset_attestation",
];

/// The file that owns the release/asset anchoring helpers.
const RELEASE_API: &str = "api/releases.rs";

/// Where the barred primitives are defined, relative to `crates/`.
///
/// It is walked like every other file rather than signed off. The definitions
/// read `pub async fn get_release(` and any call between them would be a bare
/// name, so the qualified path this guard looks for does not occur there and no
/// exemption is needed — while a blanket sign-off would quietly cover the
/// *next* function added to that module, which is the mistake
/// `authz_extractor_guard` had to undo for `rg-core/src/org/mod.rs`.
const RELEASE_SERVICE_HOME: &str = "rg-core/src/release/service.rs";

/// Files outside `rg-http` allowed to reach a release or asset by its global
/// id, each with the reason.
///
/// Empty on purpose, and the emptiness is the statement rather than an
/// oversight: nothing outside the HTTP layer holds a release id today, and the
/// only call `rg-core` makes into that module is `create_release`, which takes
/// the repository id itself and is deliberately absent from
/// [`RELEASE_PRIMITIVES`]. An empty list with a reason can be read and argued
/// with; no list at all is what the guard had before, and that is
/// indistinguishable from a rule nobody ever extended past its own crate.
const RELEASE_PRIMITIVE_SIGNED_OFF: &[(&str, &str)] = &[];

/// One family of `rg-db` primitives that address a row by a key unique across
/// the whole instance, and the files allowed to reach them.
struct UnscopedRowPrimitives {
    /// The `ops` module the calls are spelled through. Doubles as the marker a
    /// line must carry before it is worth reading, and as the module an import
    /// would hide the call behind.
    module: &'static str,
    /// Where the primitives are defined, relative to `crates/`.
    home: &'static str,
    /// The names that take a global key with no container beside it.
    names: &'static [&'static str],
    /// Files, relative to `crates/`, allowed to call them — each with the
    /// reason. The reason is the point of the entry, exactly as in
    /// [`SIGNED_OFF`]: an allow-list without one is just an exemption list.
    anchored_by: &'static [(&'static str, &'static str)],
}

/// The database primitives one layer below [`RELEASE_PRIMITIVES`], and their
/// anchors.
///
/// [`RELEASE_PRIMITIVES`] bars `rg-core` service functions. These are the
/// `rg-db` `ops` functions underneath them: the ones whose `WHERE` names a
/// single instance-wide key and no container at all, so nothing in the callee
/// can object to an id from the wrong repository. The convention is the same
/// one this whole file exists for — an id is re-tied to the gated container by
/// a named helper, and the helper is the only caller — and so is the rule: the
/// primitive is barred everywhere else in the workspace, because `pub` is what
/// sets a rule's reach.
///
/// card_07c571dfdf95 is why the table exists. Two of these primitives had **no
/// callers**: `oci_ops::complete_upload` (`UPDATE … WHERE uuid = ?`) and
/// `attachment_ops::find_by_uuid`. A dead unscoped primitive is worse than a
/// live one, not better — nobody has ever had reason to read it, and the next
/// handler finds it by name. Both were deleted rather than listed, which is why
/// neither appears below; the entries that remain are the ones with a real
/// anchor, and this table is what keeps that anchor the only way in.
///
/// The *writers* of `oci_upload` are deliberately absent for the same reason
/// `create_release` is absent from [`RELEASE_PRIMITIVES`]:
/// `update_upload_progress` and `delete_upload` take `oci_repo_id` and filter
/// on it, so they are scoped by their own signature and need no caller to be
/// careful. That absence is load-bearing — adding them here would say the
/// opposite of what the code does.
const UNSCOPED_ROW_PRIMITIVES: &[UnscopedRowPrimitives] = &[
    UnscopedRowPrimitives {
        module: "artifact_ops",
        home: "rg-db/src/ops/artifact_ops.rs",
        names: &["get_by_id", "delete_by_id", "exists_by_file_path"],
        anchored_by: &[
            (
                "rg-http/src/api/artifacts.rs",
                "`Artifact::resolve` anchors global artifact ids through job, stage and pipeline \
                 to the repository gate; deletion only receives that anchored model",
            ),
            (
                "rg-http/src/api/ci_retention.rs",
                "the retention sweep receives `artifact.id` from `list_expired`, optionally \
                 re-anchors its job to the requested repository, and hands that same model to \
                 reversible storage staging before deleting its row",
            ),
            (
                "rg-core/src/deletion_recovery.rs",
                "CI artifact publication recovery reads the exact request-private blob key from \
                 its trusted journal and uses `exists_by_file_path` only as a fail-closed \
                 ownership test: a row keeps the blob, absence permits cleanup, and a DB error \
                 permits nothing",
            ),
        ],
    },
    UnscopedRowPrimitives {
        module: "attachment_ops",
        home: "rg-db/src/ops/attachment_ops.rs",
        names: &[
            "find_by_id",
            "exists_by_blob_key",
            "delete_by_id",
            "increment_download_count",
        ],
        anchored_by: &[
            (
                "rg-core/src/attachment.rs",
                "`get_attachment` is the anchor: it reads the row by its global id and then \
                 refuses it — as a 404 — unless `attachment.repo_id` is the repository the \
                 request was gated on and the target matches. The two writers in this file are \
                 only ever handed `attachment.id` off a model that call has already placed",
            ),
            (
                "rg-http/src/api/attachments.rs",
                "`stream_attachment` bumps the download counter on the model \
                 `rg_core::attachment::get_attachment` handed back one call earlier, so the id \
                 is anchored before this file ever sees it",
            ),
            (
                "rg-core/src/deletion_recovery.rs",
                "attachment publication recovery reads the exact request-private blob key from \
                 its trusted journal and uses `exists_by_blob_key` only as a fail-closed ownership \
                 test: a row keeps the blob, absence permits cleanup, and a DB error permits \
                 nothing",
            ),
        ],
    },
    UnscopedRowPrimitives {
        module: "package_file_ops",
        home: "rg-db/src/ops/package_file_ops.rs",
        names: &["exists_by_storage_path"],
        anchored_by: &[(
            "rg-core/src/deletion_recovery.rs",
            "package publication recovery reads the exact request-private storage key from its \
             trusted journal and uses `exists_by_storage_path` only as a fail-closed ownership \
             test: a row keeps the blob, absence permits cleanup, and a DB error permits \
             nothing",
        )],
    },
    UnscopedRowPrimitives {
        module: "oci_ops",
        home: "rg-db/src/ops/oci_ops.rs",
        names: &["find_upload"],
        anchored_by: &[(
            "rg-http/src/oci.rs",
            "`upload_in_repo` is the anchor — it compares \
             `upload.oci_repository_id == oci_repo.id` and answers `BLOB_UPLOAD_UNKNOWN` \
             otherwise. Every OCI handler goes through it rather than through the read; going \
             through the read directly is what card_1cfb4519ff04 was",
        )],
    },
];

/// How many (handler, path parameter) pairs in `src/` name a global id.
///
/// The number is written down so that adding a route which takes one is a
/// deliberate act: the census fails until the new pair is classified *and* this
/// count is updated. It is the denominator the plan for this guard was missing.
const CENSUS_TOTAL: usize = 129;

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

/// Everything up to the `)` that closes the parameter list, as a byte-aligned
/// code-only view of it.
///
/// Two things were read off the raw text here and both were wrong for the same
/// reason — a parameter may carry an attribute, and an attribute may carry a
/// raw string:
///
/// * The boundary was the first `") ->"` in the body. A parameter attribute
///   spelled `#[doc = r#"reader) -> decoy"#]` ends the signature at the decoy,
///   so every parameter behind it leaves the census — the pair is not
///   classified, it is simply gone from the denominator — and the consumer scan
///   that starts at `body[sig.len()..]` starts *inside* the signature. So the
///   `)` is found by balancing parentheses in a [`rust_code_only`] view, where
///   no literal has delimiters left to contribute; balancing rather than
///   scanning for `") ->"` also keeps a `impl Fn(i64) -> bool` parameter from
///   closing the list early, and gives the right answer for a function that
///   declares no return type at all.
/// * The consumers then match `InstanceAdmin` and the [`GATE_BINDINGS`] against
///   that text. On the raw view a `#[doc = r#"InstanceAdmin"#]` reads as the
///   instance gate and excuses the handler from anchoring anything, which is
///   the whole finding inverted. They are handed the masked view instead, and
///   since it is byte-for-byte aligned every existing `sig.len()` / `sig.find`
///   offset still addresses the same place in the original body.
fn signature(body: &str) -> String {
    let code = rust_code_only(body);
    match params_close(&code) {
        Some(at) => code[..=at].to_string(),
        None => code,
    }
}

/// Byte offset of the `)` that closes the parameter list, in a code-only view.
///
/// The list is located from the `fn` token rather than from the first `(`:
/// `pub(crate) async fn handler(…)` has a paren of its own three characters in,
/// and balancing from there returns after `(crate)` — the same trap
/// `source_scan::signature_params` documents.
fn params_close(code: &str) -> Option<usize> {
    let declaration = code.lines().next()?;
    let after_fn = declaration.find("fn ")? + "fn ".len();
    let open = after_fn + code[after_fn..].find('(')?;

    let bytes = code.as_bytes();
    let mut depth = 0usize;
    for (at, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
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

/// Whether a path parameter names a key that is unique across the instance
/// rather than inside the gated container.
///
/// The name is the only signal a source scan has, and for a while the rule was
/// "`id` or `*_id`" — which is a statement about spelling, not about scope. The
/// OCI registry addresses an upload session by `{uuid}`, a key drawn from the
/// same instance-wide pool as any primary key, and `src/oci.rs` was therefore
/// not merely unanchored but outside the denominator: the census counted zero
/// pairs there, so its `0/0` read the same as full coverage (card_1cfb4519ff04).
///
/// `uuid` / `*_uuid` is the widening that finding forced. The other non-`_id`
/// path parameters were re-read at the same time and are scoped by
/// construction, not by an anchor: `digest` and `oid` are content addresses
/// whose lookups build a path from `{owner}/{repo}`, `reference` / `version` /
/// `pkg_name` / `secret_name` / `number` name a row *within* a container the
/// gate proved. They stay out of the census on purpose — but the fact that a
/// parameter's name is a weak proxy for its scope is now written down here
/// rather than assumed.
fn is_global_id(param: &str) -> bool {
    !param.starts_with('_')
        && (param == "id" || param == "uuid" || param.ends_with("_id") || param.ends_with("_uuid"))
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

#[test]
fn consumer_calls_survive_delimiter_shaped_raw_literals() {
    let source = r#####"pub async fn synthetic(Path(id): Path<i64>) -> impl IntoResponse {
    let _label = r###"// ), ("###; rg_db::ops::release::get_release(&state.db, id).await;
}
"#####;
    let handler = handlers(source)
        .into_iter()
        .next()
        .expect("synthetic handler");
    let sig = signature(&handler.body);
    assert_eq!(path_params(&sig), vec!["id"]);

    let code = rust_code_only(&handler.body[sig.len()..]);
    let local_fns = functions(source)
        .into_iter()
        .map(|function| function.name)
        .collect();
    assert!(
        code.contains("rg_db::ops::release::get_release(&state.db, id)"),
        "the code-only view lost the live consumer: {code:?}"
    );
    let consumers = consumer_calls(&code, "id", &local_fns);

    assert_eq!(consumers.len(), 1, "consumer scan returned {consumers:?}");
    assert_eq!(consumers[0].0, "rg_db::ops::release::get_release");
    assert!(mentions(&consumers[0].1, "id"));
}

/// A parameter may carry an attribute, and an attribute may carry a raw string.
///
/// Everything [`signature`] hands out is read back here from a handler whose
/// first parameter is annotated with one that spells, in order, an instance
/// gate, a scoped binding and a `") ->"`. Read off the raw text, that one
/// attribute is enough to make the guard green three separate ways: the
/// decoy `) ->` ends the signature before the real `Path((…))`, so the pair
/// leaves the census denominator entirely and the consumer scan starts inside
/// the signature; `InstanceAdmin` excuses whatever is left from anchoring; and
/// `repo:` binds a name the gate never produced, so any call carrying it reads
/// as scoped.
#[test]
fn the_signature_boundary_survives_delimiter_shaped_parameter_attributes() {
    let source = r#####"pub(crate) async fn synthetic(
    #[doc = r###"InstanceAdmin, repo: forged, reader) -> decoy"###]
    RepoRead { repo, .. }: RepoRead,
    Path((owner, name, release_id)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    rg_db::ops::release::get_release(&state.db, release_id).await;
}
"#####;
    let handler = handlers(source)
        .into_iter()
        .next()
        .expect("synthetic handler");
    let sig = signature(&handler.body);

    assert_eq!(
        path_params(&sig),
        vec!["owner", "name", "release_id"],
        "a raw parameter attribute ended the signature before the real `Path((…))`, so the \
         census never saw the pair: {sig:?}"
    );
    assert!(
        !mentions(&sig, "InstanceAdmin"),
        "a documented type name read as the instance gate, which excuses a handler from \
         anchoring anything: {sig:?}"
    );
    let scoped = scoped_bindings(&handler);
    assert!(
        !scoped.contains("forged"),
        "a `repo:` inside a literal bound a name the gate never produced: {scoped:?}"
    );
    assert!(
        scoped.contains("repo"),
        "the real `RepoRead` gate binding was lost with it: {scoped:?}"
    );

    let code = rust_code_only(&handler.body[sig.len()..]);
    let local_fns = functions(source)
        .into_iter()
        .map(|function| function.name)
        .collect();
    let consumers = consumer_calls(&code, "release_id", &local_fns);
    assert_eq!(
        consumers.len(),
        1,
        "the body the consumer scan starts on began inside the signature: {consumers:?}"
    );
    assert_eq!(consumers[0].0, "rg_db::ops::release::get_release");
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

    for param in path_params(&sig) {
        if GATE_IDENTITY_PARAMS.contains(&param.as_str()) {
            scoped.insert(param);
        }
    }
    for (gate, model) in GATE_BINDINGS {
        if mentions(&sig, gate) {
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

    let code = rust_code_only(&function.body[sig.len()..]);
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
            let params: Vec<String> = path_params(&sig)
                .into_iter()
                .filter(|param| is_global_id(param))
                .collect();
            if params.is_empty() {
                continue;
            }
            let scoped = scoped_bindings(&handler);
            let code = rust_code_only(&handler.body[sig.len()..]);

            for param in params {
                population += 1;

                if mentions(&sig, "InstanceAdmin") {
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
            let params = path_params(&signature(&handler.body));
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

/// Every line of `text` that reaches one of `names` through `module`'s
/// qualified path, or imports the module that path is spelled through.
///
/// `rel` only labels the offenders, so the same rule reads the same way in
/// `crates/rg-http/src` and in the rest of the workspace — the callers differ
/// in which tree they walk and in nothing else. That matters more than the
/// saved lines: the crate-scoped guard existed for a while before the
/// workspace one, and a second, hand-copied line rule is how the two would have
/// drifted apart.
///
/// The module and the names are parameters rather than the two literals they
/// started as because a third caller now exists —
/// [`the_unscoped_row_primitives_are_only_reachable_from_the_files_that_anchor_them`]
/// asks the same question of the `rg-db` primitives one layer below. Writing
/// that scan out a second time is precisely the drift this doc comment already
/// warned about.
///
/// **Test code is not a subject of this rule.** The defect it names is a served
/// request reaching a row by an instance-wide id without the repository beside
/// it; a `#[cfg(test)]` fixture calling the same primitive answers nobody and
/// has the whole database to itself by construction. So the scan reads
/// [`production_rust_code_only`], the same view the liveness half of this file
/// already uses through [`production_calls_qualified`] (card_2624b261cef7) —
/// otherwise the two halves of one rule disagree about what counts as code, and
/// the offender half is the one that reports a defect nobody can fix without
/// deleting a test. `rg-core/src/repo/service.rs` alone holds some forty gate
/// calls inside its test module; that none of them is an offender today is an
/// accident of `functions()` anchoring on column 0, not a decision.
fn unscoped_primitive_offenders(
    rel: &str,
    text: &str,
    module: &str,
    names: &[&str],
) -> Vec<String> {
    let mut offenders = Vec::new();
    let code_only = production_rust_code_only(text);

    for (n, (line, original)) in code_only.lines().zip(text.lines()).enumerate() {
        let code = line.trim_start();
        if !line.contains(module) {
            continue;
        }
        // An import would let the qualified path — the thing this guard reads
        // — disappear from the call site entirely, whether it names the
        // functions (`use rg_core::release::service::delete_asset;`) or only
        // the module (`use rg_core::release::service;`, and then a bare
        // `service::delete_asset(…)` this scan cannot see).
        if code.starts_with("use ") {
            offenders.push(format!("  {rel}:{} — {}", n + 1, original.trim()));
            continue;
        }
        for name in names {
            if line.contains(&format!("{module}::{name}(")) {
                offenders.push(format!("  {rel}:{} — {}", n + 1, original.trim()));
                break;
            }
        }
    }

    offenders
}

#[test]
fn unscoped_primitive_scan_ignores_non_code_decoys_and_keeps_original_lines() {
    const SAMPLE: &str = r####"
// use rg_core::release::service;
/* release::service::delete_asset(db, id).await?; */
let normal = "release::service::delete_asset(db, id)";
let raw = r#"use release::service; delete_asset(db, id)"#;
let bytes = b"release::service::delete_asset(db, id)";
let _ = raw; release::service::delete_asset(db, id).await?;
use release::service::get_release;

#[cfg(test)]
mod tests {
    use release::service::delete_asset;
    async fn fixture(db: &DatabaseConnection, id: i64) {
        let brace_in_a_literal = "}";
        let _ = brace_in_a_literal;
        release::service::delete_asset(db, id).await.unwrap();
    }
}

let _ = release::service::get_release(db, id).await?;
"####;

    let offenders = unscoped_primitive_offenders(
        "sample.rs",
        SAMPLE,
        "release::service",
        &["delete_asset", "get_release"],
    );
    // Lines 10-18 are the inline test item: neither its `use` nor its call is
    // an offender, and the production line that follows it still is — a
    // file-tail exemption would have swallowed that one with them.
    assert_eq!(
        offenders,
        vec![
            "  sample.rs:7 — let _ = raw; release::service::delete_asset(db, id).await?;",
            "  sample.rs:8 — use release::service::get_release;",
            "  sample.rs:20 — let _ = release::service::get_release(db, id).await?;",
        ]
    );
}

#[test]
fn anchor_liveness_ignores_non_code_and_test_only_calls_and_keeps_live_calls() {
    const DECOYS: &str = r####"
// attachment_ops::find_by_id(db, id);
/* attachment_ops::find_by_id(db, id); */
let normal = "attachment_ops::find_by_id(db, id)";
let raw = r#"attachment_ops::find_by_id(db, id)"#;
let bytes = b"attachment_ops::find_by_id(db, id)";
let raw_bytes = br#"attachment_ops::find_by_id(db, id)"#;
let _ = (normal, raw, bytes, raw_bytes);
not_attachment_ops::find_by_id(db, id);
other_ops::find_by_id(db, id);

#[cfg(test)]
mod tests {
    async fn fixture(db: &DatabaseConnection, id: i64) {
        let brace_in_a_literal = "}";
        let _ = brace_in_a_literal;
        rg_db::ops::attachment_ops::find_by_id(db, id).await.unwrap();
    }
}
"####;

    assert!(
        !production_calls_qualified(DECOYS, "attachment_ops", "find_by_id"),
        "comments, literals, prefix collisions, a same-name call through another module, and a \
         call that only exists inside a `#[cfg(test)]` item are not evidence that the allowed \
         anchor still reaches attachment_ops::find_by_id"
    );

    // Appended *after* the inline test item on purpose: the exemption has to
    // survive on production code that follows a test module, not only on code
    // above the first one.
    let live = format!("{DECOYS}\nrg_db::ops::attachment_ops::find_by_id(db, id).await?;\n");
    assert!(production_calls_qualified(
        &live,
        "attachment_ops",
        "find_by_id"
    ));
}

/// [`unscoped_primitive_offenders`] for the release family.
fn release_primitive_offenders(rel: &str, text: &str) -> Vec<String> {
    unscoped_primitive_offenders(rel, text, "release::service", RELEASE_PRIMITIVES)
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
        offenders.extend(release_primitive_offenders(&rel, &text));
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

/// The same rule, in the crates the guard above cannot see.
///
/// [`the_unscoped_release_primitives_are_only_reachable_from_the_file_that_anchors_ids`]
/// walks `crates/rg-http/src` and stops there — its own doc comment said "barred
/// everywhere else in `rg-http`", which was honest and was also the whole
/// problem. The eleven names it bars are `pub` in `rg_core::release::service`,
/// so a job in `rg-ci`, a handler in `rg-mcp`, or another service function in
/// `rg-core` deleting an asset by its id would have sat one crate over from
/// every assertion in this file and been read by none of them.
///
/// This is the third time the same widening has been needed: `PREDICATES` after
/// `fork_repo` was found holding a copy of the repository read rule, then
/// `ORG_MEMBERSHIP`, now these. The lesson each time is the same — the scan
/// belongs to the rule's reach, not to the crate the guard happens to live in,
/// and `pub` is what sets that reach.
///
/// Nothing is red today, and that is the point of doing it while nothing is:
/// the guard is what stops the first such call from being written, and a guard
/// added after the fact is a post-mortem.
#[test]
fn the_unscoped_release_primitives_are_not_reached_from_the_other_crates_either() {
    let crates_dir = workspace_crates();
    let mut offenders = Vec::new();
    let mut scanned = 0usize;

    for entry in fs::read_dir(&crates_dir).expect("read crates dir") {
        let krate = entry.expect("dir entry").path();
        // `rg-http` is the subject of the guard above, which holds it to the
        // stricter rule: there, one named file *may* reach the primitives.
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
            let rel = crate_relative(file);
            scanned += 1;
            if RELEASE_PRIMITIVE_SIGNED_OFF
                .iter()
                .any(|(allowed, _)| rel == *allowed)
            {
                continue;
            }
            let text = fs::read_to_string(file).expect("read source file");
            offenders.extend(release_primitive_offenders(&rel, &text));
        }
    }

    // An empty offender list means one of two things — nobody out here reaches
    // a release by its id, or the walk never ran — and only this tells them
    // apart. It is the assertion the crate-scoped guard makes with `files.len()`.
    assert!(
        scanned > 50,
        "only {scanned} file(s) scanned outside rg-http — the guard is not running"
    );
    assert!(
        offenders.is_empty(),
        "a release or asset was reached by its global id from outside `rg-http`.\n\
         Those service functions take an id with no repository beside them, so the caller is \
         what keeps them inside the right repository — and out here there is no gate in the \
         signature to be that proof at all. Anchor the id in {RELEASE_API}, where \
         `release_in_repo` / `asset_in_repo` do it, and pass the resolved model on; or give the \
         service function a `repo_id` of its own and check it inside. A caller that legitimately \
         addresses the whole instance goes in `RELEASE_PRIMITIVE_SIGNED_OFF` with the reason \
         written down — the list is empty, not absent.\n{}",
        offenders.join("\n")
    );
}

/// The same rule as the two tests above, one layer down: the `rg-db`
/// primitives of [`UNSCOPED_ROW_PRIMITIVES`].
///
/// It is a single walk over every crate rather than the crate-scoped /
/// workspace-scoped pair the release guard needs, because these anchors do not
/// all live in `rg-http`: `attachment_ops` is anchored in `rg-core` and reached
/// from `rg-http`, so a rule that split the workspace at the crate boundary
/// would have to say the same thing twice. Paths are crate-relative
/// throughout — out here the crate is the first thing you need to know about an
/// offending line.
///
/// The defining modules are **not** exempted. They spell their own functions
/// bare (`pub async fn find_by_id(`), so the qualified path this scan reads
/// never occurs there and no exemption is needed — while a blanket sign-off
/// would quietly cover the *next* function added to those modules, which is the
/// mistake [`RELEASE_SERVICE_HOME`] already records for `rg-core/src/org/mod.rs`.
#[test]
fn the_unscoped_row_primitives_are_only_reachable_from_the_files_that_anchor_them() {
    let crates_dir = workspace_crates();
    let mut offenders = Vec::new();
    let mut scanned = 0usize;

    for entry in fs::read_dir(&crates_dir).expect("read crates dir") {
        let src = entry.expect("dir entry").path().join("src");
        if !src.is_dir() {
            continue;
        }

        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for file in &files {
            let rel = crate_relative(file);
            scanned += 1;
            let text = fs::read_to_string(file).expect("read source file");
            for family in UNSCOPED_ROW_PRIMITIVES {
                if family
                    .anchored_by
                    .iter()
                    .any(|(allowed, _)| rel == *allowed)
                {
                    continue;
                }
                offenders.extend(unscoped_primitive_offenders(
                    &rel,
                    &text,
                    family.module,
                    family.names,
                ));
            }
        }
    }

    // An empty offender list means one of two things — nobody reaches these
    // rows by a global key, or the walk never ran — and only this tells them
    // apart.
    assert!(
        scanned > 50,
        "only {scanned} file(s) scanned — the guard is not running"
    );
    assert!(
        offenders.is_empty(),
        "a row was reached by a key that is unique across the whole instance, from a file that \
         does not anchor it.\n\
         These `rg-db` primitives filter on that key alone: the row they return may belong to \
         any repository on the instance, and nothing inside the callee can object. Go through \
         the anchor named in `UNSCOPED_ROW_PRIMITIVES` — it re-ties the id to the container the \
         access gate actually checked and answers 404 on a mismatch — or give the primitive a \
         container column of its own and put it in the `WHERE`, the way \
         `oci_ops::update_upload_progress` takes `oci_repo_id`. A file that legitimately belongs \
         on the short list goes in `anchored_by` with the reason written down.\n{}",
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
                .filter(|h| path_params(&signature(&h.body)).iter().any(|p| p == param))
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
            if mentions(&sig, "InstanceAdmin") {
                continue;
            }
            if (*name == "*" || handler.name == *name)
                && path_params(&sig).iter().any(|p| p == param)
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
            .filter(|h| path_params(&signature(&h.body)).iter().any(|p| p == param))
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
        let code = rust_code_only(&body);

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

/// Whether `text` still reads `field` in code the binary ships.
///
/// The view has to be [`production_rust_code_only`] rather than
/// [`rust_code_only`], and the difference is the whole point of the assertion
/// this backs: it says the field is still read *in executable code*, and a
/// `#[cfg(test)]` fixture is not that. `api/issues.rs` has both
/// `assignee_id: None` and `milestone_id: None` inside its test module, so
/// under the weaker view the two `BODY_BORNE_IDS` entries for that file would
/// stay green after the production read they exist to watch was deleted —
/// which is the one thing a liveness floor is for. Same view, same reason, as
/// the offender half of this file and as `declarations()` next door
/// (card_dfd5da074447, card_d67b6f433341).
fn reads_field_in_production(text: &str, field: &str) -> bool {
    production_rust_code_only(text).contains(field)
}

#[test]
fn a_field_read_only_by_a_test_fixture_is_not_a_live_read() {
    const SAMPLE: &str = r####"
// milestone_id: None,
let quoted = "milestone_id";

#[cfg(test)]
mod tests {
    fn fixture() {
        let row = Row { milestone_id: None, assignee_id: None };
    }
}

pub async fn create_issue(payload: Payload) {
    let _ = payload.assignee_id;
}
"####;

    assert!(
        !reads_field_in_production(SAMPLE, "milestone_id"),
        "a field named only by a comment, a literal and a `#[cfg(test)]` fixture counts as a \
         live read — the BODY_BORNE_IDS entry then survives the deletion of the production \
         read it exists to watch"
    );
    assert!(
        reads_field_in_production(SAMPLE, "assignee_id"),
        "a production read written *after* an inline test module is invisible — blanking the \
         fixture must not take the code below it, or every entry in a file with a test module \
         goes red for the wrong reason"
    );
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
            declarations(&text)
                .iter()
                .any(|declared| declared.name == *anchor && declared.is_async),
            "{rel} is recorded as anchoring the body field `{field}` with {anchor}(), but does \
             not define it — either the anchor was renamed and the note is stale, or the check \
             is gone"
        );
        assert!(
            reads_field_in_production(&text, field),
            "{rel} no longer reads `{field}` in executable code — drop the BODY_BORNE_IDS entry, \
             or the note claims a gap that closed"
        );
    }
}

/// Same reason, for the barred primitives: a name that no longer exists guards
/// nothing, and a rename that silently empties the list is exactly how a
/// source-grep test turns into decoration.
#[test]
fn every_barred_release_primitive_still_exists() {
    let service = workspace_crates().join(RELEASE_SERVICE_HOME);
    let text = fs::read_to_string(&service).unwrap_or_else(|e| {
        panic!("RELEASE_SERVICE_HOME names {RELEASE_SERVICE_HOME}, which cannot be read: {e}")
    });

    for name in RELEASE_PRIMITIVES {
        assert!(
            declares_public_async(&text, name),
            "RELEASE_PRIMITIVES names `{name}`, but rg-core no longer exposes it — drop the \
             entry or follow the rename, or the guard quietly stops covering it"
        );
    }
}

/// A fixture that names a barred primitive does not hold the floor up.
///
/// Both floors above ask [`declares_public_async`] of a whole home file, and a
/// home file is free to have a test module. `pub async fn delete_asset` written
/// there answers for a production function that has been deleted: the floor
/// stays green, and every guard over the list it was meant to keep populated
/// goes quiet in the same commit — precisely the failure the floor exists to
/// make loud. The same mechanism was proven by mutation one guard over
/// (`audit_writer_guard`, card_dfd5da074447), which is why it is pinned here
/// rather than assumed.
///
/// The two samples differ by one production declaration, so a view that blanks
/// the whole file after the first test item fails the second half.
#[test]
fn a_barred_primitive_declared_only_in_a_test_module_does_not_hold_the_floor() {
    const FIXTURE_ONLY: &str = r###"#[cfg(test)]
mod tests {
    pub async fn delete_asset(id: i64) -> bool {
        true
    }
}
"###;
    const STILL_SHIPPED: &str = r###"#[cfg(test)]
mod tests {
    pub async fn delete_asset(id: i64) -> bool {
        true
    }
}

pub async fn delete_asset(id: i64) -> bool {
    true
}
"###;

    // What the floor greps for is present in both, which is the whole point.
    assert!(FIXTURE_ONLY.contains("pub async fn delete_asset("));

    assert!(
        !declares_public_async(FIXTURE_ONLY, "delete_asset"),
        "a `#[cfg(test)]` fixture keeps the floor green after the production primitive it was          watching is deleted — the barred list is then empty and silent"
    );
    assert!(
        declares_public_async(STILL_SHIPPED, "delete_asset"),
        "the shipped primitive is declared after the test module, and blanking that item must          not take it with it"
    );
}

/// Same, for [`UNSCOPED_ROW_PRIMITIVES`] — plus the half the release table has
/// no equivalent of.
///
/// A barred name that no longer exists guards nothing, and that is the failure
/// the test above already names. The second assertion is the one this table
/// needs on its own account: every entry in `anchored_by` claims a file is the
/// anchor, and a file that has stopped calling the primitive altogether is a
/// standing permission for something nobody does any more — the next call
/// written there would be admitted with no anchor in sight and nothing red.
///
/// "Calling it" means calling it in production code. `rg-core/src/attachment.rs`
/// carries a `#[cfg(test)]` call to `attachment_ops::find_by_id` of its own, so
/// reading the whole file left the exemption resting on a fixture: deleting both
/// production calls kept this test green (card_2624b261cef7). The liveness
/// question is asked of [`production_calls_qualified`] for that reason.
#[test]
fn every_barred_row_primitive_still_exists_and_its_anchors_still_reach_it() {
    for family in UNSCOPED_ROW_PRIMITIVES {
        let home = workspace_crates().join(family.home);
        let text = fs::read_to_string(&home).unwrap_or_else(|e| {
            panic!(
                "UNSCOPED_ROW_PRIMITIVES names {}, which cannot be read: {e}",
                family.home
            )
        });

        for name in family.names {
            assert!(
                declares_public_async(&text, name),
                "UNSCOPED_ROW_PRIMITIVES bars `{}::{name}`, but {} no longer defines it — drop \
                 the entry or follow the rename, or the guard quietly stops covering it",
                family.module,
                family.home
            );
        }

        for (allowed, _) in family.anchored_by {
            let path = workspace_crates().join(allowed);
            let source = fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!("UNSCOPED_ROW_PRIMITIVES names {allowed} as an anchor, but it cannot be read: {e}")
            });
            assert!(
                family.names.iter().any(|name| production_calls_qualified(
                    &source,
                    family.module,
                    name
                )),
                "{allowed} is allowed to reach `{}` in production code but calls none of it any \
                 more — a `#[cfg(test)]` fixture does not keep the exemption alive, and the entry \
                 is now a standing permission for nothing, so the next call written there gets in \
                 unnoticed. Drop it from `anchored_by`",
                family.module
            );
        }
    }
}
