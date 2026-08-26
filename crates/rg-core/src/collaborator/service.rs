//! Collaborator service — repo collaborators + permission management.

use anyhow::Result;
use chrono::Utc;
use sea_orm::{DatabaseConnection, Set};

use rg_db::entities::repo_collaborator::{self, Model as RepoCollaborator};
use rg_db::ops::repo_collaborator_ops;

/// Add a collaborator to a repo.
pub async fn add_collaborator(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    user_id: i64,
    permission: String,
) -> Result<RepoCollaborator> {
    let repo = resolve_repo(db, owner, repo_name).await?;

    // Validate permission. Typed like its sibling in `update_permission`: only
    // these two outcomes are the caller's fault, and only they may answer 400 —
    // the `resolve_repo` above and the insert below are ours.
    match permission.as_str() {
        "read" | "write" | "admin" => {}
        _ => {
            return Err(crate::error::invalid_request(format!(
                "invalid permission: {permission}, must be read/write/admin"
            )));
        }
    }

    // Check if already a collaborator.
    //
    // This read is the fast path only — the membership row can appear between
    // here and the insert below, which is why that insert classifies its own
    // failure instead of trusting this answer.
    if let Some(existing) =
        repo_collaborator_ops::find_by_repo_and_user(db, repo.id, user_id).await?
    {
        return Err(already_a_collaborator(user_id, Some(&existing.permission)));
    }

    let model = repo_collaborator::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo.id),
        user_id: Set(user_id),
        permission: Set(permission),
        created_at: Set(Utc::now()),
    };

    let created = match repo_collaborator_ops::create(db, model).await {
        Ok(created) => created,
        // Losing the `idx_repo_collaborators_repo_user` race is the same
        // outcome the read above reports, reached a moment later: someone else
        // added this user first. Left unclassified it was a raw constraint
        // failure, and the handler rendered a 500 for something the caller
        // neither caused nor can fix.
        //
        // Only that one loss is folded — a foreign-key failure or a database
        // outage stays an error, or a broken server would be reported as the
        // client's own conflict.
        //
        // The winner's permission is not knowable from here (this attempt only
        // knows the one it asked for), so it is re-read to answer in the
        // pre-read branch's own words. A membership removed again in the
        // meantime, or a read that fails too, drops that clause rather than
        // turning a settled 409 back into a 500.
        Err(error) if rg_db::is_unique_violation_anyhow(&error) => {
            let winner = repo_collaborator_ops::find_by_repo_and_user(db, repo.id, user_id).await;
            let permission = match &winner {
                Ok(Some(existing)) => Some(existing.permission.as_str()),
                Ok(None) | Err(_) => None,
            };
            return Err(already_a_collaborator(user_id, permission));
        }
        Err(error) => return Err(error),
    };
    crate::repo::service::invalidate_perm_cache_user(db, repo.id, user_id);
    Ok(created)
}

/// The one answer both the pre-read and the losing insert give, so a caller
/// cannot tell which of the two noticed. Carries no constraint or `db:` text —
/// this message reaches the client verbatim.
///
/// A `Conflict`, not an `InvalidRequest`: the permission is one of the three
/// valid ones (the `match` in [`add_collaborator`] owns that, and answers 400)
/// and the user exists — an existing membership row refuses the request.
/// Adding them again is impossible until it is removed, or the permission is
/// changed through `PATCH`, which is what the message points at.
fn already_a_collaborator(user_id: i64, permission: Option<&str>) -> anyhow::Error {
    match permission {
        Some(permission) => crate::error::conflict(format!(
            "user {user_id} is already a collaborator (permission: {permission})"
        )),
        None => crate::error::conflict(format!("user {user_id} is already a collaborator")),
    }
}

/// List all collaborators for a repo.
pub async fn list_collaborators(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<Vec<RepoCollaborator>> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    repo_collaborator_ops::list_by_repo(db, repo.id).await
}

/// Update a collaborator's permission.
///
/// `repo_id` is the repository the caller was authorized against. The
/// collaborator id is a global primary key, so scoping the lookup to it is what
/// stops an admin of one repository from rewriting the access list of another —
/// and a row belonging to a different repo has to be indistinguishable from one
/// that does not exist, or the id space itself becomes an existence oracle.
pub async fn update_permission(
    db: &DatabaseConnection,
    repo_id: i64,
    collaborator_id: i64,
    permission: String,
) -> Result<RepoCollaborator> {
    update_permission_after_read(db, repo_id, collaborator_id, permission, || {
        std::future::ready(Ok(()))
    })
    .await
}

/// Testable boundary between the scoped read and the conditional write.
async fn update_permission_after_read<F, Fut>(
    db: &DatabaseConnection,
    repo_id: i64,
    collaborator_id: i64,
    permission: String,
    after_read: F,
) -> Result<RepoCollaborator>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    match permission.as_str() {
        "read" | "write" | "admin" => {}
        _ => {
            return Err(crate::error::invalid_request(format!(
                "invalid permission: {permission}, must be read/write/admin"
            )));
        }
    }

    let collab = repo_collaborator_ops::find_by_id(db, collaborator_id)
        .await?
        .filter(|collab| collab.repo_id == repo_id)
        .ok_or_else(|| crate::error::not_found("collaborator"))?;

    after_read().await?;

    let updated = repo_collaborator_ops::update_permission(db, collab.id, permission)
        .await?
        .ok_or_else(|| crate::error::not_found("collaborator"))?;
    if updated.changed {
        crate::repo::service::invalidate_perm_cache_user(db, repo_id, updated.collaborator.user_id);
    }
    Ok(updated.collaborator)
}

/// Remove a collaborator from a repo.
///
/// `repo_id` is the repository the caller was authorized against — the same
/// scoping [`update_permission`] gets, and it comes from the same extractor, so
/// the route no longer resolves the repository a second time.
///
/// `user_id` is a `users.id`, *not* the `repo_collaborators.id` that `PATCH` on
/// the very same path expects. That divergence is forced (axum refuses to mount
/// two verbs with differently named segments in one position), so it has to be
/// survivable: a delete that matched no row reports
/// [`crate::error::NotFound`] instead of a silent success. Answering `204` to
/// it made "removed" and "there was nothing to remove — you passed the wrong
/// key, or the row id happened to collide with someone else's `users.id`" the
/// same response.
pub async fn remove_collaborator(
    db: &DatabaseConnection,
    repo_id: i64,
    user_id: i64,
) -> Result<()> {
    if !repo_collaborator_ops::delete_by_repo_and_user(db, repo_id, user_id).await? {
        return Err(crate::error::not_found("collaborator"));
    }
    crate::repo::service::invalidate_perm_cache_user(db, repo_id, user_id);
    Ok(())
}

// `get_effective_permission` used to live here: a second answer to "what may
// this account do with this repository", returned as a bare
// `"admin" | "write" | "read"` string, with no caller anywhere in the tree. The
// live answer is `rg_http::api::repo_access`, which is a layer rather than a
// string — deleted rather than kept for a future caller to find and adopt
// (card_ab36709fa0c7).

// ── Helpers ───────────────────────────────────────────────────────────

async fn resolve_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<rg_db::entities::repository::Model> {
    crate::repo::service::find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| crate::error::not_found("repository"))
}

#[cfg(test)]
mod update_delete_tests {
    use super::*;
    use sea_orm::{NotSet, Set};

    async fn fixture() -> (tempfile::TempDir, DatabaseConnection, i64, i64, i64) {
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
            "collab-race-owner",
            "collab-race-owner@example.invalid",
            "",
            "Owner",
        )
        .await
        .expect("create owner");
        let collaborator = rg_db::ops::user_ops::create_user(
            &db,
            "collab-race-user",
            "collab-race-user@example.invalid",
            "",
            "Collaborator",
        )
        .await
        .expect("create collaborator");
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("collab-race-repo".to_string()),
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
        let row = repo_collaborator_ops::create(
            &db,
            repo_collaborator::ActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                user_id: Set(collaborator.id),
                permission: Set("read".to_string()),
                created_at: Set(now),
            },
        )
        .await
        .expect("create collaborator row");

        (dir, db, repo.id, row.id, collaborator.id)
    }

    #[tokio::test]
    async fn delete_after_the_scoped_read_is_typed_not_found() {
        let (_dir, db, repo_id, collaborator_id, user_id) = fixture().await;

        let error = update_permission_after_read(
            &db,
            repo_id,
            collaborator_id,
            "write".to_string(),
            || async {
                assert!(
                    repo_collaborator_ops::delete_by_repo_and_user(&db, repo_id, user_id)
                        .await
                        .expect("the competing collaborator delete succeeds")
                );
                Ok(())
            },
        )
        .await
        .expect_err("a winning delete must not become a successful permission update");

        let typed = error
            .downcast_ref::<crate::error::NotFound>()
            .expect("the lost race must stay classifiable as HTTP 404");
        assert_eq!(typed.resource, "collaborator");
    }
}
