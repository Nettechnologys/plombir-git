//! One pass over every repository-scoped route that also carries a *global* id.
//!
//! `route_access_sweep_tests` asks each route whether it turns the wrong caller
//! away. This one asks the question that survives a correct answer: the gate on
//! `{owner}/{name}` passed honestly, and then the handler acted on an id that is
//! an instance-wide primary key. Nothing about the gate is wrong — the caller
//! really does own the repository in the path — but the row it reached lives in
//! somebody else's.
//!
//!     PATCH /repos/<attacker>/<attacker-repo>/releases/<id-from-a-private-repo>
//!
//! Four instances of exactly this were closed by hand (`cross_repo_release_tests`,
//! `cross_repo_label_milestone_tests`, `delete_time_entry`, the `milestone_id`
//! in an issue body). This is the mechanism behind them, walked over the route
//! table so that the next one is caught by a red test rather than by reading.
//!
//! # The shape
//!
//! Two accounts. The **victim** owns a private repository with one of every
//! resource in it; the **attacker** owns a repository of their own with nothing
//! but the locators a path needs (issue `#1`, pull request `#1`, a wiki page).
//! Each route is then driven twice, and the two requests differ in exactly one
//! character-range — *which repository the path names*:
//!
//! - **the probe** — the attacker's own repository, the victim's ids. Owed
//!   `401`/`403`/`404`.
//! - **the baseline** — the victim's repository, the same victim ids, the
//!   victim's own session. Owed anything but a denial.
//!
//! The baseline is not decoration. A route that answers `404` to everything —
//! because the fixture never seeded it, because the body was rejected, because
//! the id was never valid — would otherwise read as a *passing security test*.
//! Here it reads as a dead fixture and fails the run.
//!
//! # What is deliberately not probed
//!
//! Nothing is skipped implicitly. [`coverage`] classifies every repository-scoped
//! route that carries a global id, and a route it does not recognise fails the
//! run rather than being passed over: a new resource has to be seeded or signed
//! off, and either way somebody has to have thought about it. The sign-offs
//! carry their reason inline, next to the route they excuse.
//!
//! Per-repository placeholders are not this sweep's business and are listed in
//! [`PER_REPO_PLACEHOLDERS`]: an issue `{number}` restarts at 1 in every
//! repository, a `{sha}` names a commit, a `{path}` names a file. Probing those
//! across repositories asks nothing — which is why the placeholder lists are
//! checked against the table too, so a new placeholder cannot slip in
//! unclassified.
//!
//! Ids that arrive in a *request body* are only half here: the sweep sends the
//! victim's ids in the few bodies it has to fill (`comment_ids`, `positions`,
//! `column_id`), but it walks paths, not schemas. `boards::issue_in_repo`,
//! `issues::require_milestone_in_repo` and
//! `pr_permission_tests::review_and_parent_ids_in_the_body_are_scoped_to_their_pull_request`
//! cover that side.
//!
//! # What the first run found
//!
//! Every route held the scope — and two said so with a status that gave the
//! answer away. `review/service.rs` answered `400 "review comment does not
//! belong to this PR"` for a comment id from another pull request while an id
//! that does not exist answered `404`, so the pair enumerated every review
//! comment on the instance from a pull request of one's own. The scope was
//! never the hole; the *difference between the two refusals* was. That is the
//! reason [`denied`] accepts `401`/`403`/`404` and nothing else: a refusal that
//! needs its own status code is not a refusal this sweep can read, and neither
//! can a caller.

use std::collections::{BTreeSet, HashMap};

use chrono::Utc;
use reqwest::multipart::{Form, Part};
use reqwest::{Client, StatusCode};
use rg_http::route_table::RouteFact;
use sea_orm::Set;

use crate::common::{register_full, spawn_test_app_with_routes_and_db};

const ATTACKER: &str = "scopeattacker";
const VICTIM: &str = "scopevictim";
/// The attacker's own repository — the one every probe's path names.
const HOST: &str = "scopehost";
/// The victim's private repository — the one every probe's ids come from.
const VAULT: &str = "scopevault";

const REPO_PREFIX: &str = "/api/v1/repos/{owner}/{name}";

/// Placeholders that carry an instance-wide primary key, and are therefore the
/// subject of this sweep.
const GLOBAL_ID_PLACEHOLDERS: &[&str] = &[
    "id",
    "comment_id",
    "attachment_id",
    "asset_id",
    "release_id",
    "card_id",
    "col_id",
    "rev_id",
    "job_id",
    "pipeline_id",
    "delivery_id",
];

/// Placeholders that are per-repository by construction, each with the reason.
///
/// A cross-repository probe over one of these asks nothing: the same value in
/// another repository names that repository's own row, so there is no foreign
/// row to reach.
const PER_REPO_PLACEHOLDERS: &[(&str, &str)] = &[
    ("owner", "the repository's owner"),
    ("name", "the repository"),
    ("repo", "the repository"),
    (
        "number",
        "issues and pull requests are numbered from 1 in every repository",
    ),
    ("title", "a wiki page title, unique within its repository"),
    ("path", "a path inside the repository's tree"),
    ("file", "a path inside the repository's tree"),
    ("sha", "a commit hash, resolved inside the repository"),
    ("branch", "a ref name, resolved inside the repository"),
    ("archive", "an archive filename derived from a ref"),
    ("secret_name", "CI secrets are named per repository"),
    ("pkg_type", "a package registry kind, not an id"),
    ("pkg", "a package name, unique within its registry"),
    ("pkg_name", "a package name, unique within its registry"),
    ("gem_name", "a package name, unique within its registry"),
    (
        "filename",
        "a published file name, resolved inside the repository's own registry",
    ),
    ("c1", "a Cargo index prefix segment, or the crate name"),
    ("c2", "a Cargo index prefix segment, or the crate name"),
    ("c3", "a Cargo index prefix segment, or the crate name"),
    ("group_id", "a Maven coordinate"),
    ("artifact_id", "a Maven coordinate"),
    ("version", "a package version string"),
    ("reference", "an OCI tag or digest"),
    ("digest", "an OCI content digest"),
    ("uuid", "an OCI upload session, created by the caller"),
    ("username", "an account name"),
    (
        "oid",
        "an LFS object hash — content-addressed, and the pointer that binds it \
         to a repository is the `lfs_objects` row, not the oid",
    ),
];

/// `{m1}`..`{m9}`: one Maven coordinate segment each.
///
/// The layout routes spell the group id out segment by segment because a Maven
/// path has no fixed depth; every segment is part of a coordinate, and a
/// coordinate is resolved inside the repository's own registry.
const MAVEN_SEGMENTS: usize = 9;

// ── Fixture ────────────────────────────────────────────────────────────────

/// The victim's rows: one of every resource a repository-scoped route can name
/// by global id.
#[derive(Default, Clone)]
struct Ids {
    deploy_key: i64,
    milestone: i64,
    label: i64,
    issue_comment: i64,
    issue_attachment: i64,
    comment_attachment: i64,
    pr_attachment: i64,
    pr_review: i64,
    review_comment: i64,
    review_comment_attachment: i64,
    wiki_revision: i64,
    hook: i64,
    delivery: i64,
    pipeline: i64,
    job: i64,
    environment: i64,
    branch_protection: i64,
    tag_protection: i64,
    board: i64,
    column: i64,
    card: i64,
    time_entry: i64,
    release: i64,
    asset: i64,
}

struct Fixture {
    base: String,
    client: Client,
    attacker_token: String,
    victim_token: String,
}

impl Fixture {
    fn url(&self, owner: &str, repo: &str, tail: &str) -> String {
        format!("{}/api/v1/repos/{owner}/{repo}{tail}", self.base)
    }

    /// POST `body` and return the created resource, failing loudly on anything
    /// but success — a half-built fixture is worse than no fixture.
    async fn create(
        &self,
        token: &str,
        url: String,
        body: serde_json::Value,
        what: &str,
    ) -> serde_json::Value {
        let response = self
            .client
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|error| panic!("fixture: {what}: {error}"));
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "fixture: {what} failed ({status}) at {url}: {text}"
        );
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
    }

    async fn create_id(
        &self,
        token: &str,
        url: String,
        body: serde_json::Value,
        what: &str,
    ) -> i64 {
        self.create(token, url, body, what).await["id"]
            .as_i64()
            .unwrap_or_else(|| panic!("fixture: {what} returned no id"))
    }

    /// Upload an attachment (multipart) and return its id.
    async fn upload_attachment(&self, token: &str, url: String, what: &str) -> i64 {
        let response = self
            .client
            .post(&url)
            .bearer_auth(token)
            .multipart(
                Form::new().part(
                    "attachment",
                    Part::bytes(b"scope sweep".to_vec())
                        .file_name("scope.txt")
                        .mime_str("text/plain")
                        .unwrap(),
                ),
            )
            .send()
            .await
            .unwrap_or_else(|error| panic!("fixture: {what}: {error}"));
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "fixture: {what} failed ({status}) at {url}: {text}"
        );
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["id"]
            .as_i64()
            .unwrap_or_else(|| panic!("fixture: {what} returned no id"))
    }
}

async fn create_repo(fx: &Fixture, token: &str, name: &str) {
    let response = fx
        .client
        .post(format!("{}/api/v1/repos", fx.base))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        201,
        "fixture: creating repository '{name}' failed"
    );
}

/// Look the repository's row up so the direct inserts below can point at it.
async fn repo_id(db: &rg_db::DatabaseConnection, owner_id: i64, name: &str) -> i64 {
    rg_db::ops::repo_ops::find_by_owner_and_name(db, owner_id, name)
        .await
        .expect("fixture: repository lookup")
        .unwrap_or_else(|| panic!("fixture: repository {name} of user {owner_id} is missing"))
        .id
}

/// A pull request, inserted straight into the database.
///
/// The API route needs commits on two branches, which this harness has no git
/// fixture for; everything hanging off the pull request (reviews, inline
/// comments, their attachments) is unreachable without one.
async fn insert_pr(
    db: &rg_db::DatabaseConnection,
    repo: i64,
    author: i64,
    number: i64,
) -> rg_db::entities::pull_request::Model {
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo),
            number: Set(number),
            title: Set(format!("scope sweep PR {number}")),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(author),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            // A head commit the repository does not have. `apply_suggestions`
            // refuses a pull request without one *before* it looks a comment
            // up, so a null head would stop the probe at a `409` and prove
            // nothing about the id.
            head_sha: Set(Some("0000000000000000000000000000000000000000".to_string())),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("fixture: insert pull request")
}

/// Seed the victim's repository with one of everything, and return the ids.
#[allow(clippy::too_many_lines)]
async fn seed_vault(
    fx: &Fixture,
    db: &rg_db::DatabaseConnection,
    victim_id: i64,
    repo: i64,
    pr_id: i64,
) -> Ids {
    let token = &fx.victim_token;
    let mut ids = Ids::default();

    // ── Issues, comments, attachments ──────────────────────────────────────
    let issue = fx
        .create(
            token,
            fx.url(VICTIM, VAULT, "/issues"),
            serde_json::json!({"title": "scope sweep"}),
            "victim issue",
        )
        .await;
    let issue_number = issue["number"].as_i64().expect("issue number");
    assert_eq!(issue_number, 1, "fixture: the seeded issue must be #1");
    ids.issue_comment = fx
        .create_id(
            token,
            fx.url(VICTIM, VAULT, "/issues/1/comments"),
            serde_json::json!({"body": "scope sweep"}),
            "victim issue comment",
        )
        .await;
    ids.issue_attachment = fx
        .upload_attachment(
            token,
            fx.url(VICTIM, VAULT, "/issues/1/assets"),
            "victim issue attachment",
        )
        .await;
    ids.comment_attachment = fx
        .upload_attachment(
            token,
            fx.url(
                VICTIM,
                VAULT,
                &format!("/issues/comments/{}/assets", ids.issue_comment),
            ),
            "victim issue-comment attachment",
        )
        .await;
    ids.time_entry = fx
        .create_id(
            token,
            fx.url(VICTIM, VAULT, "/issues/1/time"),
            serde_json::json!({"duration_minutes": 30, "description": "scope sweep"}),
            "victim time entry",
        )
        .await;

    // ── Issue metadata ─────────────────────────────────────────────────────
    ids.label = fx
        .create_id(
            token,
            fx.url(VICTIM, VAULT, "/labels"),
            serde_json::json!({"name": "scope", "color": "#ff0000"}),
            "victim label",
        )
        .await;
    ids.milestone = fx
        .create_id(
            token,
            fx.url(VICTIM, VAULT, "/milestones"),
            serde_json::json!({"title": "scope"}),
            "victim milestone",
        )
        .await;

    // ── Pull request side: review, inline comment, attachments ─────────────
    ids.pr_attachment = fx
        .upload_attachment(
            token,
            fx.url(VICTIM, VAULT, "/pulls/1/assets"),
            "victim pull-request attachment",
        )
        .await;
    ids.pr_review = rg_db::ops::pr_review_ops::create(
        db,
        rg_db::entities::pr_review::ActiveModel {
            id: sea_orm::NotSet,
            pr_id: Set(pr_id),
            repo_id: Set(repo),
            reviewer_id: Set(victim_id),
            action: Set("comment".to_string()),
            body: Set(Some("scope sweep".to_string())),
            commit_id: Set(None),
            created_at: Set(Utc::now()),
        },
    )
    .await
    .expect("fixture: insert review")
    .id;
    ids.review_comment = rg_db::ops::review_comment_ops::create(
        db,
        rg_db::entities::review_comment::ActiveModel {
            id: sea_orm::NotSet,
            review_id: Set(ids.pr_review),
            pr_id: Set(pr_id),
            author_id: Set(victim_id),
            path: Set("src/lib.rs".to_string()),
            position: Set(None),
            line: Set(Some(1)),
            start_line: Set(None),
            side: Set(Some("RIGHT".to_string())),
            start_side: Set(None),
            body: Set("scope sweep".to_string()),
            suggestion: Set(Some("scoped".to_string())),
            suggestion_applied_at: Set(None),
            suggestion_applied_by_id: Set(None),
            suggestion_commit_sha: Set(None),
            commit_id: Set(None),
            reply_to_id: Set(None),
            resolved_at: Set(None),
            resolved_by_id: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
        },
    )
    .await
    .expect("fixture: insert review comment")
    .id;
    ids.review_comment_attachment = fx
        .upload_attachment(
            token,
            fx.url(
                VICTIM,
                VAULT,
                &format!("/pulls/comments/{}/assets", ids.review_comment),
            ),
            "victim review-comment attachment",
        )
        .await;

    // ── Wiki ───────────────────────────────────────────────────────────────
    fx.create(
        token,
        fx.url(VICTIM, VAULT, "/wiki"),
        serde_json::json!({"title": "Home", "content": "scope sweep"}),
        "victim wiki page",
    )
    .await;
    let updated = fx
        .client
        .patch(fx.url(VICTIM, VAULT, "/wiki/Home"))
        .bearer_auth(token)
        .json(&serde_json::json!({"content": "scope sweep, revised"}))
        .send()
        .await
        .unwrap();
    assert!(
        updated.status().is_success(),
        "fixture: revising the victim's wiki page failed: {}",
        updated.status()
    );
    let history: serde_json::Value = fx
        .client
        .get(fx.url(VICTIM, VAULT, "/wiki/Home/history"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    ids.wiki_revision = history
        .as_array()
        .and_then(|revisions| revisions.first())
        .and_then(|revision| revision["id"].as_i64())
        .unwrap_or_else(|| panic!("fixture: the victim's wiki page has no revision: {history}"));

    // ── Webhooks ───────────────────────────────────────────────────────────
    //
    // Inserted rather than posted, and inactive on purpose. The API path would
    // need a URL that survives the SSRF guard — i.e. one that resolves — and
    // the redeliver probe would then hold the sweep open for a 10s connect
    // timeout against a host that does not answer. An inactive hook makes
    // `trigger_event` a no-op, so the route still runs its own lookup and
    // decides, which is the only thing under test here.
    ids.hook = rg_db::ops::webhook_ops::create_webhook(
        db,
        rg_db::entities::webhook::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo),
            url: Set("https://hooks.example.com/scope-sweep".to_string()),
            content_type: Set("json".to_string()),
            secret: Set(None),
            active: Set(false),
            events: Set("push".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
        },
    )
    .await
    .expect("fixture: insert webhook")
    .id;
    ids.delivery = rg_db::ops::webhook_ops::create_delivery(
        db,
        rg_db::entities::webhook_delivery::ActiveModel {
            id: sea_orm::NotSet,
            webhook_id: Set(ids.hook),
            event: Set("push".to_string()),
            delivery_id: Set("00000000-0000-4000-8000-000000000001".to_string()),
            response_status: Set(Some(200)),
            request_payload: Set(Some("{}".to_string())),
            response_body: Set(None),
            duration_ms: Set(Some(1)),
            created_at: Set(Utc::now()),
        },
    )
    .await
    .expect("fixture: insert webhook delivery")
    .id;

    // ── CI ─────────────────────────────────────────────────────────────────
    ids.environment = fx
        .create_id(
            token,
            fx.url(VICTIM, VAULT, "/actions/environments"),
            serde_json::json!({"name": "production"}),
            "victim environment",
        )
        .await;
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo,
        "0000000000000000000000000000000000000000",
        "refs/heads/main",
        "push",
        Some(victim_id),
    )
    .await
    .expect("fixture: insert pipeline");
    ids.pipeline = pipeline.id;
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "build", 0)
        .await
        .expect("fixture: insert stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db,
        stage.id,
        "compile",
        "true",
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        // Manual, so `play` has something to release rather than a job it must
        // refuse on state alone.
        Some("manual"),
        None,
    )
    .await
    .expect("fixture: insert job");
    ids.job = job.id;
    rg_db::ops::artifact_ops::create_artifact(
        db,
        job.id,
        "scope-artifact",
        "artifacts/scope.txt",
        11,
        None,
        None,
    )
    .await
    .expect("fixture: insert artifact");

    // ── Protection rules and keys ──────────────────────────────────────────
    ids.branch_protection = fx
        .create_id(
            token,
            fx.url(VICTIM, VAULT, "/branches/protection"),
            serde_json::json!({"branch_name": "main", "require_pr": true}),
            "victim branch protection",
        )
        .await;
    ids.tag_protection = fx
        .create_id(
            token,
            fx.url(VICTIM, VAULT, "/tags/protection"),
            serde_json::json!({"pattern": "v*"}),
            "victim tag protection",
        )
        .await;
    ids.deploy_key = fx
        .create_id(
            token,
            fx.url(VICTIM, VAULT, "/keys"),
            serde_json::json!({"title": "scope sweep", "key": VICTIM_DEPLOY_KEY}),
            "victim deploy key",
        )
        .await;

    // ── Boards ─────────────────────────────────────────────────────────────
    let board = fx
        .create(
            token,
            fx.url(VICTIM, VAULT, "/boards"),
            serde_json::json!({"name": "scope"}),
            "victim board",
        )
        .await;
    ids.board = board["id"].as_i64().expect("board id");
    let full: serde_json::Value = fx
        .client
        .get(fx.url(VICTIM, VAULT, &format!("/boards/{}", ids.board)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    ids.column = full["columns"][0]["column"]["id"]
        .as_i64()
        .unwrap_or_else(|| panic!("fixture: the victim's board has no column: {full}"));
    ids.card = fx
        .create_id(
            token,
            fx.url(
                VICTIM,
                VAULT,
                &format!("/boards/{}/columns/{}/cards", ids.board, ids.column),
            ),
            serde_json::json!({"note": "scope sweep"}),
            "victim card",
        )
        .await;

    // ── Releases ───────────────────────────────────────────────────────────
    ids.release = fx
        .create_id(
            token,
            fx.url(VICTIM, VAULT, "/releases"),
            serde_json::json!({"tag_name": "v1.0.0", "title": "scope sweep"}),
            "victim release",
        )
        .await;
    let asset = fx
        .client
        .post(fx.url(VICTIM, VAULT, &format!("/releases/{}/assets", ids.release)))
        .bearer_auth(token)
        .header("content-type", "text/plain")
        .header("content-disposition", "attachment; filename=scope.txt")
        .body("scope sweep")
        .send()
        .await
        .unwrap();
    assert_eq!(
        asset.status(),
        201,
        "fixture: uploading the victim's release asset failed"
    );
    ids.asset = asset.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .expect("asset id");
    // Signed here rather than by the sweep, so the attestation *read* routes
    // have an envelope to leak and a baseline that does not depend on the order
    // the probes happen to run in.
    let signed = fx
        .client
        .post(fx.url(
            VICTIM,
            VAULT,
            &format!("/releases/assets/{}/attestation", ids.asset),
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        signed.status(),
        201,
        "fixture: signing the victim's release asset failed"
    );

    ids
}

/// The attacker's repository carries only what a *path* needs: the probes point
/// at issue `#1`, pull request `#1` and a wiki page called `Home`, so those have
/// to resolve here too. Everything else in the URL is the victim's.
async fn seed_host(fx: &Fixture, db: &rg_db::DatabaseConnection, attacker_id: i64, repo: i64) {
    fx.create(
        &fx.attacker_token,
        fx.url(ATTACKER, HOST, "/issues"),
        serde_json::json!({"title": "host"}),
        "attacker issue",
    )
    .await;
    fx.create(
        &fx.attacker_token,
        fx.url(ATTACKER, HOST, "/wiki"),
        serde_json::json!({"title": "Home", "content": "host"}),
        "attacker wiki page",
    )
    .await;
    insert_pr(db, repo, attacker_id, 1).await;
}

/// A valid ed25519 public key. Deploy keys are unique instance-wide, so only the
/// victim's repository gets one.
const VICTIM_DEPLOY_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA scope-vault";

// ── Which routes this sweep can drive ──────────────────────────────────────

/// What the sweep can do with one route.
enum Coverage {
    /// Drive it, substituting these ids for the route's global-id placeholders.
    Probe(HashMap<&'static str, i64>),
    /// Deliberately not driven, with the reason.
    Skipped(&'static str),
    /// Nothing here classifies it — a new route, or a new resource.
    Unclassified,
}

/// Classify one repository-scoped route.
///
/// `tail` is the path with `/api/v1/repos/{owner}/{name}` stripped. Every arm is
/// either a set of the victim's ids or a signed reason; the catch-all fails the
/// run, which is the point: a repository-scoped resource cannot be added without
/// somebody deciding whether its id is reachable across repositories.
fn coverage(tail: &str, ids: &Ids) -> Coverage {
    let probe = |pairs: &[(&'static str, i64)]| Coverage::Probe(pairs.iter().copied().collect());
    match tail {
        "/keys/{id}" => probe(&[("id", ids.deploy_key)]),
        "/milestones/{id}" => probe(&[("id", ids.milestone)]),
        "/labels/{id}" => probe(&[("id", ids.label)]),

        "/issues/{number}/assets/{attachment_id}" => {
            probe(&[("attachment_id", ids.issue_attachment)])
        }
        "/issues/comments/{comment_id}/assets" => probe(&[("comment_id", ids.issue_comment)]),
        "/issues/comments/{comment_id}/assets/{attachment_id}" => probe(&[
            ("comment_id", ids.issue_comment),
            ("attachment_id", ids.comment_attachment),
        ]),
        "/issues/{number}/time/{id}" => probe(&[("id", ids.time_entry)]),

        "/pulls/{number}/assets/{attachment_id}" => probe(&[("attachment_id", ids.pr_attachment)]),
        "/pulls/comments/{comment_id}/assets" => probe(&[("comment_id", ids.review_comment)]),
        "/pulls/comments/{comment_id}/assets/{attachment_id}" => probe(&[
            ("comment_id", ids.review_comment),
            ("attachment_id", ids.review_comment_attachment),
        ]),
        "/pulls/{number}/reviews/{id}" | "/pulls/{number}/reviews/{id}/dismiss" => {
            probe(&[("id", ids.pr_review)])
        }
        "/pulls/{number}/comments/{id}/resolution"
        | "/pulls/{number}/comments/{id}/suggestion/apply" => probe(&[("id", ids.review_comment)]),

        "/wiki/{title}/revisions/{rev_id}" => probe(&[("rev_id", ids.wiki_revision)]),

        "/hooks/{id}" | "/hooks/{id}/deliveries" => probe(&[("id", ids.hook)]),
        "/hooks/{id}/deliveries/{delivery_id}/redeliver" => {
            probe(&[("id", ids.hook), ("delivery_id", ids.delivery)])
        }

        "/pipelines/{id}"
        | "/pipelines/{id}/retry"
        | "/pipelines/{id}/cancel"
        | "/pipelines/{id}/artifacts" => probe(&[("id", ids.pipeline)]),
        "/pipelines/{id}/jobs/{job_id}" | "/pipelines/{id}/jobs/{job_id}/play" => {
            probe(&[("id", ids.pipeline), ("job_id", ids.job)])
        }
        "/pipelines/{pipeline_id}/jobs/{job_id}/approve" => {
            probe(&[("pipeline_id", ids.pipeline), ("job_id", ids.job)])
        }

        "/actions/environments/{id}" => probe(&[("id", ids.environment)]),
        "/branches/protection/{id}" => probe(&[("id", ids.branch_protection)]),
        "/tags/protection/{id}" => probe(&[("id", ids.tag_protection)]),

        "/boards/{id}" | "/boards/{id}/columns" | "/boards/{id}/cards/reorder" => {
            probe(&[("id", ids.board)])
        }
        "/boards/{id}/columns/{col_id}" | "/boards/{id}/columns/{col_id}/cards" => {
            probe(&[("id", ids.board), ("col_id", ids.column)])
        }
        "/boards/{id}/cards/{card_id}" | "/boards/{id}/cards/{card_id}/move" => {
            probe(&[("id", ids.board), ("card_id", ids.card)])
        }

        "/releases/{id}" => probe(&[("id", ids.release)]),
        "/releases/{release_id}/assets" => probe(&[("release_id", ids.release)]),
        tail if tail.starts_with("/releases/assets/{asset_id}") => {
            probe(&[("asset_id", ids.asset)])
        }

        "/collaborators/{id}" => Coverage::Skipped(
            "`{id}` is a user id, and a collaboration is keyed `(repo_id, user_id)`: the id \
             names a person, not a row belonging to another repository, so a cross-repository \
             probe reaches nothing to begin with",
        ),
        "/packages/nuget/registration/{id}/index.json" => Coverage::Skipped(
            "`{id}` here is a NuGet package name rather than a primary key — package names are \
             scoped to their repository's registry",
        ),

        _ => Coverage::Unclassified,
    }
}

// ── Driving one route ──────────────────────────────────────────────────────

/// How a probe's request body is built.
enum Body {
    None,
    Json(serde_json::Value),
    /// A release asset: raw bytes plus the filename header.
    Raw,
    /// An attachment: `multipart/form-data`.
    Multipart,
}

/// The body a route needs to get past its own deserializer.
///
/// A route answered `422` before it ever looked an id up has proven nothing, so
/// anything with a required field gets one. Ids that appear in a *body* rather
/// than a path — `positions`, `comment_ids` — are the victim's too: that is the
/// same defect class one layer in.
fn body_for(method: &str, tail: &str, ids: &Ids) -> Body {
    if !matches!(method, "POST" | "PUT" | "PATCH") {
        return Body::None;
    }
    match tail {
        "/releases/{release_id}/assets" => Body::Raw,
        "/issues/comments/{comment_id}/assets" | "/pulls/comments/{comment_id}/assets" => {
            Body::Multipart
        }
        "/actions/environments/{id}" => Body::Json(serde_json::json!({"name": "production"})),
        "/tags/protection/{id}" => Body::Json(serde_json::json!({"allowed_user_ids": []})),
        "/boards/{id}/columns" => Body::Json(serde_json::json!({"name": "probe"})),
        "/boards/{id}/columns/{col_id}/cards" => Body::Json(serde_json::json!({"note": "probe"})),
        "/boards/{id}/cards/{card_id}/move" => {
            Body::Json(serde_json::json!({"column_id": ids.column, "position": 0}))
        }
        "/boards/{id}/cards/reorder" => {
            Body::Json(serde_json::json!({"positions": [[ids.card, 0]]}))
        }
        "/pulls/{number}/reviews/{id}/dismiss" => {
            Body::Json(serde_json::json!({"message": "probe"}))
        }
        "/pulls/{number}/comments/{id}/resolution" => {
            Body::Json(serde_json::json!({"resolved": true}))
        }
        "/pulls/{number}/comments/{id}/suggestion/apply" => {
            Body::Json(serde_json::json!({"comment_ids": [ids.review_comment]}))
        }
        _ => Body::Json(serde_json::json!({})),
    }
}

/// Turn a route pattern into a concrete URL.
///
/// The repository in the path is the caller's argument — that is the only thing
/// the probe and its baseline disagree about. Every global id comes from
/// `targets`; every per-repository placeholder resolves to something both
/// repositories have.
fn fill(path: &str, owner: &str, repo: &str, targets: &HashMap<&'static str, i64>) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..].find('}').expect("unclosed path placeholder") + open;
        let raw = &rest[open + 1..close];
        let name = raw.strip_prefix('*').unwrap_or(raw);
        let value = match name {
            "owner" => owner.to_string(),
            "name" | "repo" => repo.to_string(),
            // Both repositories carry issue #1, pull request #1 and a page
            // called Home, so the locator resolves either way and the only
            // foreign thing left in the URL is the id under test.
            "number" => "1".to_string(),
            "title" => "Home".to_string(),
            other => targets
                .get(other)
                .unwrap_or_else(|| panic!("no seeded id for placeholder '{other}' in {path}"))
                .to_string(),
        };
        out.push_str(&value);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

async fn drive(
    fx: &Fixture,
    fact: &RouteFact,
    owner: &str,
    repo: &str,
    token: &str,
    targets: &HashMap<&'static str, i64>,
    ids: &Ids,
) -> (StatusCode, String) {
    let url = format!("{}{}", fx.base, fill(&fact.path, owner, repo, targets));
    let mut request = match fact.method {
        "GET" => fx.client.get(url),
        "HEAD" => fx.client.head(url),
        "POST" => fx.client.post(url),
        "PUT" => fx.client.put(url),
        "PATCH" => fx.client.patch(url),
        "DELETE" => fx.client.delete(url),
        other => panic!("route table produced an unroutable method {other}"),
    };
    request = request.bearer_auth(token);
    let tail = fact
        .path
        .strip_prefix(REPO_PREFIX)
        .expect("repository-scoped path");
    request = match body_for(fact.method, tail, ids) {
        Body::None => request,
        Body::Json(body) => request.json(&body),
        Body::Raw => request
            .header("content-type", "text/plain")
            .header("content-disposition", "attachment; filename=probe.txt")
            .body("probe"),
        Body::Multipart => request.multipart(
            Form::new().part(
                "attachment",
                Part::bytes(b"probe".to_vec())
                    .file_name("probe.txt")
                    .mime_str("text/plain")
                    .unwrap(),
            ),
        ),
    };
    let response = request.send().await.expect("scope sweep request");
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    (status, body.chars().take(160).collect())
}

fn denied(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 403 | 404)
}

/// A body complaint is not a verdict on the id: the handler rejected the request
/// before it looked anything up.
fn inconclusive(status: StatusCode) -> bool {
    matches!(status.as_u16(), 400 | 409 | 415 | 422)
}

/// Routes whose probe cannot reach the id lookup, each with the reason.
///
/// Checked both ways: a route that starts answering the question has to leave
/// the list, so it cannot quietly become a blanket allowance.
const NO_VERDICT: &[(&str, &str)] = &[];

// ── The sweep ──────────────────────────────────────────────────────────────

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn no_repository_scoped_route_reaches_another_repositorys_rows() {
    let (base, facts, db) = spawn_test_app_with_routes_and_db().await;
    let client = Client::builder().build().expect("http client");
    let (attacker_token, attacker_id) =
        register_full(&base, ATTACKER, &format!("{ATTACKER}@example.com")).await;
    let (victim_token, victim_id) =
        register_full(&base, VICTIM, &format!("{VICTIM}@example.com")).await;
    let fx = Fixture {
        base,
        client,
        attacker_token,
        victim_token,
    };
    create_repo(&fx, &fx.attacker_token, HOST).await;
    create_repo(&fx, &fx.victim_token, VAULT).await;
    let host = repo_id(&db, attacker_id, HOST).await;
    let vault = repo_id(&db, victim_id, VAULT).await;
    seed_host(&fx, &db, attacker_id, host).await;
    let pr = insert_pr(&db, vault, victim_id, 1).await;
    let ids = seed_vault(&fx, &db, victim_id, vault, pr.id).await;

    assert!(
        !facts.is_empty(),
        "the route table came back empty — the sweep is not testing anything"
    );

    // Every placeholder of a repository-scoped route is either a global id or
    // per-repository, and a new one has to be classified before it can be
    // ignored.
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    for fact in &facts {
        let Some(tail) = fact.path.strip_prefix(REPO_PREFIX) else {
            continue;
        };
        for name in placeholders(tail) {
            let maven_segment = name.strip_prefix('m').is_some_and(|index| {
                index
                    .parse::<usize>()
                    .is_ok_and(|index| (1..=MAVEN_SEGMENTS).contains(&index))
            });
            if !maven_segment
                && !GLOBAL_ID_PLACEHOLDERS.contains(&name)
                && !PER_REPO_PLACEHOLDERS
                    .iter()
                    .any(|(known, _)| *known == name)
            {
                unknown.insert(format!("  {{{name}}} in {}", fact.label()));
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "a repository-scoped route names {} placeholder(s) this sweep has never heard of. Each \
         one is either an instance-wide primary key — add it to GLOBAL_ID_PLACEHOLDERS and give \
         `coverage` a row to point it at — or scoped to its repository, in which case add it to \
         PER_REPO_PLACEHOLDERS with the reason.\n{}",
        unknown.len(),
        unknown.into_iter().collect::<Vec<_>>().join("\n"),
    );

    // The routes in scope: repository-scoped, and carrying a global id.
    let mut probes: Vec<(&RouteFact, HashMap<&'static str, i64>)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut unclassified: Vec<String> = Vec::new();
    for fact in &facts {
        let Some(tail) = fact.path.strip_prefix(REPO_PREFIX) else {
            continue;
        };
        if !placeholders(tail).any(|name| GLOBAL_ID_PLACEHOLDERS.contains(&name)) {
            continue;
        }
        if !fact.access.is_repo_scoped() {
            unclassified.push(format!(
                "  {} — carries a global id but declares {:?}, which this sweep cannot drive",
                fact.label(),
                fact.access
            ));
            continue;
        }
        match coverage(tail, &ids) {
            Coverage::Probe(targets) => probes.push((fact, targets)),
            Coverage::Skipped(reason) => skipped.push(format!("  {} — {reason}", fact.label())),
            Coverage::Unclassified => unclassified.push(format!(
                "  {} — no rule in `coverage` names this route",
                fact.label()
            )),
        }
    }
    assert!(
        unclassified.is_empty(),
        "{} repository-scoped route(s) carry an instance-wide id and nothing decides whether it \
         can be reached from another repository. Seed the resource in `seed_vault` and give \
         `coverage` a row for it, or sign the route off there with the reason it cannot be \
         probed.\n{}",
        unclassified.len(),
        unclassified.join("\n"),
    );
    assert!(
        probes.len() > 50,
        "only {} route(s) are being probed — the filter is wrong, not the server",
        probes.len()
    );

    for (label, reason) in NO_VERDICT {
        assert!(
            probes.iter().any(|(fact, _)| fact.label() == *label),
            "NO_VERDICT names '{label}' ({reason}), which this sweep no longer drives — drop it"
        );
    }

    // Delete the innermost rows first, so a container is never removed out from
    // under a route that still has to be driven: `/boards/{id}/cards/{card_id}`
    // before `/boards/{id}/columns/{col_id}` (alphabetical, at equal depth)
    // before `/boards/{id}`, and a release asset before its release.
    probes.sort_by_key(|(fact, _)| {
        let destructive = fact.method == "DELETE";
        let depth = if destructive {
            usize::MAX - fact.path.matches('/').count()
        } else {
            0
        };
        (destructive, depth, fact.path.clone(), fact.method)
    });

    let mut leaks: Vec<String> = Vec::new();
    let mut no_verdict: BTreeSet<String> = BTreeSet::new();
    let mut verdicts: HashMap<String, StatusCode> = HashMap::new();

    // ── Pass one: the attacker's repository, the victim's ids ──────────────
    for (fact, targets) in &probes {
        let (status, body) =
            drive(&fx, fact, ATTACKER, HOST, &fx.attacker_token, targets, &ids).await;
        verdicts.insert(fact.label(), status);
        if denied(status) {
            continue;
        }
        if inconclusive(status) {
            no_verdict.insert(fact.label());
            if !NO_VERDICT.iter().any(|(entry, _)| *entry == fact.label()) {
                leaks.push(format!(
                    "  {}\n      answered {status} — the request never reached the id lookup, so \
                     nothing was proven. Give the route a body in `body_for`, or add it to \
                     NO_VERDICT with the reason.\n      body: {body}",
                    fact.label()
                ));
            }
            continue;
        }
        leaks.push(format!(
            "  {}\n      answered {status} for a row belonging to {VICTIM}/{VAULT}. The gate on \
             {ATTACKER}/{HOST} passed honestly; the id was never scoped to the repository in the \
             path.\n      body: {body}",
            fact.label()
        ));
    }

    // ── Pass two: the same requests, against the repository the ids live in ──
    let mut dead: Vec<String> = Vec::new();
    let mut proved = 0usize;
    for (fact, targets) in &probes {
        let (status, body) = drive(&fx, fact, VICTIM, VAULT, &fx.victim_token, targets, &ids).await;
        if !denied(status) {
            // The strongest form of the control: this exact request *worked*
            // where the ids live, and was refused where they do not.
            if status.is_success()
                && verdicts
                    .get(&fact.label())
                    .is_some_and(|probe| denied(*probe))
            {
                proved += 1;
            }
            continue;
        }
        let probe_status = verdicts
            .get(&fact.label())
            .copied()
            .unwrap_or(StatusCode::OK);
        dead.push(format!(
            "  {}\n      the owner is denied ({status}) in their own repository, so the {probe_status} \
             the probe got proves nothing. Either the fixture never seeded this row, or the probe \
             above destroyed it.\n      body: {body}",
            fact.label()
        ));
    }

    // A quarantined route that has started answering must leave its list.
    let mut healed: Vec<String> = Vec::new();
    for (label, reason) in NO_VERDICT {
        if !no_verdict.contains(*label) {
            healed.push(format!(
                "  {label} now reaches its id lookup — drop it from NO_VERDICT (was: {reason})"
            ));
        }
    }

    assert!(
        leaks.is_empty() && dead.is_empty() && healed.is_empty(),
        "cross-repository id scope: {} route(s) reached another repository's rows, {} dead \
         baseline(s), {} stale quarantine entr(ies), out of {} probed ({} signed off).\n{}{}{}",
        leaks.len(),
        dead.len(),
        healed.len(),
        probes.len(),
        skipped.len(),
        leaks.join("\n"),
        dead.join("\n"),
        healed.join("\n"),
    );

    // Not a coverage counter — a non-vacuity one. `denied` accepts `404`, and a
    // fixture that quietly stopped seeding would hand out `404` everywhere and
    // read as a clean bill of health. This counts the routes where the *same*
    // request succeeded against the repository the ids live in, so the denial
    // above is a refusal rather than an absence.
    assert!(
        proved >= 60,
        "only {proved} of {} probed route(s) both served the id in its own repository and \
         refused it in another. The sweep's denials are only meaningful where that pair holds, \
         so either the fixture stopped seeding or the routes stopped answering",
        probes.len(),
    );
}

/// The placeholder names of a path, in order.
fn placeholders(path: &str) -> impl Iterator<Item = &str> {
    path.split('{').skip(1).filter_map(|chunk| {
        chunk
            .split('}')
            .next()
            .map(|name| name.strip_prefix('*').unwrap_or(name))
    })
}
