//! Regression coverage for card_d44bb577daf5: HTTP handlers must classify a
//! database outage as 503 (retryable) rather than collapsing it into a 500.
//!
//! Before the fix, handlers converted `sea_orm::DbErr` by hand through
//! `AppError::internal(error)` (always 500), bypassing the `From<DbErr>`
//! classification in `error.rs` that maps connection-level failures to 503.
//! This test drives a real handler with a closed connection pool and asserts
//! the response is `503 SERVICE_UNAVAILABLE`, proving the handler now routes
//! the error through `AppError::from`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::common::{build_test_app_state, register_user, setup_test_db, spawn_test_app_with_db};

async fn instance_admin_probe(_admin: rg_http::api::admin::InstanceAdmin) -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn artifact_anchor_probe(_artifact: rg_http::api::artifacts::ArtifactRead) -> StatusCode {
    StatusCode::OK
}

/// Drive the production artifact anchor without unrelated global middleware.
/// Otherwise a closed pool can fail in session handling before `Artifact::resolve`
/// runs, making this regression vacuously green.
async fn artifact_anchor_probe_response(
    state: rg_http::AppState,
    artifact_id: i64,
) -> axum::response::Response {
    use tower::ServiceExt as _;

    let request = axum::http::Request::builder()
        .uri(format!("/artifact-anchor-probe/{artifact_id}"))
        .body(axum::body::Body::empty())
        .expect("build artifact anchor probe request");

    axum::Router::new()
        .route(
            "/artifact-anchor-probe/{id}",
            axum::routing::get(artifact_anchor_probe),
        )
        .with_state(state)
        .oneshot(request)
        .await
        .expect("artifact anchor probe response")
}

/// Drive the public `InstanceAdmin` extractor without the global session
/// middleware. The production router correctly has both layers, but on a closed
/// pool the session layer also returns 503; letting it answer first would make a
/// broken admin extractor look fixed.
async fn instance_admin_probe_response(
    state: rg_http::AppState,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use tower::ServiceExt as _;

    let mut request = axum::http::Request::builder()
        .uri("/instance-admin-probe")
        .body(axum::body::Body::empty())
        .expect("build instance-admin probe request");
    request.headers_mut().extend(headers);

    axum::Router::new()
        .route(
            "/instance-admin-probe",
            axum::routing::get(instance_admin_probe),
        )
        .with_state(state)
        .oneshot(request)
        .await
        .expect("instance-admin probe response")
}

/// Acceptance for card_fe74604899f7: absence, denial and a gate that could not
/// run are three different outcomes. This targets the extractor itself so the
/// outage assertion cannot pass vacuously through `session_standing_middleware`.
#[tokio::test]
async fn instance_admin_gate_distinguishes_401_403_and_503() {
    use sea_orm::ActiveModelTrait;
    use sea_orm::ActiveValue::Set;

    const NON_ADMIN_ID: i64 = 900_301;
    const ADMIN_ID: i64 = 900_302;

    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");

    let now = chrono::Utc::now();
    for (id, username, is_admin) in [
        (NON_ADMIN_ID, "instance-gate-user", false),
        (ADMIN_ID, "instance-gate-admin", true),
    ] {
        rg_db::entities::user::ActiveModel {
            id: Set(id),
            username: Set(username.to_string()),
            email: Set(format!("{username}@example.test")),
            password_hash: Set(String::new()),
            is_admin: Set(is_admin),
            is_active: Set(true),
            auth_provider: Set("local".to_string()),
            mfa_enabled: Set(false),
            login_attempts: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("insert instance-admin gate fixture user");
    }

    let state = build_test_app_state(db.clone(), repo_root);

    let anonymous =
        instance_admin_probe_response(state.clone(), axum::http::HeaderMap::new()).await;
    assert_eq!(
        anonymous.status(),
        StatusCode::UNAUTHORIZED,
        "an admin route without a session must answer 401"
    );

    let denied =
        instance_admin_probe_response(state.clone(), bearer(NON_ADMIN_ID, "instance-gate-user"))
            .await;
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "an authenticated non-admin must answer 403"
    );

    let admitted =
        instance_admin_probe_response(state.clone(), bearer(ADMIN_ID, "instance-gate-admin")).await;
    assert_eq!(
        admitted.status(),
        StatusCode::NO_CONTENT,
        "healthy baseline: the fixture admin must pass the gate"
    );

    db.close().await.expect("close instance-admin fixture pool");

    let outage =
        instance_admin_probe_response(state, bearer(ADMIN_ID, "instance-gate-admin")).await;
    assert_eq!(
        outage.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a failed admin lookup must answer 503, not admin required"
    );
}

/// A connection-level `DbErr` raised inside `runners::download_workspace`
/// (the handler named in the card) must surface as 503, not 500.
#[tokio::test]
async fn db_outage_in_handler_returns_503_not_500() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let state = build_test_app_state(db.clone(), repo_root);

    // Simulate a database outage: close the shared connection pool. Every
    // subsequent query from the handler's cloned handle fails at the
    // connection level (pool closed) — exactly the retryable-outage case.
    db.close().await.expect("close pool");

    // The very first thing `download_workspace` does is a DB lookup, so the
    // closed pool short-circuits into the error arm we converted.
    let response = rg_http::api::runners::download_workspace(State(state), Path((1_i64, 1_i64)))
        .await
        .into_response();

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "database outage must map to 503, not 500"
    );
}

/// Peripheral of the same class (card_2e5ab5773b6f): the package-registry
/// handlers converted DB errors through a local `err()` helper (which stringifies
/// into `AppError::internal` → 500) instead of `AppError::from`, and their shared
/// `resolve_repo` gateway used `.map_err(AppError::internal)`. A DB outage on a
/// packages route (here `list_packages`, whose first action is the `resolve_repo`
/// DB lookup) must now surface as 503, not 500.
#[tokio::test]
async fn db_outage_in_packages_handler_returns_503_not_500() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let state = build_test_app_state(db.clone(), repo_root);

    // Close the pool to simulate a connection-level outage.
    db.close().await.expect("close pool");

    let response = get_through_router(
        state,
        "/api/v1/repos/owner/repo/packages/cargo/list",
        axum::http::HeaderMap::new(),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "database outage on a packages route must map to 503, not 500"
    );
}

/// The contents write gate used to resolve the repository locally with
/// `.map_err(AppError::internal)`, so a closed pool on the first repo lookup
/// became 500 instead of the shared repo-access gateway's 503.
///
/// Driven through the router because that gate is now the `RepoWrite` extractor
/// — the handler cannot be called without it, which is what stops the next
/// contents endpoint from quietly shipping without a gate at all.
#[tokio::test]
async fn db_outage_in_contents_write_gate_returns_503_not_500() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let state = build_test_app_state(db.clone(), repo_root);
    let headers = bearer(42, "contents-outage");

    db.close().await.expect("close pool");

    let response = through_router(
        state,
        "POST",
        "/api/v1/repos/owner/repo/contents/README.md",
        headers,
        Some(serde_json::json!({
            "content": "# outage\n",
            "message": "write during outage",
        })),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "database outage while resolving a contents write must map to 503, not 500"
    );
}

/// Peripheral of the same class (card_2e22af5e82a2): the OCI Distribution
/// registry handlers convert DB errors through a local `oci_err()` helper that
/// hardcoded `INTERNAL_SERVER_ERROR`, bypassing the 503 classification. Unlike
/// the JSON API they can't route through `AppError` — docker/podman expect the
/// OCI `{errors:[{code,message}]}` envelope — so the fix classifies only the
/// status (via `oci_status_for`) and keeps the envelope. A DB outage on
/// `GET /v2/{owner}/{repo}/tags/list` must surface as 503 *and* still carry the
/// OCI error-envelope, not a 500 and not the AppError JSON body.
#[tokio::test]
async fn db_outage_in_oci_handler_returns_503_with_oci_envelope() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let state = build_test_app_state(db.clone(), repo_root);

    // Close the pool to simulate a connection-level outage. `list_tags` hits
    // the DB immediately (access check → repo lookup), so the closed pool
    // short-circuits into one of the converted `oci_err(oci_status_for(&e), …)`
    // arms.
    db.close().await.expect("close pool");

    let response = rg_http::oci::list_tags(
        State(state),
        axum::http::HeaderMap::new(),
        Path(("owner".to_string(), "repo".to_string())),
    )
    .await
    .into_response();

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "database outage on an OCI registry route must map to 503, not 500"
    );

    // The OCI error-envelope must be preserved: docker/podman parse
    // `{"errors":[{"code":...}]}`, not the AppError `{"error":{...}}` shape.
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("body is JSON");
    assert!(
        json.get("errors").and_then(|e| e.as_array()).is_some(),
        "response must keep the OCI error-envelope (`errors` array), got: {json}"
    );
    assert!(
        json.get("error").is_none(),
        "response must NOT be the AppError JSON body (`error` object): {json}"
    );
}

// ── Permission checks (card_23f4c3795212) ────────────────────────────────
//
// The same class seen from the authorization side. Handlers evaluated
// `can_read_repo` / `can_write_repo` / `can_write` with `.unwrap_or(false)`, so
// a permission query that could not run was indistinguishable from one that
// answered "no" — and the caller got `403 access denied` (or `401` when
// anonymous) for a failure that was entirely ours. That is the worst possible
// answer: it tells the client its credentials are the problem, so it goes off
// to re-issue a token instead of retrying.
//
// The two tests below let every other query succeed and break only the query
// *behind the permission check* — `repo_collaborator_ops::get_permission`, the
// single DB call `can_read_repo`/`can_write_repo` make for a non-owner. That is
// what makes them regression tests rather than restatements of the outage tests
// above: with the pool closed, the repository lookup would fail first and the
// handler would answer 503 whether or not the permission check was fixed.

/// Seed an owner, an outsider and one repository owned by the owner.
///
/// Ids are explicit and far out of the auto-increment range on purpose: the
/// permission cache in `rg_core::repo::service` is a process-global keyed by
/// `(repo_id, actor_id, for_write)` and every test in this binary shares it,
/// so ids 1/2 from a neighbouring test would otherwise serve a cached verdict
/// here and make these tests flaky (`sol_6f60c7e3f076`).
async fn seed_repo_with_outsider(
    db: &rg_db::DatabaseConnection,
    is_private: bool,
) -> (
    &'static str,
    &'static str,
    rg_db::entities::repository::Model,
    i64,
    &'static str,
) {
    use sea_orm::ActiveModelTrait;
    use sea_orm::ActiveValue::Set;

    const OWNER_ID: i64 = 900_101;
    const OUTSIDER_ID: i64 = 900_102;
    const REPO_ID: i64 = 900_103;

    let now = chrono::Utc::now();
    for (id, username) in [(OWNER_ID, "perm-owner"), (OUTSIDER_ID, "perm-outsider")] {
        rg_db::entities::user::ActiveModel {
            id: Set(id),
            username: Set(username.to_string()),
            email: Set(format!("{username}@example.test")),
            password_hash: Set(String::new()),
            is_admin: Set(false),
            is_active: Set(true),
            auth_provider: Set("local".to_string()),
            mfa_enabled: Set(false),
            login_attempts: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert user");
    }

    let repo = rg_db::entities::repository::ActiveModel {
        id: Set(REPO_ID),
        owner_id: Set(OWNER_ID),
        name: Set("perm-repo".to_string()),
        is_private: Set(is_private),
        default_branch: Set("main".to_string()),
        stars_count: Set(0),
        forks_count: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert repository");

    (
        "perm-owner",
        "perm-repo",
        repo,
        OUTSIDER_ID,
        "perm-outsider",
    )
}

/// The card's two densest paths must classify the failure at their own DB
/// boundary. The attachment call receives a pre-resolved `RepoRead`, while the
/// artifact probe mounts the production `ArtifactRead` extractor without the
/// global session middleware; neither assertion can be satisfied by an earlier
/// repository/session lookup returning 503.
#[tokio::test]
async fn artifact_and_attachment_db_outages_return_503_after_healthy_baselines() {
    use sea_orm::ActiveValue::Set;

    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let (owner, repo_name, repo, _outsider_id, _outsider) =
        seed_repo_with_outsider(&db, false).await;

    const ISSUE_NUMBER: i64 = 900_104;
    rg_db::ops::issue_ops::create(
        &db,
        rg_db::entities::issue::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo.id),
            number: Set(ISSUE_NUMBER),
            title: Set("DB outage attachment target".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            author_id: Set(repo.owner_id),
            assignee_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
            closed_at: Set(None),
            deleted_at: Set(None),
        },
    )
    .await
    .expect("create attachment target issue");

    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo.id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .expect("create artifact pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
        .await
        .expect("create artifact stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        &db, stage.id, "unit", "echo ok", None, None, None, None, None, false, None, None, None,
    )
    .await
    .expect("create artifact job");
    let artifact = rg_db::ops::artifact_ops::create_artifact(
        &db,
        job.id,
        "report.txt",
        "artifacts/jobs/report.txt",
        1,
        None,
        None,
    )
    .await
    .expect("create artifact row");

    let state = build_test_app_state(db.clone(), repo_root);

    let healthy_attachments = rg_http::api::attachments::list_issue_attachments(
        State(state.clone()),
        Path((owner.to_string(), repo_name.to_string(), ISSUE_NUMBER)),
        rg_http::api::repo_access::RepoRead { repo: repo.clone() },
    )
    .await;
    assert_eq!(
        healthy_attachments.status(),
        StatusCode::OK,
        "healthy attachment baseline must reach the local target resolver"
    );

    let healthy_artifact = artifact_anchor_probe_response(state.clone(), artifact.id).await;
    assert_eq!(
        healthy_artifact.status(),
        StatusCode::OK,
        "healthy artifact baseline must pass the production anchor"
    );

    db.close().await.expect("close artifact/attachment pool");

    let broken_attachments = rg_http::api::attachments::list_issue_attachments(
        State(state.clone()),
        Path((owner.to_string(), repo_name.to_string(), ISSUE_NUMBER)),
        rg_http::api::repo_access::RepoRead { repo },
    )
    .await;
    assert_eq!(
        broken_attachments.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "attachment target lookup outage must map to 503, not 500"
    );

    let broken_artifact = artifact_anchor_probe_response(state, artifact.id).await;
    assert_eq!(
        broken_artifact.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "artifact anchor lookup outage must map to 503, not 500"
    );
}

/// Break the one query a permission check makes for a non-owner, leaving the
/// rest of the schema intact.
async fn break_permission_lookup(db: &rg_db::DatabaseConnection) {
    use sea_orm::ConnectionTrait;
    db.execute_unprepared("DROP TABLE repo_collaborators")
        .await
        .expect("drop repo_collaborators");
    rg_core::repo::service::invalidate_perm_cache_all(db);
}

/// Drive a GET through the real router rather than calling the handler as a
/// function.
///
/// The repository access gate is an axum extractor now (`RepoRead` and friends
/// in `api::repo_access`), so it runs *before* the handler body and cannot be
/// constructed by hand — which is the point: a handler can no longer be invoked
/// without its gate. Going through the router exercises the same classification
/// these tests were written for, one layer earlier.
async fn get_through_router(
    state: rg_http::AppState,
    path: &str,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    through_router(state, "GET", path, headers, None).await
}

/// [`get_through_router`] for any method, with an optional JSON body.
async fn through_router(
    state: rg_http::AppState,
    method: &str,
    path: &str,
    headers: axum::http::HeaderMap,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    use tower::ServiceExt as _;

    let builder = axum::http::Request::builder().method(method).uri(path);
    let mut request = match &body {
        Some(json) => builder
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(json.to_string()))
            .expect("build request"),
        None => builder
            .body(axum::body::Body::empty())
            .expect("build request"),
    };
    request.headers_mut().extend(headers);

    rg_http::create_router_for_test(state)
        .oneshot(request)
        .await
        .expect("router response")
}

fn bearer(user_id: i64, username: &str) -> axum::http::HeaderMap {
    let token = rg_core::auth::jwt::generate_token(user_id, username, "test-secret-key", 7)
        .expect("generate token");
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {token}").parse().expect("header value"),
    );
    headers
}

/// Write side: a failing `can_write_repo` must not be reported as "forbidden".
#[tokio::test]
async fn failed_write_permission_check_is_not_reported_as_forbidden() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let (owner, repo, _repo_model, outsider_id, outsider) =
        seed_repo_with_outsider(&db, false).await;
    let state = build_test_app_state(db.clone(), repo_root);
    break_permission_lookup(&db).await;

    // Through the router, for the reason `get_through_router` gives: the write
    // check now runs in the `RepoWrite` extractor, so the handler cannot be
    // called without it — and the classification under test is the extractor's.
    let response = through_router(
        state,
        "POST",
        &format!("/api/v1/repos/{owner}/{repo}/labels"),
        bearer(outsider_id, outsider),
        Some(serde_json::json!({
            "name": "bug",
            "color": "#ff0000",
        })),
    )
    .await;

    assert_ne!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a write check that could not run must not be answered as 'forbidden'"
    );
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "the failed permission check must surface as a server-side failure"
    );
}

/// Same class, third predicate (card_1e1ed1ee06f1): `POST /repos` decided
/// organization membership itself, and its `_ =>` arm swallowed the `Err` — so a
/// membership lookup that could not run answered `403 you are not a member of
/// this organization`, telling the caller their account is the problem.
///
/// Only `organization_members` is dropped, so every other query on the path
/// still succeeds: with the pool closed the *user* lookup ahead of it would fail
/// first and the route would answer a server error whether or not this was
/// fixed.
#[tokio::test]
async fn failed_org_membership_check_is_not_reported_as_forbidden() {
    use sea_orm::ActiveModelTrait;
    use sea_orm::ActiveValue::Set;
    use sea_orm::ConnectionTrait;

    const OUTSIDER_ID: i64 = 900_201;
    const ORG_OWNER_ID: i64 = 900_202;
    const ORG_ID: i64 = 900_203;

    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();

    let now = chrono::Utc::now();
    for (id, username) in [
        (OUTSIDER_ID, "org-outage-user"),
        (ORG_OWNER_ID, "org-outage-owner"),
    ] {
        rg_db::entities::user::ActiveModel {
            id: Set(id),
            username: Set(username.to_string()),
            email: Set(format!("{username}@example.test")),
            password_hash: Set(String::new()),
            is_admin: Set(false),
            is_active: Set(true),
            auth_provider: Set("local".to_string()),
            mfa_enabled: Set(false),
            login_attempts: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("insert user");
    }
    rg_db::entities::organization::ActiveModel {
        id: Set(ORG_ID),
        name: Set("outagecorp".to_string()),
        owner_id: Set(ORG_OWNER_ID),
        visibility: Set("public".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert organization");

    let state = build_test_app_state(db.clone(), repo_root);

    // The one query the membership rule makes, and nothing else.
    db.execute_unprepared("DROP TABLE organization_members")
        .await
        .expect("drop organization_members");

    let response = through_router(
        state,
        "POST",
        "/api/v1/repos",
        bearer(OUTSIDER_ID, "org-outage-user"),
        Some(serde_json::json!({
            "name": "during-outage",
            "org": "outagecorp",
        })),
    )
    .await;

    assert_ne!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a membership check that could not run must not be answered as 'you are not a member'"
    );
    assert!(
        response.status().is_server_error(),
        "the failed membership check must surface as a server-side failure, got {}",
        response.status()
    );
}

/// Acceptance for card_7425666e2f33: `GET /repos/{owner}` must distinguish a
/// real missing owner from a user lookup that could not run. The healthy
/// baselines go through the production router; the outage assertion invokes the
/// public handler directly so a 503 cannot come from session middleware before
/// `list_repos` executes. Before the fix both closed-pool errors were swallowed
/// and this exact invocation answered 404.
#[tokio::test]
async fn repo_owner_lookup_distinguishes_absence_from_database_failure() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let (owner, repo_name, _repo, _outsider_id, _outsider) =
        seed_repo_with_outsider(&db, false).await;
    let state = build_test_app_state(db.clone(), repo_root);

    let existing = get_through_router(
        state.clone(),
        &format!("/api/v1/repos/{owner}"),
        axum::http::HeaderMap::new(),
    )
    .await;
    assert_eq!(
        existing.status(),
        StatusCode::OK,
        "healthy baseline: an existing owner must be listable"
    );
    let body = axum::body::to_bytes(existing.into_body(), usize::MAX)
        .await
        .expect("read owner listing body");
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("owner listing body is JSON");
    assert_eq!(
        json["data"][0]["name"], repo_name,
        "healthy baseline must include the owner's repository"
    );

    let missing = get_through_router(
        state.clone(),
        "/api/v1/repos/definitely-missing-owner",
        axum::http::HeaderMap::new(),
    )
    .await;
    assert_eq!(
        missing.status(),
        StatusCode::NOT_FOUND,
        "an owner absent from both users and organizations must stay 404"
    );

    db.close().await.expect("close owner-listing pool");

    let outage = rg_http::api::repos::list_repos(
        State(state),
        axum::http::HeaderMap::new(),
        Path(owner.to_string()),
        axum::extract::Query(rg_http::api::repos::ListReposQuery {
            pagination: rg_http::pagination::PaginationParams {
                page: 1,
                per_page: 20,
            },
        }),
    )
    .await;
    let outage = outage.into_response();
    assert_ne!(
        outage.status(),
        StatusCode::NOT_FOUND,
        "a failed owner lookup must not be reported as an absent owner"
    );
    assert!(
        outage.status().is_server_error(),
        "a failed owner lookup must surface as a server error, got {}",
        outage.status()
    );
}

/// Read side: same contract, and here the pre-fix answer for an anonymous
/// caller was a `401` — an instruction to authenticate that no token satisfies.
#[tokio::test]
async fn failed_read_permission_check_is_not_reported_as_forbidden() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let (owner, repo, _repo_model, outsider_id, outsider) =
        seed_repo_with_outsider(&db, true).await;
    let state = build_test_app_state(db.clone(), repo_root);
    break_permission_lookup(&db).await;

    let response = get_through_router(
        state,
        &format!("/api/v1/repos/{owner}/{repo}/issue_templates"),
        bearer(outsider_id, outsider),
    )
    .await;

    assert_ne!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a read check that could not run must not be answered as 'access denied'"
    );
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "the failed permission check must surface as a server-side failure"
    );
}

const PAT_OUTAGE_OWNER: &str = "pat-outage-owner";
const PAT_OUTAGE_REPO: &str = "private";
const PAT_OUTAGE_INFO_REFS: &str = "info/refs?service=git-upload-pack";

/// Build a real PAT fixture through the public API so both the REST middleware
/// and git smart-HTTP exercise the same production credential path.
async fn seed_pat_outage_fixture() -> (String, rg_db::DatabaseConnection, String) {
    let (base, db) = spawn_test_app_with_db().await;
    let jwt = register_user(
        &base,
        PAT_OUTAGE_OWNER,
        "pat-outage-owner@example.test",
        "Qz7$wRtm",
    )
    .await;
    let client = reqwest::Client::new();

    let repo = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "name": PAT_OUTAGE_REPO,
            "is_private": true,
        }))
        .send()
        .await
        .expect("create private PAT fixture repo");
    assert_eq!(repo.status(), StatusCode::CREATED);

    let token = client
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "name": "outage-regression",
            "scopes": "user, repo",
        }))
        .send()
        .await
        .expect("create PAT fixture");
    assert_eq!(token.status(), StatusCode::CREATED);
    let pat = token
        .json::<serde_json::Value>()
        .await
        .expect("PAT response is JSON")["token"]
        .as_str()
        .expect("PAT response carries plaintext token")
        .to_string();

    (base, db, pat)
}

async fn get_with_pat(base: &str, path: &str, pat: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{base}{path}"))
        .bearer_auth(pat)
        .send()
        .await
        .expect("PAT-authenticated request")
}

/// Acceptance for card_eb7e2c05ba1a: a valid PAT whose lookup hits a closed
/// pool is a retryable server outage on both transports, never an instruction
/// to replace the credential.
#[tokio::test]
async fn closed_pool_during_pat_resolution_returns_503_on_rest_and_git() {
    let (base, db, pat) = seed_pat_outage_fixture().await;
    let git_path = format!("/git/{PAT_OUTAGE_OWNER}/{PAT_OUTAGE_REPO}/{PAT_OUTAGE_INFO_REFS}");

    // Prove the fixture reaches both authenticated paths before faulting it.
    for path in ["/api/v1/users/me", git_path.as_str()] {
        let response = get_with_pat(&base, path, &pat).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "healthy PAT baseline failed for {path}"
        );
    }

    db.close().await.expect("close PAT fixture pool");

    let rest = get_with_pat(&base, "/api/v1/users/me", &pat).await;
    assert_eq!(
        rest.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "REST PAT lookup outage must be 503, not missing authentication"
    );

    let git = get_with_pat(&base, &git_path, &pat).await;
    assert_eq!(
        git.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "git PAT lookup outage must be 503, not missing authentication"
    );
    assert_eq!(
        git.text().await.expect("git outage body"),
        "database temporarily unavailable",
        "git transport must log the detailed cause and expose only the fixed safe body"
    );
}

/// A closed pool also breaks the repository access query, so 503 alone cannot
/// prove the PAT resolver propagated its own error. Dropping only the PAT table
/// leaves the private repository and its access gate healthy: the old resolver
/// swallowed this failure, treated the caller as anonymous, and returned 401.
#[tokio::test]
async fn failed_pat_query_is_not_reported_as_a_missing_credential() {
    use sea_orm::ConnectionTrait;

    let (base, db, pat) = seed_pat_outage_fixture().await;
    db.execute_unprepared("DROP TABLE access_tokens")
        .await
        .expect("drop only the PAT table");

    let rest = get_with_pat(&base, "/api/v1/users/me", &pat).await;
    assert!(
        rest.status().is_server_error(),
        "failed REST PAT lookup must be a server error, got {}",
        rest.status()
    );
    assert_ne!(
        rest.status(),
        StatusCode::UNAUTHORIZED,
        "failed REST PAT lookup must not masquerade as an invalid credential"
    );

    let git_path = format!("/git/{PAT_OUTAGE_OWNER}/{PAT_OUTAGE_REPO}/{PAT_OUTAGE_INFO_REFS}");
    let git = get_with_pat(&base, &git_path, &pat).await;
    assert!(
        git.status().is_server_error(),
        "failed git PAT lookup must be a server error, got {}",
        git.status()
    );
    assert_ne!(
        git.status(),
        StatusCode::UNAUTHORIZED,
        "failed git PAT lookup must not masquerade as anonymous access"
    );
    assert_eq!(
        git.text().await.expect("git failure body"),
        "internal server error",
        "git transport must not expose the database error"
    );
}
