//! Database operations for repositories.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{
    prelude::DateTimeUtc,
    sea_query::{Expr, Func, OnConflict, Query},
    ActiveValue::Set,
    *,
};

use crate::entities::organization_member::{self, Entity as OrgMemberEntity};
use crate::entities::repo_collaborator::{self, Entity as RepoCollaboratorEntity};
use crate::entities::repository::{
    self, ActiveModel as RepoActiveModel, Entity as RepoEntity, Model as Repo,
};
use crate::entities::{
    oci_blob, oci_repository, organization, package, package_file, package_registry,
    package_version, repository_transfer_lease, user,
};

/// Count non-deleted repositories — backs the `plombir_git_repositories` gauge.
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

/// Every non-deleted repo whose `owner_id` is this user, in **both** namespaces.
///
/// The deliberate opposite of [`list_personal_by_owner_visible_to`]: this one
/// exists for account deletion, where the question is not "what is in this
/// user's namespace" but "what rows does `users.id` reach".
/// `repositories.owner_id`
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
/// Every live repository whose name the git transport cannot address.
///
/// Two shapes, and both are decided in SQL rather than by walking the table:
/// a name ending in `.git` (whatever the case — both transports strip the
/// suffix, so such a repository is asked for under its neighbour's name), and
/// the two path segments `.` and `..`, which resolve away before a request is
/// ever sent. `LOWER(name)` rather than `LIKE`, because `LIKE` is
/// case-insensitive on SQLite and case-sensitive on Postgres and this must
/// answer the same on both.
///
/// Soft-deleted rows are excluded: a repository nobody can reach is not a
/// clone that will hand over the wrong code.
pub async fn list_names_the_transport_cannot_address(
    db: &DatabaseConnection,
) -> Result<Vec<(String, String)>> {
    let rows = RepoEntity::find()
        .filter(repository::Column::DeletedAt.is_null())
        .filter(
            Condition::any()
                .add(Expr::expr(Func::lower(Expr::col(repository::Column::Name))).like("%.git"))
                .add(repository::Column::Name.eq("."))
                .add(repository::Column::Name.eq("..")),
        )
        .find_also_related(user::Entity)
        .all(db)
        .await
        .context("db: list repositories the git transport cannot address")?;

    Ok(rows
        .into_iter()
        .map(|(repo, owner)| {
            let owner = owner
                .map(|owner| owner.username)
                .unwrap_or_else(|| format!("#{}", repo.owner_id));
            (owner, repo.name)
        })
        .collect())
}

/// Every live repository whose name is not ASCII.
///
/// The sibling of [`list_names_the_transport_cannot_address`], for the other
/// ambiguity a name can carry: `payment` and `раyment` (Cyrillic `р`, `а`) are
/// two repositories that render identically, so a link to one reads as a link
/// to the other. `rg_core::validate_repo_name` refuses new ones; this says who
/// was already there.
///
/// The predicate is applied in Rust rather than in SQL on purpose: "contains a
/// code point above U+007F" has no portable spelling across SQLite, Postgres
/// and MySQL, and a query that answers differently per backend is worse than a
/// scan that runs once at boot. Soft-deleted rows are excluded — a repository
/// nobody can reach cannot be mistaken for another one.
pub async fn list_non_ascii_names(db: &DatabaseConnection) -> Result<Vec<(String, String)>> {
    let rows = RepoEntity::find()
        .filter(repository::Column::DeletedAt.is_null())
        .find_also_related(user::Entity)
        .all(db)
        .await
        .context("db: list repositories whose name is not ASCII")?;

    Ok(rows
        .into_iter()
        .filter(|(repo, _)| !repo.name.is_ascii())
        .map(|(repo, owner)| {
            let owner = owner
                .map(|owner| owner.username)
                .unwrap_or_else(|| format!("#{}", repo.owner_id));
            (owner, repo.name)
        })
        .collect())
}

pub async fn create(db: &DatabaseConnection, model: RepoActiveModel) -> Result<Repo> {
    model.insert(db).await.context("db: create repo")
}

/// Point `default_branch` at the branch the repository's Git `HEAD` names.
///
/// The column is written once, by `create_repo`, from what the *request* asked
/// for. That holds for every repository whose history this instance produced —
/// but an import replaces the whole repository with a clone of an upstream, and
/// `git clone --bare` brings the upstream's `HEAD` with it. From that moment the
/// column describes a branch that need not exist (`card_0e4d6e7fcdb2`), and the
/// repository page — which resolves `default_branch` as a ref — answers `404`
/// for a repository holding the full history.
///
/// Returns whether a live row was updated: `false` means the repository was
/// deleted underneath the caller, not that the branch was already right.
pub async fn set_default_branch(db: &DatabaseConnection, id: i64, branch: &str) -> Result<bool> {
    let updated = RepoEntity::update_many()
        .col_expr(repository::Column::DefaultBranch, Expr::value(branch))
        .col_expr(repository::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(repository::Column::Id.eq(id))
        .filter(repository::Column::DeletedAt.is_null())
        .exec(db)
        .await
        .context("db: set repository default branch")?;
    Ok(updated.rows_affected == 1)
}

/// Delete a repo by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    RepoEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete repo")?;
    Ok(())
}

/// Whether the guarded soft-delete committed, or found a writer whose bytes it
/// would have orphaned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepositoryRetirement {
    /// The row is soft-deleted.
    Deleted,
    /// A mirror pass holds this repository's sync lease; nothing was written.
    MirrorSyncInFlight,
}

/// Soft-delete a repository unless a mirror pass is writing its clone directory.
///
/// `delete_repo`'s quiescence gate already reads the same lease twice — once
/// before it stages anything, once after — but a gate and the write it guards
/// are two statements, and a pass that takes the lease between them would spend
/// the next several minutes re-creating `<repo_root>/<repo_id>.mirror` by
/// absolute path, after the deletion had removed it and answered `200`
/// (`card_a1f2a20281af`). The gate narrows that window; only doing the check and
/// the write under one transaction closes it.
///
/// The UPDATE comes *before* the read deliberately, and this is the half that
/// makes the protocol total. It takes the repository row's exclusive lock — the
/// same row [`crate::ops::mirror_ops::bid_for_sync_lease`] reads under
/// `lock_exclusive` before it admits a pass — so the two transactions are
/// ordered by the database rather than by luck, whichever arrives first:
///
/// * this one first ⇒ the bid waits on the row, then reads a soft-deleted
///   repository and declines;
/// * the bid first ⇒ its lease is committed and visible here, and this
///   transaction rolls back;
/// * the bid first but still open ⇒ its lease is invisible here and this commits,
///   after which the bid's locked read finally returns a soft-deleted repository
///   and declines anyway.
///
/// Writing first is also what makes it work on SQLite, where `lock_exclusive` is
/// a no-op: a transaction that reads and then writes leaves a lock-upgrade
/// window, while one whose first statement is a write takes the single writer
/// slot outright — the ordering [`transfer_owner`] and [`bid_for_transfer_lease`]
/// already rely on.
pub async fn soft_delete_unless_mirror_syncing(
    db: &DatabaseConnection,
    id: i64,
    stale_before: DateTimeUtc,
) -> Result<RepositoryRetirement> {
    let transaction = db
        .begin()
        .await
        .context("db: begin guarded repository soft delete")?;

    let updated = RepoEntity::update_many()
        .col_expr(repository::Column::DeletedAt, Expr::value(Some(Utc::now())))
        .filter(repository::Column::Id.eq(id))
        .exec(&transaction)
        .await
        .context("db: soft delete repo")?;
    if updated.rows_affected != 1 {
        transaction
            .rollback()
            .await
            .context("db: roll back a soft delete of a repository that is not there")?;
        return Err(anyhow::anyhow!("repository not found"));
    }

    let syncing = crate::ops::mirror_ops::sync_lease_in_flight(&transaction, id, stale_before)
        .await
        .context("db: check for a mirror sync in flight while soft-deleting a repository")?;
    if syncing.is_some() {
        transaction
            .rollback()
            .await
            .context("db: roll back a soft delete a mirror sync is holding")?;
        return Ok(RepositoryRetirement::MirrorSyncInFlight);
    }

    transaction
        .commit()
        .await
        .context("db: commit guarded repository soft delete")?;
    Ok(RepositoryRetirement::Deleted)
}

/// How many repository ids one `stars_count` refresh statement may name.
///
/// The bound is on the bind markers, not on the work: SQLite builds before
/// 3.32 accept 999 parameters per statement and nothing in this crate pins a
/// newer one. Each batch is still a single statement, so the aggregate is never
/// read in one statement and written in another.
const STARS_COUNT_REFRESH_BATCH: usize = 500;

/// Update stars_count for a repository based on actual star count (atomic).
pub async fn update_stars_count(db: &DatabaseConnection, id: i64) -> Result<()> {
    refresh_stars_counts(db, std::slice::from_ref(&id)).await
}

/// Refresh `stars_count` for several repositories from the live `repo_stars`
/// rows, reading and writing each counter inside one statement.
///
/// Takes any connection so the caller can run it in the same transaction as the
/// delete that changed the rows it counts — which is what
/// [`crate::ops::user_ops::delete_by_id`] does, since the stars it removes sit
/// on repositories belonging to accounts that are not going anywhere.
///
/// The subquery is correlated rather than parameterised so one statement can
/// cover a whole batch. MySQL rejects a subquery that reads the statement's own
/// target table (error 1093); `repositories` appears here only as a correlated
/// column reference, never in the subquery's `FROM`, so all three backends
/// accept it.
pub async fn refresh_stars_counts(db: &impl ConnectionTrait, ids: &[i64]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let backend = db.get_database_backend();
    for batch in ids.chunks(STARS_COUNT_REFRESH_BATCH) {
        let markers = vec!["?"; batch.len()].join(", ");
        let sql = format!(
            "UPDATE repositories \
             SET stars_count = ( \
                 SELECT COUNT(*) FROM repo_stars WHERE repo_stars.repo_id = repositories.id \
             ) \
             WHERE id IN ({markers})"
        );
        db.execute(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(backend, &sql),
            batch.iter().map(|id| (*id).into()).collect::<Vec<Value>>(),
        ))
        .await
        .context("db: update stars count")?;
    }
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

/// How long a transfer lease may go unreleased before another transfer is
/// allowed to take the repository over.
///
/// A transfer that is still moving bytes has not abandoned anything, so this is
/// not a deadline for the move — it is the point past which a *dead* holder must
/// stop blocking the repository, and above all must stop making the namespace it
/// was moving out of impossible to delete. Generous enough that a large
/// repository on slow storage is never timed out, short enough that a crashed
/// process is not a permanent obstruction.
pub const TRANSFER_LEASE_STALE_AFTER: chrono::Duration = chrono::Duration::hours(6);

/// The answer to "may this transfer start moving this repository's storage?".
#[derive(Clone, Debug, PartialEq)]
pub enum TransferLeaseBid {
    /// The caller's token holds the lease and owns the move.
    Granted,
    /// Taken over from a holder whose lease had gone stale.
    TakenOver,
    /// Another transfer is moving this repository right now.
    Busy,
    /// The source account is being retired; its storage is not the caller's to
    /// move.
    SourceAccountClosed,
    /// Likewise for the source organization.
    SourceOrganizationClosed,
    /// The repository row is gone or already soft-deleted.
    RepositoryGone,
}

/// Take the lease that makes a repository's storage move visible to the
/// deletion of the namespace it is moving out of.
///
/// The window this closes is described in
/// `m20260806_000002_create_repository_transfer_lease`: between the first
/// storage move and the ownership commit, the bytes are at the destination while
/// the row still names the source, and nothing the source's deleter can read
/// distinguishes that from storage which was already gone.
///
/// The source lifecycle is verified under the same transaction that takes the
/// lease, so the two orders are both decided rather than raced: a retirement
/// claim that landed first is seen here and the transfer never moves a byte,
/// while a claim that lands afterwards meets the lease in
/// `delete_repo`'s quiescence gate and backs off retryably.
///
/// The insert comes first deliberately. SQLite has no row-level `FOR UPDATE`, so
/// `lock_exclusive` below is a no-op there and the transaction's first *write* is
/// what acquires its single writer slot — the same ordering
/// [`transfer_owner`] relies on, and for the same reason: a read followed by a
/// write leaves a lock-upgrade window in which a retirement can become the
/// writer and then wait on this transaction's read snapshot.
// Wide by design: the repository, the source lifecycle it verifies, the two
// namespaces recorded for the operator and the staleness horizon are all inputs
// to one decision.
#[allow(clippy::too_many_arguments)]
pub async fn bid_for_transfer_lease(
    db: &DatabaseConnection,
    repo_id: i64,
    source_owner_id: i64,
    source_org_id: Option<i64>,
    source_namespace: &str,
    destination_namespace: &str,
    token: &str,
    stale_before: DateTimeUtc,
) -> Result<TransferLeaseBid> {
    use repository_transfer_lease::Entity as Lease;
    let now = Utc::now();

    let transaction = db
        .begin()
        .await
        .context("db: begin repository transfer lease bid")?;

    Lease::insert(repository_transfer_lease::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        token: Set(token.to_string()),
        source_namespace: Set(source_namespace.to_string()),
        destination_namespace: Set(destination_namespace.to_string()),
        since: Set(now),
    })
    .on_conflict(
        OnConflict::column(repository_transfer_lease::Column::RepoId)
            // MySQL has no conflict target and needs a harmless assignment as
            // its DO NOTHING polyfill, exactly as `bid_for_publication_lease`
            // does. PostgreSQL and SQLite emit DO NOTHING for the column above.
            .do_nothing_on([repository_transfer_lease::Column::Id])
            .to_owned(),
    )
    .exec_without_returning(&transaction)
    .await
    .context("db: bid for repository transfer lease")?;

    // Whether that insert landed is not something every backend will say, so ask
    // the row who holds it.
    let held = Lease::find()
        .filter(repository_transfer_lease::Column::RepoId.eq(repo_id))
        .one(&transaction)
        .await
        .context("db: read repository transfer lease holder")?;

    let mut outcome = match held {
        // The holder released between the insert and this read, taking the row
        // with it. The repository is free but this token does not hold it, and
        // saying otherwise would hand out a lease nothing records.
        None => TransferLeaseBid::Busy,
        Some(held) if held.token == token => TransferLeaseBid::Granted,
        Some(held) => {
            // Someone else holds it. Only a stale hold may be taken over, and
            // only from the exact holder this read saw: filtering on the old
            // token is what keeps two waiters from both believing they took over
            // the same lease.
            let taken_over = Lease::update_many()
                .col_expr(repository_transfer_lease::Column::Token, Expr::value(token))
                .col_expr(repository_transfer_lease::Column::Since, Expr::value(now))
                .col_expr(
                    repository_transfer_lease::Column::SourceNamespace,
                    Expr::value(source_namespace),
                )
                .col_expr(
                    repository_transfer_lease::Column::DestinationNamespace,
                    Expr::value(destination_namespace),
                )
                .filter(repository_transfer_lease::Column::RepoId.eq(repo_id))
                .filter(repository_transfer_lease::Column::Token.eq(held.token))
                .filter(repository_transfer_lease::Column::Since.lt(stale_before))
                .exec(&transaction)
                .await
                .context("db: take over a stale repository transfer lease")?;
            if taken_over.rows_affected > 0 {
                TransferLeaseBid::TakenOver
            } else {
                TransferLeaseBid::Busy
            }
        }
    };

    // Holding the lease is only worth anything if the namespace it is moving out
    // of still exists and is not being retired.
    if matches!(
        outcome,
        TransferLeaseBid::Granted | TransferLeaseBid::TakenOver
    ) {
        match locked_source_verdict(&transaction, source_owner_id, source_org_id).await? {
            TransferSource::AccountClosed => outcome = TransferLeaseBid::SourceAccountClosed,
            TransferSource::OrganizationClosed => {
                outcome = TransferLeaseBid::SourceOrganizationClosed
            }
            TransferSource::Open => {}
        }
    }
    // …and only if the row it names is still live. A repository already
    // soft-deleted has nothing left to move.
    if matches!(
        outcome,
        TransferLeaseBid::Granted | TransferLeaseBid::TakenOver
    ) && RepoEntity::find_by_id(repo_id)
        .filter(repository::Column::DeletedAt.is_null())
        .one(&transaction)
        .await
        .context("db: read the repository a transfer lease was taken for")?
        .is_none()
    {
        outcome = TransferLeaseBid::RepositoryGone;
    }

    match outcome {
        // A refusal must not leave the lease this transaction inserted behind:
        // it would block the repository until it went stale, and block the
        // deletion that refused it for just as long.
        TransferLeaseBid::Granted | TransferLeaseBid::TakenOver => transaction
            .commit()
            .await
            .context("db: commit repository transfer lease bid")?,
        _ => transaction
            .rollback()
            .await
            .context("db: roll back a refused repository transfer lease bid")?,
    }
    Ok(outcome)
}

/// Whether the namespace a repository is moving *out of* still admits the move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransferSource {
    Open,
    AccountClosed,
    OrganizationClosed,
}

/// Read the source lifecycle under an exclusive lock.
///
/// Deliberately keyed on `deleted_at` alone and not on `is_active`, unlike the
/// destination: deactivation says nothing about whether the namespace still
/// exists, and the repositories of a merely deactivated account are not storage
/// anybody is retiring. Only the retirement claim means "these bytes are being
/// collected by somebody else".
async fn locked_source_verdict(
    transaction: &DatabaseTransaction,
    owner_id: i64,
    org_id: Option<i64>,
) -> Result<TransferSource> {
    let owner = user::Entity::find_by_id(owner_id)
        .lock_exclusive()
        .one(transaction)
        .await
        .context("db: lock repository transfer source account")?;
    if owner.is_none_or(|owner| owner.deleted_at.is_some()) {
        return Ok(TransferSource::AccountClosed);
    }

    if let Some(org_id) = org_id {
        let org = organization::Entity::find_by_id(org_id)
            .lock_exclusive()
            .one(transaction)
            .await
            .context("db: lock repository transfer source organization")?;
        if org.is_none_or(|org| org.deleted_at.is_some()) {
            return Ok(TransferSource::OrganizationClosed);
        }
    }

    Ok(TransferSource::Open)
}

/// Release a transfer lease. Returns whether this token still held it — `false`
/// means it had already been taken over, which is exactly the case where the
/// holder must not assume the storage it moved is still its own to move back.
pub async fn release_transfer_lease(
    db: &DatabaseConnection,
    repo_id: i64,
    token: &str,
) -> Result<bool> {
    let released = repository_transfer_lease::Entity::delete_many()
        .filter(repository_transfer_lease::Column::RepoId.eq(repo_id))
        .filter(repository_transfer_lease::Column::Token.eq(token))
        .exec(db)
        .await
        .context("db: release repository transfer lease")?;
    Ok(released.rows_affected > 0)
}

/// Whether a repository's storage is being moved right now.
///
/// Read by the deletion path, which must not treat a namespace emptied by a
/// transfer in flight as a namespace that was already empty. A lease past
/// `stale_before` is not an answer — its holder is gone, and a dead transfer
/// must never make a namespace undeletable.
pub async fn transfer_lease_in_flight(
    db: &DatabaseConnection,
    repo_id: i64,
    stale_before: DateTimeUtc,
) -> Result<Option<repository_transfer_lease::Model>> {
    repository_transfer_lease::Entity::find()
        .filter(repository_transfer_lease::Column::RepoId.eq(repo_id))
        .filter(repository_transfer_lease::Column::Since.gte(stale_before))
        .one(db)
        .await
        .context("db: read repository transfer lease")
}

/// Whether the guarded ownership transaction committed or found that one of the
/// lifecycles it moves between had already closed.
#[derive(Clone, Debug, PartialEq)]
pub enum TransferOwnerOutcome {
    Transferred(Repo),
    DestinationAccountClosed,
    DestinationOrganizationClosed,
    SourceAccountClosed,
    SourceOrganizationClosed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransferDestination {
    Open,
    AccountClosed,
    OrganizationClosed,
}

async fn write_repository_owner(
    transaction: &DatabaseTransaction,
    repo_id: i64,
    owner_id: i64,
    org_id: Option<i64>,
) -> Result<Repo> {
    // This must be the first statement on SQLite: an UPDATE acquires its one
    // writer slot immediately. A SELECT followed by UPDATE leaves a lock-upgrade
    // window where retirement can become the writer and then wait for this
    // transaction's read snapshot to close while this transaction waits for
    // retirement — resolved only by the busy timeout.
    let updated = RepoEntity::update_many()
        .col_expr(repository::Column::OwnerId, Expr::value(owner_id))
        .col_expr(repository::Column::OrgId, Expr::value(org_id))
        .col_expr(repository::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(repository::Column::Id.eq(repo_id))
        .exec(transaction)
        .await
        .context("db: update repo owner")?;
    if updated.rows_affected != 1 {
        return Err(anyhow::anyhow!(
            "repo not found while updating transfer owner"
        ));
    }
    RepoEntity::find_by_id(repo_id)
        .one(transaction)
        .await?
        .context("repo not found after updating transfer owner")
}

/// Lock both lifecycle ends of a transfer and report whether each still admits
/// it.
///
/// PostgreSQL/MySQL render these reads as `SELECT .. FOR UPDATE`, serializing
/// them with the retirement claims. SQLite omits that clause, so its caller
/// first performs the repository update and thereby becomes the database's
/// sole writer before reaching this check.
///
/// Rows are taken in ascending id order, accounts before organizations, and
/// every row is taken before any verdict is formed. Deciding one end and then
/// locking the other would let two transfers moving repositories in opposite
/// directions take the same two account rows in opposite orders and deadlock.
async fn lock_transfer_lifecycles(
    transaction: &DatabaseTransaction,
    source_owner_id: i64,
    source_org_id: Option<i64>,
    destination_owner_id: i64,
    destination_org_id: Option<i64>,
) -> Result<(TransferDestination, TransferSource)> {
    let mut user_ids = vec![source_owner_id, destination_owner_id];
    user_ids.sort_unstable();
    user_ids.dedup();
    let mut users = std::collections::HashMap::new();
    for id in user_ids {
        let locked = user::Entity::find_by_id(id)
            .lock_exclusive()
            .one(transaction)
            .await
            .context("db: lock a repository transfer lifecycle account")?;
        users.insert(id, locked);
    }

    let mut org_ids: Vec<i64> = [source_org_id, destination_org_id]
        .into_iter()
        .flatten()
        .collect();
    org_ids.sort_unstable();
    org_ids.dedup();
    let mut orgs = std::collections::HashMap::new();
    for id in org_ids {
        let locked = organization::Entity::find_by_id(id)
            .lock_exclusive()
            .one(transaction)
            .await
            .context("db: lock a repository transfer lifecycle organization")?;
        orgs.insert(id, locked);
    }

    let account = |id: &i64| users.get(id).and_then(Option::as_ref);
    let organization = |id: &i64| orgs.get(id).and_then(Option::as_ref);

    let destination = if !account(&destination_owner_id)
        .is_some_and(|owner| owner.is_active && owner.deleted_at.is_none())
    {
        TransferDestination::AccountClosed
    } else if destination_org_id.is_some_and(|org_id| {
        !organization(&org_id)
            .is_some_and(|org| org.owner_id == destination_owner_id && org.deleted_at.is_none())
    }) {
        TransferDestination::OrganizationClosed
    } else {
        TransferDestination::Open
    };

    // The source is judged on `deleted_at` alone — see [`locked_source_verdict`]
    // for why `is_active` is the wrong question on this side.
    let source = if account(&source_owner_id).is_none_or(|owner| owner.deleted_at.is_some()) {
        TransferSource::AccountClosed
    } else if source_org_id
        .is_some_and(|org_id| organization(&org_id).is_none_or(|org| org.deleted_at.is_some()))
    {
        TransferSource::OrganizationClosed
    } else {
        TransferSource::Open
    };

    Ok((destination, source))
}

/// Move a repository row and the namespace-bearing metadata of its registries
/// in one database transaction.
///
/// Package and OCI objects are addressed by `owner/repository` even though the
/// rows that describe them hang off the stable repository id. Their bytes are
/// moved by `rg-core` before this transaction; rewriting only `repositories`
/// would leave the metadata reading the old keys. Conversely, a failed
/// metadata rewrite must roll the repository-row update back so `rg-core` can
/// safely return every storage namespace to its source.
///
/// Both lifecycle ends are locked in the same transaction as the ownership
/// update. That makes transfer and retirement linearisable: whichever lifecycle
/// gets the lock first is the one the other observes.
///
/// The source end matters as much as the destination and for a different reason.
/// The destination check stops a repository landing in a namespace that is being
/// collected; the source check stops this transaction committing a move whose
/// bytes the source's own deleter is entitled to have retired
/// (card_507ff03ec043). It is the second line behind
/// [`bid_for_transfer_lease`], which is what keeps that deleter from starting at
/// all — this one covers the retirement claim that lands after the lease was
/// taken, and turns it into a refusal the service can compensate rather than a
/// commit nobody can undo.
#[allow(clippy::too_many_arguments)]
pub async fn transfer_owner(
    db: &DatabaseConnection,
    repo_id: i64,
    owner_id: i64,
    org_id: Option<i64>,
    source_owner_id: i64,
    source_org_id: Option<i64>,
    source_namespace: &str,
    destination_namespace: &str,
    repo_name: &str,
) -> Result<TransferOwnerOutcome> {
    let transaction = db.begin().await.context("db: begin repository transfer")?;

    // On PostgreSQL/MySQL, lock the lifecycle rows before touching the
    // repository. If transfer wins, a concurrent delete waits and its first
    // inventory sees the moved row; if deletion wins, the marker is visible and
    // this transaction changes nothing.
    //
    // SQLite has no row-level `FOR UPDATE`. Its first write is the lock, so do
    // the repository update first there. A trigger (or an already-running
    // retirement) can still mark/remove the destination before this check; in
    // that case the whole transaction is rolled back, including the owner
    // update and every trigger side effect.
    let sqlite = transaction.get_database_backend() == DatabaseBackend::Sqlite;
    let moved = if sqlite {
        Some(write_repository_owner(&transaction, repo_id, owner_id, org_id).await?)
    } else {
        None
    };
    let (admission, source) = lock_transfer_lifecycles(
        &transaction,
        source_owner_id,
        source_org_id,
        owner_id,
        org_id,
    )
    .await?;
    let refusal = match (admission, source) {
        (TransferDestination::AccountClosed, _) => {
            Some(TransferOwnerOutcome::DestinationAccountClosed)
        }
        (TransferDestination::OrganizationClosed, _) => {
            Some(TransferOwnerOutcome::DestinationOrganizationClosed)
        }
        (TransferDestination::Open, TransferSource::AccountClosed) => {
            Some(TransferOwnerOutcome::SourceAccountClosed)
        }
        (TransferDestination::Open, TransferSource::OrganizationClosed) => {
            Some(TransferOwnerOutcome::SourceOrganizationClosed)
        }
        (TransferDestination::Open, TransferSource::Open) => None,
    };
    if let Some(refusal) = refusal {
        transaction
            .rollback()
            .await
            .context("db: roll back repository transfer across a closed namespace")?;
        return Ok(refusal);
    }
    // Resolve the backend-dependent write order while the transaction can
    // still roll back. Keeping the row in an Option until after commit made a
    // violated internal invariant a process panic after the database had
    // already made the transfer durable.
    let moved = match moved {
        Some(moved) => moved,
        None => write_repository_owner(&transaction, repo_id, owner_id, org_id).await?,
    };

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
    Ok(TransferOwnerOutcome::Transferred(moved))
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
                    "plombir-git-forks-count-race-{}.db",
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
