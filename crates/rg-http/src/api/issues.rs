//! REST API handlers for Issues and Issue Comments.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::api::repo_access::{self, RepoAuthRead, RepoRead, RepoWrite};
use crate::api::user_ref::UserRef;
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::AppState;

// ── Request / Response types ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateIssueRequest {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub milestone_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct UpdateIssueRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    // `null` clears the field; an absent key leaves it alone. See
    // `crate::api::clearable` for why the attribute is load-bearing.
    #[serde(default, deserialize_with = "crate::api::clearable::double_option")]
    pub assignee_id: Option<Option<i64>>,
    /// The same field, named the way a person knows the human: a username, an
    /// e-mail, or the id as a string.
    ///
    /// `assignee_id` alone was a dead end for anything without the web app's
    /// dropdown — an API client, and the MCP agent this instance ships for,
    /// would have to obtain a number, and this instance publishes no endpoint
    /// that turns a name into one (card_e6e65ef58404). `null` clears the
    /// assignee here exactly as it does through `assignee_id`.
    #[serde(default, deserialize_with = "crate::api::clearable::double_option")]
    pub assignee: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::api::clearable::double_option")]
    pub milestone_id: Option<Option<i64>>,
}

#[derive(Deserialize)]
pub struct CreateCommentRequest {
    pub body: String,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    pub state: Option<String>,
    #[serde(default)]
    pub labels: Option<String>,
    #[serde(flatten)]
    #[param(ignore)]
    pub pagination: PaginationParams,
}

#[derive(Serialize)]
pub struct IssueResponse {
    #[serde(flatten)]
    pub issue: rg_core::issue::IssueWithLabels,
    pub author: Option<String>,
    /// The assignee's username, so a client can show who an issue is on
    /// without a lookup endpoint it does not have. `None` both when the issue
    /// is unassigned and when the assigned account no longer resolves — the
    /// number stays in `assignee_id` either way.
    pub assignee: Option<String>,
}

/// List valid Markdown issue templates from the repository default branch.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issue_templates",
    tag = "Issues",
    responses(
        (status = 200, description = "Gitea-compatible issue templates", body = serde_json::Value),
        (status = 401, description = "Authentication required", body = serde_json::Value),
    ),
)]
pub async fn list_issue_templates(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    RepoRead { repo: repo_model }: RepoRead,
) -> impl IntoResponse {
    let path = state.repo_root.join(format!("{owner}/{repo}.git"));
    let default_branch = repo_model.default_branch;
    match tokio::task::spawn_blocking(move || {
        rg_core::issue_template::discover_issue_templates(&path, &default_branch)
    })
    .await
    {
        Ok(Ok(discovery)) => {
            for (file, error) in discovery.errors {
                tracing::warn!(%file, %error, "ignored invalid issue template");
            }
            (StatusCode::OK, Json(discovery.templates)).into_response()
        }
        Ok(Err(error)) => AppError::from(error).into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

/// Read `.gitea`/`.github` issue chooser configuration.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issue_config",
    tag = "Issues",
    responses((status = 200, body = serde_json::Value)),
)]
pub async fn get_issue_config(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    RepoRead { repo: repo_model }: RepoRead,
) -> impl IntoResponse {
    let path = state.repo_root.join(format!("{owner}/{repo}.git"));
    let default_branch = repo_model.default_branch;
    match tokio::task::spawn_blocking(move || {
        rg_core::issue_template::read_issue_config(&path, &default_branch)
    })
    .await
    {
        Ok(Ok(config)) => (StatusCode::OK, Json(config)).into_response(),
        Ok(Err(error)) => AppError::from(error).into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[derive(Serialize)]
pub struct IssueConfigValidation {
    valid: bool,
    message: String,
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issue_config/validate",
    tag = "Issues",
    responses((status = 200, body = serde_json::Value)),
)]
pub async fn validate_issue_config(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    RepoRead { repo: repo_model }: RepoRead,
) -> impl IntoResponse {
    let path = state.repo_root.join(format!("{owner}/{repo}.git"));
    let default_branch = repo_model.default_branch;
    match tokio::task::spawn_blocking(move || {
        rg_core::issue_template::read_issue_config(&path, &default_branch)
    })
    .await
    {
        Ok(Ok(_)) => Json(IssueConfigValidation {
            valid: true,
            message: String::new(),
        })
        .into_response(),
        // Telling the caller *why* the config is invalid is this endpoint's
        // entire job, so it must not drop the inner cause (card_a997f30c142c).
        // Storage failures carry no InvalidRequest marker and must stay 5xx
        // rather than masquerading as a bad repository-owned config.
        Ok(Err(error))
            if error
                .downcast_ref::<rg_core::error::InvalidRequest>()
                .is_some() =>
        {
            Json(IssueConfigValidation {
                valid: false,
                message: format!("{error:#}"),
            })
            .into_response()
        }
        Ok(Err(error)) => AppError::from(error).into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pull_request_template",
    tag = "Pull Requests",
    responses(
        (status = 200, body = serde_json::Value),
        (status = 204, description = "No pull request template"),
    ),
)]
pub async fn get_pull_request_template(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    RepoRead { repo: repo_model }: RepoRead,
) -> impl IntoResponse {
    let path = state.repo_root.join(format!("{owner}/{repo}.git"));
    let default_branch = repo_model.default_branch;
    match tokio::task::spawn_blocking(move || {
        rg_core::issue_template::read_pull_request_template(&path, &default_branch)
    })
    .await
    {
        Ok(Ok(Some(template))) => (StatusCode::OK, Json(template)).into_response(),
        Ok(Ok(None)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(error)) => AppError::from(error).into_response(),
        Err(error) => AppError::internal(error).into_response(),
    }
}

#[derive(Serialize)]
pub struct CommentResponse {
    #[serde(flatten)]
    pub comment: rg_db::entities::issue_comment::Model,
    pub author: Option<String>,
}

// ── Issue handlers ──────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ListQuery,
        PaginationParams,
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_issues(
    State(state): State<AppState>,
    RepoRead { .. }: RepoRead,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<ListQuery>,
) -> impl IntoResponse {
    let state_filter = params.state.as_deref();
    let pagination = params.pagination.clamp();

    // If labels filter is present, use filtered query
    if let Some(ref labels_str) = params.labels {
        let label_names: Vec<String> = labels_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !label_names.is_empty() {
            return match rg_core::issue::list_issues_filtered_by_labels(
                &state.db,
                &owner,
                &repo,
                state_filter,
                &label_names,
                pagination.offset(),
                pagination.limit(),
            )
            .await
            {
                Ok((data, total)) => {
                    let data = match issues_with_authors(&state.db, data).await {
                        Ok(data) => data,
                        Err(error) => return error.into_response(),
                    };
                    (
                        StatusCode::OK,
                        Json(PaginatedResponse::new(data, &pagination, total as u64)),
                    )
                        .into_response()
                }
                Err(e) => AppError::from(e).into_response(),
            };
        }
    }

    match rg_core::issue::list_issues_paginated(
        &state.db,
        &owner,
        &repo,
        state_filter,
        pagination.offset(),
        pagination.limit(),
    )
    .await
    {
        Ok((data, total)) => {
            let data = match issues_with_authors(&state.db, data).await {
                Ok(data) => data,
                Err(error) => return error.into_response(),
            };
            (
                StatusCode::OK,
                Json(PaginatedResponse::new(data, &pagination, total as u64)),
            )
                .into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues/{number}",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_issue(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    match rg_core::issue::get_issue(&state.db, &owner, &repo, number).await {
        Ok(issue) => {
            let issue = match issue_with_author(&state.db, issue).await {
                Ok(issue) => issue,
                Err(error) => return error.into_response(),
            };
            (StatusCode::OK, Json(issue)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Resolve a milestone id against the repository the route was authorized for.
///
/// The id is a global `milestones` primary key wherever it arrives — in the
/// path (`/milestones/{id}`) and in the request body of `create_issue` /
/// `update_issue` alike — while the gate above it only ever proves something
/// about `{owner}/{name}`. Attaching an issue to a foreign milestone is a write
/// into someone else's repository: `count_open_by_milestone` then never reaches
/// zero there, so the victim's `notify_milestone_closed` never fires — and the
/// ids are guessable, private repositories included.
///
/// A mismatch answers 404 rather than 403: a 403 would still confirm that the
/// id exists, which is most of what an id-walking caller wants to learn.
///
/// The three `/milestones/{id}` routes each used to spell this comparison
/// inline. One named helper is the form `global_id_anchor_guard` can read, so a
/// fourth route that forgets it now fails the build rather than review.
async fn milestone_in_repo(
    state: &AppState,
    repo_id: i64,
    milestone_id: i64,
) -> Result<rg_db::entities::milestone::Model, AppError> {
    match rg_db::ops::milestone_ops::find_by_id(&state.db, milestone_id).await {
        Ok(Some(m)) if m.repo_id == repo_id => Ok(m),
        Ok(_) => Err(AppError::not_found("milestone not found".to_string())),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Resolve an `assignee_id` taken from a *request body* against the repository
/// the route was authorized for — the same sub-class as
/// [`require_milestone_in_repo`], and the last field on this route that was
/// still written unexamined.
///
/// The id is global and was going straight into `issue.assignee_id`, so the
/// row could name a user that does not exist, or one who cannot see the
/// repository they are now assigned in. The impact is narrower than the
/// milestone's — this is not a write into someone else's repository, and no
/// notification path reaches an assignee — but the result is a phantom name in
/// the UI and a foreign key pointing at nothing.
///
/// A user who cannot read the repository answers 404 rather than 403, for the
/// same reason the milestone does: a 403 would confirm the account exists.
async fn require_assignee_in_repo(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    assignee_id: i64,
) -> Result<(), AppError> {
    match rg_db::ops::user_ops::find_by_id(&state.db, assignee_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(AppError::not_found("user not found".to_string())),
        Err(e) => return Err(AppError::from(e)),
    }
    // The gate's predicate form, not `check_read_for`: this is not the caller
    // being let through, it is a *third party* being resolved, so the answer
    // has to be foldable into "no such assignee here" rather than becoming this
    // request's 401/403.
    match repo_access::may_read(state, repo, Some(assignee_id)).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(AppError::not_found("user not found".to_string())),
        Err(e) => Err(e),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/issues",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn create_issue(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAuthRead {
        repo: repo_model,
        actor_id: user_id,
    }: RepoAuthRead,
    Json(req): Json<CreateIssueRequest>,
) -> impl IntoResponse {
    // Filing an issue on read access is deliberate; deciding its labels and
    // milestone is not. `update_issue` already keeps those two behind
    // `can_write` — create let a reader of a public repository set them on the
    // way in, which is the same edit through a different door.
    if req.labels.is_some() || req.milestone_id.is_some() {
        match repo_access::may_write(&state, &repo_model, Some(user_id)).await {
            Ok(true) => {}
            Ok(false) => return AppError::forbidden("write access required").into_response(),
            Err(e) => return e.into_response(),
        }
    }

    if let Some(milestone_id) = req.milestone_id {
        if let Err(e) = milestone_in_repo(&state, repo_model.id, milestone_id).await {
            return e.into_response();
        }
    }

    match rg_core::issue::create_issue(
        &state.db,
        repo_model.id,
        user_id,
        req.title,
        req.body,
        req.labels,
        req.milestone_id,
    )
    .await
    {
        Ok(issue) => {
            // Counted by `issue::service::insert_with_repo_number`, not here:
            // the import subsystem files issues through the same allocator and
            // has no handler of its own, so a producer sitting on this branch
            // saw only the ones a human typed.
            let issue = match issue_with_author(&state.db, issue).await {
                Ok(issue) => issue,
                Err(error) => return error.into_response(),
            };
            (StatusCode::CREATED, Json(issue)).into_response()
        }
        // The service marks its one client-side outcome (an empty title) with
        // `InvalidRequest`; the blanket `bad_request` called a failed insert or a
        // dead connection a malformed request too, so nothing was ever retried.
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/issues/{number}",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
/// The route table declares this `RepoWrite` — that is what a stranger needs —
/// while the handler takes [`RepoAuthRead`]: an issue's own author may edit the
/// title and body of their issue without write access to the repository. The
/// declaration states the level owed to an arbitrary caller; the widening for
/// one specific person is decided below, against the same gate.
pub async fn update_issue(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoAuthRead {
        repo: repo_model,
        actor_id: user_id,
    }: RepoAuthRead,
    Json(req): Json<UpdateIssueRequest>,
) -> impl IntoResponse {
    let existing = match rg_core::issue::get_issue(&state.db, &owner, &repo, number).await {
        Ok(issue) => issue,
        Err(e) => return AppError::from(e).into_response(),
    };

    // Read access is already proven by the extractor. What is left is the
    // widening: write access, or authorship of this very issue and nothing
    // that only a writer may set.
    //
    // A permission check that could not run is not a permission check that
    // said "no": `unwrap_or(false)` answered 403 while the database was down,
    // sending the client off to re-issue a token that was never the problem.
    let can_write = match repo_access::may_write(&state, &repo_model, Some(user_id)).await {
        Ok(allowed) => allowed,
        Err(e) => return e.into_response(),
    };
    let touches_management_fields = req.labels.is_some()
        || req.assignee_id.is_some()
        || req.assignee.is_some()
        || req.milestone_id.is_some();

    if !can_write && (existing.author_id != user_id || touches_management_fields) {
        return AppError::forbidden("write access required").into_response();
    }

    if let Some(Some(milestone_id)) = req.milestone_id {
        if let Err(e) = milestone_in_repo(&state, repo_model.id, milestone_id).await {
            return e.into_response();
        }
    }

    // One assignee, named once. `assignee` and `assignee_id` mean the same
    // field, so a body carrying both is ambiguous the moment they disagree —
    // and picking a winner would make which one silently.
    let assignee_id = match (req.assignee_id, req.assignee) {
        (Some(_), Some(_)) => {
            return AppError::bad_request(
                "name the assignee once: send `assignee` or `assignee_id`, not both",
            )
            .into_response();
        }
        // A named assignee becomes the id the rest of this route already
        // handles. `null` clears, exactly as it does through `assignee_id`,
        // and needs no resolving.
        (None, Some(Some(identifier))) => {
            match UserRef::from_identifier(&identifier)
                .resolve(&state.db)
                .await
            {
                Ok(user) => Some(Some(user.id)),
                // The resolver's outcome is the caller's mistake, named: this
                // is the whole point of accepting the name in the first place.
                Err(e) => return AppError::from(e).into_response(),
            }
        }
        (None, Some(None)) => Some(None),
        (assignee_id, None) => assignee_id,
    };

    // `Some(None)` clears the assignee and needs no resolving — there is no id
    // to check.
    if let Some(Some(assignee_id)) = assignee_id {
        if let Err(e) = require_assignee_in_repo(&state, &repo_model, assignee_id).await {
            return e.into_response();
        }
    }

    // Capture the open→closed transition before `req.state` moves into the call
    // (idempotent re-close of an already-closed issue is not double-counted).
    let closing = req.state.as_deref() == Some("closed") && existing.state != "closed";

    match rg_core::issue::update_issue(
        &state.db,
        &owner,
        &repo,
        number,
        req.title,
        req.body,
        req.state,
        req.labels,
        assignee_id,
        req.milestone_id,
        Some(&state.delivery_tracker),
    )
    .await
    {
        Ok(issue) => {
            if closing {
                crate::metrics::recorder::issue_closed();
            }
            let issue = match issue_with_author(&state.db, issue).await {
                Ok(issue) => issue,
                Err(error) => return error.into_response(),
            };
            (StatusCode::OK, Json(issue)).into_response()
        }
        // An unknown issue is now the 404 the service reports, a rejected title
        // or state stays 400, and a failed update is finally a 5xx.
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Comment handlers ────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues/{number}/comments",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_comments(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    match rg_core::issue::list_comments(&state.db, &owner, &repo, number).await {
        Ok(comments) => {
            let comments = match comments_with_authors(&state.db, comments).await {
                Ok(comments) => comments,
                Err(error) => return error.into_response(),
            };
            (StatusCode::OK, Json(comments)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/issues/{number}/comments",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn add_comment(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoAuthRead {
        actor_id: user_id, ..
    }: RepoAuthRead,
    Json(req): Json<CreateCommentRequest>,
) -> impl IntoResponse {
    match rg_core::issue::add_comment(&state.db, &owner, &repo, number, user_id, req.body).await {
        Ok(comment) => {
            let comment = match comment_with_author(&state.db, comment).await {
                Ok(comment) => comment,
                Err(error) => return error.into_response(),
            };
            (StatusCode::CREATED, Json(comment)).into_response()
        }
        // Empty body → 400 (typed in the service), unknown issue → 404, failed
        // insert → 5xx. All three used to be 400.
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

async fn author_name(
    db: &sea_orm::DatabaseConnection,
    cache: &mut HashMap<i64, Option<String>>,
    user_id: i64,
) -> Result<Option<String>, AppError> {
    if let Some(cached) = cache.get(&user_id) {
        return Ok(cached.clone());
    }

    let name = rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .map_err(AppError::from)?
        .map(|user| user.username);
    cache.insert(user_id, name.clone());
    Ok(name)
}

/// The assignee's username, resolved through the same per-response cache as
/// the author's — an unassigned issue asks nothing of the database.
async fn assignee_name(
    db: &sea_orm::DatabaseConnection,
    cache: &mut HashMap<i64, Option<String>>,
    assignee_id: Option<i64>,
) -> Result<Option<String>, AppError> {
    match assignee_id {
        Some(assignee_id) => author_name(db, cache, assignee_id).await,
        None => Ok(None),
    }
}

async fn issue_with_author(
    db: &sea_orm::DatabaseConnection,
    issue: rg_db::entities::issue::Model,
) -> Result<IssueResponse, AppError> {
    let mut cache = HashMap::new();
    let author = author_name(db, &mut cache, issue.author_id).await?;
    let assignee = assignee_name(db, &mut cache, issue.assignee_id).await?;
    let issue = rg_core::issue::issue_with_labels(db, issue)
        .await
        .map_err(AppError::from)?;
    Ok(IssueResponse {
        issue,
        author,
        assignee,
    })
}

async fn issues_with_authors(
    db: &sea_orm::DatabaseConnection,
    issues: Vec<rg_db::entities::issue::Model>,
) -> Result<Vec<IssueResponse>, AppError> {
    let mut cache = HashMap::new();
    let mut responses = Vec::with_capacity(issues.len());
    let issues = rg_core::issue::issues_with_labels(db, issues)
        .await
        .map_err(AppError::from)?;
    for issue in issues {
        let author = author_name(db, &mut cache, issue.issue.author_id).await?;
        let assignee = assignee_name(db, &mut cache, issue.issue.assignee_id).await?;
        responses.push(IssueResponse {
            issue,
            author,
            assignee,
        });
    }
    Ok(responses)
}

async fn comment_with_author(
    db: &sea_orm::DatabaseConnection,
    comment: rg_db::entities::issue_comment::Model,
) -> Result<CommentResponse, AppError> {
    let mut cache = HashMap::new();
    let author = author_name(db, &mut cache, comment.author_id).await?;
    Ok(CommentResponse { comment, author })
}

async fn comments_with_authors(
    db: &sea_orm::DatabaseConnection,
    comments: Vec<rg_db::entities::issue_comment::Model>,
) -> Result<Vec<CommentResponse>, AppError> {
    let mut cache = HashMap::new();
    let mut responses = Vec::with_capacity(comments.len());
    for comment in comments {
        let author = author_name(db, &mut cache, comment.author_id).await?;
        responses.push(CommentResponse { comment, author });
    }
    Ok(responses)
}

// ── Milestone handlers ──────────────────────────────────────────────────

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListMilestonesQuery {
    pub state: Option<String>,
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/milestones",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ListMilestonesQuery,
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_milestones(
    State(state): State<AppState>,
    RepoRead { repo }: RepoRead,
    Path((_, _)): Path<(String, String)>,
    Query(params): Query<ListMilestonesQuery>,
) -> impl IntoResponse {
    match rg_db::ops::milestone_ops::list_by_repo(&state.db, repo.id, params.state.as_deref()).await
    {
        Ok(milestones) => (StatusCode::OK, Json(serde_json::json!(milestones))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

#[derive(Deserialize)]
pub struct CreateMilestoneRequest {
    pub title: String,
    pub description: Option<String>,
    pub due_date: Option<String>,
    pub state: Option<String>,
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/milestones",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn create_milestone(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<CreateMilestoneRequest>,
) -> impl IntoResponse {
    let now = chrono::Utc::now();
    // A state the listing cannot filter on makes the milestone unreachable:
    // `list_by_repo` compares `state` for equality, so `clsoed` answered `201`
    // and produced a row that shows up under neither tab (card_09b2665584ed).
    let milestone_state = match body.state.as_deref() {
        None => rg_core::issue::MilestoneState::Open,
        Some(state) => match rg_core::issue::MilestoneState::parse(state) {
            Ok(state) => state,
            Err(error) => return AppError::from(error).into_response(),
        },
    };
    let due_date = body
        .due_date
        .as_deref()
        .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let model = rg_db::entities::milestone::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: sea_orm::Set(repo.id),
        title: sea_orm::Set(body.title),
        description: sea_orm::Set(body.description),
        state: sea_orm::Set(milestone_state.as_str().to_string()),
        due_date: sea_orm::Set(due_date),
        created_at: sea_orm::Set(now),
        updated_at: sea_orm::Set(now),
    };
    match rg_db::ops::milestone_ops::create(&state.db, model).await {
        Ok(m) => (StatusCode::CREATED, Json(serde_json::json!(m))).into_response(),
        // Nothing about this call can fail on the caller's account — the request
        // was fully validated above and this is a plain insert. `bad_request`
        // here turned a dead connection pool into "your milestone is malformed".
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/milestones/{id}",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_milestone(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    // The access check above is about `owner/name`, so the milestone it guards
    // has to be the one that lives there. Without this, the route reads any
    // milestone id through whatever repo the caller can open.
    match milestone_in_repo(&state, repo.id, id).await {
        Ok(m) => (StatusCode::OK, Json(serde_json::json!(m))).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
pub struct UpdateMilestoneRequest {
    pub title: Option<String>,
    // `null` clears the field; an absent key leaves it alone. See
    // `crate::api::clearable` for why the attribute is load-bearing.
    #[serde(default, deserialize_with = "crate::api::clearable::double_option")]
    pub description: Option<Option<String>>,
    pub state: Option<String>,
    #[serde(default, deserialize_with = "crate::api::clearable::double_option")]
    pub due_date: Option<Option<String>>,
}

#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/milestones/{id}",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_milestone(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
    Json(body): Json<UpdateMilestoneRequest>,
) -> impl IntoResponse {
    // The write check the extractor ran was about `owner/name`, so the milestone it guards
    // has to be the one that lives there — matching `get_milestone`. Without the
    // `repo_id` comparison, write access to a single repository was enough to
    // edit the milestones of every other one, and a 403 on the mismatch would
    // still confirm that the id exists.
    let existing = match milestone_in_repo(&state, repo.id, id).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match update_milestone_after_read(&state.db, existing, body, || std::future::ready(Ok(())))
        .await
    {
        Ok(m) => (StatusCode::OK, Json(serde_json::json!(m))).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Testable boundary between the repository-scoped read and the conditional
/// milestone write.
async fn update_milestone_after_read<F, Fut>(
    db: &sea_orm::DatabaseConnection,
    existing: rg_db::entities::milestone::Model,
    body: UpdateMilestoneRequest,
    after_read: F,
) -> Result<rg_db::entities::milestone::Model, AppError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), AppError>>,
{
    let state = match body.state {
        Some(state) => {
            // There used to be no `else` here: an unrecognised state was
            // dropped on the floor and the response carried the milestone in
            // its old state under a `200 OK` (card_09b2665584ed).
            Some(
                rg_core::issue::MilestoneState::parse(&state)
                    .map_err(AppError::from)?
                    .as_str()
                    .to_string(),
            )
        }
        None => None,
    };
    let due_date = body.due_date.map(|date| {
        date.as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&chrono::Utc))
    });

    after_read().await?;

    rg_db::ops::milestone_ops::update(
        db,
        existing.id,
        body.title,
        body.description,
        state,
        due_date,
        chrono::Utc::now(),
    )
    .await
    .map_err(AppError::from)?
    .ok_or_else(|| AppError::not_found("milestone not found".to_string()))
}

#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/milestones/{id}",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Deleted", body = serde_json::Value),
        (status = 204, description = "No content"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "No such milestone, or it belongs to another repository",
         body = serde_json::Value),
    ),
)]
pub async fn delete_milestone(
    State(state): State<AppState>,
    Path((_, _, id)): Path<(String, String, i64)>,
    RepoWrite { repo, .. }: RepoWrite,
) -> impl IntoResponse {
    // `delete_by_id` deletes whatever row carries that id, so the milestone has
    // to be read and matched against the repository whose write access was
    // checked first — otherwise write access to one repository deleted the
    // milestones of any other, silently and without even a lookup.
    if let Err(e) = milestone_in_repo(&state, repo.id, id).await {
        return e.into_response();
    }
    // That lookup and this `DELETE` are two statements, so a concurrent delete
    // can land in between; the 204 therefore comes from `rows_affected` rather
    // than from the row having existed a moment ago.
    match rg_db::ops::milestone_ops::delete_by_id(&state.db, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => AppError::not_found("milestone not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Issue Labels handlers ───────────────────────────────────────────────

/// GET /api/v1/repos/:owner/:name/issues/:number/labels
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/issues/{number}/labels",
    tag = "Issues",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_issue_labels(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    // Same gate as `get_issue` / `list_comments`: the labels of an issue are as
    // private as the issue itself, and without `HeaderMap` this handler could
    // not tell an anonymous caller from the owner at all.

    match rg_core::issue::get_issue(&state.db, &owner, &repo, number).await {
        Ok(issue) => match rg_core::label::service::get_issue_labels(&state.db, issue.id).await {
            Ok(labels) => (StatusCode::OK, Json(serde_json::json!(labels))).into_response(),
            Err(e) => AppError::from(e).into_response(),
        },
        Err(e) => AppError::from(e).into_response(),
    }
}

#[cfg(test)]
mod author_enrichment_tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{ConnectOptions, ConnectionTrait, Database};

    async fn test_db() -> sea_orm::DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options).await.expect("connect test DB");
        rg_db::run_migrations(&db)
            .await
            .expect("run test migrations");
        db
    }

    fn issue(author_id: i64) -> rg_db::entities::issue::Model {
        let now = Utc::now();
        rg_db::entities::issue::Model {
            id: 1,
            repo_id: 1,
            number: 1,
            title: "enrichment boundary".to_string(),
            body: None,
            state: "open".to_string(),
            author_id,
            assignee_id: None,
            milestone_id: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
            deleted_at: None,
        }
    }

    fn comment(id: i64, author_id: i64) -> rg_db::entities::issue_comment::Model {
        let now = Utc::now();
        rg_db::entities::issue_comment::Model {
            id,
            issue_id: 1,
            author_id,
            body: format!("comment {id}"),
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn author_enrichment_keeps_existing_and_genuinely_missing_users_distinct() {
        let db = test_db().await;
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "enrichment-author",
            "enrichment-author@example.com",
            "irrelevant-in-this-test",
            "",
        )
        .await
        .expect("create author");

        let enriched = issue_with_author(&db, issue(user.id))
            .await
            .expect("existing author lookup");
        assert_eq!(enriched.author.as_deref(), Some("enrichment-author"));
        assert_eq!(enriched.issue.labels, None);

        let enriched = comment_with_author(&db, comment(1, i64::MAX))
            .await
            .expect("missing author is a valid result");
        assert_eq!(enriched.author, None);
    }

    #[tokio::test]
    async fn author_enrichment_propagates_a_broken_users_table_for_single_and_list_paths() {
        let db = test_db().await;
        db.execute_unprepared("DROP TABLE users")
            .await
            .expect("break author lookup after loading the primary models");

        let single = issue_with_author(&db, issue(1)).await;
        assert!(
            single
                .err()
                .is_some_and(|error| error.status().is_server_error()),
            "single-item enrichment must fail with 5xx when the user lookup fails"
        );

        let list = comments_with_authors(&db, vec![comment(1, 1), comment(2, 1)]).await;
        assert!(
            list.err()
                .is_some_and(|error| error.status().is_server_error()),
            "list enrichment must fail with 5xx when the user lookup fails"
        );
    }
}

#[cfg(test)]
mod milestone_update_delete_tests {
    use super::*;
    use sea_orm::{NotSet, Set};

    async fn fixture() -> (
        tempfile::TempDir,
        sea_orm::DatabaseConnection,
        rg_db::entities::milestone::Model,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", dir.path().join("t.db").display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            4,
        )
        .await
        .expect("connect sqlite");
        rg_db::run_migrations(&db).await.expect("run migrations");

        let owner = rg_db::ops::user_ops::create_user(
            &db,
            "milestone-race-owner",
            "milestone-race-owner@example.invalid",
            "",
            "Owner",
        )
        .await
        .expect("create owner");
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("milestone-race-repo".to_string()),
                description: Set(None),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .expect("create repository");
        let milestone = rg_db::ops::milestone_ops::create(
            &db,
            rg_db::entities::milestone::ActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                title: Set("One".to_string()),
                description: Set(None),
                state: Set("open".to_string()),
                due_date: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            },
        )
        .await
        .expect("create milestone");

        (dir, db, milestone)
    }

    #[tokio::test]
    async fn delete_after_the_scoped_read_is_http_not_found() {
        let (_dir, db, milestone) = fixture().await;
        let milestone_id = milestone.id;

        let error = update_milestone_after_read(
            &db,
            milestone,
            UpdateMilestoneRequest {
                title: Some("Too late".to_string()),
                description: None,
                state: Some("closed".to_string()),
                due_date: None,
            },
            || async {
                assert!(rg_db::ops::milestone_ops::delete_by_id(&db, milestone_id)
                    .await
                    .map_err(AppError::from)?);
                Ok(())
            },
        )
        .await
        .expect_err("a winning delete must not become a successful milestone update");

        assert_eq!(error.status(), StatusCode::NOT_FOUND);
    }
}
