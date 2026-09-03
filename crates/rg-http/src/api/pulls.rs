//! REST API handlers for Pull Requests.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};

use rg_core::pull_request::merge_queue::CancelOutcome;

use crate::api::repo_access::{self, RepoAuthRead, RepoRead, RepoWrite};
use crate::error::AppError;
use crate::pagination::{PaginatedResponse, PaginationParams};
use crate::AppState;

/// Run the post-push hooks for the base-branch move a merge just made.
///
/// The seam itself lives on [`AppState::spawn_merge_push_hooks`] — every
/// ref-moving path of this crate shares it (card_73a1ec5b32f3). This wrapper is
/// only the `Option` → `Vec` adapter for the single-merge callers below.
fn spawn_hooks_for_merge(
    state: &AppState,
    actor_id: i64,
    base_ref_update: Option<rg_core::pull_request::MergedRef>,
) {
    state.spawn_merge_push_hooks(Some(actor_id), base_ref_update.into_iter().collect());
}

// ── Request / Response types ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreatePrRequest {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    /// Head branch reference. Supports "owner:branch" for fork PRs, or just "branch" for same-repo.
    pub head: String,
    pub base: String,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Deserialize)]
pub struct UpdatePrRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub draft: Option<bool>,
}

#[derive(Deserialize)]
pub struct MergePrRequest {
    /// merge / squash / rebase
    pub strategy: String,
}

#[derive(Deserialize)]
pub struct EnableAutoMergeRequest {
    /// merge / squash / rebase
    pub strategy: String,
}

#[derive(Serialize)]
pub struct MergeQueueEntryResponse {
    pub id: i64,
    pub position: usize,
    pub pr_id: i64,
    pub pr_number: i64,
    pub title: String,
    pub strategy: String,
    pub status: String,
    pub enqueued_by_id: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    pub state: Option<String>,
    #[serde(flatten)]
    #[param(ignore)]
    pub pagination: PaginationParams,
}

// ── PR handlers ─────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls",
    tag = "Pull Requests",
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
pub async fn list_prs(
    State(state): State<AppState>,
    RepoRead { .. }: RepoRead,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<ListQuery>,
) -> impl IntoResponse {
    let state_filter = params.state.as_deref();
    let pagination = params.pagination.clamp();
    match rg_core::pull_request::list_prs_paginated(
        &state.db,
        &owner,
        &repo,
        state_filter,
        pagination.offset(),
        pagination.limit(),
    )
    .await
    {
        Ok((data, total)) => (
            StatusCode::OK,
            Json(PaginatedResponse::new(data, &pagination, total as u64)),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls/{number}",
    tag = "Pull Requests",
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
pub async fn get_pr(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => (StatusCode::OK, Json(pr)).into_response(),
        // `AppError::from`, not `not_found`: only a `rg_core::error::NotFound`
        // means the PR is absent. A failed lookup stays a 5xx and keeps its
        // detail in the operator log instead of in the response body.
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls",
    tag = "Pull Requests",
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
pub async fn create_pr(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    RepoAuthRead {
        repo: repo_model,
        actor_id: user_id,
    }: RepoAuthRead,
    Json(req): Json<CreatePrRequest>,
) -> impl IntoResponse {
    let repo_id = repo_model.id;

    if req.head.trim().is_empty() || req.base.trim().is_empty() {
        return AppError::bad_request("head and base branches are required").into_response();
    };

    match rg_core::pull_request::resolve_head_ref(&state.db, repo_id, &req.head).await {
        Ok((head_branch, head_repo_id)) => {
            match rg_core::pull_request::create_pr(
                &state.db,
                &state.repo_root,
                repo_id,
                user_id,
                req.title,
                req.body,
                head_branch,
                req.base,
                head_repo_id,
                req.draft,
                Some(&state.delivery_tracker),
            )
            .await
            {
                Ok(pr) => {
                    // Counted by `pull_request::service::insert_with_repo_number`,
                    // not here — same reason as issues: the import subsystem
                    // reaches that allocator and never this handler.
                    // The `pull_request` CI event. Nothing emitted it before
                    // card_074d93bfe327, so a repository whose CI is a single
                    // `.gitea/workflows/pr.yml` with `on: pull_request` got a
                    // matcher that handled the event, unit tests that covered
                    // it, and no pipeline ever.
                    state.spawn_pull_request_ci(pr.clone(), Some(user_id));
                    // CODEOWNERS is advisory: a malformed/missing file or an
                    // unavailable diff must not prevent PR creation.
                    match rg_core::pull_request::compute_diff(
                        &state.db,
                        &state.repo_root,
                        &owner,
                        &repo,
                        pr.number,
                    )
                    .await
                    {
                        Ok(diff) => {
                            let paths = diff
                                .files_changed
                                .into_iter()
                                .map(|file| file.path)
                                .collect::<Vec<_>>();
                            let repo_path = state.repo_root.join(format!("{owner}/{repo}.git"));
                            match rg_core::review::codeowners::request_codeowners(
                                &state.db,
                                &repo_path,
                                &pr.base_branch,
                                &paths,
                                &repo_model,
                                pr.id,
                                pr.author_id,
                                user_id,
                            )
                            .await
                            {
                                Ok(outcome) => {
                                    log_codeowners_diagnostics(pr.id, &outcome.diagnostics);
                                }
                                Err(error) => tracing::warn!(
                                    pr_id = pr.id,
                                    error = %format!("{error:#}"),
                                    "CODEOWNERS reviewer request failed"
                                ),
                            }
                        }
                        Err(error) => {
                            tracing::warn!(pr_id = pr.id, error = %format!("{error:#}"), "CODEOWNERS diff unavailable");
                        }
                    }
                    (StatusCode::CREATED, Json(pr)).into_response()
                }
                // `create_pr` marks the two client-side rejections (empty title,
                // head == base) with `InvalidRequest`; its git reads and inserts
                // are ours and now keep their 5xx instead of being reported as a
                // malformed request the caller can never fix.
                Err(e) => AppError::from(e).into_response(),
            }
        }
        // Same for `resolve_head_ref`: an unknown head owner or a head repo that
        // is not a fork is a 400, a failed lookup behind either is not.
        Err(e) => AppError::from(e).into_response(),
    }
}

fn log_codeowners_diagnostics(
    pr_id: i64,
    diagnostics: &[rg_core::review::codeowners::CodeownersDiagnostic],
) {
    for diagnostic in diagnostics {
        tracing::warn!(
            pr_id,
            line = diagnostic.line,
            declaration = %diagnostic.declaration,
            reason = %diagnostic.reason,
            "CODEOWNERS declaration ignored"
        );
    }
}

#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/pulls/{number}",
    tag = "Pull Requests",
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
pub async fn update_pr(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoAuthRead {
        repo: repo_model,
        actor_id,
    }: RepoAuthRead,
    Json(req): Json<UpdatePrRequest>,
) -> impl IntoResponse {
    let existing = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(e) => return AppError::from(e).into_response(),
    };
    // `unwrap_or(false)` here told a repository writer "you may not update this
    // PR" whenever the permission query failed — a 403 that no retry or new
    // token can clear, for a failure that was ours.
    let can_write = match repo_access::may_write(&state, &repo_model, Some(actor_id)).await {
        Ok(allowed) => allowed,
        Err(e) => return e.into_response(),
    };
    if existing.author_id != actor_id && !can_write {
        return AppError::forbidden("only the PR author or a repository writer may update this PR")
            .into_response();
    }
    if req.state.as_deref() == Some("merged") {
        return AppError::bad_request("use the merge endpoint to merge a pull request")
            .into_response();
    }

    match rg_core::pull_request::update_pr(
        &state.db,
        &owner,
        &repo,
        number,
        req.title,
        req.body,
        req.state,
        req.draft,
        actor_id,
        Some(&state.delivery_tracker),
    )
    .await
    {
        Ok(pr) => {
            // Reopening is a `pull_request` event of its own: the head may have
            // moved (or the base may have) while the PR sat closed, and nothing
            // ran CI for it in the meantime.
            if existing.state != "open" && pr.state == "open" {
                state.spawn_pull_request_ci(pr.clone(), Some(actor_id));
            }
            (StatusCode::OK, Json(pr)).into_response()
        }
        // Rejected title/state/draft transition → 400 from the service's own
        // markers; an absent PR → 404; a failed write → 5xx.
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls/{number}/diff",
    tag = "Pull Requests",
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
pub async fn get_diff(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoRead { .. }: RepoRead,
) -> impl IntoResponse {
    match rg_core::pull_request::compute_diff(&state.db, &state.repo_root, &owner, &repo, number)
        .await
    {
        Ok(diff) => (StatusCode::OK, Json(diff)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls/{number}/merge",
    tag = "Pull Requests",
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
pub async fn merge_pr(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoWrite {
        repo: repo_model,
        actor_id,
    }: RepoWrite,
    Json(req): Json<MergePrRequest>,
) -> impl IntoResponse {
    let strategy = match rg_core::pull_request::MergeStrategy::parse(&req.strategy) {
        Ok(strategy) => strategy,
        Err(error) => return AppError::bad_request(error).into_response(),
    };

    // Check branch protection before merging.
    //
    // Keep this as an early refusal for the REST path. The core merge service
    // repeats the check immediately before claiming the PR, so a future caller
    // cannot bypass protection and a rule change between these two reads fails
    // closed. The lookup may still not be `if let Ok(pr)`: a failed read is our
    // error, not permission to continue to the core call.
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(e) => return AppError::from(e).into_response(),
    };
    if pr.is_draft {
        return AppError::conflict("draft pull requests cannot be merged").into_response();
    }
    if let Err(e) = rg_core::branch_protection::service::check_merge_allowed(
        &state.db,
        repo_model.id,
        &pr.base_branch,
        pr.id,
    )
    .await
    {
        // A rule refusing the merge carries `rg_core::error::Forbidden` and
        // stays a 403; the check itself failing (the database, the pipeline
        // lookup) is ours and must not be dressed up as "you are not allowed".
        return AppError::from(e).into_response();
    }

    match rg_core::pull_request::merge_pr(
        &state.db,
        &state.repo_root,
        &owner,
        &repo,
        number,
        actor_id,
        strategy,
        Some(&state.delivery_tracker),
    )
    .await
    {
        // `pr_merged` is recorded inside `rg_core::pull_request::merge_pr` so the
        // REST, auto-merge, and merge-queue paths all count through one site.
        Ok(result) => {
            spawn_hooks_for_merge(&state, actor_id, result.base_ref_update.clone());
            (StatusCode::OK, Json(result)).into_response()
        }
        // Every way a merge fails used to be the client's fault: a closed PR, a
        // draft, a racing attempt and a merge conflict all answered 400 — as did
        // a dead database and a failed git invocation, with the `db: ...`
        // context or the git command line in the body. The state outcomes now
        // carry `rg_core::error::Conflict` (409); everything else falls through
        // to the funnel, which classifies and sanitizes it.
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/pulls/{number}/auto-merge",
    tag = "Pull Requests",
    request_body(content = serde_json::Value),
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
    ),
    responses((status = 200, body = serde_json::Value), (status = 403, body = serde_json::Value))
)]
pub async fn enable_auto_merge(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoWrite { actor_id, .. }: RepoWrite,
    Json(req): Json<EnableAutoMergeRequest>,
) -> impl IntoResponse {
    let strategy = match rg_core::pull_request::MergeStrategy::parse(&req.strategy) {
        Ok(strategy) => strategy,
        Err(error) => return AppError::bad_request(error).into_response(),
    };
    if let Err(error) = rg_core::pull_request::enable_auto_merge(
        &state.db, &owner, &repo, number, strategy, actor_id,
    )
    .await
    {
        // A closed PR, a draft or a queue run in progress are typed `Conflict`
        // and answer 409; an unknown PR is 404; the update behind them is ours.
        return AppError::from(error).into_response();
    }
    match rg_core::pull_request::try_auto_merge(&state.db, &state.repo_root, &owner, &repo, number)
        .await
    {
        Ok(outcome) => {
            // Enabling auto-merge on a PR whose conditions are already met
            // merges it right here, moving the base branch — same debt as the
            // explicit merge above.
            spawn_hooks_for_merge(
                &state,
                actor_id,
                outcome
                    .merge
                    .as_ref()
                    .and_then(|merge| merge.base_ref_update.clone()),
            );
            (StatusCode::OK, Json(outcome)).into_response()
        }
        // `try_auto_merge` returns every *unsatisfied* condition as a pending
        // `Ok(outcome)`, so an `Err` here is only ever a git or database failure
        // — precisely the thing that must never be reported as a bad request.
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/pulls/{number}/auto-merge",
    tag = "Pull Requests",
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
    ),
    responses((status = 200, body = serde_json::Value), (status = 403, body = serde_json::Value))
)]
pub async fn disable_auto_merge(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoWrite { actor_id, .. }: RepoWrite,
) -> impl IntoResponse {
    match rg_core::pull_request::disable_auto_merge(&state.db, &owner, &repo, number, actor_id)
        .await
    {
        Ok(pr) => (StatusCode::OK, Json(pr)).into_response(),
        // Nothing here is the caller's to get wrong — the PR was resolved and the
        // write is unconditional — so an unknown PR is 404 and the rest is a 5xx.
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls/{number}/ci-approval",
    tag = "Pull Requests",
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
    ),
    responses(
        (status = 200, description = "CI approved for the PR's current head", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "The head moved while the approval was being recorded", body = serde_json::Value),
    ),
)]
/// POST /api/v1/repos/:owner/:name/pulls/:number/ci-approval
///
/// The maintainer action a fork PR waits for. A pipeline runs under the *base*
/// repository's id and is handed that repository's CI secrets, so an unreviewed
/// head must not start one on its own — and until this endpoint existed the
/// answer to that was that a fork PR got no CI at all (card_94834ecee708).
///
/// `RepoWrite` and not the PR author: the whole point is that somebody who
/// already has write access has looked at the diff. The approval is recorded
/// against the head commit, so it does not survive the next push.
pub async fn approve_pr_ci(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoWrite { actor_id, .. }: RepoWrite,
) -> impl IntoResponse {
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_core::pull_request::approve_pull_request_ci(&state.db, &pr, actor_id).await {
        Ok(approved) => {
            // Same shape as reopening a PR: the approval is what makes this head
            // eligible, so the run it unblocks starts from here rather than
            // waiting for the next push that nobody may ever make.
            state.spawn_pull_request_ci(approved.clone(), Some(actor_id));
            (StatusCode::OK, Json(approved)).into_response()
        }
        // A closed PR or one with no head is `InvalidRequest` → 400; a head that
        // moved under the approver is `Conflict` → 409; the reload behind them
        // is ours and stays a 5xx.
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/merge-queue",
    tag = "Pull Requests",
    params(("owner" = String, Path), ("name" = String, Path)),
    responses((status = 200, body = serde_json::Value))
)]
pub async fn list_merge_queue(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoRead { repo: repository }: RepoRead,
) -> impl IntoResponse {
    let entries = match rg_db::ops::merge_queue_ops::list_by_repo(&state.db, repository.id).await {
        Ok(entries) => entries,
        Err(error) => return AppError::from(error).into_response(),
    };
    let mut response = Vec::with_capacity(entries.len());
    for (index, entry) in entries.into_iter().enumerate() {
        let pr = match rg_db::entities::pull_request::Entity::find_by_id(entry.pr_id)
            .one(&state.db)
            .await
        {
            Ok(Some(pr)) if pr.repo_id == repository.id => pr,
            Ok(_) => continue,
            Err(error) => return AppError::from(error).into_response(),
        };
        response.push(MergeQueueEntryResponse {
            id: entry.id,
            position: index + 1,
            pr_id: entry.pr_id,
            pr_number: pr.number,
            title: pr.title,
            strategy: entry.strategy,
            status: entry.status,
            enqueued_by_id: entry.enqueued_by_id,
            created_at: entry.created_at,
        });
    }
    (StatusCode::OK, Json(response)).into_response()
}

#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/pulls/{number}/merge-queue",
    tag = "Pull Requests",
    request_body(content = serde_json::Value),
    params(("owner" = String, Path), ("name" = String, Path), ("number" = i64, Path)),
    responses(
        (status = 200, body = serde_json::Value),
        (status = 403, body = serde_json::Value),
        (status = 404, description = "The pull request or repository disappeared while it was being enqueued", body = serde_json::Value),
        (status = 409, description = "The pull request is closed or a draft: state the caller can wait out, as on POST .../merge", body = serde_json::Value),
    )
)]
pub async fn enqueue_merge_queue(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoWrite {
        repo: repository,
        actor_id,
    }: RepoWrite,
    Json(req): Json<EnableAutoMergeRequest>,
) -> impl IntoResponse {
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(error) => return AppError::from(error).into_response(),
    };
    let strategy = match rg_core::pull_request::MergeStrategy::parse(&req.strategy) {
        Ok(strategy) => strategy,
        Err(error) => return AppError::bad_request(error).into_response(),
    };
    let entry = match rg_core::pull_request::merge_queue::enqueue(
        &state.db,
        &repository,
        &pr,
        actor_id,
        strategy,
    )
    .await
    {
        Ok(entry) => entry,
        Err(error) => return AppError::from(error).into_response(),
    };
    let process = match rg_core::pull_request::merge_queue::process_repository_with_ci(
        &state.db,
        &state.repo_root,
        &repository,
        &state.pipeline_ci(),
    )
    .await
    {
        Ok(process) => process,
        Err(error) => return AppError::from(error).into_response(),
    };
    // Enqueueing runs the queue, and a queue run merges: the branches it moved
    // owe the hooks just like any other merge does (card_73a1ec5b32f3).
    state.spawn_merge_push_hooks(Some(actor_id), process.merged_ref_updates.clone());
    (
        StatusCode::OK,
        Json(serde_json::json!({"entry": entry, "process": process})),
    )
        .into_response()
}

#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/pulls/{number}/merge-queue",
    tag = "Pull Requests",
    params(("owner" = String, Path), ("name" = String, Path), ("number" = i64, Path)),
    responses(
        (status = 204),
        (status = 404, body = serde_json::Value),
        (status = 409, description = "The merge queue is already merging this pull request", body = serde_json::Value),
    )
)]
pub async fn cancel_merge_queue(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    RepoWrite {
        repo: repository,
        actor_id,
    }: RepoWrite,
) -> impl IntoResponse {
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_core::pull_request::merge_queue::cancel(
        &state.db,
        &state.repo_root,
        &repository,
        &pr,
        actor_id,
    )
    .await
    {
        Ok(CancelOutcome::Canceled) => StatusCode::NO_CONTENT.into_response(),
        Ok(CancelOutcome::NotQueued) => {
            AppError::not_found("pull request is not queued").into_response()
        }
        // Not a 404: the entry is right there, a worker is merging it, and the
        // caller is simply too late. A 404 here reads as "you have the wrong
        // PR" and sends the client looking for a resource it already has.
        Ok(CancelOutcome::AlreadyMerging) => AppError::conflict(
            "the merge queue is already merging this pull request; it can no longer be canceled",
        )
        .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

#[cfg(test)]
mod codeowners_diagnostic_logging_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogs {
        type Writer = CapturedLogs;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn every_codeowners_diagnostic_reaches_the_operator_log() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let diagnostics = [
            rg_core::review::codeowners::CodeownersDiagnostic {
                line: 4,
                declaration: "docs@example.com".into(),
                reason: "unsupported email owner".into(),
            },
            rg_core::review::codeowners::CodeownersDiagnostic {
                line: 7,
                declaration: "@missing".into(),
                reason: "account does not exist".into(),
            },
        ];

        log_codeowners_diagnostics(42, &diagnostics);

        let rendered = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert_eq!(
            rendered.matches("CODEOWNERS declaration ignored").count(),
            2
        );
        for expected in [
            "pr_id=42",
            "line=4",
            "declaration=docs@example.com",
            "reason=unsupported email owner",
            "line=7",
            "declaration=@missing",
            "reason=account does not exist",
        ] {
            assert!(
                rendered.contains(expected),
                "missing `{expected}` in {rendered}"
            );
        }
    }

    #[test]
    fn create_pr_keeps_the_diagnostic_logger_wired() {
        let production = rust_source::production_rust_code_only(include_str!("pulls.rs"));

        assert!(
            production.contains("log_codeowners_diagnostics(pr.id, &outcome.diagnostics);"),
            "create_pr must surface successful CODEOWNERS diagnostics"
        );
    }
}
