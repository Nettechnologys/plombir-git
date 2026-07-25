//! Axum router construction: the CORS layer, the full REST/Git/OCI route table,
//! and the production vs. test router assembly.

use axum::http::{header, HeaderValue, Method};
use axum::routing::{delete, get, patch, post, put, MethodRouter};
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::{
    api, git_http, handlers, metrics, middleware, oci, pat_auth, rate_limit, security, ws, AppState,
};

/// Build a restrictive CORS layer.
///
/// If `FORGEKEEP_CORS_ORIGINS` is set (comma-separated URLs), only those
/// origins are allowed. Otherwise, all origins are reflected (for development
/// convenience) with a warning logged.
///
/// Replaces `CorsLayer::permissive()` — restricts allowed methods and headers
/// to only what ForgeKeep needs.
fn build_cors_layer() -> CorsLayer {
    use std::time::Duration;

    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
        Method::OPTIONS,
    ];

    let headers_list = [header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT];

    match rg_core::env_compat::env_var_compat("FORGEKEEP_CORS_ORIGINS", "IRONFORGE_CORS_ORIGINS") {
        Some(origins_str) if !origins_str.is_empty() => {
            let origins: Vec<HeaderValue> = origins_str
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .filter_map(|s| HeaderValue::from_str(s).ok())
                .collect();

            if origins.is_empty() {
                tracing::warn!(
                    "FORGEKEEP_CORS_ORIGINS set but no valid origins parsed — CORS disabled"
                );
                CorsLayer::new()
                    .allow_methods(methods)
                    .allow_headers(headers_list)
            } else {
                tracing::info!(origins = ?origins, "CORS: allowing configured origins");
                CorsLayer::new()
                    .allow_origin(origins)
                    .allow_methods(methods)
                    .allow_headers(headers_list)
                    .allow_credentials(true)
                    .max_age(Duration::from_secs(3600))
            }
        }
        _ => {
            tracing::warn!(
                "FORGEKEEP_CORS_ORIGINS not set — CORS allows all origins (not recommended for production)"
            );
            CorsLayer::new()
                .allow_origin(tower_http::cors::AllowOrigin::mirror_request())
                .allow_methods(methods)
                .allow_headers(headers_list)
                .allow_credentials(true)
                .max_age(Duration::from_secs(3600))
        }
    }
}

/// Create the Axum router (Git + REST API + health).
///
/// `rate_limiter` is the global per-IP limiter applied to every request;
/// `auth_rate_limiter` is a separate, stricter limiter applied only to the
/// unauthenticated credential endpoints (`/users/register`, `/users/login`) to
/// blunt registration spam and password guessing independently of the global
/// limit (which is off by default).
pub(crate) fn create_router(
    state: AppState,
    rate_limiter: rate_limit::RateLimiter,
    auth_rate_limiter: rate_limit::RateLimiter,
) -> Router {
    build_router(state, rate_limiter, auth_rate_limiter)
}

/// Shared router builder used by both production and test routers.
///
/// CRITICAL: Axum `nest()` State requirement (pitfall #2)
///
/// All nested routers MUST share the same `State<AppState>` type.
/// If `git_routes` or `api_v1` use a different State type,
/// Axum will reject the route with a compile-time error.
///
/// Correct pattern (used here):
///   let git_routes = Router::new()...with_state(state.clone());
///   let api_v1 = Router::new()...with_state(state.clone());
///   Router::new().nest("/git", git_routes).nest("/api/v1", api_v1)
///
/// Wrong pattern (will not compile):
///   let git_routes = Router::new()...with_state(git_state);  // different type
///   let api_v1 = Router::new()...with_state(api_state);     // different type
///   Router::new().nest("/git", git_routes).nest("/api/v1", api_v1)  // ERROR
fn build_router(
    state: AppState,
    rate_limiter: rate_limit::RateLimiter,
    auth_rate_limiter: rate_limit::RateLimiter,
) -> Router {
    let (api_v1, git_routes) = build_routes(&state, Some(&auth_rate_limiter));
    let v2_routes = build_v2_routes(&state);
    let docs_routes = build_docs_routes(&state);

    Router::new()
        .nest("/git", git_routes)
        // ── Root-level Git Smart HTTP routes (standard git client format) ───
        // Git clients request /{owner}/{repo}.git/info/refs etc.
        // These must be at root level (no /git prefix) for compatibility.
        .route("/{owner}/{repo}/info/refs", get(git_http::handle_info_refs))
        .route(
            "/{owner}/{repo}/git-upload-pack",
            post(git_http::handle_git_upload_pack),
        )
        .route(
            "/{owner}/{repo}/git-receive-pack",
            post(git_http::handle_git_receive_pack),
        )
        .nest("/api/v1", api_v1)
        .nest("/v2", v2_routes)
        .route("/health", get(handlers::health))
        .route("/metrics", get(metrics::metrics_handler))
        .merge(docs_routes)
        // Serve SvelteKit static assets if the build directory exists
        .fallback_service(
            // SPA fallback: serve static assets, and for any unmatched path
            // (client-side routes like /login, /dashboard) return index.html
            // with a per-request CSP nonce injected into all <script> tags (H-2).
            ServeDir::new("web/build").fallback(get(handlers::spa_index_handler)),
        )
        // ── Middleware layers (order: bottom-up, last .layer() runs first) ──
        .layer(axum::middleware::from_fn(
            middleware::http_metrics_middleware,
        ))
        .layer(axum::middleware::from_fn(
            security::security_headers_middleware,
        ))
        .layer(axum::middleware::from_fn(middleware::request_id_middleware))
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &axum::http::Request<axum::body::Body>| {
                let request_id = request
                    .headers()
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-");
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    uri = %request.uri(),
                    status = tracing::field::Empty,
                    request_id = %request_id,
                )
            },
        ))
        .layer(build_cors_layer())
        .layer(axum::middleware::from_extractor::<
            axum::extract::ConnectInfo<std::net::SocketAddr>,
        >())
        .layer(axum::middleware::from_fn_with_state(
            rate_limiter.clone(),
            rate_limit::rate_limit_middleware,
        ))
        // Maintenance mode check (runs early, before most handlers)
        .layer(axum::middleware::from_fn(
            middleware::maintenance_middleware,
        ))
        .with_state(state)
}

/// Build OCI Distribution v2 routes (Docker/OCI container registry).
fn build_v2_routes(state: &AppState) -> Router<AppState> {
    // 10 GiB body limit for blob upload requests.
    let upload_body_limit = RequestBodyLimitLayer::new(10 * 1024 * 1024 * 1024);

    // Upload sub-router with body size limit.
    //
    // The OCI distribution spec starts every blob push at
    // `POST /v2/<name>/blobs/uploads/` — **with** the trailing slash (endpoint
    // end-4a), and that is what docker/podman/containerd actually send. Under
    // `nest`, axum 0.8 matches the inner `"/"` route at the prefix *without* a
    // trailing slash and 404s the spec form, so both spellings are registered
    // explicitly. There is no path-normalizing layer in front of the router to
    // paper over the difference.
    let upload_routes = Router::new()
        .route("/{owner}/{repo}/blobs/uploads", post(oci::start_upload))
        .route("/{owner}/{repo}/blobs/uploads/", post(oci::start_upload))
        .route(
            "/{owner}/{repo}/blobs/uploads/{uuid}",
            patch(oci::chunk_upload).put(oci::complete_upload),
        )
        .layer(upload_body_limit);

    Router::new()
        // API version check
        .route("/", get(oci::api_version_check))
        // Token authentication
        .route("/auth/token", get(oci::get_token))
        // Tags
        .route("/{owner}/{repo}/tags/list", get(oci::list_tags))
        // Manifests
        .route(
            "/{owner}/{repo}/manifests/{reference}",
            get(oci::get_manifest)
                .head(oci::head_manifest)
                .put(oci::put_manifest),
        )
        // Blobs
        .route(
            "/{owner}/{repo}/blobs/{digest}",
            get(oci::get_blob).head(oci::head_blob),
        )
        // Uploads (with body size limit)
        .merge(upload_routes)
        .with_state(state.clone())
}

/// Build API docs routes with authentication required.
fn build_docs_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/api-docs/openapi.json", get(handlers::openapi_handler))
        .route("/api-docs", get(handlers::swagger_ui_root_handler))
        .route("/api-docs/", get(handlers::swagger_ui_root_handler))
        .route("/api-docs/{*tail}", get(handlers::swagger_ui_handler))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            pat_auth::docs_auth_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            pat_auth::pat_auth_middleware,
        ))
        .with_state(state.clone())
}

/// Build route definitions (shared between production and test routers).
///
/// `auth_rate_limiter` is layered only onto the unauthenticated credential
/// endpoints (`/users/register`, `/users/login`). The test router passes
/// `None` so those routes carry no extra layer: the limiter middleware extracts
/// `ConnectInfo`, which the test harness (plain `oneshot`, no
/// `into_make_service_with_connect_info`) does not provide.
fn build_routes(
    state: &AppState,
    auth_rate_limiter: Option<&rate_limit::RateLimiter>,
) -> (Router<AppState>, Router<AppState>) {
    // Stricter per-route limiter for the credential endpoints. Applied via a
    // per-route `.layer()` (same mechanism the OCI upload body-limit uses),
    // keyed by the same client-IP resolution as the global limiter. `layer()`
    // returns the same `MethodRouter<AppState>` type in both arms, so the
    // attach-or-not choice stays type-consistent.
    let apply_auth_rl = |mr: MethodRouter<AppState>| -> MethodRouter<AppState> {
        match auth_rate_limiter {
            Some(limiter) => mr.layer(axum::middleware::from_fn_with_state(
                limiter.clone(),
                rate_limit::rate_limit_middleware,
            )),
            None => mr,
        }
    };
    // ── Git Smart HTTP routes ──────────────────────────────────────────────
    let git_routes = Router::new()
        .route("/{owner}/{repo}/info/refs", get(git_http::handle_info_refs))
        .route(
            "/{owner}/{repo}/git-upload-pack",
            post(git_http::handle_git_upload_pack),
        )
        .route(
            "/{owner}/{repo}/git-receive-pack",
            post(git_http::handle_git_receive_pack),
        );

    // Runner routes that require authentication (single middleware layer)
    let runners_auth = Router::new()
        .route("/runners/{id}/heartbeat", post(api::runners::heartbeat))
        .route("/runners/{id}/deregister", post(api::runners::deregister))
        .route("/runners/{id}/jobs/poll", get(api::runners::poll_job))
        .route(
            "/runners/{id}/jobs/{job_id}/start",
            post(api::runners::start_job),
        )
        .route(
            "/runners/{id}/jobs/{job_id}/log",
            post(api::runners::upload_log),
        )
        .route(
            "/runners/{id}/jobs/{job_id}/workspace",
            get(api::runners::download_workspace),
        )
        .route(
            "/runners/{id}/jobs/{job_id}/cache",
            get(api::runners::download_cache)
                .put(api::runners::upload_cache)
                .layer(RequestBodyLimitLayer::new(1024 * 1024 * 1024)),
        )
        .route(
            "/runners/{id}/jobs/{job_id}/finish",
            post(api::runners::finish_job),
        )
        .route(
            "/runners/{id}/jobs/{job_id}/artifacts",
            post(api::artifacts::upload_artifact),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api::runners::authenticate_runner,
        ))
        .with_state(state.clone());

    // ── REST API routes ───────────────────────────────────────────────────
    let api_v1 = Router::new()
        // Users
        .route(
            "/users/register",
            apply_auth_rl(post(api::users::register)),
        )
        .route("/users/login", apply_auth_rl(post(api::users::login)))
        .route("/users/logout", post(api::users::logout))
        .route("/users/me", get(api::users::me))
        .route("/users/forgot-password", post(api::users::forgot_password))
        .route("/users/reset-password", post(api::users::reset_password))
        // PAT
        .route(
            "/users/tokens",
            get(api::users::list_tokens).post(api::users::create_token),
        )
        .route("/users/tokens/{id}", delete(api::users::delete_token))
        // SSH keys
        .route(
            "/users/ssh-keys",
            get(api::ssh_keys::list_ssh_keys).post(api::ssh_keys::create_ssh_key),
        )
        .route(
            "/users/ssh-keys/{id}",
            delete(api::ssh_keys::delete_ssh_key),
        )
        .route(
            "/repos/{owner}/{name}/keys",
            get(api::deploy_keys::list_deploy_keys).post(api::deploy_keys::create_deploy_key),
        )
        .route(
            "/repos/{owner}/{name}/keys/{id}",
            delete(api::deploy_keys::delete_deploy_key),
        )
        // MFA
        .route("/users/mfa/setup", post(api::mfa::setup_mfa))
        .route("/users/mfa/enable", post(api::mfa::enable_mfa))
        .route("/users/mfa/verify", post(api::mfa::verify_mfa))
        .route("/users/mfa/backup", get(api::mfa::get_backup_codes))
        .route("/users/mfa/disable", post(api::mfa::disable_mfa))
        // Passkeys (WebAuthn)
        .route(
            "/users/passkeys",
            get(api::passkeys::list_passkeys),
        )
        .route(
            "/users/passkeys/{id}",
            delete(api::passkeys::delete_passkey),
        )
        .route(
            "/users/passkeys/register/start",
            post(api::passkeys::register_start),
        )
        .route(
            "/users/passkeys/register/finish",
            post(api::passkeys::register_finish),
        )
        .route(
            "/users/passkeys/login/start",
            post(api::passkeys::login_start),
        )
        .route(
            "/users/passkeys/login/finish",
            post(api::passkeys::login_finish),
        )
        // SSO
        .route("/auth/sso/providers", get(api::sso::list_providers))
        .route("/auth/sso/{slug}", get(api::sso::authorize))
        .route("/auth/sso/{slug}/callback", get(api::sso::callback))
        .route("/auth/sso/{slug}/refresh", post(api::sso::refresh_token))
        .route(
            "/auth/sso/{slug}/unlink",
            delete(api::sso::unlink_oauth_account),
        )
        // Repos
        .route("/repos", post(api::repos::create_repo))
        // Template listing & explore (must be before /repos/{owner} to avoid route conflict)
        .route(
            "/repos/templates/gitignores",
            get(api::repos::list_gitignore_templates),
        )
        .route(
            "/repos/templates/licenses",
            get(api::repos::list_license_templates),
        )
        .route(
            "/repos/templates/readmes",
            get(api::repos::list_readme_templates),
        )
        .route("/repos/templates/labels", get(api::repos::list_label_sets))
        .route("/repos/explore", get(api::repos::explore))
        .route("/repos/{owner}", get(api::repos::list_repos))
        .route("/repos/{owner}/{name}", get(api::repos::get_repo))
        // Milestones (before issues to avoid routing conflicts)
        .route(
            "/repos/{owner}/{name}/milestones",
            get(api::issues::list_milestones).post(api::issues::create_milestone),
        )
        .route(
            "/repos/{owner}/{name}/milestones/{id}",
            get(api::issues::get_milestone)
                .patch(api::issues::update_milestone)
                .delete(api::issues::delete_milestone),
        )
        // Labels
        .route(
            "/repos/{owner}/{name}/labels",
            get(api::labels::list_labels).post(api::labels::create_label),
        )
        .route(
            "/repos/{owner}/{name}/labels/{id}",
            get(api::labels::get_label)
                .patch(api::labels::update_label)
                .delete(api::labels::delete_label),
        )
        // Issues
        .route(
            "/repos/{owner}/{name}/issue_templates",
            get(api::issues::list_issue_templates),
        )
        .route(
            "/repos/{owner}/{name}/issue_config",
            get(api::issues::get_issue_config),
        )
        .route(
            "/repos/{owner}/{name}/issue_config/validate",
            get(api::issues::validate_issue_config),
        )
        .route(
            "/repos/{owner}/{name}/pull_request_template",
            get(api::issues::get_pull_request_template),
        )
        .route(
            "/repos/{owner}/{name}/issues",
            get(api::issues::list_issues).post(api::issues::create_issue),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}",
            get(api::issues::get_issue).patch(api::issues::update_issue),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/labels",
            get(api::issues::get_issue_labels),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/comments",
            get(api::issues::list_comments).post(api::issues::add_comment),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/assets",
            get(api::attachments::list_issue_attachments)
                .post(api::attachments::create_issue_attachment)
                .layer(RequestBodyLimitLayer::new(101 * 1024 * 1024)),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}",
            get(api::attachments::get_issue_attachment)
                .delete(api::attachments::delete_issue_attachment),
        )
        .route(
            "/repos/{owner}/{name}/issues/comments/{comment_id}/assets",
            get(api::attachments::list_issue_comment_attachments)
                .post(api::attachments::create_issue_comment_attachment)
                .layer(RequestBodyLimitLayer::new(101 * 1024 * 1024)),
        )
        .route(
            "/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}",
            get(api::attachments::get_issue_comment_attachment)
                .delete(api::attachments::delete_issue_comment_attachment),
        )
        // Pull Requests
        .route(
            "/repos/{owner}/{name}/pulls",
            get(api::pulls::list_prs).post(api::pulls::create_pr),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}",
            get(api::pulls::get_pr).patch(api::pulls::update_pr),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/assets",
            get(api::attachments::list_pull_request_attachments)
                .post(api::attachments::create_pull_request_attachment)
                .layer(RequestBodyLimitLayer::new(101 * 1024 * 1024)),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}",
            get(api::attachments::get_pull_request_attachment)
                .delete(api::attachments::delete_pull_request_attachment),
        )
        .route(
            "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets",
            get(api::attachments::list_review_comment_attachments)
                .post(api::attachments::create_review_comment_attachment)
                .layer(RequestBodyLimitLayer::new(101 * 1024 * 1024)),
        )
        .route(
            "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}",
            get(api::attachments::get_review_comment_attachment)
                .delete(api::attachments::delete_review_comment_attachment),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/diff",
            get(api::pulls::get_diff),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/merge",
            post(api::pulls::merge_pr),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/auto-merge",
            put(api::pulls::enable_auto_merge).delete(api::pulls::disable_auto_merge),
        )
        .route(
            "/repos/{owner}/{name}/merge-queue",
            get(api::pulls::list_merge_queue),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/merge-queue",
            put(api::pulls::enqueue_merge_queue).delete(api::pulls::cancel_merge_queue),
        )
        // PR Reviews
        .route(
            "/repos/{owner}/{name}/pulls/{number}/reviews",
            get(api::reviews::list_reviews).post(api::reviews::submit_review),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/reviews/{id}",
            get(api::reviews::get_review),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/reviews/{id}/dismiss",
            post(api::reviews::dismiss_review),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/comments",
            get(api::reviews::list_review_comments).post(api::reviews::create_review_comment),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/timeline",
            get(api::reviews::get_review_timeline),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution",
            patch(api::reviews::set_thread_resolution),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply",
            post(api::reviews::apply_review_suggestion),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/suggestions/apply",
            post(api::reviews::apply_review_suggestions),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/reviewers",
            get(api::reviews::list_requested_reviewers).post(api::reviews::request_reviewer),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/reviewers/{username}",
            delete(api::reviews::remove_requested_reviewer),
        )
        // Wiki
        .route(
            "/repos/{owner}/{name}/wiki",
            get(api::wiki::list_pages).post(api::wiki::create_page),
        )
        .route(
            "/repos/{owner}/{name}/wiki/{title}",
            get(api::wiki::get_page)
                .patch(api::wiki::update_page)
                .delete(api::wiki::delete_page),
        )
        .route(
            "/repos/{owner}/{name}/wiki/{title}/history",
            get(api::wiki::list_revisions),
        )
        .route(
            "/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}",
            get(api::wiki::get_revision),
        )
        // LFS (body size limit for object uploads: 10 GiB)
        .route(
            "/repos/{owner}/{name}/lfs/objects/batch",
            post(api::lfs::batch),
        )
        .route(
            "/repos/{owner}/{name}/lfs/objects/{oid}",
            get(api::lfs::download_object)
                .put(api::lfs::upload_object)
                .layer(RequestBodyLimitLayer::new(10 * 1024 * 1024 * 1024)),
        )
        // Webhooks
        .route(
            "/repos/{owner}/{name}/hooks",
            get(api::webhooks::list_webhooks).post(api::webhooks::create_webhook),
        )
        .route(
            "/repos/{owner}/{name}/hooks/{id}",
            get(api::webhooks::get_webhook)
                .patch(api::webhooks::update_webhook)
                .delete(api::webhooks::delete_webhook),
        )
        .route(
            "/repos/{owner}/{name}/hooks/{id}/deliveries",
            get(api::webhooks::list_deliveries),
        )
        .route(
            "/repos/{owner}/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver",
            post(api::webhooks::redeliver),
        )
        // CI/CD Pipelines
        .route(
            "/repos/{owner}/{name}/pipelines",
            get(api::ci::list_pipelines).post(api::ci::trigger_pipeline),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{id}",
            get(api::ci::get_pipeline),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{id}/retry",
            post(api::ci::retry_pipeline),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{id}/cancel",
            post(api::ci::cancel_pipeline),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}",
            get(api::ci::get_job),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}/play",
            post(api::ci::play_job),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{pipeline_id}/jobs/{job_id}/approve",
            post(api::ci_environments::approve),
        )
        .route(
            "/repos/{owner}/{name}/actions/environments",
            get(api::ci_environments::list).post(api::ci_environments::create),
        )
        .route(
            "/repos/{owner}/{name}/actions/environments/{id}",
            axum::routing::put(api::ci_environments::update).delete(api::ci_environments::delete),
        )
        .route(
            "/ci/oidc/.well-known/openid-configuration",
            get(api::ci_oidc::discovery),
        )
        .route("/ci/oidc/jwks", get(api::ci_oidc::jwks))
        .route("/ci/oidc/token", get(api::ci_oidc::token))
        .route(
            "/repos/{owner}/{name}/actions/retention",
            get(api::ci_retention::get_policy).put(api::ci_retention::update_policy),
        )
        .route(
            "/repos/{owner}/{name}/actions/retention/expired",
            axum::routing::delete(api::ci_retention::cleanup),
        )
        .route(
            "/repos/{owner}/{name}/actions/secrets",
            get(api::ci_secrets::list),
        )
        .route(
            "/repos/{owner}/{name}/actions/secrets/{secret_name}",
            axum::routing::put(api::ci_secrets::put).delete(api::ci_secrets::delete),
        )
        // Repository archive download
        .route(
            "/repos/{owner}/{name}/archive/{archive}",
            get(api::archive::download_archive),
        )
        // Branch Protection
        .route(
            "/repos/{owner}/{name}/branches/protection",
            get(api::branch_protection::list_protections)
                .post(api::branch_protection::create_protection),
        )
        .route(
            "/repos/{owner}/{name}/branches/protection/{id}",
            get(api::branch_protection::get_protection)
                .patch(api::branch_protection::update_protection)
                .delete(api::branch_protection::delete_protection),
        )
        .route(
            "/repos/{owner}/{name}/tags/protection",
            get(api::tag_protection::list).post(api::tag_protection::create),
        )
        .route(
            "/repos/{owner}/{name}/tags/protection/{id}",
            patch(api::tag_protection::update).delete(api::tag_protection::delete),
        )
        // Collaborators
        .route(
            "/repos/{owner}/{name}/collaborators",
            get(api::collaborators::list_collaborators).post(api::collaborators::add_collaborator),
        )
        .route(
            "/repos/{owner}/{name}/collaborators/{id}",
            patch(api::collaborators::update_permission)
                .delete(api::collaborators::remove_collaborator),
        )
        // Repo Content Browsing
        .route(
            "/repos/{owner}/{name}/tree",
            get(api::repo_content::list_tree),
        )
        .route(
            "/repos/{owner}/{name}/blob/{*path}",
            get(api::repo_content::get_blob),
        )
        .route("/repos/{owner}/{name}/log", get(api::repo_content::get_log))
        .route(
            "/repos/{owner}/{name}/branches",
            get(api::repo_content::list_branches),
        )
        .route(
            "/repos/{owner}/{name}/tags",
            get(api::repo_content::list_tags),
        )
        // GPG Signatures
        .route(
            "/repos/{owner}/{name}/commits/{sha}/signature",
            get(api::repo_content::get_commit_signature),
        )
        // File creation/update/deletion
        .route(
            "/repos/{owner}/{name}/contents/{*path}",
            post(api::repo_content::create_or_update_file).delete(api::repo_content::delete_file),
        )
        // Mirror
        .route(
            "/repos/{owner}/{name}/mirror",
            get(api::mirrors::get_mirror)
                .post(api::mirrors::create_mirror)
                .patch(api::mirrors::update_mirror)
                .delete(api::mirrors::delete_mirror),
        )
        .route(
            "/repos/{owner}/{name}/mirror/sync",
            post(api::mirrors::trigger_mirror_sync),
        )
        // Imports (GitHub/GitLab migration)
        .route(
            "/imports",
            post(api::imports::start_import).get(api::imports::list_imports),
        )
        .route(
            "/imports/{id}",
            get(api::imports::get_import_status).delete(api::imports::delete_import),
        )
        // Project Boards
        .route(
            "/repos/{owner}/{name}/boards",
            get(api::boards::list_boards).post(api::boards::create_board),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}",
            get(api::boards::get_board)
                .patch(api::boards::update_board)
                .delete(api::boards::delete_board),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/columns",
            post(api::boards::create_column),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/columns/{col_id}",
            patch(api::boards::update_column).delete(api::boards::delete_column),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/columns/{col_id}/cards",
            post(api::boards::create_card),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/cards/{card_id}",
            patch(api::boards::update_card).delete(api::boards::delete_card),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/cards/{card_id}/move",
            post(api::boards::move_card),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/cards/reorder",
            post(api::boards::reorder_cards),
        )
        // Time Tracking
        .route(
            "/repos/{owner}/{name}/issues/{number}/time",
            get(api::time_tracking::list_time_entries).post(api::time_tracking::add_time),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/time/total",
            get(api::time_tracking::total_time),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/time/{id}",
            delete(api::time_tracking::delete_time_entry),
        )
        // Commit Statuses
        .route(
            "/repos/{owner}/{name}/statuses/{sha}",
            post(api::repos::create_commit_status),
        )
        .route(
            "/repos/{owner}/{name}/commits/{sha}/statuses",
            get(api::repos::list_commit_statuses),
        )
        .route(
            "/repos/{owner}/{name}/commits/{sha}/status",
            get(api::repos::get_combined_status),
        )
        // Organizations
        .route(
            "/orgs",
            get(api::orgs::list_orgs).post(api::orgs::create_org),
        )
        .route(
            "/orgs/{name}",
            get(api::orgs::get_org)
                .patch(api::orgs::update_org)
                .delete(api::orgs::delete_org),
        )
        .route(
            "/orgs/{name}/members",
            get(api::orgs::list_org_members).post(api::orgs::add_org_member),
        )
        .route(
            "/orgs/{name}/members/{user_id}",
            delete(api::orgs::remove_org_member),
        )
        .route(
            "/orgs/{name}/teams",
            get(api::orgs::list_org_teams).post(api::orgs::create_team),
        )
        .route(
            "/orgs/{name}/teams/{team_id}",
            get(api::orgs::get_team).delete(api::orgs::delete_team),
        )
        .route(
            "/orgs/{name}/teams/{team_id}/members",
            get(api::orgs::list_team_members).post(api::orgs::add_team_member),
        )
        .route(
            "/orgs/{name}/teams/{team_id}/members/{user_id}",
            delete(api::orgs::remove_team_member),
        )
        // Notifications
        .route(
            "/notifications",
            get(api::notifications::list_notifications),
        )
        .route(
            "/notifications/unread-count",
            get(api::notifications::unread_count),
        )
        .route(
            "/notifications/mark-all-read",
            post(api::notifications::mark_all_read),
        )
        .route(
            "/notifications/{id}/read",
            post(api::notifications::mark_read),
        )
        .route(
            "/notifications/{id}",
            delete(api::notifications::delete_notification),
        )
        // Star/Watch
        .route("/repos/{owner}/{name}/star", put(api::repos::star_repo))
        .route(
            "/repos/{owner}/{name}/starred",
            get(api::repos::get_starred_status),
        )
        .route(
            "/repos/{owner}/{name}/stargazers",
            get(api::repos::get_stargazers),
        )
        .route(
            "/repos/{owner}/{name}/watch",
            get(api::repos::get_watch_status)
                .put(api::repos::watch_repo)
                .delete(api::repos::unwatch_repo),
        )
        // Repo Delete (combined with GET)
        .route(
            "/repos/{owner}/{name}",
            delete(api::repos::delete_repo_handler),
        )
        // Releases
        .route(
            "/repos/{owner}/{name}/releases",
            get(api::releases::list_releases).post(api::releases::create_release),
        )
        .route(
            "/repos/{owner}/{name}/releases/{id}",
            get(api::releases::get_release)
                .patch(api::releases::update_release)
                .delete(api::releases::delete_release),
        )
        // Release Assets
        .route(
            "/repos/{owner}/{name}/releases/{release_id}/assets",
            get(api::releases::list_assets).post(api::releases::upload_asset),
        )
        .route(
            "/repos/{owner}/{name}/releases/assets/{asset_id}",
            get(api::releases::get_asset).delete(api::releases::delete_asset),
        )
        .route(
            "/repos/{owner}/{name}/releases/assets/{asset_id}/download",
            get(api::releases::download_asset),
        )
        .route(
            "/repos/{owner}/{name}/releases/assets/{asset_id}/attestation",
            post(api::releases::sign_asset_attestation)
                .get(api::releases::get_asset_attestation),
        )
        .route(
            "/repos/{owner}/{name}/releases/assets/{asset_id}/attestation/verify",
            post(api::releases::verify_asset_attestation),
        )
        // Fork
        .route(
            "/repos/{owner}/{name}/fork",
            post(api::repos::fork_repo_handler),
        )
        .route(
            "/repos/{owner}/{name}/forks",
            get(api::repos::list_forks_handler),
        )
        // Transfer
        .route(
            "/repos/{owner}/{name}/transfer",
            post(api::repos::transfer_repo_handler),
        )
        // Package Registry — protocol-specific routes first (before generic catch-all)
        .route(
            "/repos/{owner}/{name}/packages",
            get(api::packages::list_registries),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/publish",
            post(api::packages::publish),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/list",
            get(api::packages::list_packages),
        )
        // Cargo sparse index protocol
        .route(
            "/repos/{owner}/{name}/packages/cargo/index/{pkg}",
            get(api::packages::cargo_sparse_index),
        )
        // npm registry protocol
        .route(
            "/repos/{owner}/{name}/packages/npm/publish",
            post(api::packages::publish_npm),
        )
        .route(
            "/repos/{owner}/{name}/packages/npm/list",
            get(api::packages::list_npm_packages),
        )
        .route(
            "/repos/{owner}/{name}/packages/npm/{pkg_name}",
            get(api::packages::npm_registry_metadata),
        )
        // PyPI Simple Repository API (PEP 503)
        .route(
            "/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}",
            get(api::packages::pypi_simple_index),
        )
        // Maven metadata endpoint
        .route(
            "/repos/{owner}/{name}/packages/maven/{group_id}/{artifact_id}/maven-metadata.xml",
            get(api::packages::maven_metadata),
        )
        // NuGet protocol endpoints
        .route(
            "/repos/{owner}/{name}/packages/nuget/index.json",
            get(api::packages::nuget_service_index),
        )
        .route(
            "/repos/{owner}/{name}/packages/nuget/registration/{id}/index.json",
            get(api::packages::nuget_registration_index),
        )
        .route(
            "/repos/{owner}/{name}/packages/nuget/query",
            get(api::packages::nuget_search),
        )
        // RubyGems protocol endpoints
        .route(
            "/repos/{owner}/{name}/packages/rubygems/api/v1/dependencies",
            get(api::packages::rubygems_dependencies),
        )
        .route(
            "/repos/{owner}/{name}/packages/rubygems/api/v1/gems/{gem_name}",
            get(api::packages::rubygems_gem_info),
        )
        // Helm repository index
        .route(
            "/repos/{owner}/{name}/packages/helm/index.yaml",
            get(api::packages::helm_index),
        )
        // Composer packages.json
        .route(
            "/repos/{owner}/{name}/packages/composer/packages.json",
            get(api::packages::composer_packages_json),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}",
            get(api::packages::get_package),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/versions",
            get(api::packages::list_versions),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}",
            get(api::packages::get_version).delete(api::packages::delete_version),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/yank",
            patch(api::packages::yank_version),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/{*file}",
            get(api::packages::download_file),
        )
        // CI/CD Runners
        .route("/runners/register", post(api::runners::register))
        .merge(runners_auth)
        .route(
            "/repos/{owner}/{name}/pipelines/{id}/artifacts",
            get(api::artifacts::list_pipeline_artifacts),
        )
        .route("/artifacts/{id}", get(api::artifacts::get_artifact))
        .route(
            "/artifacts/{id}/download",
            get(api::artifacts::download_artifact),
        )
        .route("/artifacts/{id}", delete(api::artifacts::delete_artifact))
        // Admin
        .route("/admin/runners", get(api::runners::list_runners_admin))
        .route(
            "/admin/runners/{id}",
            delete(api::runners::delete_runner_admin),
        )
        .route("/admin/users", get(api::admin::list_users))
        .route("/admin/users/{id}", get(api::admin::get_user))
        .route("/admin/users/{id}", patch(api::admin::update_user))
        .route("/admin/users/{id}", delete(api::admin::delete_user))
        .route("/admin/users/{id}/unlock", post(api::admin::unlock_user))
        .route("/admin/orgs", get(api::admin::list_orgs))
        .route("/admin/orgs/{name}", get(api::admin::get_org))
        .route("/admin/orgs/{name}", delete(api::admin::delete_org))
        // Admin SSO
        .route(
            "/admin/sso/providers",
            get(api::admin::list_sso_providers).post(api::admin::create_sso_provider),
        )
        .route(
            "/admin/sso/providers/{id}",
            get(api::admin::get_sso_provider)
                .patch(api::admin::update_sso_provider)
                .delete(api::admin::delete_sso_provider),
        )
        .route(
            "/admin/sso/providers/{id}/test",
            post(api::admin::test_sso_provider_connection),
        )
        // Audit logs (admin only)
        .route("/admin/audit/logs", get(api::audit::list_audit_logs))
        .route("/admin/audit/logs/{id}", get(api::audit::get_audit_log))
        .route(
            "/admin/login-attempts",
            get(api::audit::list_login_attempts),
        )
        // Admin instance settings
        .route(
            "/admin/settings",
            get(api::admin::get_settings).patch(api::admin::update_settings),
        )
        // Global Search
        .route("/search", get(api::search::search))
        // External CI/CD Webhook
        .route(
            "/repos/{owner}/{name}/webhooks/external/ci",
            post(api::webhooks_external::external_ci_webhook),
        )
        // ── AI Agent endpoints ─────────────────────────────
        .route(
            "/ai/repos/{owner}/{name}/summary",
            get(api::ai::ai_repo_summary),
        )
        .route(
            "/ai/repos/{owner}/{name}/issues",
            get(api::ai::ai_list_issues),
        )
        .route("/ai/repos/{owner}/{name}/prs", get(api::ai::ai_list_prs))
        .route("/ai/repos/{owner}/{name}/tree", get(api::ai::ai_repo_tree))
        .route(
            "/ai/repos/{owner}/{name}/search/code",
            get(api::ai::ai_search_code),
        )
        // .route("/ai/repos/{owner}/{name}/index", post(api::ai::ai_index_repository))  // Temporarily disabled: Axum Handler trait issue, using a CLI command instead
        // WebSocket
        .route("/ws/notifications", get(ws::ws_notifications_handler))
        .route("/ws/job/{job_id}", get(ws::ws_job_log_handler));

    // Accept Personal Access Tokens on the REST API by translating them to a
    // Bearer JWT before the (JWT-only) handlers run.
    let api_v1 = api_v1.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        pat_auth::pat_auth_middleware,
    ));

    (api_v1, git_routes)
}

/// Create the Axum router for testing (no rate limiter, no static file serving).
pub(crate) fn build_test_router(state: AppState) -> Router {
    // No auth limiter in tests: the limiter middleware extracts ConnectInfo,
    // which the test harness does not supply. Passing None skips that layer.
    let (api_v1, git_routes) = build_routes(&state, None);
    let v2_routes = build_v2_routes(&state);
    let docs_routes = build_docs_routes(&state);

    Router::new()
        .nest("/git", git_routes)
        .route("/{owner}/{repo}/info/refs", get(git_http::handle_info_refs))
        .route(
            "/{owner}/{repo}/git-upload-pack",
            post(git_http::handle_git_upload_pack),
        )
        .route(
            "/{owner}/{repo}/git-receive-pack",
            post(git_http::handle_git_receive_pack),
        )
        .nest("/api/v1", api_v1)
        .nest("/v2", v2_routes)
        .merge(docs_routes)
        .route("/health", get(handlers::health))
        // ── Middleware layers (no rate limiter for tests) ──────────────────
        .layer(axum::middleware::from_fn(middleware::request_id_middleware))
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &axum::http::Request<axum::body::Body>| {
                let request_id = request
                    .headers()
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-");
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    uri = %request.uri(),
                    status = tracing::field::Empty,
                    request_id = %request_id,
                )
            },
        ))
        .layer(build_cors_layer())
        .with_state(state)
}
