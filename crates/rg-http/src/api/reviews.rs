//! REST API handlers for PR code reviews.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use utoipa::ToSchema;

use super::repo_access::{require_authenticated_read, require_read, require_write};
use crate::error::AppError;
use crate::AppState;
use rg_db::entities::{
    merge_queue_entry, pr_event, pr_review, pr_reviewer_request, pull_request, review_comment,
};

// ── Request / Response types ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct SubmitReviewRequest {
    /// comment / approve / request_changes / dismiss
    pub action: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub commit_id: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateReviewCommentRequest {
    #[serde(default)]
    pub review_id: Option<i64>,
    pub path: String,
    #[serde(default)]
    pub line: Option<i64>,
    #[serde(default)]
    pub start_line: Option<i64>,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub start_side: Option<String>,
    pub body: String,
    #[serde(default)]
    pub suggestion: Option<String>,
    #[serde(default)]
    pub commit_id: Option<String>,
    #[serde(default)]
    pub reply_to_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct RequestReviewerRequest {
    pub username: String,
}

#[derive(Serialize)]
pub struct RequestedReviewerResponse {
    pub id: i64,
    pub reviewer_id: i64,
    pub username: String,
    pub requested_by_id: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize)]
pub struct SetThreadResolutionRequest {
    pub resolved: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct ApplySuggestionsRequest {
    pub comment_ids: Vec<i64>,
}

#[derive(Serialize)]
pub struct TimelineActor {
    pub id: i64,
    pub username: String,
}

#[derive(Serialize)]
pub struct ReviewTimelineEvent {
    pub id: String,
    pub kind: String,
    pub actor: Option<TimelineActor>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub body: Option<String>,
    pub metadata: serde_json::Value,
}

async fn require_pr_manager(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    owner: &str,
    repo: &str,
    number: i64,
) -> Result<
    (
        rg_db::entities::repository::Model,
        i64,
        rg_db::entities::pull_request::Model,
    ),
    AppError,
> {
    let (repo_model, actor_id) = require_authenticated_read(state, headers, owner, repo).await?;
    let pr = rg_core::pull_request::get_pr(&state.db, owner, repo, number)
        .await
        .map_err(AppError::from)?;
    // A permission check that failed is not a permission check that said "no":
    // `unwrap_or(false)` handed a repository writer a 403 whenever the query
    // behind it broke, hiding our outage behind the caller's credentials.
    let can_write = match rg_core::repo::service::can_write_repo(
        &state.db,
        &repo_model,
        Some(actor_id),
    )
    .await
    {
        Ok(allowed) => allowed,
        Err(error) => return Err(AppError::from(error)),
    };
    if pr.author_id != actor_id && !can_write {
        return Err(AppError::forbidden(
            "only the PR author or a repository writer may manage reviewers",
        ));
    }
    Ok((repo_model, actor_id, pr))
}

async fn require_suggestion_source(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    owner: &str,
    repo: &str,
    number: i64,
) -> Result<
    (
        i64,
        rg_db::entities::user::Model,
        rg_db::entities::pull_request::Model,
        rg_db::entities::repository::Model,
        String,
    ),
    AppError,
> {
    let (_, actor_id) = require_authenticated_read(state, headers, owner, repo).await?;
    let pr = rg_core::pull_request::get_pr(&state.db, owner, repo, number)
        .await
        .map_err(AppError::from)?;
    if pr.state != "open" {
        return Err(AppError::conflict("pull request is not open"));
    }
    let source_repo_id = pr.head_repo_id.unwrap_or(pr.repo_id);
    let source_repo = rg_db::entities::repository::Entity::find_by_id(source_repo_id)
        .one(&state.db)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("source repository not found"))?;
    let can_write =
        match rg_core::repo::service::can_write_repo(&state.db, &source_repo, Some(actor_id)).await
        {
            Ok(allowed) => allowed,
            Err(error) => return Err(AppError::from(error)),
        };
    if !can_write {
        return Err(AppError::forbidden(
            "write access to the PR source repository is required",
        ));
    }
    let actor = rg_db::ops::user_ops::find_by_id(&state.db, actor_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::unauthorized("user not found"))?;
    let source_namespace = if let Some(org_id) = source_repo.org_id {
        rg_db::ops::org_ops::get_org(&state.db, org_id)
            .await
            .map_err(AppError::internal)?
            .ok_or_else(|| AppError::not_found("source repository organization not found"))?
            .name
    } else {
        rg_db::ops::user_ops::find_by_id(&state.db, source_repo.owner_id)
            .await
            .map_err(AppError::internal)?
            .ok_or_else(|| AppError::not_found("source repository owner not found"))?
            .username
    };
    Ok((actor_id, actor, pr, source_repo, source_namespace))
}

async fn after_suggestions_applied(
    state: &AppState,
    actor_id: i64,
    pr: &rg_db::entities::pull_request::Model,
    source_repo: &rg_db::entities::repository::Model,
    source_namespace: &str,
    commit_sha: &str,
) {
    let source_repo_path = state
        .repo_root
        .join(format!("{source_namespace}/{}.git", source_repo.name));
    if state.ci_engine.has_ci_config(&source_repo_path, commit_sha) {
        let ref_name = format!("refs/heads/{}", pr.head_branch);
        if let Err(error) = state
            .ci_engine
            .trigger_pipeline(rg_core::ci::TriggerPipelineParams {
                db: &state.db,
                repo_path: &source_repo_path,
                repo_id: source_repo.id,
                commit_sha,
                ref_name: &ref_name,
                trigger_type: "suggestion",
                triggered_by: Some(actor_id),
                docker_enabled: state.docker_enabled,
                external_runners: state.external_runners,
                allow_host_runner: state.allow_host_runner,
                jwt_secret: Some(&state.jwt_secret),
                external_url: state.external_url.as_deref(),
            })
            .await
        {
            tracing::warn!(pr_id = pr.id, error = %format!("{error:#}"), "CI trigger after suggestion failed");
        }
    }
    // Applying a suggestion writes a commit, so it can unblock an auto-merge or
    // the queue — and whatever they merge moves a base branch that owes the
    // post-push hooks, exactly as if the commit had been pushed
    // (card_73a1ec5b32f3).
    state
        .evaluate_merges_and_spawn_hooks(source_repo.id, commit_sha, Some(actor_id))
        .await;
}

// ── Review handlers ───────────────────────────────────────────────────

/// List reviews for a PR.
/// GET /api/v1/repos/:owner/:name/pulls/:number/reviews
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls/{number}/reviews",
    tag = "Reviews",
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
pub async fn list_reviews(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_read(&state, &headers, &owner, &repo).await {
        return e.into_response();
    }

    match rg_core::review::service::list_reviews(&state.db, &owner, &repo, number).await {
        Ok(reviews) => (StatusCode::OK, Json(reviews)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Submit a review on a PR.
/// POST /api/v1/repos/:owner/:name/pulls/:number/reviews
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls/{number}/reviews",
    tag = "Reviews",
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
pub async fn submit_review(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SubmitReviewRequest>,
) -> impl IntoResponse {
    let (repo_model, user_id) =
        match require_authenticated_read(&state, &headers, &owner, &repo).await {
            Ok(access) => access,
            Err(e) => return e.into_response(),
        };
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(e) => return AppError::from(e).into_response(),
    };

    let action = match rg_core::review::service::ReviewAction::parse_action(&req.action) {
        Ok(a) => a,
        Err(e) => return AppError::from(e).into_response(),
    };
    if matches!(action.as_str(), "approve" | "request_changes") && pr.author_id == user_id {
        return AppError::bad_request(
            "a PR author cannot approve or request changes on their own PR",
        )
        .into_response();
    }
    let should_attempt_auto_merge = action.as_str() == "approve";

    match rg_core::review::service::submit_review(
        &state.db,
        repo_model.id,
        number,
        user_id,
        action,
        req.body,
        req.commit_id,
    )
    .await
    {
        Ok(review) => {
            if should_attempt_auto_merge {
                // An approval is the last condition an auto-merge or a queued PR
                // was waiting on, so this is a merge path like any other: the
                // base branch it moves owes the post-push hooks, and until
                // card_73a1ec5b32f3 both ref moves here were dropped.
                let mut merged = Vec::new();
                match rg_core::pull_request::try_auto_merge(
                    &state.db,
                    &state.repo_root,
                    &owner,
                    &repo,
                    number,
                )
                .await
                {
                    Ok(outcome) => {
                        tracing::info!(pr_id = pr.id, status = %outcome.status, "auto-merge evaluated after approval");
                        merged.extend(outcome.merge.and_then(|merge| merge.base_ref_update));
                    }
                    Err(error) => {
                        tracing::warn!(
                            pr_id = pr.id,
                            error = %format!("{error:#}"),
                            "auto-merge attempt after approval failed"
                        )
                    }
                }
                match rg_core::pull_request::merge_queue::process_repository_with_ci(
                    &state.db,
                    &state.repo_root,
                    &repo_model,
                    &state.merge_queue_ci(),
                )
                .await
                {
                    Ok(process) => merged.extend(process.merged_ref_updates),
                    Err(error) => {
                        tracing::warn!(
                            repo_id = repo_model.id,
                            error = %format!("{error:#}"),
                            "merge queue evaluation after approval failed"
                        );
                    }
                }
                state.spawn_merge_push_hooks(Some(user_id), merged);
            }
            (StatusCode::CREATED, Json(review)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Get a single review.
/// GET /api/v1/repos/:owner/:name/pulls/:number/reviews/:id
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls/{number}/reviews/{id}",
    tag = "Reviews",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
        ("id" = i64, Path, description = "id"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_review(
    State(state): State<AppState>,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let repo_model = match require_read(&state, &headers, &owner, &repo).await {
        Ok(repo) => repo,
        Err(e) => return e.into_response(),
    };
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(e) => return AppError::from(e).into_response(),
    };
    match rg_core::review::service::get_review(&state.db, id).await {
        Ok(review) if review.repo_id == repo_model.id && review.pr_id == pr.id => {
            (StatusCode::OK, Json(review)).into_response()
        }
        Ok(_) => AppError::not_found("review not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Dismiss a review.
/// POST /api/v1/repos/:owner/:name/pulls/:number/reviews/:id/dismiss
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls/{number}/reviews/{id}/dismiss",
    tag = "Reviews",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("number" = i64, Path, description = "number"),
        ("id" = i64, Path, description = "id"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn dismiss_review(
    State(state): State<AppState>,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<DismissReviewRequest>,
) -> impl IntoResponse {
    let (repo_model, user_id) = match require_write(&state, &headers, &owner, &repo).await {
        Ok(access) => access,
        Err(e) => return e.into_response(),
    };
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(e) => return AppError::from(e).into_response(),
    };
    let review = match rg_core::review::service::get_review(&state.db, id).await {
        Ok(review) if review.repo_id == repo_model.id && review.pr_id == pr.id => review,
        Ok(_) => return AppError::not_found("review not found").into_response(),
        Err(e) => return AppError::from(e).into_response(),
    };

    match rg_core::review::service::dismiss_review(&state.db, review.id, user_id, req.message).await
    {
        Ok(review) => (StatusCode::OK, Json(review)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Review comment handlers ───────────────────────────────────────────

/// List review comments for a PR.
/// GET /api/v1/repos/:owner/:name/pulls/:number/comments
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls/{number}/comments",
    tag = "Reviews",
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
pub async fn list_review_comments(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_read(&state, &headers, &owner, &repo).await {
        return e.into_response();
    }

    match rg_core::review::service::list_review_comments(&state.db, &owner, &repo, number).await {
        Ok(comments) => (StatusCode::OK, Json(comments)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls/{number}/timeline",
    tag = "Reviews",
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
    ),
    responses((status = 200, body = serde_json::Value))
)]
pub async fn get_review_timeline(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = require_read(&state, &headers, &owner, &repo).await {
        return error.into_response();
    }
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(error) => return AppError::from(error).into_response(),
    };
    let data = match load_timeline_data(&state.db, pr).await {
        Ok(data) => data,
        Err(response) => return response,
    };
    let timeline = build_review_timeline(data);
    (StatusCode::OK, Json(timeline)).into_response()
}

/// Everything the review timeline is assembled from, fetched up front.
struct TimelineData {
    pr: pull_request::Model,
    persisted_events: Vec<(pr_event::Model, serde_json::Value)>,
    reviews: Vec<pr_review::Model>,
    comments: Vec<review_comment::Model>,
    reviewer_requests: Vec<pr_reviewer_request::Model>,
    queue_entry: Option<merge_queue_entry::Model>,
    users: HashMap<i64, String>,
}

/// Load every source the timeline draws from for one PR. Any DB error is mapped
/// to the same `Response` the handler would have returned inline.
async fn load_timeline_data(
    db: &sea_orm::DatabaseConnection,
    pr: pull_request::Model,
) -> Result<TimelineData, axum::response::Response> {
    let persisted_events = rg_db::ops::pr_event_ops::list_by_pr(db, pr.id)
        .await
        .map_err(|error| AppError::from(error).into_response())?
        .into_iter()
        .map(|event| {
            let metadata =
                serde_json::from_str(&event.metadata).unwrap_or_else(|_| serde_json::json!({}));
            (event, metadata)
        })
        .collect::<Vec<_>>();
    let reviews = rg_db::ops::pr_review_ops::list_by_pr(db, pr.id)
        .await
        .map_err(|error| AppError::from(error).into_response())?;
    let comments = rg_db::ops::review_comment_ops::list_by_pr(db, pr.id)
        .await
        .map_err(|error| AppError::from(error).into_response())?;
    let reviewer_requests = rg_db::ops::pr_reviewer_request_ops::list_by_pr(db, pr.id)
        .await
        .map_err(|error| AppError::from(error).into_response())?;
    let queue_entry = rg_db::ops::merge_queue_ops::find_by_pr(db, pr.id)
        .await
        .map_err(|error| AppError::from(error).into_response())?;

    let actor_ids = collect_timeline_actor_ids(
        &pr,
        &persisted_events,
        &reviews,
        &comments,
        &reviewer_requests,
        &queue_entry,
    );
    let users = rg_db::entities::user::Entity::find()
        .filter(rg_db::entities::user::Column::Id.is_in(actor_ids))
        .all(db)
        .await
        .map_err(|error| AppError::from(error).into_response())?
        .into_iter()
        .map(|user| (user.id, user.username))
        .collect::<HashMap<_, _>>();

    Ok(TimelineData {
        pr,
        persisted_events,
        reviews,
        comments,
        reviewer_requests,
        queue_entry,
        users,
    })
}

/// Gather every user id referenced anywhere in the timeline so their usernames
/// can be resolved in a single query.
fn collect_timeline_actor_ids(
    pr: &pull_request::Model,
    persisted_events: &[(pr_event::Model, serde_json::Value)],
    reviews: &[pr_review::Model],
    comments: &[review_comment::Model],
    reviewer_requests: &[pr_reviewer_request::Model],
    queue_entry: &Option<merge_queue_entry::Model>,
) -> HashSet<i64> {
    let mut actor_ids = HashSet::from([pr.author_id]);
    actor_ids.extend(
        persisted_events
            .iter()
            .filter_map(|(event, _)| event.actor_id),
    );
    actor_ids.extend(reviews.iter().map(|review| review.reviewer_id));
    for comment in comments {
        actor_ids.insert(comment.author_id);
        actor_ids.extend(comment.suggestion_applied_by_id);
        actor_ids.extend(comment.resolved_by_id);
    }
    for request in reviewer_requests {
        actor_ids.insert(request.reviewer_id);
        actor_ids.insert(request.requested_by_id);
    }
    actor_ids.extend(pr.auto_merge_enabled_by_id);
    if let Some(entry) = queue_entry {
        actor_ids.insert(entry.enqueued_by_id);
    }
    actor_ids
}

/// Assemble the ordered timeline from the pre-loaded data.
fn build_review_timeline(data: TimelineData) -> Vec<ReviewTimelineEvent> {
    let TimelineData {
        pr,
        persisted_events,
        reviews,
        comments,
        reviewer_requests,
        queue_entry,
        users,
    } = data;
    let mut builder = TimelineBuilder {
        persisted_events,
        users,
        timeline: Vec::new(),
    };
    builder.push_opened(&pr);
    builder.push_reviews(reviews);
    builder.push_comments(comments);
    builder.push_reviewer_requests(reviewer_requests);
    builder.push_lifecycle(&pr, queue_entry);
    builder.finish()
}

/// Accumulates synthetic + persisted timeline events, sharing the actor lookup
/// and "was this already persisted?" checks across all event sources.
struct TimelineBuilder {
    persisted_events: Vec<(pr_event::Model, serde_json::Value)>,
    users: HashMap<i64, String>,
    timeline: Vec<ReviewTimelineEvent>,
}

impl TimelineBuilder {
    fn actor(&self, id: i64) -> Option<TimelineActor> {
        self.users.get(&id).map(|username| TimelineActor {
            id,
            username: username.clone(),
        })
    }

    fn has_event(&self, kind: &str) -> bool {
        self.persisted_events
            .iter()
            .any(|(event, _)| event.event_type == kind)
    }

    fn has_resource_event(&self, kind: &str, field: &str, id: i64) -> bool {
        self.persisted_events.iter().any(|(event, metadata)| {
            event.event_type == kind
                && metadata.get(field).and_then(|value| value.as_i64()) == Some(id)
        })
    }

    fn push_opened(&mut self, pr: &pull_request::Model) {
        if self.has_event("pull_request_opened") {
            return;
        }
        let actor = self.actor(pr.author_id);
        self.timeline.push(ReviewTimelineEvent {
            id: format!("pr:{}:opened", pr.id),
            kind: "pull_request_opened".to_string(),
            actor,
            created_at: pr.created_at,
            body: pr.body.clone(),
            metadata: serde_json::json!({"title": pr.title, "head_sha": pr.head_sha}),
        });
    }

    fn push_reviews(&mut self, reviews: Vec<pr_review::Model>) {
        for review in reviews {
            let kind = format!("review_{}", review.action);
            if self.has_resource_event(&kind, "review_id", review.id) {
                continue;
            }
            let actor = self.actor(review.reviewer_id);
            self.timeline.push(ReviewTimelineEvent {
                id: format!("review:{}", review.id),
                kind,
                actor,
                created_at: review.created_at,
                body: review.body,
                metadata: serde_json::json!({"commit_id": review.commit_id}),
            });
        }
    }

    fn push_comments(&mut self, comments: Vec<review_comment::Model>) {
        for comment in comments {
            let comment_kind = if comment.reply_to_id.is_some() {
                "review_reply"
            } else if comment.suggestion.is_some() {
                "code_suggestion"
            } else {
                "review_comment"
            };
            if !self.has_resource_event(comment_kind, "comment_id", comment.id) {
                let actor = self.actor(comment.author_id);
                self.timeline.push(ReviewTimelineEvent {
                    id: format!("comment:{}", comment.id),
                    kind: comment_kind.to_string(),
                    actor,
                    created_at: comment.created_at,
                    body: Some(comment.body.clone()),
                    metadata: serde_json::json!({
                        "comment_id": comment.id,
                        "path": comment.path,
                        "start_line": comment.start_line,
                        "line": comment.line,
                        "side": comment.side,
                        "reply_to_id": comment.reply_to_id
                    }),
                });
            }
            if let (Some(applied_at), Some(applied_by_id)) = (
                comment.suggestion_applied_at,
                comment.suggestion_applied_by_id,
            ) {
                if !self.has_resource_event("suggestion_applied", "comment_id", comment.id) {
                    let actor = self.actor(applied_by_id);
                    self.timeline.push(ReviewTimelineEvent {
                        id: format!("comment:{}:applied", comment.id),
                        kind: "suggestion_applied".to_string(),
                        actor,
                        created_at: applied_at,
                        body: None,
                        metadata: serde_json::json!({
                            "comment_id": comment.id,
                            "commit_sha": comment.suggestion_commit_sha
                        }),
                    });
                }
            }
            if let (Some(resolved_at), Some(resolved_by_id)) =
                (comment.resolved_at, comment.resolved_by_id)
            {
                if !self.has_resource_event("thread_resolved", "comment_id", comment.id) {
                    let actor = self.actor(resolved_by_id);
                    self.timeline.push(ReviewTimelineEvent {
                        id: format!("comment:{}:resolved", comment.id),
                        kind: "thread_resolved".to_string(),
                        actor,
                        created_at: resolved_at,
                        body: None,
                        metadata: serde_json::json!({"comment_id": comment.id}),
                    });
                }
            }
        }
    }

    fn push_reviewer_requests(&mut self, reviewer_requests: Vec<pr_reviewer_request::Model>) {
        for request in reviewer_requests {
            if self.has_resource_event("reviewer_requested", "request_id", request.id) {
                continue;
            }
            let actor = self.actor(request.requested_by_id);
            let reviewer = self.users.get(&request.reviewer_id).cloned();
            self.timeline.push(ReviewTimelineEvent {
                id: format!("reviewer-request:{}", request.id),
                kind: "reviewer_requested".to_string(),
                actor,
                created_at: request.created_at,
                body: None,
                metadata: serde_json::json!({
                    "reviewer_id": request.reviewer_id,
                    "reviewer": reviewer
                }),
            });
        }
    }

    fn push_lifecycle(
        &mut self,
        pr: &pull_request::Model,
        queue_entry: Option<merge_queue_entry::Model>,
    ) {
        if let (Some(enabled_at), Some(enabled_by_id)) =
            (pr.auto_merge_enabled_at, pr.auto_merge_enabled_by_id)
        {
            if !self.has_event("auto_merge_enabled") {
                let actor = self.actor(enabled_by_id);
                self.timeline.push(ReviewTimelineEvent {
                    id: format!("pr:{}:auto-merge", pr.id),
                    kind: "auto_merge_enabled".to_string(),
                    actor,
                    created_at: enabled_at,
                    body: None,
                    metadata: serde_json::json!({"strategy": pr.auto_merge_strategy}),
                });
            }
        }
        if let Some(entry) = queue_entry {
            if !self.has_resource_event("merge_queue_enqueued", "entry_id", entry.id) {
                let actor = self.actor(entry.enqueued_by_id);
                self.timeline.push(ReviewTimelineEvent {
                    id: format!("queue:{}:enqueued", entry.id),
                    kind: "merge_queue_enqueued".to_string(),
                    actor,
                    created_at: entry.created_at,
                    body: None,
                    metadata: serde_json::json!({"strategy": entry.strategy}),
                });
            }
            if let Some(finished_at) = entry.finished_at {
                let kind = format!("merge_queue_{}", entry.status);
                if !self.has_resource_event(&kind, "entry_id", entry.id) {
                    self.timeline.push(ReviewTimelineEvent {
                        id: format!("queue:{}:{}", entry.id, entry.status),
                        kind,
                        actor: None,
                        created_at: finished_at,
                        body: entry.failure_reason,
                        metadata: serde_json::json!({}),
                    });
                }
            }
        }
        if let Some(closed_at) = pr.closed_at {
            let kind = if pr.state == "merged" {
                "pull_request_merged"
            } else {
                "pull_request_closed"
            };
            if !self.has_event(kind) {
                self.timeline.push(ReviewTimelineEvent {
                    id: format!("pr:{}:{}", pr.id, pr.state),
                    kind: kind.to_string(),
                    actor: None,
                    created_at: closed_at,
                    body: None,
                    metadata: serde_json::json!({
                        "strategy": pr.merge_strategy,
                        "commit_sha": pr.merge_commit_sha
                    }),
                });
            }
        }
    }

    /// Append the raw persisted events, then sort the whole timeline.
    fn finish(mut self) -> Vec<ReviewTimelineEvent> {
        let persisted_events = std::mem::take(&mut self.persisted_events);
        for (event, metadata) in persisted_events {
            let actor = event.actor_id.and_then(|id| self.actor(id));
            self.timeline.push(ReviewTimelineEvent {
                id: format!("event:{}", event.id),
                kind: event.event_type,
                actor,
                created_at: event.created_at,
                body: event.body,
                metadata,
            });
        }
        self.timeline.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        self.timeline
    }
}

/// Create a review comment.
/// POST /api/v1/repos/:owner/:name/pulls/:number/comments
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls/{number}/comments",
    tag = "Reviews",
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
pub async fn create_review_comment(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateReviewCommentRequest>,
) -> impl IntoResponse {
    let (repo_model, user_id) =
        match require_authenticated_read(&state, &headers, &owner, &repo).await {
            Ok(access) => access,
            Err(e) => return e.into_response(),
        };
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(e) => return AppError::from(e).into_response(),
    };
    let review = match req.review_id {
        Some(review_id) => match rg_core::review::service::get_review(&state.db, review_id).await {
            Ok(review) if review.repo_id == repo_model.id && review.pr_id == pr.id => review,
            Ok(_) => return AppError::not_found("review not found").into_response(),
            Err(e) => return AppError::from(e).into_response(),
        },
        None => match rg_core::review::service::submit_review(
            &state.db,
            repo_model.id,
            number,
            user_id,
            rg_core::review::service::ReviewAction::Comment,
            None,
            req.commit_id.clone(),
        )
        .await
        {
            Ok(review) => review,
            Err(e) => return AppError::from(e).into_response(),
        },
    };

    match rg_core::review::service::create_review_comment(
        &state.db,
        repo_model.id,
        number,
        review.id,
        user_id,
        req.path,
        req.line,
        req.start_line,
        req.side,
        req.start_side,
        req.body,
        req.suggestion,
        req.commit_id,
        req.reply_to_id,
    )
    .await
    {
        Ok(comment) => (StatusCode::CREATED, Json(comment)).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply",
    tag = "Reviews",
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
        ("id" = i64, Path),
    ),
    responses((status = 200, body = serde_json::Value), (status = 409, body = serde_json::Value))
)]
pub async fn apply_review_suggestion(
    State(state): State<AppState>,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let (actor_id, actor, pr, source_repo, source_namespace) =
        match require_suggestion_source(&state, &headers, &owner, &repo, number).await {
            Ok(access) => access,
            Err(error) => return error.into_response(),
        };

    match rg_core::review::service::apply_suggestion(
        &state.db,
        &state.repo_root,
        &source_repo,
        &source_namespace,
        &pr,
        id,
        &actor,
    )
    .await
    {
        Ok(applied) => {
            after_suggestions_applied(
                &state,
                actor_id,
                &pr,
                &source_repo,
                &source_namespace,
                &applied.commit_sha,
            )
            .await;
            (StatusCode::OK, Json(applied)).into_response()
        }
        // "Outdated suggestion" and "branch head changed" carry
        // `rg_core::error::Conflict` and still answer 409; the shape checks carry
        // `InvalidRequest` and answer 400. Matching on the rendered message used
        // to decide both, so a reworded string silently reclassified the outcome
        // and a git or database failure was answered as a bad request.
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls/{number}/suggestions/apply",
    tag = "Reviews",
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
    ),
    request_body = ApplySuggestionsRequest,
    responses((status = 200, body = serde_json::Value), (status = 409, body = serde_json::Value))
)]
pub async fn apply_review_suggestions(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ApplySuggestionsRequest>,
) -> impl IntoResponse {
    let (actor_id, actor, pr, source_repo, source_namespace) =
        match require_suggestion_source(&state, &headers, &owner, &repo, number).await {
            Ok(access) => access,
            Err(error) => return error.into_response(),
        };
    match rg_core::review::service::apply_suggestions(
        &state.db,
        &state.repo_root,
        &source_repo,
        &source_namespace,
        &pr,
        &request.comment_ids,
        &actor,
    )
    .await
    {
        Ok(applied) => {
            after_suggestions_applied(
                &state,
                actor_id,
                &pr,
                &source_repo,
                &source_namespace,
                &applied.commit_sha,
            )
            .await;
            (StatusCode::OK, Json(applied)).into_response()
        }
        // "Outdated suggestion" and "branch head changed" carry
        // `rg_core::error::Conflict` and still answer 409; the shape checks carry
        // `InvalidRequest` and answer 400. Matching on the rendered message used
        // to decide both, so a reworded string silently reclassified the outcome
        // and a git or database failure was answered as a bad request.
        Err(error) => AppError::from(error).into_response(),
    }
}

// ── Requested reviewers ──────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/pulls/{number}/reviewers",
    tag = "Reviews",
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
    ),
    responses((status = 200, body = serde_json::Value))
)]
pub async fn list_requested_reviewers(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = require_read(&state, &headers, &owner, &repo).await {
        return error.into_response();
    }
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(e) => return AppError::from(e).into_response(),
    };
    let requests = match rg_db::ops::pr_reviewer_request_ops::list_by_pr(&state.db, pr.id).await {
        Ok(requests) => requests,
        Err(error) => return AppError::from(error).into_response(),
    };

    let mut response = Vec::with_capacity(requests.len());
    for request in requests {
        let username = match rg_db::ops::user_ops::find_by_id(&state.db, request.reviewer_id).await
        {
            Ok(Some(user)) => user.username,
            Ok(None) => continue,
            Err(error) => return AppError::from(error).into_response(),
        };
        response.push(RequestedReviewerResponse {
            id: request.id,
            reviewer_id: request.reviewer_id,
            username,
            requested_by_id: request.requested_by_id,
            created_at: request.created_at,
        });
    }
    (StatusCode::OK, Json(response)).into_response()
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/pulls/{number}/reviewers",
    tag = "Reviews",
    request_body(content = serde_json::Value),
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
    ),
    responses(
        (status = 201, body = serde_json::Value),
        (status = 403, body = serde_json::Value),
        (status = 409, body = serde_json::Value),
    )
)]
pub async fn request_reviewer(
    State(state): State<AppState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RequestReviewerRequest>,
) -> impl IntoResponse {
    let (repo_model, actor_id, pr) =
        match require_pr_manager(&state, &headers, &owner, &repo, number).await {
            Ok(result) => result,
            Err(error) => return error.into_response(),
        };
    let username = body.username.trim();
    if username.is_empty() {
        return AppError::bad_request("reviewer username is required").into_response();
    }
    let reviewer = match rg_db::ops::user_ops::find_by_username(&state.db, username).await {
        Ok(Some(user)) if user.is_active && user.deleted_at.is_none() => user,
        Ok(_) => return AppError::not_found("reviewer not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    if reviewer.id == pr.author_id {
        return AppError::bad_request("the PR author cannot be requested as a reviewer")
            .into_response();
    }
    // This one leaned even further the wrong way: a failed check became a 400,
    // telling the requester their *reviewer* lacks access to the repository —
    // a statement about someone else that we never actually established.
    let reviewer_can_read = match rg_core::repo::service::can_read_repo(
        &state.db,
        &repo_model,
        Some(reviewer.id),
    )
    .await
    {
        Ok(allowed) => allowed,
        Err(error) => return AppError::from(error).into_response(),
    };
    if !reviewer_can_read {
        return AppError::bad_request("reviewer does not have access to this repository")
            .into_response();
    }
    match rg_db::ops::pr_reviewer_request_ops::find(&state.db, pr.id, reviewer.id).await {
        Ok(Some(_)) => return AppError::conflict("reviewer is already requested").into_response(),
        Ok(None) => {}
        Err(error) => return AppError::from(error).into_response(),
    }

    let model = rg_db::entities::pr_reviewer_request::ActiveModel {
        id: sea_orm::NotSet,
        pr_id: sea_orm::Set(pr.id),
        reviewer_id: sea_orm::Set(reviewer.id),
        requested_by_id: sea_orm::Set(actor_id),
        created_at: sea_orm::Set(chrono::Utc::now()),
    };
    match rg_db::ops::pr_reviewer_request_ops::create(&state.db, model).await {
        Ok(request) => {
            if let Err(error) = rg_db::ops::pr_event_ops::record(
                &state.db,
                repo_model.id,
                pr.id,
                Some(actor_id),
                "reviewer_requested",
                None,
                serde_json::json!({
                    "request_id": request.id,
                    "reviewer_id": request.reviewer_id,
                    "reviewer": reviewer.username
                }),
            )
            .await
            {
                return AppError::from(error).into_response();
            }
            (
                StatusCode::CREATED,
                Json(RequestedReviewerResponse {
                    id: request.id,
                    reviewer_id: request.reviewer_id,
                    username: reviewer.username,
                    requested_by_id: request.requested_by_id,
                    created_at: request.created_at,
                }),
            )
                .into_response()
        }
        Err(error) if error.to_string().to_ascii_lowercase().contains("unique") => {
            AppError::conflict("reviewer is already requested").into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/pulls/{number}/reviewers/{username}",
    tag = "Reviews",
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
        ("username" = String, Path),
    ),
    responses((status = 204), (status = 404, body = serde_json::Value))
)]
pub async fn remove_requested_reviewer(
    State(state): State<AppState>,
    Path((owner, repo, number, username)): Path<(String, String, i64, String)>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let (repo_model, actor_id, pr) =
        match require_pr_manager(&state, &headers, &owner, &repo, number).await {
            Ok(result) => result,
            Err(error) => return error.into_response(),
        };
    let reviewer = match rg_db::ops::user_ops::find_by_username(&state.db, &username).await {
        Ok(Some(user)) => user,
        Ok(None) => return AppError::not_found("requested reviewer not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_db::ops::pr_reviewer_request_ops::delete(&state.db, pr.id, reviewer.id).await {
        Ok(0) => AppError::not_found("requested reviewer not found").into_response(),
        Ok(_) => {
            if let Err(error) = rg_db::ops::pr_event_ops::record(
                &state.db,
                repo_model.id,
                pr.id,
                Some(actor_id),
                "reviewer_removed",
                None,
                serde_json::json!({
                    "reviewer_id": reviewer.id,
                    "reviewer": reviewer.username
                }),
            )
            .await
            {
                return AppError::from(error).into_response();
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

// ── Review thread resolution ─────────────────────────────────────────

#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution",
    tag = "Reviews",
    request_body(content = serde_json::Value),
    params(
        ("owner" = String, Path),
        ("name" = String, Path),
        ("number" = i64, Path),
        ("id" = i64, Path),
    ),
    responses((status = 200, body = serde_json::Value), (status = 403, body = serde_json::Value))
)]
pub async fn set_thread_resolution(
    State(state): State<AppState>,
    Path((owner, repo, number, comment_id)): Path<(String, String, i64, i64)>,
    headers: axum::http::HeaderMap,
    Json(body): Json<SetThreadResolutionRequest>,
) -> impl IntoResponse {
    let (repo_model, actor_id) =
        match require_authenticated_read(&state, &headers, &owner, &repo).await {
            Ok(result) => result,
            Err(error) => return error.into_response(),
        };
    let pr = match rg_core::pull_request::get_pr(&state.db, &owner, &repo, number).await {
        Ok(pr) => pr,
        Err(e) => return AppError::from(e).into_response(),
    };
    let root = match rg_core::review::service::get_thread_root(&state.db, pr.id, comment_id).await {
        Ok(root) => root,
        Err(e) => return AppError::from(e).into_response(),
    };
    let can_write = match rg_core::repo::service::can_write_repo(
        &state.db,
        &repo_model,
        Some(actor_id),
    )
    .await
    {
        Ok(allowed) => allowed,
        Err(error) => return AppError::from(error).into_response(),
    };
    if root.author_id != actor_id && pr.author_id != actor_id && !can_write {
        return AppError::forbidden(
            "only the thread author, PR author, or a repository writer may resolve this thread",
        )
        .into_response();
    }

    match rg_core::review::service::set_thread_resolved(
        &state.db,
        pr.id,
        root.id,
        actor_id,
        body.resolved,
    )
    .await
    {
        Ok(comment) => (StatusCode::OK, Json(comment)).into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

// ── Extra request types ───────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DismissReviewRequest {
    pub message: String,
}
