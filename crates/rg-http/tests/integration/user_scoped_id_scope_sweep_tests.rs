//! One pass over every route whose gate is `Access::User` and whose path
//! carries an instance-wide id.
//!
//! Three axes of the same doctrine exist, and this is the last one to get a
//! sweep. `cross_repo_id_scope_sweep_tests` walks the routes that name a
//! repository and then act on a global id; `anchored_scope_sweep_tests` walks
//! the ones that resolve the repository *out of* the row. Both of those are
//! about a repository. This one is about an account: the gate is `AuthUser` and
//! nothing more, so being *some* user is the whole of what the caller proved,
//! and `{id}` is a primary key over every access token, SSH key, passkey, import
//! task and notification on the instance.
//!
//!     DELETE /api/v1/users/tokens/<an-id-belonging-to-somebody-else>
//!
//! # Why a sweep, when the axis was already covered
//!
//! It was covered *by name*, and half of it. `user_scoped_id_scope_tests` holds
//! two routes by hand — `/users/tokens/{id}` and `/users/ssh-keys/{id}` — and
//! the doctrine is written down in `api::users::delete_token` ("another
//! account's token answers 404, not 403"). The other five routes of the same
//! shape were checked by nobody: `/users/passkeys/{id}`, `GET` and `DELETE` on
//! `/imports/{id}`, and — invisible to the census the card for this file was
//! written from — `POST /notifications/{id}/read` and
//! `DELETE /notifications/{id}`. A hand-written pair per route is a statement
//! about the routes that existed when somebody last counted, which is precisely
//! how the count came out at five for a population of seven.
//!
//! `global_id_anchor_guard` reads the *shape* of these handlers and cannot close
//! this: it sees whether an id is anchored — a line of code that was not
//! written — and no source guard can compare two *answers*. `import_task_of_user`
//! answering `import task not found` for a foreign row and `not your import` for
//! an absent one would satisfy every rule in that file while enumerating the
//! instance, because a divergent wording is a working anchor and a live oracle at
//! the same time.
//!
//! # The shape
//!
//! Two accounts, and only one of them owns anything. Each route is driven four
//! times, and any two of the requests differ in exactly one place:
//!
//! - **the probe** — the outsider's own valid session, the owner's real id. Owed
//!   a masked refusal: `401` or `404`, and specifically not `403`.
//! - **the reference** — the same outsider, the same route, [`ABSENT_ID`]. Owed
//!   the same answer as the probe, *body included*.
//! - **the anonymous pair** — both of those again with no token at all. `AuthUser`
//!   answers `401` before it resolves anything, so which code they agree on is
//!   not the assertion; that they agree is.
//! - **the baseline** — the owner, the same real id. Owed a success.
//!
//! The baseline is not decoration. A fixture that seeded nothing would answer
//! `404` to everybody, and a wall of `404`s reads as a passing security test.
//! Here it reads as a dead fixture and fails the run.
//!
//! Neither is the reference. `403` for a real id and `404` for an absent one is
//! the pair that makes `{id}` an enumeration oracle — and so is
//! `404 "token not found"` against `404 "not your token"`, one level lower, where
//! no assertion about a status can see it. That is where the oracle went in
//! `card_2179245d41db`, and it is why the comparison is a *pair* rather than a
//! constant.
//!
//! # Two selectors, checked against each other
//!
//! The population is read twice, by two readers that fail for different reasons.
//! [`Access::User`] plus a global-id placeholder is the primary one: it trusts
//! the *declaration* in the route table. The second reads the *code* — a handler
//! whose signature carries `AuthUser` — and holds every route it finds to being
//! driven here too.
//!
//! One reader is not enough, which the anchored sweep learned by mutation
//! (`card_f87450bec7f6`): coverage dies not from a wrong assertion but from a
//! selector that goes quiet. A route mis-declared as `Public` over an `AuthUser`
//! handler is invisible to the first selector and caught by the second; a route
//! that takes its id through an extractor the signature reader cannot parse is
//! invisible to the second and caught by the first. Neither can go quiet on the
//! same day.
//!
//! # What it does not do
//!
//! It sends no request bodies. Every route of this shape today is a `GET`, a
//! `DELETE`, or a `POST` with no required body, and a route that answers
//! `400`/`422` from its deserializer before the gate decides anything is reported
//! as a route this sweep cannot judge rather than passed over — so the body
//! support arrives with the route that needs it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use reqwest::{Client, StatusCode};
use rg_http::route_table::{Access, RouteFact};
use sea_orm::Set;

use crate::common::answer::Answer;
use crate::common::route_path::placeholders;
use crate::common::source_scan::{
    functions, param_base_types, relative, rust_files, signature_params, src_root,
};
use crate::common::{register_full, spawn_test_app_with_routes_and_db};

const OWNER: &str = "userscopeowner";
const OUTSIDER: &str = "userscopeoutsider";

/// An id no row on the instance has ever carried — the reference every probe is
/// measured against.
///
/// The same value `user_scoped_id_scope_tests::UNUSED_ID` and
/// `cross_repo_id_scope_sweep_tests::ABSENT_ID` hold, and for the same reason:
/// every primary key here comes out of a sequence starting at 1, so one value
/// this far out stands in for all of them.
const ABSENT_ID: i64 = 999_999;

/// A valid ed25519 public key. SSH keys are unique instance-wide, so the value
/// only has to be distinct from the ones other tests register.
const OWNER_SSH_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgIC sweep@plombir-git";

/// Placeholders on an `Access::User` route that do **not** name a row, each with
/// the reason.
///
/// The classification is mandatory, not a filter: a placeholder that is neither
/// a global id nor listed here fails the run. Without that, the next
/// `/users/…/{handle}` would be dropped by a silent `continue` and the sweep
/// would report full coverage of a population it had quietly shrunk.
const NOT_A_ROW_ID: &[(&str, &str)] = &[
    (
        "slug",
        "an SSO provider's slug — instance configuration named by an administrator, not a row \
         owned by the caller, so there is no per-account id space to walk",
    ),
    (
        "tail",
        "the `/api-docs/{*tail}` wildcard — a path inside the bundled Swagger UI bundle, held to \
         its gate by `openapi_docs_auth_tests`",
    ),
    (
        "bot",
        "a bot's username — an account the caller owns, addressed by name. `fill` points it at \
         the owner's seeded bot, so the token id under it is probed here; another person's bot \
         and no bot at all answering the same `404` is held by `agent_accounts_tests`",
    ),
];

/// Routes this sweep deliberately does not drive, each with the reason.
///
/// Checked both ways, like every other quarantine list in this directory: an
/// entry whose route this sweep has started driving fails the run, and so does
/// one naming a route that no longer exists. Empty on purpose — the emptiness is
/// the statement.
///
/// An entry excuses a route from being *reported*, never from being *driven*.
/// The first version of this file skipped a signed-off route before it reached
/// `coverage`, which put the heal check below out of reach: the route could not
/// end up in `driven`, so seeding it later would have left a standing exemption
/// nothing could ever notice. That is the shape of exemption this directory keeps
/// finding and undoing, and it is cheap to build by accident.
const NOT_PROBED: &[(&str, &str)] = &[];

fn signed_off(label: &str) -> bool {
    NOT_PROBED.iter().any(|(entry, _)| *entry == label)
}

/// Whether a placeholder names an instance-wide primary key.
///
/// The same predicate `global_id_anchor_guard` applies to a handler's path
/// parameters, which is what keeps the two files counting one population: the
/// guard demands the id be *anchored*, this sweep demands the anchor's two
/// refusals be *indistinguishable*, and a name that satisfies one reading and
/// not the other would be a gap between them.
fn is_global_id(name: &str) -> bool {
    name == "id" || name.ends_with("_id")
}

/// The segment a route's global id hangs off — `/api/v1/users/ssh-keys/{id}` ⇒
/// `ssh-keys`.
///
/// Keyed on the resource rather than on the whole path so that a second route
/// over the same row is covered the day it is written: `POST
/// /notifications/{id}/read` and `DELETE /notifications/{id}` are one entry in
/// [`coverage`], and a `POST /users/tokens/{id}/rotate` would need none at all.
/// A path-string table is what rotted in the id-scope sweep's first selector.
fn resource(path: &str) -> Option<&str> {
    let mut previous = None;
    for segment in path.split('/') {
        if let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            if is_global_id(name) {
                return previous;
            }
        }
        previous = Some(segment);
    }
    None
}

// ── The fixture ────────────────────────────────────────────────────────────

/// The owner's rows: one per resource family a user-scoped route addresses by
/// global id.
struct Seeded {
    token: i64,
    ssh_key: i64,
    signing_key: i64,
    passkey: i64,
    import: i64,
    notification: i64,
    /// A bot the owner owns, and one token of it — the token id under
    /// `/users/bots/{bot}/tokens/{id}` is an instance-wide key like any other.
    bot: String,
    bot_token: i64,
}

/// What this sweep can do about one resource.
enum Coverage {
    /// A row of this kind belongs to the owner; here is the id that addresses it.
    Probe(i64),
    /// Nobody decided. Fails the run — see [`NOT_PROBED`].
    Unclassified,
}

fn coverage(resource: &str, path: &str, seeded: &Seeded) -> Coverage {
    match resource {
        "tokens" if path.contains("/users/bots/{bot}/") => Coverage::Probe(seeded.bot_token),
        "tokens" => Coverage::Probe(seeded.token),
        "ssh-keys" => Coverage::Probe(seeded.ssh_key),
        "signing-keys" => Coverage::Probe(seeded.signing_key),
        "passkeys" => Coverage::Probe(seeded.passkey),
        "imports" => Coverage::Probe(seeded.import),
        "notifications" => Coverage::Probe(seeded.notification),
        _ => Coverage::Unclassified,
    }
}

struct Fixture {
    base: String,
    client: Client,
    owner_token: String,
    outsider_token: String,
}

impl Fixture {
    /// POST as the owner and return the created row, failing loudly on anything
    /// but success — a half-built fixture is worse than no fixture.
    async fn create(&self, path: &str, body: serde_json::Value, what: &str) -> i64 {
        let response = self
            .client
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.owner_token)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("create {what}: {e}"));
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert_eq!(status, 201, "the fixture {what} was not created: {body}");
        serde_json::from_str::<serde_json::Value>(&body)
            .unwrap_or_else(|e| panic!("{what} response was not json: {e}"))["id"]
            .as_i64()
            .unwrap_or_else(|| panic!("{what} response carried no id: {body}"))
    }
}

/// One row of every kind, all belonging to the owner.
///
/// Passkeys, import tasks and notifications are inserted through `rg_db`
/// directly: a passkey needs a WebAuthn authenticator to register, an import
/// needs a source forge to clone from, and a notification is only ever raised by
/// an event elsewhere in the server. The rows are what this sweep is about, not
/// the routes that would otherwise produce them.
async fn seed(fx: &Fixture, db: &rg_db::DatabaseConnection, owner_id: i64) -> Seeded {
    let token = fx
        .create(
            "/api/v1/users/tokens",
            serde_json::json!({ "name": "sweep" }),
            "access token",
        )
        .await;
    let ssh_key = fx
        .create(
            "/api/v1/users/ssh-keys",
            serde_json::json!({ "title": "Sweep", "key": OWNER_SSH_KEY }),
            "SSH key",
        )
        .await;
    // This sweep checks ownership of an existing row. Registration's verified
    // email requirement is covered by the signing-key endpoint tests.
    let signing_key = rg_db::ops::commit_signing_key_ops::create(
        db,
        rg_db::entities::commit_signing_key::ActiveModel {
            user_id: Set(owner_id),
            title: Set("Sweep commit signer".to_string()),
            kind: Set("ssh".to_string()),
            public_key: Set(OWNER_SSH_KEY.to_string()),
            fingerprint: Set(
                rg_core::auth::ssh_key::fingerprint_from_openssh(OWNER_SSH_KEY)
                    .expect("fixture signing key is valid"),
            ),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        },
    )
    .await
    .expect("seed commit signing key")
    .id;

    let passkey = rg_db::ops::passkey_credential_ops::create(
        db,
        owner_id,
        "user-scope-sweep-credential",
        "{}",
        "sweep passkey",
        "scope-sweep.example.test",
    )
    .await
    .expect("seed passkey")
    .id;

    let now = chrono::Utc::now();
    let import = rg_db::ops::import_task_ops::create(
        db,
        rg_db::entities::import_task::ActiveModel {
            user_id: Set(owner_id),
            platform: Set("github".to_string()),
            // `.invalid` never resolves (RFC 6761); nothing here starts a
            // worker. A terminal status keeps the owner-delete case focused
            // on ID scoping rather than the import cancellation contract.
            source_url: Set("https://example.invalid/octo/widgets.git".to_string()),
            target_owner: Set(OWNER.to_string()),
            target_name: Set("widgets".to_string()),
            status: Set("completed".to_string()),
            progress: Set(100),
            import_repo: Set(true),
            import_issues: Set(false),
            import_pull_requests: Set(false),
            import_wiki: Set(false),
            import_releases: Set(false),
            import_labels: Set(false),
            import_milestones: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("seed import task")
    .id;

    let notification = rg_db::ops::notification_ops::create_notification(
        db,
        owner_id,
        "issue",
        "sweep notification",
        Some("private body"),
        None,
    )
    .await
    .expect("seed notification")
    .id;

    let bot = "userscopeowner-agent".to_string();
    fx.create(
        "/api/v1/users/bots",
        serde_json::json!({ "username": bot }),
        "bot",
    )
    .await;
    let bot_token = fx
        .create(
            &format!("/api/v1/users/bots/{bot}/tokens"),
            serde_json::json!({ "name": "sweep" }),
            "bot token",
        )
        .await;

    Seeded {
        token,
        ssh_key,
        signing_key,
        passkey,
        import,
        notification,
        bot,
        bot_token,
    }
}

// ── Reading the population out of the source ───────────────────────────────

/// `api/users.rs` + `delete_token` ⇒ `rg_http::api::users::delete_token`, the
/// spelling `RouteFact::handler` carries.
fn handler_type_name(file: &str, name: &str) -> String {
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    let module = stem.strip_suffix("/mod").unwrap_or(stem);
    format!("rg_http::{}::{name}", module.replace('/', "::"))
}

/// Every handler in the tree whose signature carries `AuthUser`, by the
/// `type_name` its route records.
///
/// This is the second selector. `AuthUser` in a signature means the *only* thing
/// the gate proved is that somebody is signed in — no repository, no
/// organization, no instance role — so a path parameter naming an
/// instance-wide row on such a handler is this sweep's subject whatever level the
/// route happens to declare.
fn auth_user_handlers() -> BTreeSet<String> {
    let mut files = Vec::new();
    rust_files(&src_root(), &mut files);
    files.sort();
    let mut out = BTreeSet::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("read source file");
        for function in functions(&text) {
            let Some(params) = signature_params(&text, &function.name) else {
                continue;
            };
            if param_base_types(&params).contains(&"AuthUser") {
                out.insert(handler_type_name(&relative(file), &function.name));
            }
        }
    }
    out
}

// ── Driving one route ──────────────────────────────────────────────────────

/// Fill a route's path with one id.
///
/// A path with a second placeholder returns `None`: this sweep seeds one row per
/// resource and nothing else, so another locator is something it would have to
/// build, and guessing at it is how a probe comes to prove nothing.
fn fill(path: &str, id: i64, seeded: &Seeded) -> Option<String> {
    // The one named locator this sweep seeds: the owner's bot.
    let path = path.replace("{bot}", &seeded.bot);
    let path = path.as_str();
    let open = path.find('{')?;
    let close = path[open..].find('}')? + open;
    if path[close + 1..].contains('{') {
        return None;
    }
    Some(format!("{}{id}{}", &path[..open], &path[close + 1..]))
}

/// One section of a failure report: the lines, newline-terminated, or nothing at
/// all when there are none.
fn block(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

async fn drive(fx: &Fixture, fact: &RouteFact, url: &str, token: Option<&str>) -> Answer {
    let url = format!("{}{url}", fx.base);
    let mut request = match fact.method {
        "GET" => fx.client.get(url),
        "HEAD" => fx.client.head(url),
        "POST" => fx.client.post(url),
        "PUT" => fx.client.put(url),
        "PATCH" => fx.client.patch(url),
        "DELETE" => fx.client.delete(url),
        other => panic!("route table produced an unroutable method {other}"),
    };
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    Answer::of(request.send().await.expect("user-scope sweep request")).await
}

/// The probe's predicate: refused *without* being told the row exists — `401` or
/// `404`, and specifically **not** `403`.
///
/// `403` is the one answer this sweep's subject matter forbids, and the handlers
/// say so themselves (`api::users::delete_token`: "another account's token
/// answers 404, not 403: a 403 would confirm the id exists"). `401` is
/// admissible and is not a leak — `AuthUser` runs before any lookup, so it
/// answers the same way whether the row exists or not.
fn masked(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 404)
}

/// A body complaint is not a verdict on the id: the handler rejected the request
/// before it looked anything up.
fn inconclusive(status: StatusCode) -> bool {
    matches!(status.as_u16(), 400 | 409 | 415 | 422)
}

// ── The sweep ──────────────────────────────────────────────────────────────

/// One route to drive: where it is, which resource it addresses, and the two
/// URLs whose answers have to match.
struct Probe<'a> {
    fact: &'a RouteFact,
    resource: String,
    url: String,
    absent: String,
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn no_user_scoped_route_confirms_another_accounts_row() {
    let (base, facts, db) = spawn_test_app_with_routes_and_db().await;
    let (owner_token, owner_id) =
        register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    let (outsider_token, _outsider_id) =
        register_full(&base, OUTSIDER, &format!("{OUTSIDER}@example.com")).await;
    let fx = Fixture {
        base,
        client: Client::builder().build().expect("http client"),
        owner_token,
        outsider_token,
    };
    let seeded = seed(&fx, &db, owner_id).await;

    assert!(
        !facts.is_empty(),
        "the route table came back empty — the sweep is not testing anything"
    );

    // ── Every placeholder of a user-scoped route is classified ──────────────
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    for fact in &facts {
        if fact.access != Access::User {
            continue;
        }
        for name in placeholders(&fact.path) {
            if !is_global_id(name) && !NOT_A_ROW_ID.iter().any(|(known, _)| *known == name) {
                unknown.insert(format!("  {{{name}}} in {}", fact.label()));
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "a route gated by nothing but a session names {} placeholder(s) this sweep has never heard \
         of. Each one is either an instance-wide primary key — in which case give `coverage` a row \
         to point it at and seed it — or it names something the caller does not own, in which case \
         add it to NOT_A_ROW_ID with the reason. Left unclassified it would be dropped by a silent \
         `continue`, and the sweep would report full coverage of a population it had shrunk.\n{}",
        unknown.len(),
        unknown.into_iter().collect::<Vec<_>>().join("\n"),
    );

    // ── Selector one: the level the route declares ──────────────────────────
    let mut probes: Vec<Probe> = Vec::new();
    let mut unclassified: Vec<String> = Vec::new();
    for fact in &facts {
        if fact.access != Access::User {
            continue;
        }
        let Some(resource) = resource(&fact.path) else {
            continue;
        };
        let Coverage::Probe(id) = coverage(resource, &fact.path, &seeded) else {
            if !signed_off(&fact.label()) {
                unclassified.push(format!(
                    "  {} — addresses a `{resource}` row by its instance-wide id and this sweep \
                     owns no such row. Seed one in `seed` and give `coverage` a line, or sign the \
                     route off in NOT_PROBED with the reason it cannot be probed",
                    fact.label(),
                ));
            }
            continue;
        };
        let (Some(url), Some(absent)) = (
            fill(&fact.path, id, &seeded),
            fill(&fact.path, ABSENT_ID, &seeded),
        ) else {
            if !signed_off(&fact.label()) {
                unclassified.push(format!(
                    "  {} — carries a second placeholder besides the id. Teach `fill` to seed that \
                     locator; a placeholder filled by guesswork is a probe that proves nothing",
                    fact.label(),
                ));
            }
            continue;
        };
        probes.push(Probe {
            fact,
            resource: resource.to_string(),
            url,
            absent,
        });
    }

    // ── Selector two: the gate the handler actually holds ───────────────────
    //
    // Read out of the source, and blind to what the route table declares. A
    // route mis-declared as `Public` or `PublicFiltered` over an `AuthUser`
    // handler is invisible to the selector above — its whole reading is
    // `fact.access` — and would leave the strongest gate in the tree unswept
    // while every assertion here stayed green.
    let auth_user = auth_user_handlers();
    assert!(
        auth_user.len() > 20,
        "only {} handler(s) in the tree read as taking `AuthUser` — the signature reader is broken, \
         and a broken second selector cannot contradict the first one about anything",
        auth_user.len()
    );
    let mut unseen: Vec<String> = Vec::new();
    for fact in &facts {
        if !auth_user.contains(fact.handler) {
            continue;
        }
        // A path that also names a repository or an organization is the other
        // two sweeps' subject: there the gate proved something about a
        // container, and the id is judged against that container.
        let names_container = placeholders(&fact.path)
            .any(|name| matches!(name, "owner" | "name" | "repo" | "org" | "org_name"));
        if names_container || !placeholders(&fact.path).any(is_global_id) {
            continue;
        }
        if probes
            .iter()
            .any(|probe| probe.fact.label() == fact.label())
            || signed_off(&fact.label())
        {
            continue;
        }
        unseen.push(format!(
            "  {} declares {:?} while its handler ({}) takes `AuthUser` and its path names an \
             instance-wide id and no container. Being signed in is therefore the whole of what the \
             gate proves, and this sweep is not driving it — so either the declared level is wrong, \
             or the selector above cannot see the route and needs to be told how.",
            fact.label(),
            fact.access,
            fact.handler,
        ));
    }

    // A sign-off that has stopped being one has to go.
    let driven: BTreeSet<String> = probes.iter().map(|probe| probe.fact.label()).collect();
    let mut healed: Vec<String> = Vec::new();
    for (label, reason) in NOT_PROBED {
        if driven.contains(*label) {
            healed.push(format!(
                "  {label} is being driven now — drop it from NOT_PROBED (was: {reason})"
            ));
        } else if !facts.iter().any(|fact| fact.label() == *label) {
            healed.push(format!(
                "  NOT_PROBED names {label}, which is no longer a route — drop it (was: {reason})"
            ));
        }
    }
    for (name, reason) in NOT_A_ROW_ID {
        if !facts.iter().any(|fact| {
            fact.access == Access::User && placeholders(&fact.path).any(|found| found == *name)
        }) {
            healed.push(format!(
                "  NOT_A_ROW_ID names {{{name}}}, which no user-scoped route carries any more — \
                 drop it (was: {reason})"
            ));
        }
    }

    assert!(
        unclassified.is_empty() && unseen.is_empty() && healed.is_empty(),
        "the user-scoped routes and the ones this sweep drives have come apart: {} route(s) \
         nothing can address, {} route(s) whose handler is gated by a session alone and which \
         nothing here drives, {} stale sign-off(s).\nA route of this shape is invisible to \
         `cross_repo_id_scope_sweep_tests` and `anchored_scope_sweep_tests` (its path names no \
         repository, and no row resolves one), and `route_access_sweep_tests` owes an outsider only \
         `Expect::Allowed` on it — every non-denial satisfies that, so a `403` confirming another \
         account's row would pass there. This sweep is the only pass that asks.\n{}{}{}",
        unclassified.len(),
        unseen.len(),
        healed.len(),
        block(&unclassified),
        block(&unseen),
        block(&healed),
    );
    // Not a coverage target — a collapse detector. The two selectors above are
    // what keeps the population honest; this only catches a filter that selected
    // nothing at all and would otherwise pass every assertion below vacuously.
    assert!(
        probes.len() >= 5,
        "only {} user-scoped route(s) carrying a global id are being driven — the filter is wrong, \
         not the server",
        probes.len()
    );

    // The destructive rows last, so a probe is never driven against a row an
    // earlier baseline has already deleted: `POST /notifications/{id}/read`
    // before `DELETE /notifications/{id}`.
    probes.sort_by_key(|probe| {
        let destructive = probe.fact.method == "DELETE";
        (destructive, probe.fact.path.clone(), probe.fact.method)
    });

    let mut leaks: Vec<String> = Vec::new();
    let mut oracles: Vec<String> = Vec::new();
    // How many pairs of each resource were compared with a body on both sides.
    let mut paired: BTreeMap<String, usize> = BTreeMap::new();

    // ── Pass one: an outsider with a valid session, against a real row ──────
    for probe in &probes {
        let label = probe.fact.label();
        let real = drive(&fx, probe.fact, &probe.url, Some(&fx.outsider_token)).await;
        let absent = drive(&fx, probe.fact, &probe.absent, Some(&fx.outsider_token)).await;

        if real.status == StatusCode::FORBIDDEN {
            leaks.push(format!(
                "  {label}\n      answered 403 for a row belonging to {OWNER}. The refusal \
                 confirmed the id exists, which is the one thing a masked denial may not do: the \
                 caller supplied an opaque integer, so walking it enumerates every row of this kind \
                 on the instance.\n      body: {}",
                real.excerpt()
            ));
        } else if inconclusive(real.status) {
            leaks.push(format!(
                "  {label}\n      answered {} — the request never reached the row, so nothing was \
                 proven. This sweep sends no request bodies (no route of this shape needed one \
                 yet); give it the body this route wants.\n      body: {}",
                real.status,
                real.excerpt()
            ));
        } else if !masked(real.status) {
            leaks.push(format!(
                "  {label}\n      answered {} for a row belonging to {OWNER} to a caller who owns \
                 none of it. Being signed in is all the gate on this route proves, and the handler \
                 never compared the row's owner against the account that authenticated.\n      \
                 body: {}",
                real.status,
                real.excerpt()
            ));
        } else if real.shape() != absent.shape() {
            oracles.push(format!(
                "  {label}\n      refused a row of {OWNER}'s and an id that never existed, but not \
                 with the same answer — so the pair tells the caller which of the two he hit, and \
                 walking the id space enumerates every row of this kind on the instance.\n      \
                 real   ({}): {}\n      absent ({}): {}",
                real.status,
                real.excerpt(),
                absent.status,
                absent.excerpt(),
            ));
        } else if real.speaks() && absent.speaks() {
            *paired.entry(probe.resource.clone()).or_default() += 1;
        }

        // The same pair with no credential at all. Which code the two agree on is
        // the gate's business — `AuthUser` answers `401` before it resolves
        // anything, and that `401` is not an oracle precisely because it arrives
        // first — but they have to agree: a `401` raised *after* the lookup would
        // tell a caller with no account at all which ids are real.
        let anon_real = drive(&fx, probe.fact, &probe.url, None).await;
        let anon_absent = drive(&fx, probe.fact, &probe.absent, None).await;
        if anon_real.shape() != anon_absent.shape() {
            oracles.push(format!(
                "  {label}\n      an anonymous caller is answered differently for a real row and \
                 for an id that never existed, so the route enumerates private rows to callers \
                 with no account at all.\n      real   ({}): {}\n      absent ({}): {}",
                anon_real.status,
                anon_real.excerpt(),
                anon_absent.status,
                anon_absent.excerpt(),
            ));
        }
    }

    // ── Pass two: the owner, on his own row ─────────────────────────────────
    //
    // Held to a *success*, not merely to "not refused". Every route here serves
    // its owner today, and `masked` accepts `404`: a fixture that quietly stopped
    // seeding would hand out `404` to owner and outsider alike and read as a
    // clean bill of health.
    let mut dead: Vec<String> = Vec::new();
    for probe in &probes {
        let baseline = drive(&fx, probe.fact, &probe.url, Some(&fx.owner_token)).await;
        if !baseline.status.is_success() {
            dead.push(format!(
                "  {}\n      the owner is answered {} on his own row, so the refusals above \
                 describe a broken fixture rather than a gate. Either the row was never seeded, or \
                 an earlier baseline in this run consumed it — the probes are ordered so that a \
                 `DELETE` runs last, and a second destructive route over one row needs a second \
                 row.\n      body: {}",
                probe.fact.label(),
                baseline.status,
                baseline.excerpt(),
            ));
        }
    }

    assert!(
        leaks.is_empty() && oracles.is_empty() && dead.is_empty(),
        "user-scoped id scope: {} route(s) let one account reach another's row, {} told a real id \
         apart from an absent one, {} dead baseline(s), out of {} driven.\n{}{}{}",
        leaks.len(),
        oracles.len(),
        dead.len(),
        probes.len(),
        block(&leaks),
        block(&oracles),
        block(&dead),
    );

    // Not a coverage counter — a non-vacuity one, and tied to the population
    // rather than to a number that rots. Every assertion above is satisfied by
    // two empty bodies, so a resource whose routes stopped answering with a body
    // — or whose pair stopped being compared at all — would keep the whole file
    // green while asserting nothing. That is exactly how the status half of the
    // id-scope sweep went vacuous in `card_c46c354ec3ae`.
    let mut silent: Vec<String> = Vec::new();
    for probe in &probes {
        if paired.get(&probe.resource).copied().unwrap_or_default() == 0 {
            silent.push(format!(
                "  `{}` — every pair compared was empty on one side, so only the status was \
                 asserted",
                probe.resource
            ));
        }
    }
    silent.sort();
    silent.dedup();
    assert!(
        silent.is_empty(),
        "{} resource(s) had no masked refusal compared against an absent id's body and all. The \
         oracle-in-the-body half of this sweep is only asserted where that pair holds.\n{}",
        silent.len(),
        block(&silent),
    );
}
