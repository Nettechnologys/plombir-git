//! OpenAPI (Swagger) documentation for Plombir Git REST API.
//!
//! Provides auto-generated OpenAPI 3.0 spec via utoipa.
//! Access at:
//!   - OpenAPI JSON: GET /api-docs/openapi.json
//!   - Swagger UI:    GET /api-docs/

use std::collections::HashMap;
use std::sync::Arc;

use utoipa::openapi::path::{Operation, PathItem};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::route_table::{Access, RouteFact};

/// Where the documented paths are mounted, spelled once more where code can
/// read it: the `servers(...)` entry of `#[openapi(...)]` is a literal the macro
/// consumes, and [`stamp_security`] has to resolve an annotation's prefix-less
/// path back to the route table row that carries the prefix.
///
/// The two spellings agreeing is not left to a reading:
/// `openapi-route-coverage-contract-check.mjs` asserts the `servers(...)` entry
/// is `/api/v1`, and `openapi_security_guard` asserts the served document
/// declares the same string this constant holds — a divergence would make every
/// lookup below miss, which the same guard fails on.
pub const API_SERVER_PREFIX: &str = "/api/v1";

/// The scheme a Plombir Git session satisfies.
///
/// A Personal Access Token satisfies it too: `pat_auth::pat_auth_middleware`
/// translates a PAT into the session JWT the handlers read, on the way in.
pub const SESSION_SCHEME: &str = "bearerAuth";

/// The scheme a route that checks its own credential requires — the levels the
/// route table signs off as [`Access::Foreign`].
pub const FOREIGN_SCHEME: &str = "foreignToken";

/// Paginated response wrapper for repository listing.
#[derive(utoipa::ToSchema)]
pub struct PaginatedRepoResponse {
    pub data: Vec<crate::api::repos::RepoResponse>,
    pub pagination: crate::pagination::PaginationMeta,
}

/// Paginated response wrapper for the public explore listing.
#[derive(utoipa::ToSchema)]
pub struct PaginatedExploreRepoResponse {
    pub data: Vec<crate::api::repos::ExploreRepoResponse>,
    pub pagination: crate::pagination::PaginationMeta,
}

/// Paginated response wrapper for repository stargazers.
#[derive(utoipa::ToSchema)]
pub struct PaginatedStargazerResponse {
    pub data: Vec<crate::api::repos::StargazerResponse>,
    pub pagination: crate::pagination::PaginationMeta,
}

/// Paginated response wrapper for repository forks.
#[derive(utoipa::ToSchema)]
pub struct PaginatedForkResponse {
    pub data: Vec<crate::api::repos::ForkResponse>,
    pub pagination: crate::pagination::PaginationMeta,
}

/// Publishes the two credentials this API takes.
///
/// `utoipa` derives nothing about authentication on its own, so without this the
/// document declared no `securitySchemes` at all: Swagger UI had no "Authorize"
/// button, a generated client had no field to put a token in, and
/// `scripts/openapi-interface-smoke.mjs` — which decides from the document
/// whether to send one — replayed every protected endpoint anonymously and
/// stopped at the 401 wall (card_018b2dd39652).
///
/// The schemes are the static half. Which operation requires which one is the
/// derived half, and it is not written here: see [`stamp_security`].
pub struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            SESSION_SCHEME,
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .description(Some(
                        "A Plombir Git session token, or a Personal Access Token — the API accepts \
                         either, because a PAT is translated into a session token on the way in. \
                         Obtain one from `POST /users/login` or `POST /users/tokens`.",
                    ))
                    .build(),
            ),
        );
        components.add_security_scheme(
            FOREIGN_SCHEME,
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some(
                        "A credential the endpoint checks itself instead of going through the \
                         session gate: a CI runner token, a CI job token, an LFS action token. A \
                         session token is **not** accepted on these — the operation's description \
                         names the one it wants.",
                    ))
                    .build(),
            ),
        );
    }
}

/// What [`stamp_security`] did, so a caller can report it instead of assuming.
#[derive(Debug, Default)]
pub struct SecurityStamp {
    /// Operations that now demand a credential.
    pub required: usize,
    /// Operations that accept a credential without demanding one — the levels
    /// whose answer depends on whether the resource is public.
    pub optional: usize,
    /// Operations left anonymous, because the route they name is public.
    pub anonymous: usize,
    /// `"GET /foo"` for every operation no route table row matched. Published as
    /// requiring a session — the safe direction — and reported here, because an
    /// unresolved operation means the document and the router disagree about a
    /// URL and the level below it is a guess.
    pub unresolved: Vec<String>,
}

impl SecurityStamp {
    /// Operations carrying a `security` list — the number the smoke counts back.
    pub fn declared(&self) -> usize {
        self.required + self.optional
    }
}

/// The `security` requirement naming one scheme and no scopes.
fn requirement(scheme: &str) -> SecurityRequirement {
    SecurityRequirement::new(scheme, Vec::<String>::new())
}

/// Every operation of a `PathItem`, paired with its HTTP method.
///
/// `PathItem` keeps one field per verb rather than a map, so this is the only
/// place the eight of them are enumerated; a verb missing here would be an
/// operation silently published without security.
fn operations_mut(item: &mut PathItem) -> Vec<(&'static str, &mut Operation)> {
    let mut out: Vec<(&'static str, &mut Operation)> = Vec::new();
    if let Some(op) = item.get.as_mut() {
        out.push(("GET", op));
    }
    if let Some(op) = item.put.as_mut() {
        out.push(("PUT", op));
    }
    if let Some(op) = item.post.as_mut() {
        out.push(("POST", op));
    }
    if let Some(op) = item.delete.as_mut() {
        out.push(("DELETE", op));
    }
    if let Some(op) = item.options.as_mut() {
        out.push(("OPTIONS", op));
    }
    if let Some(op) = item.head.as_mut() {
        out.push(("HEAD", op));
    }
    if let Some(op) = item.patch.as_mut() {
        out.push(("PATCH", op));
    }
    if let Some(op) = item.trace.as_mut() {
        out.push(("TRACE", op));
    }
    out
}

/// Give every operation the `security` its route declares.
///
/// The access level is already stated exactly once, in the `RouteTable` row that
/// registers the route (`crate::route_table::Access`). Writing
/// `security(("bearerAuth" = []))` into 287 `#[utoipa::path]` annotations would
/// have made a third copy of a decision that already exists twice; this derives
/// it from the one that the persona sweep, the gate-rank guard and the contract
/// checks all read, so the document cannot drift away from the router without
/// the route table moving first.
///
/// Public levels are left with no `security` key at all — that is what "anyone
/// may call this" means to a client, and `Access::PublicFiltered` is included
/// because its filtering is a property of the *answer*, not a demand on the
/// caller.
pub(crate) fn stamp_security(
    doc: &mut utoipa::openapi::OpenApi,
    facts: &[RouteFact],
) -> SecurityStamp {
    let by_route: HashMap<(&str, &str), Access> = facts
        .iter()
        .map(|fact| ((fact.method, fact.path.as_str()), fact.access))
        .collect();

    let mut stamp = SecurityStamp::default();
    for (path, item) in doc.paths.paths.iter_mut() {
        let mounted = format!("{API_SERVER_PREFIX}{path}");
        for (method, operation) in operations_mut(item) {
            match by_route.get(&(method, mounted.as_str())).copied() {
                Some(access) if access.is_public() => {
                    operation.security = None;
                    stamp.anonymous += 1;
                }
                // Anonymous for a public repository or organization, not for a
                // private one. An empty requirement alongside the named one is
                // how OpenAPI spells "optional": a client may send a token, and
                // what it can see depends on whether it did.
                Some(Access::RepoRead | Access::OrgRead) => {
                    operation.security = Some(vec![
                        SecurityRequirement::default(),
                        requirement(SESSION_SCHEME),
                    ]);
                    stamp.optional += 1;
                }
                Some(Access::Foreign(_)) => {
                    operation.security = Some(vec![requirement(FOREIGN_SCHEME)]);
                    stamp.required += 1;
                }
                Some(_) => {
                    operation.security = Some(vec![requirement(SESSION_SCHEME)]);
                    stamp.required += 1;
                }
                None => {
                    operation.security = Some(vec![requirement(SESSION_SCHEME)]);
                    stamp.required += 1;
                    stamp.unresolved.push(format!("{method} {path}"));
                }
            }
        }
    }
    stamp
}

/// Plombir Git API — OpenAPI specification.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Plombir Git API",
        version = "0.1.0",
        description = "Plombir Git is a self-hosted Git platform written in Rust. \
            This API provides repository management, issue tracking, pull requests, \
            CI/CD pipelines, wiki, LFS, webhooks, and more.",
    ),
    // Where the paths below are actually served.
    //
    // Every `#[utoipa::path(path = "…")]` string is published verbatim, and
    // they are written relative to the router's nest prefix — `"/repos/{owner}"`
    // for a route mounted at `/api/v1/repos/{owner}` by
    // `RouteTable::new("/api/v1")` in `routes::build_all_routes`. Without this
    // declaration the document said nothing about that prefix, so every
    // consumer resolved the paths against the document's own origin: a
    // generated client, Swagger UI's "Try it out", and
    // `scripts/openapi-interface-smoke.mjs` all requested `/repos/…`, which no
    // route claims and the SPA fallback answers with `index.html`
    // (card_b23fa617838f).
    //
    // A relative server URL is resolved by the consumer against the document's
    // origin (OpenAPI 3.0 §4.7.5), which is what makes one spelling work for
    // localhost, the deploy behind a reverse proxy, and the smoke alike.
    //
    // This is the ONE place the prefix is written on the spec side, and
    // `scripts/openapi-route-coverage-contract-check.mjs` enforces that: an
    // annotation that spells `/api/v1` itself would now resolve to
    // `/api/v1/api/v1/…` and fails that gate.
    servers(
        (url = "/api/v1", description = "The REST API, relative to the server's own origin"),
    ),
    // Publishes `components.securitySchemes`. Which operation requires which
    // scheme is not written here and not written in the annotations either — it
    // is derived from the route table by `stamp_security`, which `spec_json`
    // runs before the document is served.
    modifiers(&SecurityAddon),
    paths(
        // Users
        crate::api::users::register,
        crate::api::users::login,
        crate::api::users::logout,
        crate::api::users::me,
        crate::api::users::list_tokens,
        crate::api::users::create_token,
        crate::api::users::delete_token,
        // Bot accounts
        crate::api::bots::list_bots,
        crate::api::bots::create_bot,
        crate::api::bots::delete_bot,
        crate::api::bots::list_bot_tokens,
        crate::api::bots::create_bot_token,
        crate::api::bots::delete_bot_token,
        // MCP over HTTP
        crate::api::mcp::mcp_endpoint,
        crate::api::ssh_keys::list_ssh_keys,
        crate::api::ssh_keys::create_ssh_key,
        crate::api::ssh_keys::delete_ssh_key,
        crate::api::deploy_keys::list_deploy_keys,
        crate::api::deploy_keys::create_deploy_key,
        crate::api::deploy_keys::delete_deploy_key,
        crate::api::users::forgot_password,
        crate::api::users::reset_password,
        // Account self-service
        crate::api::account::change_password,
        crate::api::account::set_initial_password,
        crate::api::account::update_profile,
        crate::api::account::request_email_change,
        crate::api::account::confirm_email,
        crate::api::account::delete_account,
        crate::api::account::upload_avatar,
        crate::api::account::delete_avatar,
        crate::api::account::get_avatar,
        // MFA
        crate::api::mfa::setup_mfa,
        crate::api::mfa::enable_mfa,
        crate::api::mfa::verify_mfa,
        crate::api::mfa::get_backup_codes,
        crate::api::mfa::regenerate_backup_codes,
        crate::api::mfa::disable_mfa,
        // Passkeys
        crate::api::passkeys::list_passkeys,
        crate::api::passkeys::delete_passkey,
        crate::api::passkeys::register_start,
        crate::api::passkeys::register_finish,
        crate::api::passkeys::login_start,
        crate::api::passkeys::login_finish,
        // SSO
        crate::api::sso::list_providers,
        crate::api::sso::authorize,
        crate::api::sso::callback,
        crate::api::sso::start_link,
        crate::api::sso::unlink_oauth_account,
        crate::api::sso::list_my_links,
        // Repositories
        crate::api::repos::create_repo,
        crate::api::repos::list_repos,
        crate::api::repos::get_repo,
        crate::api::repos::delete_repo_handler,
        crate::api::repos::star_repo,
        crate::api::repos::get_starred_status,
        crate::api::repos::get_stargazers,
        crate::api::repos::get_watch_status,
        crate::api::repos::watch_repo,
        crate::api::repos::unwatch_repo,
        crate::api::repos::fork_repo_handler,
        crate::api::repos::list_forks_handler,
        crate::api::repos::transfer_repo_handler,
        crate::api::repos::update_repo_handler,
        crate::api::repos::create_commit_status,
        crate::api::repos::list_commit_statuses,
        crate::api::repos::get_combined_status,
        crate::api::repos::explore,
        crate::api::repos::list_gitignore_templates,
        crate::api::repos::list_license_templates,
        crate::api::repos::list_readme_templates,
        crate::api::repos::list_label_sets,
        // Archive
        crate::api::archive::download_archive,
        // External CI
        crate::api::webhooks_external::external_ci_webhook,
        // Issues
        crate::api::issues::list_issues,
        crate::api::issues::get_issue,
        crate::api::issues::create_issue,
        crate::api::issues::update_issue,
        crate::api::issues::list_comments,
        crate::api::issues::add_comment,
        crate::api::issues::delete_issue,
        crate::api::issues::delete_comment,
        crate::api::issues::edit_comment,
        crate::api::issues::list_milestones,
        crate::api::issues::create_milestone,
        crate::api::issues::get_milestone,
        crate::api::issues::update_milestone,
        crate::api::issues::delete_milestone,
        crate::api::issues::get_issue_labels,
        crate::api::issues::list_issue_templates,
        crate::api::issues::get_issue_config,
        crate::api::issues::validate_issue_config,
        crate::api::issues::get_pull_request_template,
        // Attachments
        crate::api::attachments::list_issue_attachments,
        crate::api::attachments::create_issue_attachment,
        crate::api::attachments::get_issue_attachment,
        crate::api::attachments::delete_issue_attachment,
        crate::api::attachments::list_pull_request_attachments,
        crate::api::attachments::create_pull_request_attachment,
        crate::api::attachments::get_pull_request_attachment,
        crate::api::attachments::delete_pull_request_attachment,
        crate::api::attachments::list_issue_comment_attachments,
        crate::api::attachments::create_issue_comment_attachment,
        crate::api::attachments::get_issue_comment_attachment,
        crate::api::attachments::delete_issue_comment_attachment,
        crate::api::attachments::list_review_comment_attachments,
        crate::api::attachments::create_review_comment_attachment,
        crate::api::attachments::get_review_comment_attachment,
        crate::api::attachments::delete_review_comment_attachment,
        // Labels
        crate::api::labels::list_labels,
        crate::api::labels::get_label,
        crate::api::labels::create_label,
        crate::api::labels::update_label,
        crate::api::labels::delete_label,
        // Pull Requests
        crate::api::pulls::list_prs,
        crate::api::pulls::get_pr,
        crate::api::pulls::create_pr,
        crate::api::pulls::update_pr,
        crate::api::pulls::get_diff,
        crate::api::pulls::compare,
        crate::api::pulls::merge_pr,
        crate::api::pulls::approve_pr_ci,
        crate::api::pulls::enable_auto_merge,
        crate::api::pulls::disable_auto_merge,
        crate::api::pulls::list_merge_queue,
        crate::api::pulls::enqueue_merge_queue,
        crate::api::pulls::cancel_merge_queue,
        // Reviews
        crate::api::reviews::list_reviews,
        crate::api::reviews::submit_review,
        crate::api::reviews::get_review,
        crate::api::reviews::dismiss_review,
        crate::api::reviews::list_review_comments,
        crate::api::reviews::get_review_timeline,
        crate::api::reviews::create_review_comment,
        crate::api::reviews::delete_review_comment,
        crate::api::reviews::edit_review_comment,
        crate::api::reviews::set_thread_resolution,
        crate::api::reviews::apply_review_suggestion,
        crate::api::reviews::apply_review_suggestions,
        crate::api::reviews::list_requested_reviewers,
        crate::api::reviews::request_reviewer,
        crate::api::reviews::remove_requested_reviewer,
        // Wiki
        crate::api::wiki::list_pages,
        crate::api::wiki::get_page,
        crate::api::wiki::create_page,
        crate::api::wiki::update_page,
        crate::api::wiki::delete_page,
        crate::api::wiki::list_revisions,
        crate::api::wiki::get_revision,
        // LFS
        crate::api::lfs::batch,
        crate::api::lfs::upload_object,
        crate::api::lfs::download_object,
        crate::api::lfs_locks::create_lock,
        crate::api::lfs_locks::list_locks,
        crate::api::lfs_locks::verify_locks,
        crate::api::lfs_locks::unlock,
        crate::api::lfs_storage::usage,
        crate::api::lfs_storage::list_objects,
        crate::api::lfs_storage::list_orphans,
        crate::api::lfs_storage::prune,
        // Webhooks
        crate::api::webhooks::list_webhooks,
        crate::api::webhooks::create_webhook,
        crate::api::webhooks::get_webhook,
        crate::api::webhooks::update_webhook,
        crate::api::webhooks::delete_webhook,
        crate::api::webhooks::list_deliveries,
        crate::api::webhooks::redeliver,
        // CI/CD
        crate::api::ci::list_pipelines,
        crate::api::ci::get_pipeline,
        crate::api::ci::get_job,
        crate::api::ci::play_job,
        crate::api::ci_environments::list,
        crate::api::ci_environments::create,
        crate::api::ci_environments::update,
        crate::api::ci_environments::delete,
        crate::api::ci_environments::approve,
        crate::api::ci_oidc::discovery,
        crate::api::ci_oidc::jwks,
        crate::api::ci_oidc::token,
        crate::api::ci_retention::get_policy,
        crate::api::ci_retention::update_policy,
        crate::api::ci_retention::cleanup,
        crate::api::ci::trigger_pipeline,
        crate::api::ci::get_workflow_dispatch_schema,
        crate::api::ci::retry_pipeline,
        crate::api::ci::cancel_pipeline,
        // Releases
        crate::api::releases::list_releases,
        crate::api::releases::create_release,
        crate::api::releases::get_release,
        crate::api::releases::update_release,
        crate::api::releases::delete_release,
        crate::api::releases::list_assets,
        crate::api::releases::upload_asset,
        crate::api::releases::get_asset,
        crate::api::releases::download_asset,
        crate::api::releases::delete_asset,
        crate::api::releases::sign_asset_attestation,
        crate::api::releases::get_asset_attestation,
        crate::api::releases::verify_asset_attestation,
        // Organizations
        crate::api::orgs::create_org,
        crate::api::orgs::get_org,
        crate::api::orgs::list_orgs,
        crate::api::orgs::update_org,
        crate::api::orgs::delete_org,
        crate::api::orgs::list_org_members,
        crate::api::orgs::add_org_member,
        crate::api::orgs::remove_org_member,
        crate::api::orgs::create_team,
        crate::api::orgs::list_org_teams,
        crate::api::orgs::get_team,
        crate::api::orgs::delete_team,
        crate::api::orgs::list_team_members,
        crate::api::orgs::add_team_member,
        crate::api::orgs::remove_team_member,
        // Notifications
        crate::api::notifications::list_notifications,
        crate::api::notifications::unread_count,
        crate::api::notifications::mark_read,
        crate::api::notifications::mark_all_read,
        crate::api::notifications::delete_notification,
        crate::api::notifications::get_notification_settings,
        crate::api::notifications::update_notification_settings,
        crate::api::notifications::get_issue_subscription,
        crate::api::notifications::subscribe_issue,
        crate::api::notifications::unsubscribe_issue,
        crate::api::notifications::get_pull_subscription,
        crate::api::notifications::subscribe_pull,
        crate::api::notifications::unsubscribe_pull,
        // Search
        crate::api::search::search,
        // Branch Protection
        crate::api::branch_protection::list_protections,
        crate::api::branch_protection::create_protection,
        crate::api::branch_protection::get_protection,
        crate::api::branch_protection::update_protection,
        crate::api::branch_protection::delete_protection,
        crate::api::tag_protection::list,
        crate::api::tag_protection::create,
        crate::api::tag_protection::update,
        crate::api::tag_protection::delete,
        crate::api::ci_secrets::list,
        crate::api::ci_secrets::put,
        crate::api::ci_secrets::delete,
        // Collaborators
        crate::api::collaborators::list_collaborators,
        crate::api::collaborators::add_collaborator,
        crate::api::collaborators::update_permission,
        crate::api::collaborators::remove_collaborator,
        // Repository Content
        crate::api::repo_content::list_tree,
        crate::api::repo_content::get_blob,
        crate::api::repo_content::get_raw,
        crate::api::repo_content::get_log,
        crate::api::repo_content::list_branches,
        crate::api::repo_content::list_tags,
        crate::api::repo_content::create_branch,
        crate::api::repo_content::delete_branch,
        crate::api::repo_content::delete_tag,
        crate::api::repo_content::get_commit_signature,
        crate::api::repo_content::create_or_update_file,
        crate::api::repo_content::delete_file,
        // Package registry
        crate::api::packages::publish,
        crate::api::packages::list_registries,
        crate::api::packages::list_packages,
        crate::api::packages::get_package,
        crate::api::packages::list_versions,
        crate::api::packages::get_version,
        crate::api::packages::delete_version,
        crate::api::packages::yank_version,
        crate::api::packages::download_file,
        crate::api::packages::publish_npm,
        crate::api::packages::publish_npm_packument,
        crate::api::packages::npm_attestations,
        crate::api::packages::npm_dist_tags,
        crate::api::packages::set_npm_dist_tag,
        crate::api::packages::delete_npm_dist_tag,
        crate::api::packages::pypi_legacy_upload,
        crate::api::packages::list_npm_packages,
        // Imports
        crate::api::imports::start_import,
        crate::api::imports::list_imports,
        crate::api::imports::get_import_status,
        crate::api::imports::delete_import,
        // Runners
        crate::api::runners::register,
        crate::api::runners::heartbeat,
        crate::api::runners::poll_job,
        crate::api::runners::start_job,
        crate::api::runners::upload_log,
        crate::api::runners::finish_job,
        crate::api::runners::list_runners_admin,
        crate::api::runners::get_runner_admin,
        crate::api::runners::delete_runner_admin,
        crate::api::runners::deregister,
        crate::api::runners::download_workspace,
        crate::api::runners::download_cache,
        crate::api::runners::upload_cache,
        // Artifacts
        crate::api::artifacts::stage_artifact,
        crate::api::artifacts::upload_artifact,
        crate::api::artifacts::list_pipeline_artifacts,
        crate::api::artifacts::get_artifact,
        crate::api::artifacts::download_artifact,
        crate::api::artifacts::delete_artifact,
        // Admin SSO
        crate::api::admin::list_sso_providers,
        crate::api::admin::get_sso_provider,
        crate::api::admin::create_sso_provider,
        crate::api::admin::update_sso_provider,
        crate::api::admin::delete_sso_provider,
        crate::api::admin::test_sso_provider_connection,
        // Audit logs
        crate::api::audit::list_audit_logs,
        crate::api::audit::get_audit_log,
        crate::api::audit::list_login_attempts,
        // Admin
        crate::api::admin::list_users,
        crate::api::admin::get_user,
        crate::api::admin::update_user,
        crate::api::admin::delete_user,
        crate::api::admin::create_user,
        crate::api::admin::reset_user_password,
        crate::api::admin::unlock_user,
        crate::api::admin::list_orgs,
        crate::api::admin::get_org,
        crate::api::admin::delete_org,
        // Admin settings
        crate::api::admin::get_settings,
        crate::api::admin::update_settings,
        // The public half of them
        crate::api::instance::get_instance,
        // AI Agent endpoints
        crate::api::ai::ai_repo_summary,
        crate::api::ai::ai_list_issues,
        crate::api::ai::ai_list_prs,
        crate::api::ai::ai_repo_tree,
        crate::api::ai::ai_search_code,
        crate::api::ai::ai_index_repository,
        // Mirrors
        crate::api::mirrors::create_mirror,
        crate::api::mirrors::get_mirror,
        crate::api::mirrors::update_mirror,
        crate::api::mirrors::delete_mirror,
        crate::api::mirrors::trigger_mirror_sync,
        // Boards
        crate::api::boards::create_board,
        crate::api::boards::list_boards,
        crate::api::boards::get_board,
        crate::api::boards::update_board,
        crate::api::boards::delete_board,
        crate::api::boards::create_column,
        crate::api::boards::update_column,
        crate::api::boards::delete_column,
        crate::api::boards::create_card,
        crate::api::boards::update_card,
        crate::api::boards::move_card,
        crate::api::boards::reorder_cards,
        crate::api::boards::delete_card,
        // Time Tracking
        crate::api::time_tracking::add_time,
        crate::api::time_tracking::list_time_entries,
        crate::api::time_tracking::total_time,
        crate::api::time_tracking::delete_time_entry,
    ),
    components(
        schemas(
            crate::api::instance::InstanceInfo,
            crate::api::users::RegisterRequest,
            crate::api::users::LoginRequest,
            crate::api::users::AuthResponse,
            crate::api::users::UserProfile,
            crate::api::users::CreateTokenRequest,
            crate::api::bots::TokenNarrowing,
            crate::api::bots::TokenNarrowingResponse,
            crate::api::bots::BotResponse,
            crate::api::bots::CreateBotRequest,
            crate::api::bots::CreateBotTokenRequest,
            crate::api::bots::BotTokenResponse,
            crate::api::ssh_keys::CreateSshKeyRequest,
            crate::api::ssh_keys::SshKeyResponse,
            crate::api::deploy_keys::CreateDeployKeyRequest,
            crate::api::deploy_keys::DeployKeyResponse,
            crate::api::users::ForgotPasswordRequest,
            crate::api::users::ResetPasswordRequest,
            crate::api::account::ChangePasswordRequest,
            crate::api::account::InitialPasswordRequest,
            crate::api::account::UpdateProfileRequest,
            crate::api::account::ChangeEmailRequest,
            crate::api::account::ConfirmEmailRequest,
            crate::api::account::DeleteAccountRequest,
            crate::api::admin::CreateUserRequest,
            crate::api::mfa::SetupMfaResponse,
            crate::api::mfa::EnableMfaRequest,
            crate::api::mfa::EnableMfaResponse,
            crate::api::mfa::VerifyMfaRequest,
            crate::api::mfa::VerifyMfaResponse,
            crate::api::mfa::DisableMfaRequest,
            crate::api::mfa::RegenerateBackupCodesRequest,
            crate::api::mfa::RegenerateBackupCodesResponse,
            crate::api::ci::TriggerPipelineRequest,
            crate::api::passkeys::PasskeyInfo,
            crate::api::passkeys::PasskeyRegisterStartResponse,
            crate::api::passkeys::PasskeyCreationOptions,
            crate::api::passkeys::PasskeyRelyingParty,
            crate::api::passkeys::PasskeyUserEntity,
            crate::api::passkeys::PasskeyCredentialParameter,
            crate::api::passkeys::PasskeyCredentialDescriptor,
            crate::api::passkeys::PasskeyAuthenticatorSelection,
            crate::api::passkeys::RegisterFinishRequest,
            crate::api::passkeys::PasskeyRegistrationCredential,
            crate::api::passkeys::PasskeyAttestationResponse,
            crate::api::passkeys::LoginStartRequest,
            crate::api::passkeys::PasskeyLoginStartResponse,
            crate::api::passkeys::PasskeyRequestOptions,
            crate::api::passkeys::PasskeyAuthenticationCredential,
            crate::api::passkeys::PasskeyAssertionResponse,
            crate::api::passkeys::PasskeyLoginResponse,
            crate::api::sso::SsoProviderInfo,
            crate::api::sso::SsoLinkInfo,
            crate::api::sso::SsoLinkStart,
            crate::api::sso::LoginResponse,
            crate::api::webhooks_external::ExternalCiWebhook,
            crate::api::webhooks_external::ExternalCiResponse,
            crate::api::audit::AuditLogEntry,
            crate::api::audit::AuditLogResponse,
            crate::api::audit::LoginAttemptEntry,
            crate::api::audit::LoginAttemptResponse,
            crate::api::repos::CreateRepoRequest,
            crate::api::repos::ForkRequest,
            crate::api::repos::RepoResponse,
            crate::api::repos::WatchRequest,
            crate::api::repos::TransferRequest,
            crate::api::repos::CreateCommitStatusRequest,
            crate::pagination::PaginationParams,
            crate::pagination::PaginationMeta,
            PaginatedRepoResponse,
            crate::api::runners::RegisterRunnerRequest,
            crate::api::runners::RegisterRunnerResponse,
            crate::api::runners::HeartbeatResponse,
            crate::api::runners::PollJobResponse,
            crate::api::runners::RunnerInfoResponse,
            crate::api::runners::FinishJobRequest,
            crate::api::ci_secrets::PutSecretRequest,
            crate::api::ci_secrets::SecretResponse,
            crate::api::tag_protection::CreateTagProtectionRequest,
            crate::api::tag_protection::UpdateTagProtectionRequest,
            crate::api::tag_protection::TagProtectionResponse,
            crate::api::artifacts::ArtifactResponse,
            crate::api::artifacts::UploadArtifactResponse,
            crate::api::artifacts::UploadArtifactRequest,
            crate::api::ai::RepoSummary,
            crate::api::ai::IssueSummary,
            crate::api::ai::PrSummary,
            crate::api::mirrors::CreateMirrorRequest,
            crate::api::mirrors::UpdateMirrorRequest,
            crate::api::mirrors::MirrorResponse,
            crate::api::webhooks::WebhookResponse,
            crate::api::boards::CreateBoardRequest,
            crate::api::boards::UpdateBoardRequest,
            crate::api::boards::CreateColumnRequest,
            crate::api::boards::UpdateColumnRequest,
            crate::api::boards::CreateCardRequest,
            crate::api::boards::UpdateCardRequest,
            crate::api::boards::MoveCardRequest,
            crate::api::boards::ReorderCardsRequest,
            crate::api::time_tracking::AddTimeRequest,
            crate::api::imports::StartImportRequest,
            crate::api::packages::YankRequest,
            crate::api::packages::NpmPublishPackument,
            crate::api::packages::NpmPublishVersion,
            crate::api::packages::NpmPublishAttachment,
            crate::api::ci::WorkflowDispatchSchemaResponse,
            crate::api::ci::WorkflowDispatchWorkflowResponse,
            crate::api::ci::WorkflowDispatchInputResponse,
        )
    ),
    tags(
        (name = "Users", description = "User registration, authentication, and profile"),
        (name = "Repositories", description = "Repository CRUD and management"),
        (name = "Issues", description = "Issue tracking"),
        (name = "Labels", description = "Label management"),
        (name = "Pull Requests", description = "Pull request workflow"),
        (name = "Reviews", description = "Code review"),
        (name = "Wiki", description = "Wiki pages"),
        (name = "LFS", description = "Git Large File Storage"),
        (name = "Webhooks", description = "Webhook management"),
        (name = "CI/CD", description = "Continuous Integration and Delivery"),
        (name = "Releases", description = "Release management"),
        (name = "Organizations", description = "Organization management"),
        (name = "Notifications", description = "User notifications"),
        (name = "Search", description = "Full-text search"),
        (name = "Branch Protection", description = "Branch protection rules"),
        (name = "Tag Protection", description = "Protected tag patterns"),
        (name = "Collaborators", description = "Repository collaborators"),
        (name = "Repository Content", description = "Browse repository files"),
        (name = "Runners", description = "CI/CD runner management"),
        (name = "Artifacts", description = "CI/CD artifacts"),
        (name = "Admin", description = "Administration"),
        (name = "AI", description = "AI-agent endpoints providing AI-friendly repository/Issue/PR data"),
        (name = "Mirrors", description = "Repository mirroring"),
        (name = "Boards", description = "Project boards (Kanban)"),
        (name = "Time Tracking", description = "Issue time tracking"),
        (name = "Imports", description = "Repository migration imports"),
        (name = "SSO", description = "Single Sign-On and OAuth authentication"),
        (name = "MFA", description = "Multi-Factor Authentication"),
        (name = "Passkeys", description = "WebAuthn passkey registration and passwordless login"),
        (name = "Audit", description = "Audit logs"),
        (name = "Packages", description = "Package registry"),
        (name = "MCP", description = "Model Context Protocol over HTTP: the agent tools, served in-process"),
    )
)]
pub struct ApiDoc;

/// The document as it is published, and what deriving its `security` found.
///
/// Built once, when the router is built, because that is the one moment the
/// route table exists: `routes::build_docs_routes` hands the facts in and puts
/// the result behind the `/api-docs/openapi.json` handler. There is no
/// facts-free spelling of this on purpose — one existed, and a document served
/// without the access levels is exactly the defect this pair fixes.
pub(crate) fn spec_json(facts: &[RouteFact]) -> (String, SecurityStamp) {
    let mut doc = ApiDoc::openapi();
    let stamp = stamp_security(&mut doc, facts);
    // A failure here is deterministic — the document is built from compile-time
    // data — so it is a fault of this binary, not of a request, and it surfaces
    // at startup rather than as an empty spec served forever with a 200.
    let json = doc
        .to_pretty_json()
        .expect("the OpenAPI document must serialize");
    (json, stamp)
}

/// Lazy-initialized Swagger UI config (avoids re-computing on every request).
pub fn swagger_config() -> Arc<utoipa_swagger_ui::Config<'static>> {
    Arc::new(utoipa_swagger_ui::Config::from("/api-docs/openapi.json"))
}
