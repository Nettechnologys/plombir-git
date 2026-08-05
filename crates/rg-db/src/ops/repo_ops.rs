//! Database operations for repositories.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{sea_query::Query, ActiveValue::Set, *};

use crate::entities::organization_member::{self, Entity as OrgMemberEntity};
use crate::entities::repo_collaborator::{self, Entity as RepoCollaboratorEntity};
use crate::entities::repository::{
    self, ActiveModel as RepoActiveModel, Entity as RepoEntity, Model as Repo,
};
use crate::entities::{
    oci_blob, oci_repository, package, package_file, package_registry, package_version,
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

/// Every non-deleted repo whose `owner_id` is this user, in **both** namespaces.
///
/// The deliberate opposite of [`list_personal_by_owner`]: this one exists for
/// account deletion, where the question is not "what is in this user's
/// namespace" but "what rows does `users.id` reach". `repositories.owner_id`
/// carries `ON DELETE CASCADE`, so an organization repository that still names
/// this account as its owner is destroyed by a `DELETE FROM users` just like a
/// personal one — filtering it out here would hide exactly the row that must
/// stop the deletion.
pub async fn list_active_by_owner_id(db: &DatabaseConnection, owner_id: i64) -> Result<Vec<Repo>> {
    RepoEntity::find()
        .filter(repository::Column::OwnerId.eq(owner_id))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_asc(repository::Column::Id)
        .all(db)
        .await
        .context("db: list repos by owner id")
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
        .order_by_asc(repository::Column::Name)
        .order_by_asc(repository::Column::Id);

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
        .order_by_asc(repository::Column::Name)
        .order_by_asc(repository::Column::Id);

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
///
/// `updated_at` is the loosest sort key in this file: a bulk import or a
/// migration touches many repositories in the same instant, so the `/explore`
/// feed is exactly where ties cluster. Without the `id` tiebreaker the two
/// halves of a tie are ordered however the engine scans, and paging over that
/// shows one repository on two pages while another appears on none.
pub async fn list_public_paginated(
    db: &DatabaseConnection,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Repo>, i64)> {
    let base = RepoEntity::find()
        .filter(repository::Column::IsPrivate.eq(false))
        .filter(repository::Column::DeletedAt.is_null())
        .order_by_desc(repository::Column::UpdatedAt)
        .order_by_desc(repository::Column::Id);

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
    let backend = db.get_database_backend();
    db.execute(Statement::from_sql_and_values(
        backend,
        crate::prepare_sql(
            backend,
            "UPDATE repositories \
             SET stars_count = (SELECT COUNT(*) FROM repo_stars WHERE repo_id = ?) \
             WHERE id = ?",
        ),
        [id.into(), id.into()],
    ))
    .await
    .context("db: update stars count")?;
    Ok(())
}

fn forks_count_refresh_sql(backend: DatabaseBackend) -> &'static str {
    match backend {
        // MySQL rejects a single-table UPDATE that reads its target table in a
        // subquery (error 1093). GROUP BY makes this derived table
        // non-mergeable, so the multi-table UPDATE reads a materialized count.
        // LEFT JOIN is deliberate: with no live forks there is no grouped row,
        // and the cached count still has to be reset to zero.
        DatabaseBackend::MySql => {
            "UPDATE repositories AS target \
             LEFT JOIN ( \
                 SELECT origin_repo_id, COUNT(*) AS fork_count \
                 FROM repositories \
                 WHERE origin_repo_id = ? AND deleted_at IS NULL \
                 GROUP BY origin_repo_id \
             ) AS counts ON counts.origin_repo_id = target.id \
             SET target.forks_count = COALESCE(counts.fork_count, 0) \
             WHERE target.id = ?"
        }
        DatabaseBackend::Postgres | DatabaseBackend::Sqlite => {
            "UPDATE repositories \
             SET forks_count = ( \
                 SELECT COUNT(*) \
                 FROM repositories AS forks \
                 WHERE forks.origin_repo_id = ? AND forks.deleted_at IS NULL \
             ) \
             WHERE id = ?"
        }
    }
}

async fn update_forks_count_after<F>(
    db: &DatabaseConnection,
    id: i64,
    before_statement: F,
) -> Result<()>
where
    F: std::future::Future<Output = ()>,
{
    let backend = db.get_database_backend();
    // Production passes a ready future. The controlled race test stops here:
    // this is the last instant before the statement reads and writes the count.
    before_statement.await;
    db.execute(Statement::from_sql_and_values(
        backend,
        crate::prepare_sql(backend, forks_count_refresh_sql(backend)),
        [id.into(), id.into()],
    ))
    .await
    .context("db: update forks count")?;
    Ok(())
}

/// Refresh `forks_count` from the live fork rows in one database statement.
pub async fn update_forks_count(db: &DatabaseConnection, id: i64) -> Result<()> {
    update_forks_count_after(db, id, std::future::ready(())).await
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
        .order_by_asc(repository::Column::CreatedAt)
        .order_by_asc(repository::Column::Id);
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

/// Move a repository row and the namespace-bearing metadata of its registries
/// in one database transaction.
///
/// Package and OCI objects are addressed by `owner/repository` even though the
/// rows that describe them hang off the stable repository id.  Their bytes are
/// moved by `rg-core` before this transaction; rewriting only `repositories`
/// would leave the metadata reading the old keys.  Conversely, a failed
/// metadata rewrite must roll the repository-row update back so `rg-core` can
/// safely return every storage namespace to its source.
#[allow(clippy::too_many_arguments)]
pub async fn transfer_owner(
    db: &DatabaseConnection,
    repo_id: i64,
    owner_id: i64,
    org_id: Option<i64>,
    source_namespace: &str,
    destination_namespace: &str,
    repo_name: &str,
) -> Result<()> {
    let transaction = db.begin().await.context("db: begin repository transfer")?;

    let repo = RepoEntity::find_by_id(repo_id)
        .one(&transaction)
        .await?
        .context("repo not found")?;
    let mut active: RepoActiveModel = repo.into();
    active.owner_id = Set(owner_id);
    active.org_id = Set(org_id);
    active.updated_at = Set(Utc::now());
    active
        .update(&transaction)
        .await
        .context("db: update repo owner")?;

    let source_package_prefix = format!("packages/{source_namespace}/{repo_name}/");
    let destination_package_prefix = format!("packages/{destination_namespace}/{repo_name}/");
    let registries = package_registry::Entity::find()
        .filter(package_registry::Column::RepoId.eq(repo_id))
        .all(&transaction)
        .await?;
    for registry in registries {
        let packages = package::Entity::find()
            .filter(package::Column::PackageRegistryId.eq(registry.id))
            .all(&transaction)
            .await?;
        for package_model in packages {
            let package_id = package_model.id;
            let mut active: package::ActiveModel = package_model.into();
            active.owner_id = Set(owner_id);
            active.update(&transaction).await?;

            let versions = package_version::Entity::find()
                .filter(package_version::Column::PackageId.eq(package_id))
                .all(&transaction)
                .await?;
            for version in versions {
                let files = package_file::Entity::find()
                    .filter(package_file::Column::VersionId.eq(version.id))
                    .all(&transaction)
                    .await?;
                for file in files {
                    let Some(suffix) = file
                        .storage_path
                        .strip_prefix(&source_package_prefix)
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    let mut active: package_file::ActiveModel = file.into();
                    active.storage_path = Set(format!("{destination_package_prefix}{suffix}"));
                    active.update(&transaction).await?;
                }
            }
        }
    }

    if let Some(oci_repo) = oci_repository::Entity::find()
        .filter(oci_repository::Column::RepoId.eq(repo_id))
        .one(&transaction)
        .await?
    {
        let source_oci_prefix = format!("oci/{source_namespace}/{repo_name}/");
        let destination_oci_prefix = format!("oci/{destination_namespace}/{repo_name}/");
        let oci_repo_id = oci_repo.id;
        let mut active: oci_repository::ActiveModel = oci_repo.into();
        active.namespace = Set(format!("{destination_namespace}/{repo_name}"));
        active.owner_id = Set(owner_id);
        active.updated_at = Set(Utc::now());
        active.update(&transaction).await?;

        let blobs = oci_blob::Entity::find()
            .filter(oci_blob::Column::OciRepositoryId.eq(oci_repo_id))
            .all(&transaction)
            .await?;
        for blob in blobs {
            let Some(suffix) = blob
                .storage_path
                .strip_prefix(&source_oci_prefix)
                .map(str::to_owned)
            else {
                continue;
            };
            let mut active: oci_blob::ActiveModel = blob.into();
            active.storage_path = Set(format!("{destination_oci_prefix}{suffix}"));
            active.update(&transaction).await?;
        }
    }

    transaction
        .commit()
        .await
        .context("db: commit repository transfer")?;
    Ok(())
}

#[cfg(test)]
mod forks_count_tests {
    use super::*;

    struct TempDb {
        path: std::path::PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            Self {
                path: std::env::temp_dir().join(format!(
                    "forgekeep-forks-count-race-{}.db",
                    uuid::Uuid::new_v4().simple()
                )),
            }
        }

        fn url(&self) -> String {
            format!("sqlite://{}?mode=rwc", self.path.display())
        }
    }

    impl Drop for TempDb {
        #[allow(
            clippy::let_underscore_must_use,
            reason = "cleanup must not mask the assertion that failed the test"
        )]
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
            }
        }
    }

    async fn setup() -> (DatabaseConnection, TempDb) {
        let temp = TempDb::new();
        let db = crate::connect_with_pool(&temp.url(), crate::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to throwaway database");
        crate::run_migrations(&db).await.expect("run migrations");
        (db, temp)
    }

    fn repo(owner_id: i64, name: &str, origin_repo_id: Option<i64>) -> RepoActiveModel {
        let now = Utc::now();
        RepoActiveModel {
            id: NotSet,
            owner_id: Set(owner_id),
            name: Set(name.to_string()),
            description: Set(None),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            origin_repo_id: Set(origin_repo_id),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refresh_started_first_can_write_last_without_restoring_its_old_snapshot() {
        let (db, _temp) = setup().await;
        let owner = crate::ops::user_ops::create_user(
            &db,
            "fork-owner",
            "fork-owner@example.invalid",
            "",
            "Fork Owner",
        )
        .await
        .expect("create owner");
        let source = create(&db, repo(owner.id, "source", None))
            .await
            .expect("create source repository");
        create(&db, repo(owner.id, "fork-one", Some(source.id)))
            .await
            .expect("create first fork");

        let (at_statement_tx, at_statement_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let first_db = db.clone();
        let source_id = source.id;
        let first = tokio::spawn(async move {
            update_forks_count_after(&first_db, source_id, async move {
                at_statement_tx
                    .send(())
                    .expect("test still waits for the first refresh");
                release_rx.await.expect("release the first refresh");
            })
            .await
        });

        at_statement_rx
            .await
            .expect("the first refresh reached its statement");
        create(&db, repo(owner.id, "fork-two", Some(source.id)))
            .await
            .expect("create second fork while the first refresh waits");
        update_forks_count(&db, source.id)
            .await
            .expect("the newer refresh writes two");

        release_tx.send(()).expect("the first refresh still waits");
        first
            .await
            .expect("first refresh task did not panic")
            .expect("the first refresh writes last");

        let source = find_by_id(&db, source.id)
            .await
            .expect("read source repository")
            .expect("source repository exists");
        let live_forks = RepoEntity::find()
            .filter(repository::Column::OriginRepoId.eq(Some(source.id)))
            .filter(repository::Column::DeletedAt.is_null())
            .count(&db)
            .await
            .expect("count live forks") as i64;
        assert_eq!(live_forks, 2);
        assert_eq!(
            source.forks_count, live_forks,
            "the refresh that started first also writes last; if it holds COUNT(*) \
             in application memory while waiting, it restores 1 over the newer 2"
        );
    }
}
