//! Database operations for repositories.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{sea_query::Query, ActiveValue::Set, *};

use crate::entities::organization_member::{self, Entity as OrgMemberEntity};
use crate::entities::repo_collaborator::{self, Entity as RepoCollaboratorEntity};
use crate::entities::repository::{
    self, ActiveModel as RepoActiveModel, Entity as RepoEntity, Model as Repo,
};

/// Count non-deleted repositories — backs the `forgekeep_repositories` gauge.
pub async fn count_non_deleted(db: &DatabaseConnection) -> Result<u64> {
    RepoEntity::find()
        .filter(repository::Column::DeletedAt.is_null())
        .count(db)
        .await
        .context("db: count non-deleted repositories")
}

/// Find a non-deleted repository by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Repo>> {
    RepoEntity::find_by_id(id)
        .filter(repository::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find repo by id")
}

/// Find a repo in a user's **personal** namespace — `owner_id` *and* no
/// organization. Excludes soft-deleted repos.
///
/// The `org_id IS NULL` half is the point. A repository belongs to exactly one
/// namespace, a user account or an organization, but the row hangs off a user
/// either way: an organization's repository carries `owner_id = org.owner_id`
/// (see `rg_core::repo::service::resolve_owner`). So a filter on `owner_id`
/// alone answers "anything in this account *or* in any organization this
/// account owns", which made `/{org}/{repo}` and `/{org-owner}/{repo}` resolve
/// to the same row and made "is this name taken?" reach across the namespace
/// boundary (card_92019cc97dcd).
///
/// For the organization side use [`find_by_org_and_name`]; to resolve an
/// `owner/name` pair where `owner` may be either, use
/// `rg_core::repo::service::find_repo_by_owner_name`.
pub async fn find_personal_by_owner_and_name(
    db: &DatabaseConnection,
    owner_id: i64,
    name: &str,
) -> Result<Option<Repo>> {
    RepoEntity::find()
        .filter(repository::Column::OwnerId.eq(owner_id))
        .filter(repository::Column::OrgId.is_null())
        .filter(repository::Column::Name.eq(name))
        .filter(repository::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find personal repo by owner and name")
}

/// Any repo named `name` hanging off `owner_id`'s account — **both** namespaces
/// at once, the personal one and every organization this account owns.
///
/// This is deliberately not a namespace lookup and is almost never what a
/// caller wants; [`find_personal_by_owner_and_name`] is. It exists for one
/// reason: `repositories` still carries the `UNIQUE (owner_id, name)` table
/// constraint from its first migration, so the *table* cannot yet hold a
/// personal `alice/db` next to an `acme/db` owned by alice even though those
/// are different namespaces. Callers use this to answer that collision as a
/// `400` naming the real cause instead of letting the insert surface as a 5xx.
/// It goes away with the constraint (card_615e00843297).
pub async fn find_in_owner_account_by_name(
    db: &DatabaseConnection,
    owner_id: i64,
    name: &str,
) -> Result<Option<Repo>> {
    RepoEntity::find()
        .filter(repository::Column::OwnerId.eq(owner_id))
        .filter(repository::Column::Name.eq(name))
        .filter(repository::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find repo by owner account and name")
}

/// List all non-deleted repos in a user's **personal** namespace.
///
/// `org_id IS NULL` for the same reason as
/// [`find_personal_by_owner_and_name`]: without it, the repositories of every
/// organization this user owns are listed as if they were their own.
pub async fn list_personal_by_owner(db: &DatabaseConnection, owner_id: i64) -> Result<Vec<Repo>> {
    RepoEntity::find()
        .filter(repository::Column::OwnerId.eq(owner_id))
        .filter(repository::Column::OrgId.is_null())
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_asc(repository::Column::Name)
        .all(db)
        .await
        .context("db: list personal repos by owner")
}

/// The repos `viewer_id` (`None` = anonymous) is allowed to see: every public
/// one, plus the private ones they own, collaborate on, or reach through the
/// owning organization.
///
/// This mirrors `rg_core::repo::service::can_read_repo` — the two must stay in
/// step. It lives here rather than in the handler because a listing has to
/// filter *before* LIMIT/OFFSET: dropping invisible rows after the query would
/// hand the caller short pages and a total that counts repos they cannot see.
fn visible_to(viewer_id: Option<i64>) -> Condition {
    let visible = Condition::any().add(repository::Column::IsPrivate.eq(false));

    let Some(viewer) = viewer_id else {
        return visible;
    };

    visible
        .add(repository::Column::OwnerId.eq(viewer))
        .add(
            repository::Column::Id.in_subquery(
                Query::select()
                    .column(repo_collaborator::Column::RepoId)
                    .from(RepoCollaboratorEntity)
                    .and_where(repo_collaborator::Column::UserId.eq(viewer))
                    .to_owned(),
            ),
        )
        .add(
            repository::Column::OrgId.in_subquery(
                Query::select()
                    .column(organization_member::Column::OrgId)
                    .from(OrgMemberEntity)
                    .and_where(organization_member::Column::UserId.eq(viewer))
                    .to_owned(),
            ),
        )
}

/// Paginated list of non-deleted repos in a user's **personal** namespace that
/// `viewer_id` may see.
///
/// `org_id IS NULL` is the namespace half and is separate from `visible_to`:
/// that one answers "may this viewer see the row", this one "does the row
/// belong to the account being listed". An organization's repository hangs off
/// its owner's `owner_id`, so without the filter `GET /repos/{org-owner}`
/// advertised the organization's repositories as the owner's own
/// (card_92019cc97dcd). They are listed by `GET /repos/{org}`, which goes
/// through [`list_by_org_visible_to`].
pub async fn list_personal_by_owner_visible_to(
    db: &DatabaseConnection,
    owner_id: i64,
    viewer_id: Option<i64>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Repo>, i64)> {
    let base = RepoEntity::find()
        .filter(repository::Column::OwnerId.eq(owner_id))
        .filter(repository::Column::OrgId.is_null())
        .filter(repository::Column::DeletedAt.is_null())
        .filter(visible_to(viewer_id))
        .order_by_asc(repository::Column::Name);

    let total = base
        .clone()
        .count(db)
        .await
        .context("db: count personal repos by owner")? as i64;
    let repos = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list personal repos by owner (paginated)")?;

    Ok((repos, total))
}

/// Find a repo by (org_id, name). Excludes soft-deleted repos.
pub async fn find_by_org_and_name(
    db: &DatabaseConnection,
    org_id: i64,
    name: &str,
) -> Result<Option<Repo>> {
    RepoEntity::find()
        .filter(repository::Column::OrgId.eq(org_id))
        .filter(repository::Column::Name.eq(name))
        .filter(repository::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find repo by org and name")
}

/// List all non-deleted repos belonging to an organization.
pub async fn list_by_org(db: &DatabaseConnection, org_id: i64) -> Result<Vec<Repo>> {
    RepoEntity::find()
        .filter(repository::Column::OrgId.eq(org_id))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_asc(repository::Column::Name)
        .all(db)
        .await
        .context("db: list repos by org")
}

/// Paginated list of non-deleted org repos that `viewer_id` may see.
pub async fn list_by_org_visible_to(
    db: &DatabaseConnection,
    org_id: i64,
    viewer_id: Option<i64>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Repo>, i64)> {
    let base = RepoEntity::find()
        .filter(repository::Column::OrgId.eq(org_id))
        .filter(repository::Column::DeletedAt.is_null())
        .filter(visible_to(viewer_id))
        .order_by_asc(repository::Column::Name);

    let total = base
        .clone()
        .count(db)
        .await
        .context("db: count repos by org")? as i64;
    let repos = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list repos by org (paginated)")?;

    Ok((repos, total))
}

/// Paginated list of public, non-deleted repos — ordered by recently updated.
pub async fn list_public_paginated(
    db: &DatabaseConnection,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Repo>, i64)> {
    let base = RepoEntity::find()
        .filter(repository::Column::IsPrivate.eq(false))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_desc(repository::Column::UpdatedAt);

    let total = base
        .clone()
        .count(db)
        .await
        .context("db: count public repos")? as i64;
    let repos = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list public repos (paginated)")?;

    Ok((repos, total))
}

/// Create a new repo.
pub async fn create(db: &DatabaseConnection, model: RepoActiveModel) -> Result<Repo> {
    model.insert(db).await.context("db: create repo")
}

/// Delete a repo by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    RepoEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete repo")?;
    Ok(())
}

/// Soft-delete a repository (set deleted_at timestamp).
pub async fn soft_delete(db: &DatabaseConnection, id: i64) -> Result<()> {
    let repo = RepoEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find repo for soft delete")?
        .ok_or_else(|| anyhow::anyhow!("repository not found"))?;

    let mut model: RepoActiveModel = repo.into();
    model.deleted_at = Set(Some(Utc::now()));
    model.update(db).await.context("db: soft delete repo")?;

    Ok(())
}

/// Update stars_count for a repository based on actual star count (atomic).
pub async fn update_stars_count(db: &DatabaseConnection, id: i64) -> Result<()> {
    let count = crate::ops::repo_star_ops::count_by_repo(db, id).await?;
    let backend = db.get_database_backend();
    db.execute(Statement::from_sql_and_values(
        backend,
        crate::prepare_sql(
            backend,
            "UPDATE repositories SET stars_count = ? WHERE id = ?",
        ),
        [count.into(), id.into()],
    ))
    .await
    .context("db: update stars count")?;
    Ok(())
}

/// Update forks_count for a repository based on actual fork count (atomic).
pub async fn update_forks_count(db: &DatabaseConnection, id: i64) -> Result<()> {
    use crate::entities::repository::Column;
    let count = RepoEntity::find()
        .filter(Column::OriginRepoId.eq(Some(id)))
        .filter(Column::DeletedAt.is_null())
        .count(db)
        .await
        .context("db: count forks")? as i64;
    let backend = db.get_database_backend();
    db.execute(Statement::from_sql_and_values(
        backend,
        crate::prepare_sql(
            backend,
            "UPDATE repositories SET forks_count = ? WHERE id = ?",
        ),
        [count.into(), id.into()],
    ))
    .await
    .context("db: update forks count")?;
    Ok(())
}

/// List all forks of a repo.
pub async fn list_forks(
    db: &DatabaseConnection,
    origin_repo_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Repo>, i64)> {
    let base = RepoEntity::find()
        .filter(repository::Column::OriginRepoId.eq(Some(origin_repo_id)))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_asc(repository::Column::CreatedAt);
    let total = base.clone().count(db).await.context("db: count forks")? as i64;
    let repos = base
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list forks")?;
    Ok((repos, total))
}

/// Update repo owner (for transfer).
pub async fn update_owner(
    db: &DatabaseConnection,
    repo_id: i64,
    owner_id: i64,
    org_id: Option<i64>,
) -> Result<()> {
    let repo = RepoEntity::find_by_id(repo_id)
        .one(db)
        .await?
        .context("repo not found")?;
    let mut active: RepoActiveModel = repo.into();
    active.owner_id = Set(owner_id);
    active.org_id = Set(org_id);
    active.updated_at = Set(Utc::now());
    active.update(db).await.context("db: update repo owner")?;
    Ok(())
}
