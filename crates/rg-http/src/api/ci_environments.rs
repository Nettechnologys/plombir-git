use crate::agent_scope::TokenGrant;
use crate::api::access_audit::{grant_actor, named_grant_list, record_grant};
use crate::api::repo_access::{self, RepoAdmin, RepoAuthRead, RepoRead};
use crate::api::user_ref::{name_allow_list, resolve_allow_list, AllowedUser};
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use sea_orm::{NotSet, Set};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct EnvironmentRequest {
    pub name: String,
    #[serde(default)]
    pub protected: bool,
    #[serde(default = "default_required_approvals")]
    pub required_approvals: i32,
    /// The approvers as ids — what a client written before names were accepted
    /// still sends. See [`allowed_approvers`](Self::allowed_approvers).
    #[serde(default)]
    pub allowed_approver_ids: Option<Vec<i64>>,
    /// The same approvers, named: a `username` or a bare id (an e-mail is refused, see `user_ref`), one
    /// entry per person. This is the field the settings form fills.
    ///
    /// Approving a deployment *is* handing out access, and this route asked for
    /// it in numbers the person filling the form has no endpoint to look up —
    /// the third and last target of `user_grants::Target` to be left that way
    /// (card_1614d7e0612a).
    #[serde(default)]
    pub allowed_approvers: Option<Vec<String>>,
}
fn default_required_approvals() -> i32 {
    1
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnvironmentResponse {
    pub id: i64,
    pub name: String,
    pub protected: bool,
    pub required_approvals: i32,
    pub allowed_approver_ids: Vec<i64>,
    /// The same list with each approver named, for a screen that has nowhere to
    /// look an id up. See [`AllowedUser`].
    #[schema(value_type = Vec<serde_json::Value>)]
    pub allowed_approvers: Vec<AllowedUser>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
/// Decode the stored approver allow-list of one environment.
///
/// `NULL` is a configured absence — no allow-list — and reads as an empty vec.
/// A present-but-undecodable column is a broken row, and must not read as one
/// either: `[]` would tell the operator the environment has no approvers, and
/// the obvious next move (`GET`, edit a field, `PUT` the object back) would
/// then write that emptiness over a list that was merely unreadable.
fn decode_allowed_approver_ids(
    model: &rg_db::entities::ci_environment::Model,
) -> Result<Vec<i64>, AppError> {
    let Some(json) = model.allowed_approver_ids.as_deref() else {
        return Ok(Vec::new());
    };
    serde_json::from_str(json).map_err(|error| {
        tracing::error!(
            environment_id = model.id,
            error = %error,
            "stored allowed_approver_ids is not a JSON array of user ids"
        );
        AppError::internal("stored environment approver list is unreadable")
    })
}

async fn response(
    db: &rg_db::DatabaseConnection,
    model: rg_db::entities::ci_environment::Model,
) -> Result<EnvironmentResponse, AppError> {
    let allowed_approver_ids = decode_allowed_approver_ids(&model)?;
    let allowed_approvers = name_allow_list(db, &allowed_approver_ids)
        .await
        .map_err(AppError::from)?;
    Ok(EnvironmentResponse {
        id: model.id,
        allowed_approver_ids,
        allowed_approvers,
        name: model.name,
        protected: model.protected,
        required_approvals: model.required_approvals,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && !name.chars().any(char::is_control)
}
fn validate_request(body: &EnvironmentRequest) -> Result<(), AppError> {
    if !valid_name(body.name.trim()) {
        return Err(AppError::bad_request(
            "environment name must contain 1-255 printable characters",
        ));
    }
    if !(1..=10).contains(&body.required_approvals) {
        return Err(AppError::bad_request(
            "required_approvals must be between 1 and 10",
        ));
    }
    Ok(())
}

/// The approver list this request names, resolved to the ids it is stored as.
///
/// Both `POST` and `PUT` carry the whole environment, so a body naming no
/// approvers means "no allow-list" — every repository admin approves — and not
/// "leave the stored one alone". That is why the `None` [`resolve_allow_list`]
/// answers for a body carrying neither field becomes an empty list here.
async fn requested_approvers(
    db: &rg_db::DatabaseConnection,
    body: &EnvironmentRequest,
) -> Result<Vec<i64>, AppError> {
    Ok(resolve_allow_list(
        db,
        body.allowed_approvers.as_deref(),
        body.allowed_approver_ids.clone(),
    )
    .await
    .map_err(AppError::from)?
    .unwrap_or_default())
}

/// An environment cannot demand more approvals than it has approvers to give
/// them.
///
/// Counted on the *resolved* list, not on what the body spelled: two entries
/// naming one person are one approver, and `user_grants::replace` stores them
/// as one grant — so counting the raw entries would let `required_approvals: 2`
/// be accepted for a single approver, and the environment would then wait for
/// an approval nobody can supply.
fn validate_approver_count(body: &EnvironmentRequest, approvers: &[i64]) -> Result<(), AppError> {
    let mut unique = approvers.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if body.protected && !unique.is_empty() && body.required_approvals as usize > unique.len() {
        return Err(AppError::bad_request(
            "required approvals exceed the approver list",
        ));
    }
    Ok(())
}

/// What the journal records about a deployment environment.
///
/// The approver list is written out whole and by name: approving a protected
/// environment is a grant like any other, and "who could release to production
/// on the 14th" must be answerable from one row.
fn environment_details(response: &EnvironmentResponse) -> serde_json::Value {
    serde_json::json!({
        "environment": response.name,
        "protected": response.protected,
        "required_approvals": response.required_approvals,
        "allowed_approvers": named_grant_list(&response.allowed_approvers),
    })
}

fn grant_write_app_error(error: anyhow::Error) -> AppError {
    match rg_db::user_grants::invalid_principal_message(&error) {
        Some(message) => AppError::bad_request(message),
        None => AppError::from(error),
    }
}

fn grant_write_error(error: anyhow::Error) -> axum::response::Response {
    grant_write_app_error(error).into_response()
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/actions/environments", tag = "CI/CD", responses((status = 200, body = [EnvironmentResponse])))]
pub async fn list(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoRead { repo }: RepoRead,
) -> impl IntoResponse {
    let items = match rg_db::ops::ci_environment_ops::list(&state.db, repo.id).await {
        Ok(items) => items,
        Err(error) => return AppError::from(error).into_response(),
    };
    let mut named = Vec::with_capacity(items.len());
    for item in items {
        match response(&state.db, item).await {
            Ok(item) => named.push(item),
            Err(error) => return error.into_response(),
        }
    }
    Json(named).into_response()
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/actions/environments", tag = "CI/CD", request_body = EnvironmentRequest, responses((status = 201, body = EnvironmentResponse)))]
pub async fn create(
    State(state): State<AppState>,
    Path((owner, _)): Path<(String, String)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
    Json(body): Json<EnvironmentRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = validate_request(&body) {
        return error.into_response();
    }
    let approvers = match requested_approvers(&state.db, &body).await {
        Ok(approvers) => approvers,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = validate_approver_count(&body, &approvers) {
        return error.into_response();
    }
    let now = chrono::Utc::now();
    let model = rg_db::entities::ci_environment::ActiveModel {
        id: NotSet,
        repo_id: Set(repo.id),
        name: Set(body.name.trim().to_string()),
        protected: Set(body.protected),
        required_approvals: Set(body.required_approvals),
        allowed_approver_ids: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    match rg_db::ops::ci_environment_ops::create_with_approvers(&state.db, model, approvers).await {
        Ok(model) => match response(&state.db, model).await {
            Ok(body) => {
                record_grant(
                    &state,
                    &audit_actor,
                    "repo.environment_create",
                    &owner,
                    &repo,
                    &headers,
                    environment_details(&body),
                )
                .await;
                (StatusCode::CREATED, Json(body)).into_response()
            }
            Err(error) => error.into_response(),
        },
        // `(repo_id, name)` is UNIQUE (`uq_ci_environments_repo_name`), and
        // there is no pre-check: a second environment of the same name lands
        // here every time, not just on a race.
        Err(error) if rg_db::is_unique_violation_anyhow(&error) => {
            AppError::conflict("environment already exists").into_response()
        }
        Err(error) => grant_write_error(error),
    }
}

#[utoipa::path(put, path = "/repos/{owner}/{name}/actions/environments/{id}", tag = "CI/CD", request_body = EnvironmentRequest, responses((status = 200, body = EnvironmentResponse)))]
pub async fn update(
    State(state): State<AppState>,
    Path((owner, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
    Json(body): Json<EnvironmentRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = validate_request(&body) {
        return error.into_response();
    }
    let approvers = match requested_approvers(&state.db, &body).await {
        Ok(approvers) => approvers,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = validate_approver_count(&body, &approvers) {
        return error.into_response();
    }
    let model = match environment_in_repo(&state, repo.id, id).await {
        Ok(model) => model,
        Err(error) => return error.into_response(),
    };
    let db = state.db.clone();
    match update_after_read(
        &db,
        model,
        body,
        approvers,
        || async { Ok(()) },
        |model| async move {
            let body = response(&state.db, model).await?;
            record_grant(
                &state,
                &audit_actor,
                "repo.environment_update",
                &owner,
                &repo,
                &headers,
                environment_details(&body),
            )
            .await;
            Ok(Json(body).into_response())
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// Testable boundary between the repository-scoped read and the transactional
/// environment/grant publication.
///
/// `publish` contains response construction and the audit write in production.
/// It is deliberately reached only after the conditional update and both grant
/// representations have committed, so a losing DELETE cannot leave a journal
/// entry confirming a write that never happened.
async fn update_after_read<F, Fut, P, PublishFut, T>(
    db: &rg_db::DatabaseConnection,
    existing: rg_db::entities::ci_environment::Model,
    body: EnvironmentRequest,
    approvers: Vec<i64>,
    after_read: F,
    publish: P,
) -> Result<T, AppError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), AppError>>,
    P: FnOnce(rg_db::entities::ci_environment::Model) -> PublishFut,
    PublishFut: std::future::Future<Output = Result<T, AppError>>,
{
    after_read().await?;

    let updated = match rg_db::ops::ci_environment_ops::update_with_approvers(
        db,
        existing.id,
        existing.repo_id,
        body.name.trim().to_string(),
        body.protected,
        body.required_approvals,
        chrono::Utc::now(),
        approvers,
    )
    .await
    {
        Ok(Some(model)) => model,
        Ok(None) => return Err(AppError::not_found("environment not found")),
        // Renaming onto a name a sibling environment already holds.
        Err(error) if rg_db::is_unique_violation_anyhow(&error) => {
            return Err(AppError::conflict("environment already exists"))
        }
        Err(error) => return Err(grant_write_app_error(error)),
    };

    publish(updated).await
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/actions/environments/{id}", tag = "CI/CD", responses((status = 204)))]
pub async fn delete(
    State(state): State<AppState>,
    Path((owner, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    // The scoping lookup already reads the row, and its name is what the journal
    // entry is about: deleting a protected environment removes the approval gate
    // in front of a deploy target, and "environment #4 was deleted" does not say
    // which target that was.
    let removed = match environment_in_repo(&state, repo.id, id).await {
        Ok(model) => model,
        Err(error) => return error.into_response(),
    };
    match rg_db::ops::ci_environment_ops::has_jobs(&state.db, id).await {
        Ok(true) => {
            return AppError::conflict("environment is referenced by pipeline history")
                .into_response()
        }
        Ok(false) => {}
        Err(error) => return AppError::from(error).into_response(),
    }
    match rg_db::ops::ci_environment_ops::delete(&state.db, id).await {
        Ok(true) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.environment_delete",
                &owner,
                &repo,
                &headers,
                serde_json::json!({
                    "environment": removed.name,
                    "protected": removed.protected,
                }),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        // The scoping lookup and the job check above are separate statements
        // from the DELETE. A request that removed nothing did not delete the
        // environment, and answers like a request for one that is not there.
        Ok(false) => AppError::not_found("environment not found").into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// Fetch a CI environment and re-anchor it to the repository the caller was
/// authorized for.
///
/// `{id}` is a global `ci_environments` primary key while `RepoAdmin` only ever
/// proves something about `{owner}/{name}`, so administering one repository must
/// not reach another one's environments — which is where deployment secrets and
/// the approver list live. A mismatch answers 404 rather than 403: a 403 would
/// still confirm the id exists, which is most of what an id-walking caller wants
/// to learn.
///
/// `update` and `delete` each spelled this comparison inline, and
/// [`authorize_approval`] spelled a third copy. A named helper is the form
/// `global_id_anchor_guard` can read — a comparison is not.
async fn environment_in_repo(
    state: &AppState,
    repo_id: i64,
    environment_id: i64,
) -> Result<rg_db::entities::ci_environment::Model, AppError> {
    match rg_db::ops::ci_environment_ops::find_by_id(&state.db, environment_id).await {
        Ok(Some(model)) if model.repo_id == repo_id => Ok(model),
        Ok(_) => Err(AppError::not_found("environment not found")),
        Err(error) => Err(AppError::from(error)),
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApprovalResponse {
    pub job_id: i64,
    pub approvals: u64,
    pub required_approvals: i32,
    pub released: bool,
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/pipelines/{pipeline_id}/jobs/{job_id}/approve", tag = "CI/CD", responses((status = 200, body = ApprovalResponse), (status = 403, description = "A human approver with access is required", body = serde_json::Value)))]
pub async fn approve(
    State(state): State<AppState>,
    Path((owner, _, pipeline_id, job_id)): Path<(String, String, i64, i64)>,
    RepoAuthRead { repo, actor_id }: RepoAuthRead,
    headers: HeaderMap,
    grant: Option<Extension<TokenGrant>>,
) -> impl IntoResponse {
    if let Some(Extension(grant)) = grant.filter(|Extension(grant)| grant.owner().is_bot()) {
        return grant
            .deny(
                &headers,
                "a person must approve a protected environment",
                serde_json::json!({
                    "reason": "human_approval_required",
                    "action": "environment_deploy",
                    "repo_id": repo.id,
                    "pipeline_id": pipeline_id,
                    "job_id": job_id,
                }),
            )
            .await
            .into_response();
    }
    let ctx = match authorize_approval(&state, repo, actor_id, pipeline_id, job_id).await {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    let newly_approved = match rg_db::ops::ci_environment_ops::add_approval(
        &state.db,
        job_id,
        ctx.environment.id,
        ctx.actor_id,
    )
    .await
    {
        Ok(newly_approved) => newly_approved,
        Err(error) => return AppError::from(error).into_response(),
    };
    let approvals = match authorized_approvals(&state, &ctx, job_id).await {
        Ok(count) => count,
        Err(error) => return error.into_response(),
    };
    let release_ready = approvals >= ctx.environment.required_approvals as u64;
    if !newly_approved && !release_ready {
        return AppError::conflict("user already approved this job").into_response();
    }
    let released = if release_ready {
        match release_if_ready(&state, &ctx, &owner, pipeline_id, job_id).await {
            Ok(released) => released,
            Err(response) => return response,
        }
    } else {
        false
    };
    Json(ApprovalResponse {
        job_id,
        approvals,
        required_approvals: ctx.environment.required_approvals,
        released,
    })
    .into_response()
}

/// The repo/stage/environment context needed to record an approval, plus the
/// authenticated approver, resolved after all authorization checks pass.
struct ApprovalContext {
    repo: rg_db::entities::repository::Model,
    actor_id: i64,
    stage: rg_db::entities::pipeline_stage::Model,
    environment: rg_db::entities::ci_environment::Model,
}

/// Load the pipeline/job/stage/environment for an approval request and enforce
/// that the caller is allowed to approve it. Errors are already `Response`s.
async fn authorize_approval(
    state: &AppState,
    repo: rg_db::entities::repository::Model,
    actor_id: i64,
    pipeline_id: i64,
    job_id: i64,
) -> Result<ApprovalContext, axum::response::Response> {
    // The pipeline half of the anchor is `api/ci.rs`'s rule verbatim, so it is
    // that file's helper rather than a fourth copy of the comparison — the copy
    // that used to sit here is what its doc comment means by "the gate was
    // copied into the next module".
    let pipeline = crate::api::ci::pipeline_in_repo(state, &repo, pipeline_id)
        .await
        .map_err(IntoResponse::into_response)?;
    let job = match rg_db::ops::pipeline_ops::get_job(&state.db, job_id).await {
        Ok(Some(job)) => job,
        Ok(None) => return Err(AppError::not_found("job not found").into_response()),
        Err(error) => return Err(AppError::from(error).into_response()),
    };
    let stage = match rg_db::ops::pipeline_ops::get_stage_by_id(&state.db, job.stage_id).await {
        Ok(Some(stage)) if stage.pipeline_id == pipeline_id => stage,
        Ok(_) => return Err(AppError::not_found("job not found").into_response()),
        Err(error) => return Err(AppError::from(error).into_response()),
    };
    // Already approved, already rejected, or never protected: the request is
    // well-formed and authorized, and it is the job's state that has no
    // approval left to give. 409 sends the client to re-read that state
    // instead of to re-examine a request with nothing wrong in it.
    if job.status != "waiting_approval" || pipeline.status != "waiting_approval" {
        return Err(AppError::conflict("job is not awaiting environment approval").into_response());
    }
    let environment_id = match job.environment_id {
        Some(id) => id,
        // The job says it is waiting for approval and carries no environment to
        // be approved for: nothing the caller sent produced that, it is our own
        // rows disagreeing with each other. 409 puts it next to the branch
        // above — same request, same authorization, a state that admits no
        // approval — instead of telling the caller to fix a correct request.
        None => return Err(AppError::conflict("job has no protected environment").into_response()),
    };
    let environment = match environment_in_repo(state, repo.id, environment_id).await {
        // Gone, belonging to another repository, or no longer protected: the
        // job is waiting on an environment that cannot admit it, which makes
        // the request stale rather than the resource missing — so not the
        // helper's 404. Stale is precisely what 409 names, though: the caller
        // is to re-read the state, not to re-examine a request that was never
        // malformed.
        Ok(environment) if environment.protected => environment,
        Ok(_) | Err(AppError::NotFound(_)) => {
            return Err(AppError::conflict("protected environment no longer exists").into_response())
        }
        Err(error) => return Err(error.into_response()),
    };
    let is_admin = match repo_access::may_admin(state, &repo, Some(actor_id)).await {
        Ok(value) => value,
        Err(error) => return Err(error.into_response()),
    };
    // The allow-list is read only where it decides, and an unreadable one is a
    // broken row rather than "nobody is an approver": answering the old
    // `403 user is not an allowed environment approver` would tell an approver
    // who *is* listed that they are not.
    if !is_admin {
        let allowed =
            match rg_db::ops::ci_environment_ops::allowed_approver_ids(&state.db, &environment)
                .await
            {
                Ok(allowed) => allowed,
                Err(error) => return Err(AppError::from(error).into_response()),
            };
        if !allowed.contains(&actor_id) {
            return Err(
                AppError::forbidden("user is not an allowed environment approver").into_response(),
            );
        }
    }
    Ok(ApprovalContext {
        repo,
        actor_id,
        stage,
        environment,
    })
}

/// Count the approvals on `job_id` whose approvers may approve this
/// environment *now*.
///
/// An approval row records that somebody was allowed to say yes when they said
/// it. Removing them from the allow-list, or taking away the administration
/// that let them approve without one, revokes the right — and must revoke the
/// vote still waiting for the threshold with it, or a revoked approver plus one
/// newcomer would release a deployment neither alone could (card_aa1d374901e3).
/// So each vote is judged by `authorize_approval`'s rule as it stands, the way
/// branch protection re-checks write access for a review approval at merge
/// time. The rows themselves stay, as history.
async fn authorized_approvals(
    state: &AppState,
    ctx: &ApprovalContext,
    job_id: i64,
) -> Result<u64, AppError> {
    let approvers = rg_db::ops::ci_environment_ops::live_approver_ids(&state.db, job_id).await?;
    // Read only once a non-administrator's vote needs it, as in
    // `authorize_approval`: an unreadable list must not fail a release that
    // administrators alone carry.
    let mut allowed = None;
    let mut counted = 0;
    for approver in approvers {
        if repo_access::may_admin(state, &ctx.repo, Some(approver)).await? {
            counted += 1;
            continue;
        }
        if allowed.is_none() {
            allowed = Some(
                rg_db::ops::ci_environment_ops::allowed_approver_ids(&state.db, &ctx.environment)
                    .await?,
            );
        }
        if allowed
            .as_ref()
            .is_some_and(|allowed| allowed.contains(&approver))
        {
            counted += 1;
        }
    }
    Ok(counted)
}

/// Once enough approvals exist, release the job and — if the whole stage is now
/// unblocked — resume the pipeline. Returns whether the job itself was released.
async fn release_if_ready(
    state: &AppState,
    ctx: &ApprovalContext,
    owner: &str,
    pipeline_id: i64,
    job_id: i64,
) -> Result<bool, axum::response::Response> {
    // Resolve the only fallible spawn prerequisite before the transaction.
    // Once the gate is released the same approval request must not be needed
    // to repair an internal-runner handoff.
    let storage_owner = resolve_storage_owner(state, &ctx.repo, owner).await?;
    let repo_path = state
        .repo_root
        .join(format!("{storage_owner}/{}.git", ctx.repo.name));
    let release = match rg_db::ops::pipeline_ops::release_approved_job_and_resume_approval_chain(
        &state.db,
        pipeline_id,
        ctx.stage.id,
        job_id,
    )
    .await
    {
        Ok(release) => release,
        Err(error) => return Err(AppError::from(error).into_response()),
    };
    if release.resumed_pipeline {
        if let Err(error) = state
            .ci_engine
            .resume_pipeline(rg_core::ci::ResumePipelineParams {
                db: &state.db,
                repo_path: &repo_path,
                repo_id: ctx.repo.id,
                pipeline_id,
                docker_enabled: state.docker_enabled,
                external_runners: state.external_runners,
                allow_host_runner: state.allow_host_runner,
                jwt_secret: Some(&state.jwt_secret),
                encryption_key: Some(&state.encryption_key),
                external_url: state.external_url.as_deref(),
            })
            .await
        {
            return Err(AppError::from(error).into_response());
        }
    }
    Ok(release.released)
}

/// Resolve the on-disk storage owner for a repo: the route owner for org repos,
/// otherwise the owning user's username.
async fn resolve_storage_owner(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    owner: &str,
) -> Result<String, axum::response::Response> {
    if repo.org_id.is_some() {
        return Ok(owner.to_string());
    }
    match rg_db::ops::user_ops::find_by_id(&state.db, repo.owner_id).await {
        Ok(Some(user)) => Ok(user.username),
        Ok(None) => Err(AppError::internal("repository owner not found").into_response()),
        Err(error) => Err(AppError::from(error).into_response()),
    }
}

#[cfg(test)]
mod update_delete_tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use super::*;
    use sea_orm::{ColumnTrait, EntityTrait, NotSet, PaginatorTrait, QueryFilter, Set};

    async fn fixture() -> (
        tempfile::TempDir,
        rg_db::DatabaseConnection,
        rg_db::entities::ci_environment::Model,
        i64,
    ) {
        let directory = tempfile::tempdir().expect("tempdir");
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("ci-environment-race.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            4,
        )
        .await
        .expect("connect sqlite");
        rg_db::run_migrations(&db).await.expect("run migrations");

        let owner = rg_db::ops::user_ops::create_user(
            &db,
            "environment-race-owner",
            "environment-race-owner@example.invalid",
            "",
            "Environment Owner",
        )
        .await
        .expect("create owner");
        let replacement = rg_db::ops::user_ops::create_user(
            &db,
            "environment-race-approver",
            "environment-race-approver@example.invalid",
            "",
            "Replacement Approver",
        )
        .await
        .expect("create replacement approver");
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("environment-race-repo".to_string()),
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
        let environment = rg_db::ops::ci_environment_ops::create_with_approvers(
            &db,
            rg_db::entities::ci_environment::ActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                name: Set("production".to_string()),
                protected: Set(true),
                required_approvals: Set(1),
                allowed_approver_ids: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            },
            vec![owner.id],
        )
        .await
        .expect("create protected environment");

        (directory, db, environment, replacement.id)
    }

    fn update_request(replacement_id: i64) -> EnvironmentRequest {
        EnvironmentRequest {
            name: "production-renamed".to_string(),
            protected: true,
            required_approvals: 1,
            allowed_approver_ids: Some(vec![replacement_id]),
            allowed_approvers: None,
        }
    }

    #[tokio::test]
    async fn delete_after_the_scoped_read_is_404_and_publishes_neither_grants_nor_audit() {
        let (_directory, db, environment, replacement_id) = fixture().await;
        let environment_id = environment.id;
        let published = Arc::new(AtomicBool::new(false));
        let publication = Arc::clone(&published);
        let (_, audit_before) =
            rg_db::ops::audit_log_ops::list_paginated(&db, 0, 10, None, None, None, None, None)
                .await
                .expect("count audit rows before the losing update");

        let error = update_after_read(
            &db,
            environment,
            update_request(replacement_id),
            vec![replacement_id],
            || async {
                assert!(
                    rg_db::ops::ci_environment_ops::delete(&db, environment_id)
                        .await
                        .map_err(AppError::from)?,
                    "the injected DELETE must be the writer that removed the row"
                );
                Ok(())
            },
            move |_| {
                let publication = Arc::clone(&publication);
                async move {
                    publication.store(true, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
        .expect_err("a DELETE that wins after the scoped read must abort publication");

        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert!(
            !published.load(Ordering::SeqCst),
            "the success continuation contains the audit write and must not run"
        );
        assert!(
            rg_db::ops::ci_environment_ops::find_by_id(&db, environment_id)
                .await
                .expect("look for a resurrected environment")
                .is_none(),
            "the losing update must not recreate the deleted environment"
        );
        assert_eq!(
            rg_db::entities::ci_environment_approver_grant::Entity::find()
                .filter(
                    rg_db::entities::ci_environment_approver_grant::Column::EnvironmentId
                        .eq(environment_id),
                )
                .count(&db)
                .await
                .expect("count grants left for the deleted environment"),
            0,
            "neither the old nor requested approver set may survive the DELETE"
        );
        let (_, audit_after) =
            rg_db::ops::audit_log_ops::list_paginated(&db, 0, 10, None, None, None, None, None)
                .await
                .expect("count audit rows after the losing update");
        assert_eq!(
            audit_after, audit_before,
            "the losing PUT wrote an audit row"
        );
    }

    #[tokio::test]
    async fn a_successful_update_commits_the_model_and_both_grant_representations_before_publish() {
        let (_directory, db, environment, replacement_id) = fixture().await;
        let published = Arc::new(AtomicBool::new(false));
        let publication = Arc::clone(&published);

        let updated = update_after_read(
            &db,
            environment,
            update_request(replacement_id),
            vec![replacement_id],
            || async { Ok(()) },
            move |model| {
                let publication = Arc::clone(&publication);
                async move {
                    publication.store(true, Ordering::SeqCst);
                    Ok(model)
                }
            },
        )
        .await
        .expect("update the environment and its approver set");

        assert!(published.load(Ordering::SeqCst));
        assert_eq!(updated.name, "production-renamed");
        assert_eq!(
            rg_db::ops::ci_environment_ops::allowed_approver_ids(&db, &updated)
                .await
                .expect("load and cross-check both grant representations"),
            vec![replacement_id]
        );
    }

    #[test]
    fn protected_environment_still_rejects_more_required_approvals_than_approvers() {
        let body = EnvironmentRequest {
            required_approvals: 2,
            ..update_request(7)
        };
        let error = validate_approver_count(&body, &[7])
            .expect_err("one approver cannot satisfy two required approvals");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }
}
