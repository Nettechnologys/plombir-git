use super::repo_access::{RepoAdmin, RepoRead};
use crate::api::access_audit::{grant_actor, named_grant_list, record_grant};
use crate::api::user_ref::{name_allow_list, resolve_allow_list, AllowedUser};
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use rg_git::protocol::receive_pack::validate_tag_protection_pattern;
use sea_orm::{NotSet, Set};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTagProtectionRequest {
    pub pattern: String,
    /// The exceptions as ids — what a client written before names were
    /// accepted still sends. See [`allowed_users`](Self::allowed_users).
    pub allowed_user_ids: Option<Vec<i64>>,
    /// The same exceptions, named: a `username`, an e-mail, or a bare id, one
    /// entry per person. This is the field the settings form fills.
    pub allowed_users: Option<Vec<String>>,
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTagProtectionRequest {
    /// See [`CreateTagProtectionRequest::allowed_user_ids`]. Optional only so
    /// that the named field can stand in for it — a body carrying neither is
    /// refused, because the allow-list is the only thing this route updates and
    /// a missing one would make the call a no-op reported as `200`.
    #[serde(default)]
    pub allowed_user_ids: Option<Vec<i64>>,
    /// See [`CreateTagProtectionRequest::allowed_users`].
    #[serde(default)]
    pub allowed_users: Option<Vec<String>>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct TagProtectionResponse {
    pub id: i64,
    pub pattern: String,
    pub allowed_user_ids: Vec<i64>,
    /// The same list with each person named, for a screen that has nowhere to
    /// look an id up. See [`AllowedUser`].
    #[schema(value_type = Vec<serde_json::Value>)]
    pub allowed_users: Vec<AllowedUser>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Decode the stored allow-list of one tag-protection rule.
///
/// `NULL` is a configured absence — no allow-list — and reads as an empty vec.
/// A present-but-undecodable column must not read as one too: `[]` would show
/// the operator a rule that admits nobody, and a `GET` → edit → `PATCH` round
/// trip would then persist that emptiness over a list that was only unreadable.
fn decode_allowed_user_ids(
    model: &rg_db::entities::protected_tag::Model,
) -> Result<Vec<i64>, AppError> {
    let Some(json) = model.allowed_user_ids.as_deref() else {
        return Ok(Vec::new());
    };
    serde_json::from_str(json).map_err(|error| {
        tracing::error!(
            protected_tag_id = model.id,
            error = %error,
            "stored allowed_user_ids is not a JSON array of user ids"
        );
        AppError::internal("stored tag protection allow-list is unreadable")
    })
}

async fn response(
    db: &rg_db::DatabaseConnection,
    model: rg_db::entities::protected_tag::Model,
) -> Result<TagProtectionResponse, AppError> {
    let allowed_user_ids = decode_allowed_user_ids(&model)?;
    let allowed_users = name_allow_list(db, &allowed_user_ids)
        .await
        .map_err(AppError::from)?;
    Ok(TagProtectionResponse {
        id: model.id,
        allowed_user_ids,
        allowed_users,
        pattern: model.pattern,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

/// The allow-list this request names, refusing a body that names none.
///
/// `PATCH` updates the allow-list and nothing else, so a body with neither
/// field is not "leave it alone" — it is a request with no content, and
/// answering it `200` would tell the operator a change was made.
async fn requested_allow_list(
    db: &rg_db::DatabaseConnection,
    named: Option<&[String]>,
    ids: Option<Vec<i64>>,
) -> Result<Vec<i64>, AppError> {
    resolve_allow_list(db, named, ids)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::bad_request("allowed_users or allowed_user_ids is required"))
}
/// What the journal records about a tag protection rule.
///
/// The exception list is written out whole and by name for the same reason the
/// branch rule's is: "who could move `v*` on the 14th" has to be answerable from
/// one row rather than by replaying every edit before it.
fn tag_protection_details(response: &TagProtectionResponse) -> serde_json::Value {
    serde_json::json!({
        "pattern": response.pattern,
        "allowed_users": named_grant_list(&response.allowed_users),
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

/// Testable boundary between the repository-scoped read and the transactional
/// tag/grant publication.
///
/// `publish` contains response construction and the audit write in production,
/// so a DELETE that wins before the conditional update cannot publish a journal
/// entry confirming a write that never happened.
async fn update_after_read<F, Fut, P, PublishFut, T>(
    db: &rg_db::DatabaseConnection,
    mut model: rg_db::entities::protected_tag::Model,
    allowed_user_ids: Vec<i64>,
    after_read: F,
    publish: P,
) -> Result<T, AppError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), AppError>>,
    P: FnOnce(rg_db::entities::protected_tag::Model) -> PublishFut,
    PublishFut: std::future::Future<Output = Result<T, AppError>>,
{
    after_read().await?;
    model.updated_at = chrono::Utc::now();

    let updated =
        match rg_db::ops::protected_tag_ops::update_with_push_grants(db, model, allowed_user_ids)
            .await
        {
            Ok(Some(updated)) => updated,
            Ok(None) => return Err(AppError::not_found("tag protection not found")),
            Err(error) => return Err(grant_write_app_error(error)),
        };

    publish(updated).await
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/tags/protection", tag = "Tag Protection", params(("owner" = String, Path), ("name" = String, Path)), responses((status = 200, body = [TagProtectionResponse])))]
pub async fn list(State(state): State<AppState>, RepoRead { repo }: RepoRead) -> impl IntoResponse {
    let items = match rg_db::ops::protected_tag_ops::list_by_repo(&state.db, repo.id).await {
        Ok(items) => items,
        Err(e) => return AppError::from(e).into_response(),
    };
    let mut named = Vec::with_capacity(items.len());
    for item in items {
        match response(&state.db, item).await {
            Ok(item) => named.push(item),
            Err(e) => return e.into_response(),
        }
    }
    (StatusCode::OK, Json(named)).into_response()
}

#[utoipa::path(post, path = "/repos/{owner}/{name}/tags/protection", tag = "Tag Protection", request_body = CreateTagProtectionRequest, params(("owner" = String, Path), ("name" = String, Path)), responses((status = 201, body = TagProtectionResponse)))]
pub async fn create(
    State(state): State<AppState>,
    Path((owner, _)): Path<(String, String)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
    Json(body): Json<CreateTagProtectionRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let pattern = body.pattern.trim();
    if let Err(error) = validate_tag_protection_pattern(pattern) {
        return AppError::bad_request(error.to_string()).into_response();
    }
    let now = chrono::Utc::now();
    let model = rg_db::entities::protected_tag::ActiveModel {
        id: NotSet,
        repo_id: Set(repo.id),
        pattern: Set(pattern.to_owned()),
        allowed_user_ids: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let allowed_user_ids = match resolve_allow_list(
        &state.db,
        body.allowed_users.as_deref(),
        body.allowed_user_ids,
    )
    .await
    {
        Ok(ids) => ids,
        Err(e) => return AppError::from(e).into_response(),
    };
    match rg_db::ops::protected_tag_ops::create_with_push_grants(&state.db, model, allowed_user_ids)
        .await
    {
        Ok(v) => match response(&state.db, v).await {
            Ok(body) => {
                record_grant(
                    &state,
                    &audit_actor,
                    "repo.tag_protection_create",
                    &owner,
                    &repo,
                    &headers,
                    tag_protection_details(&body),
                )
                .await;
                (StatusCode::CREATED, Json(body)).into_response()
            }
            Err(e) => e.into_response(),
        },
        // `(repo_id, pattern)` is unique and nothing pre-checks it, so every
        // repeat of an existing pattern arrives here.
        Err(e) if rg_db::is_unique_violation_anyhow(&e) => {
            AppError::conflict("tag protection pattern already exists").into_response()
        }
        Err(e) => grant_write_error(e),
    }
}

#[utoipa::path(patch, path = "/repos/{owner}/{name}/tags/protection/{id}", tag = "Tag Protection", request_body = UpdateTagProtectionRequest, params(("owner" = String, Path), ("name" = String, Path), ("id" = i64, Path)), responses((status = 200, body = TagProtectionResponse)))]
pub async fn update(
    State(state): State<AppState>,
    Path((owner, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
    Json(body): Json<UpdateTagProtectionRequest>,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let model = match tag_protection_in_repo(&state, repo.id, id).await {
        Ok(model) => model,
        Err(e) => return e.into_response(),
    };
    let allowed_user_ids = match requested_allow_list(
        &state.db,
        body.allowed_users.as_deref(),
        body.allowed_user_ids,
    )
    .await
    {
        Ok(ids) => ids,
        Err(e) => return e.into_response(),
    };
    let db = state.db.clone();
    match update_after_read(
        &db,
        model,
        allowed_user_ids,
        || async { Ok(()) },
        |model| async move {
            let body = response(&state.db, model).await?;
            record_grant(
                &state,
                &audit_actor,
                "repo.tag_protection_update",
                &owner,
                &repo,
                &headers,
                tag_protection_details(&body),
            )
            .await;
            Ok((StatusCode::OK, Json(body)).into_response())
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/tags/protection/{id}", tag = "Tag Protection", params(("owner" = String, Path), ("name" = String, Path), ("id" = i64, Path)), responses((status = 204), (status = 404, description = "No such rule, or it belongs to another repository", body = serde_json::Value)))]
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
    // The scoping lookup already reads the rule, and the pattern it protected is
    // the whole content of the journal entry — after the delete there is nothing
    // left to name it by.
    let removed = match tag_protection_in_repo(&state, repo.id, id).await {
        Ok(model) => model,
        Err(e) => return e.into_response(),
    };
    // That lookup and this `DELETE` are two statements, so a concurrent delete
    // can land in between; the 204 therefore comes from `rows_affected` rather
    // than from the row having existed a moment ago.
    match rg_db::ops::protected_tag_ops::delete_by_id(&state.db, id).await {
        Ok(true) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.tag_protection_delete",
                &owner,
                &repo,
                &headers,
                serde_json::json!({ "pattern": removed.pattern }),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => AppError::not_found("tag protection rule not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Fetch a tag protection rule and re-anchor it to the repository the caller
/// was authorized for.
///
/// `{id}` is a global `protected_tags` primary key while `RepoAdmin` only ever
/// proves something about `{owner}/{name}`, so administering one repository
/// must not reach another one's rules. A mismatch answers 404 rather than 403:
/// a 403 would still confirm the id exists, which is most of what an id-walking
/// caller wants to learn.
///
/// `update` and `delete` each spelled this comparison inline. A named helper is
/// the form `global_id_anchor_guard` can read — a comparison is not — so a third
/// route that forgets the anchor now fails the build rather than review.
async fn tag_protection_in_repo(
    state: &AppState,
    repo_id: i64,
    protection_id: i64,
) -> Result<rg_db::entities::protected_tag::Model, AppError> {
    match rg_db::ops::protected_tag_ops::find_by_id(&state.db, protection_id).await {
        Ok(Some(v)) if v.repo_id == repo_id => Ok(v),
        Ok(_) => Err(AppError::not_found("tag protection not found")),
        Err(e) => Err(AppError::from(e)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use super::*;
    use sea_orm::{ColumnTrait, EntityTrait, NotSet, PaginatorTrait, QueryFilter, Set};

    async fn update_fixture() -> (
        tempfile::TempDir,
        rg_db::DatabaseConnection,
        rg_db::entities::protected_tag::Model,
        i64,
    ) {
        let directory = tempfile::tempdir().expect("tempdir");
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("protected-tag-race.db").display()
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
            "protected-tag-race-owner",
            "protected-tag-race-owner@example.invalid",
            "",
            "Tag Owner",
        )
        .await
        .expect("create owner");
        let replacement = rg_db::ops::user_ops::create_user(
            &db,
            "protected-tag-race-replacement",
            "protected-tag-race-replacement@example.invalid",
            "",
            "Replacement Pusher",
        )
        .await
        .expect("create replacement pusher");
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("protected-tag-race-repo".to_string()),
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
        let protection = rg_db::ops::protected_tag_ops::create_with_push_grants(
            &db,
            rg_db::entities::protected_tag::ActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                pattern: Set("v*".to_string()),
                allowed_user_ids: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            },
            Some(vec![owner.id]),
        )
        .await
        .expect("create protected tag");

        (directory, db, protection, replacement.id)
    }

    #[tokio::test]
    async fn delete_after_the_scoped_read_is_404_and_publishes_neither_grants_nor_audit() {
        let (_directory, db, protection, replacement_id) = update_fixture().await;
        let protection_id = protection.id;
        let published = Arc::new(AtomicBool::new(false));
        let publication = Arc::clone(&published);

        let error = update_after_read(
            &db,
            protection,
            vec![replacement_id],
            || async {
                assert!(
                    rg_db::ops::protected_tag_ops::delete_by_id(&db, protection_id)
                        .await
                        .map_err(AppError::from)?,
                    "the injected DELETE must remove the protected tag"
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
            rg_db::ops::protected_tag_ops::find_by_id(&db, protection_id)
                .await
                .expect("look for a resurrected protection")
                .is_none(),
            "the losing PATCH must not recreate the deleted protection"
        );
        assert_eq!(
            rg_db::entities::protected_tag_push_grant::Entity::find()
                .filter(
                    rg_db::entities::protected_tag_push_grant::Column::ProtectedTagId
                        .eq(protection_id),
                )
                .count(&db)
                .await
                .expect("count grants left for the deleted protection"),
            0,
            "neither the old nor requested grant set may survive the DELETE"
        );
    }

    #[tokio::test]
    async fn a_successful_patch_commits_both_grant_representations_before_publish() {
        let (_directory, db, protection, replacement_id) = update_fixture().await;

        let updated = update_after_read(
            &db,
            protection,
            vec![replacement_id],
            || async { Ok(()) },
            |model| async move { Ok(model) },
        )
        .await
        .expect("update protected tag and grants");

        assert_eq!(
            rg_db::user_grants::load_verified(
                &db,
                rg_db::user_grants::Target::ProtectedTag(updated.id),
                updated.allowed_user_ids.as_deref(),
            )
            .await
            .expect("load and cross-check both tag grant representations"),
            vec![replacement_id]
        );
    }

    #[test]
    fn validates_only_patterns_the_receive_pack_matcher_can_honour() {
        assert!(validate_tag_protection_pattern("v*").is_ok());
        assert!(validate_tag_protection_pattern("release/**").is_ok());
        assert!(validate_tag_protection_pattern("refs/tags/v*").is_err());
        assert!(validate_tag_protection_pattern("bad pattern").is_err());
        assert!(validate_tag_protection_pattern("release.lock").is_err());

        for (pattern, metacharacter) in [("v1.?", '?'), ("v[0-9]*", '['), ("release+", '+')] {
            let message = validate_tag_protection_pattern(pattern)
                .expect_err("unsupported glob syntax must be refused")
                .to_string();
            assert!(message.contains(metacharacter), "{message}");
            assert!(message.contains("only '*' is supported"), "{message}");
        }
    }
}
