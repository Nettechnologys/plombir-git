//! Repository service — business logic for repo creation and access control.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{ActiveValue::Set, DatabaseConnection};
use std::collections::{HashMap, HashSet};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use rg_db::{
    entities::repository::ActiveModel as RepoActiveModel,
    ops::{repo_ops, user_ops},
};

use super::templates;
use crate::blob_storage::{BlobKey, BlobStorage};
use crate::platform::fs::discard_dir;

/// One actionable error for a failure to stage a temporary git working tree.
///
/// Creating a repository, editing a file from the web UI and committing a batch
/// of files all clone into `TMPDIR/forgekeep-*`. That path is generated here and
/// never reaches the caller, so a bare `?` puts an unqualified `os error 13`
/// into the editor's HTTP response.
///
/// Unlike [`path_error`](crate::platform::fs::path_error) the remedy is appended
/// unconditionally: on a permission failure the uid diagnostic says which
/// directory is wrong, but only `TMPDIR` says where to move it.
fn temp_tree_error(what: &str, path: &std::path::Path, error: &std::io::Error) -> anyhow::Error {
    let described = crate::platform::fs::describe_path_error(what, path, error, "");
    anyhow::anyhow!(
        "{described}\n  hint: {}",
        crate::platform::fs::TEMP_DIR_HINT
    )
}

/// Options for repository creation (aligned with Gitea's CreateRepoOption).
#[derive(Debug, Clone)]
pub struct CreateRepoOptions {
    pub owner_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub is_private: bool,
    pub org_id: Option<i64>,
    /// Default branch name (default: "main")
    pub default_branch: Option<String>,
    /// Whether to auto-initialize the repo with initial files
    pub auto_init: bool,
    /// .gitignore template key (e.g., "go", "rust")
    pub gitignores: Option<String>,
    /// LICENSE template key (e.g., "mit", "apache-2.0")
    pub license: Option<String>,
    /// README template key (e.g., "default")
    pub readme: Option<String>,
    /// Default issue label set (e.g., "default", "scrum", "none")
    pub issue_labels: Option<String>,
    /// Owner's display name for license substitution
    pub owner_display_name: String,
    /// Git author name used for auto-initialization commits.
    pub git_author_name: Option<String>,
    /// Git author email used for auto-initialization commits.
    pub git_author_email: Option<String>,
}

// ── Permission cache (30s TTL) ──────────────────────────────────────────

const PERM_CACHE_TTL: Duration = Duration::from_secs(30);

/// Permission cache key: (database instance, repo_id, actor_id, for_write).
/// for_write=false → read check, for_write=true → write check.
///
/// `repo_id` and `actor_id` are only unique *within* one database, so the
/// instance has to be part of the key — see [`PermCache`].
type PermKey = (rg_db::InstanceId, i64, Option<i64>, bool);
type PermEntry = (bool, Instant);

/// A permission-decision cache with a 30s TTL, keyed by
/// `(database instance, repo_id, actor_id, for_write)`.
///
/// The instance component is what keeps the cache honest when a process talks
/// to more than one database. A server talks to exactly one, but every test
/// opens its own, and each of those starts its autoincrement ids at 1 — so
/// `(repo 1, user 2, read)` is a different question in each database and the
/// answers are frequently opposite. Keyed on ids alone, one test's correct
/// "outsider may not read repo 1" answered another test's "collaborator may
/// read repo 1" with a 403, and vice versa, for as long as the TTL held.
///
/// Production uses a single process-global instance (`perm_cache()`), but the
/// type is standalone so the invalidation logic can be unit-tested against a
/// private, hermetic instance instead of racing on the shared global static —
/// the whole test suite compiles into one binary, so any test that clears the
/// global mid-flight would otherwise flake the cache unit tests.
#[derive(Default)]
struct PermCache {
    entries: RwLock<HashMap<PermKey, PermEntry>>,
}

impl PermCache {
    fn check(
        &self,
        instance: rg_db::InstanceId,
        repo_id: i64,
        actor_id: Option<i64>,
        for_write: bool,
    ) -> Option<bool> {
        let cache = self.entries.read().unwrap_or_else(|e| e.into_inner());
        cache
            .get(&(instance, repo_id, actor_id, for_write))
            .filter(|(_, ts)| ts.elapsed() < PERM_CACHE_TTL)
            .map(|(v, _)| *v)
    }

    fn set(
        &self,
        instance: rg_db::InstanceId,
        repo_id: i64,
        actor_id: Option<i64>,
        for_write: bool,
        value: bool,
    ) {
        let mut cache = self.entries.write().unwrap_or_else(|e| e.into_inner());
        // This also reaps the entries of databases that are gone entirely: a
        // test's database dies with its test, and nothing would otherwise
        // invalidate what it left here before the process exits.
        cache.retain(|_, (_, ts)| ts.elapsed() < PERM_CACHE_TTL);
        cache.insert(
            (instance, repo_id, actor_id, for_write),
            (value, Instant::now()),
        );
    }

    /// Drop the cached read+write decisions for a specific user on a repo.
    fn invalidate_user(&self, instance: rg_db::InstanceId, repo_id: i64, user_id: i64) {
        let mut cache = self.entries.write().unwrap_or_else(|e| e.into_inner());
        cache.remove(&(instance, repo_id, Some(user_id), false));
        cache.remove(&(instance, repo_id, Some(user_id), true));
    }

    /// Drop every cached entry belonging to a repo.
    fn invalidate_repo(&self, instance: rg_db::InstanceId, repo_id: i64) {
        self.entries
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(inst, rid, _, _), _| (*inst, *rid) != (instance, repo_id));
    }

    /// Drop every entry belonging to one database.
    fn invalidate_all(&self, instance: rg_db::InstanceId) {
        self.entries
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(inst, _, _, _), _| *inst != instance);
    }
}

static PERM_CACHE: OnceLock<PermCache> = OnceLock::new();

fn perm_cache() -> &'static PermCache {
    PERM_CACHE.get_or_init(PermCache::default)
}

fn check_perm_cache(
    db: &DatabaseConnection,
    repo_id: i64,
    actor_id: Option<i64>,
    for_write: bool,
) -> Option<bool> {
    let instance = rg_db::instance_id(db)?;
    perm_cache().check(instance, repo_id, actor_id, for_write)
}

fn set_perm_cache(
    db: &DatabaseConnection,
    repo_id: i64,
    actor_id: Option<i64>,
    for_write: bool,
    value: bool,
) {
    // No identity, no caching: an unidentifiable handle (mock/disconnected)
    // must not share a key space with a real database.
    if let Some(instance) = rg_db::instance_id(db) {
        perm_cache().set(instance, repo_id, actor_id, for_write, value);
    }
}

/// Invalidate cached read+write permission for a specific user on a repo.
///
/// Call after a collaborator is added/updated/removed so that granted or
/// revoked access takes effect immediately instead of after the 30s TTL.
///
/// `db` selects whose entries to drop: the cache is keyed per database
/// instance, so an invalidation reaches the database it is handed and no other.
/// A server has exactly one, so there it drops what it always dropped.
pub fn invalidate_perm_cache_user(db: &DatabaseConnection, repo_id: i64, user_id: i64) {
    if let Some(instance) = rg_db::instance_id(db) {
        perm_cache().invalidate_user(instance, repo_id, user_id);
    }
}

/// Invalidate every cached permission entry for a repo (e.g. owner transfer
/// or deletion, which changes who can read/write).
pub fn invalidate_perm_cache_repo(db: &DatabaseConnection, repo_id: i64) {
    if let Some(instance) = rg_db::instance_id(db) {
        perm_cache().invalidate_repo(instance, repo_id);
    }
}

/// Clear the permission cache for one database. Used for org/team membership
/// changes that can affect access across many repositories at once.
pub fn invalidate_perm_cache_all(db: &DatabaseConnection) {
    if let Some(instance) = rg_db::instance_id(db) {
        perm_cache().invalidate_all(instance);
    }
}

/// Resolve an "owner" string to either a user ID or an org ID.
/// Returns (owner_id, org_id, owner_name_for_path).
/// - If owner is a username: returns (user_id, None, username)
/// - If owner is an org name: returns (org_owner_id, Some(org_id), org_name)
async fn resolve_owner(db: &DatabaseConnection, owner: &str) -> Result<(i64, Option<i64>, String)> {
    // Try user first. An account already claimed for retirement is not a
    // namespace anything may still enter — and `repositories.owner_id` cascades
    // on its deletion, so a repository admitted here would be destroyed rather
    // than orphaned (card_da1abc6074ac).
    if let Some(user) = user_ops::find_active_by_username(db, owner).await? {
        return Ok((user.id, None, user.username.clone()));
    }

    // Try organization. An organization already claimed for retirement is not a
    // namespace anything may still enter, so it resolves like a name that is
    // not there — the alternative is admitting a repository whose owner is
    // being deleted out from under it (card_b6dd1fb60659).
    if let Some(org) = rg_db::ops::org_ops::find_active_org_by_name(db, owner).await? {
        return Ok((org.owner_id, Some(org.id), org.name.clone()));
    }

    // The owner name is client input (a repo's target namespace), so naming one
    // that does not exist is a bad request, not a failed query.
    Err(crate::error::invalid_request(format!(
        "owner '{owner}' not found (neither user nor organization)"
    )))
}

/// Whether a freshly-committed repository row is still allowed to exist in the
/// namespace it named.
///
/// Both claims have to be re-read, not just the organization's. Every
/// repository row carries a user in `owner_id` — an organization's repositories
/// keep the organization's owner there — and that column is declared
/// `REFERENCES users(id) ON DELETE CASCADE`, so an account deletion that
/// finishes after this row commits does not merely orphan it, it *destroys* it:
/// the Git tree, the blob prefixes, the CI cache and the registry data stay
/// live with no row left naming them and no sweep that walks them
/// (card_da1abc6074ac).
///
/// `Err` is the reason the caller must undo itself with — a conflict when a
/// claim is in place, the read failure itself when the database could not
/// answer. An unanswerable read is deliberately not treated as "still open":
/// the whole point of the check is that guessing here costs bytes nothing can
/// reach.
async fn namespace_still_accepts_repository(
    db: &DatabaseConnection,
    owner_id: i64,
    org_id: Option<i64>,
    path_prefix: &str,
    name: &str,
) -> Result<()> {
    if !user_ops::user_namespace_is_open(db, owner_id).await? {
        return Err(crate::error::conflict(format!(
            "the account owning '{path_prefix}' is being deleted; repository '{name}' was not \
             created"
        )));
    }
    if let Some(org_id) = org_id {
        if !rg_db::ops::org_ops::org_is_active(db, org_id).await? {
            return Err(crate::error::conflict(format!(
                "organization '{path_prefix}' is being deleted; repository '{name}' was not created"
            )));
        }
    }
    Ok(())
}

/// Resolve the directory name that owns a repository row on disk.
///
/// `owner_id` alone is not enough: organization repositories retain the
/// organization's owner there, while their directory lives under the
/// organization name. Keeping this resolution next to create/delete prevents
/// the two lifecycle ends from deriving different paths.
async fn repository_namespace_name(
    db: &DatabaseConnection,
    owner_id: i64,
    org_id: Option<i64>,
) -> Result<String> {
    if let Some(org_id) = org_id {
        return rg_db::ops::org_ops::get_org(db, org_id)
            .await?
            .map(|org| org.name)
            .context("repository organization not found");
    }

    user_ops::find_by_id(db, owner_id)
        .await?
        .map(|user| user.username)
        .context("repository owner not found")
}

/// Find a repository by owner name (user or org) and repo name.
///
/// The two branches are two namespaces, not two ways of spelling one. A
/// username resolves to the *personal* namespace only: an organization's
/// repository is stored under `owner_id = org.owner_id` (see [`resolve_owner`]),
/// so looking it up by `owner_id` alone answered `/{org-owner}/{repo}` with the
/// organization's repository as well (card_92019cc97dcd).
pub async fn find_repo_by_owner_name(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<Option<rg_db::entities::repository::Model>> {
    // Try as user
    if let Some(user) = user_ops::find_by_username(db, owner).await? {
        return repo_ops::find_personal_by_owner_and_name(db, user.id, repo_name).await;
    }

    // Try as organization
    if let Some(org) = rg_db::ops::org_ops::get_org_by_name(db, owner).await? {
        return repo_ops::find_by_org_and_name(db, org.id, repo_name).await;
    }

    Ok(None)
}

/// Refuse `name` if it is already taken in the namespace it is being claimed
/// in — `Some(org_id)` for an organization, `None` for `owner_id`'s personal
/// account.
///
/// One helper rather than a copy per call site, because the check is the part
/// that has to know which namespace it is asking about: `create_repo_with_opts`,
/// `fork_repo` and `transfer_repo` each asked `find_by_owner_and_name(owner_id,
/// name)`, which for an organization is *its owner's* account. That found the
/// repository being transferred as its own destination collision, so a
/// repository could be moved into an organization but never back out
/// (card_92019cc97dcd).
///
/// `taken_message` is the caller's, because the three sites answer the same
/// refusal in their own words ("already exists", "…in your account", "…at
/// destination") and those wordings are what their tests read.
///
/// The refusal is a [`Conflict`](crate::error::Conflict), not an
/// [`InvalidRequest`](crate::error::InvalidRequest): the name is well-formed
/// and the request is correct — an existing repository refuses it, and only
/// deleting or renaming that repository changes the answer. Same reading as
/// `label '…' already exists in this repository` and the taken-username branch
/// of registration.
///
/// `except_repo_id` is the repository the name is being claimed *for*, when one
/// already exists — a transfer moves a row rather than adding one, and a row
/// never collides with itself.
async fn ensure_repo_name_free(
    db: &DatabaseConnection,
    owner_id: i64,
    org_id: Option<i64>,
    name: &str,
    except_repo_id: Option<i64>,
    taken_message: &str,
) -> Result<()> {
    let occupies = |found: Option<rg_db::entities::repository::Model>| {
        found.filter(|repo| Some(repo.id) != except_repo_id)
    };

    let taken = match org_id {
        Some(org_id) => repo_ops::find_by_org_and_name(db, org_id, name).await?,
        None => repo_ops::find_personal_by_owner_and_name(db, owner_id, name).await?,
    };
    if occupies(taken).is_some() {
        return Err(crate::error::conflict(taken_message.to_string()));
    }

    Ok(())
}

/// Check whether `actor_id` (None = anonymous) can read the given repo.
/// Use this when you already have the repo model to avoid duplicate queries.
/// Takes into account: public repos, private repos (owner + collaborators + org members).
pub async fn can_read_repo(
    db: &DatabaseConnection,
    repo: &rg_db::entities::repository::Model,
    actor_id: Option<i64>,
) -> Result<bool> {
    if !repo.is_private {
        return Ok(true);
    }

    // Check permission cache (30s TTL) to avoid repeated DB queries
    if let Some(cached) = check_perm_cache(db, repo.id, actor_id, false) {
        return Ok(cached);
    }

    let result = match actor_id {
        Some(id) => {
            if id == repo.owner_id {
                true
            } else {
                let perm =
                    rg_db::ops::repo_collaborator_ops::get_permission(db, repo.id, id).await?;
                if perm.is_some() {
                    true
                } else if let Some(org_id) = repo.org_id {
                    rg_db::ops::org_ops::is_org_member(db, org_id, id).await?
                } else {
                    false
                }
            }
        }
        None => false,
    };

    set_perm_cache(db, repo.id, actor_id, false, result);
    Ok(result)
}

/// Check whether `actor_id` (None = anonymous) can read `owner/repo`.
/// Takes into account: public repos, private repos (owner + collaborators + org members).
///
/// "No such repository" is returned as a typed [`crate::error::NotFound`], not
/// as an anonymous `anyhow!`: the caller has to tell it apart from "the lookup
/// itself failed", and with both flattened into a bare `anyhow::Error` the git
/// transport answered `404 repository not found` to a database outage.
pub async fn can_read(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    actor_id: Option<i64>,
) -> Result<bool> {
    let repo = find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| crate::error::not_found("repository"))?;
    can_read_repo(db, &repo, actor_id).await
}

/// Check whether `actor_id` can write to the given repo.
/// Use this when you already have the repo model to avoid duplicate queries.
/// Owner always has write. Collaborators with "write" or "admin" can write.
/// Org admins/members with write team permission can write.
pub async fn can_write_repo(
    db: &DatabaseConnection,
    repo: &rg_db::entities::repository::Model,
    actor_id: Option<i64>,
) -> Result<bool> {
    // Use a separate cache key prefix pattern: (repo_id, Some(user_id) or None)
    // We use the same cache key space as can_read_repo to reuse results.
    if let Some(cached) = check_perm_cache(db, repo.id, actor_id, true) {
        return Ok(cached);
    }

    let result = match actor_id {
        Some(id) => {
            if id == repo.owner_id {
                true
            } else {
                let perm =
                    rg_db::ops::repo_collaborator_ops::get_permission(db, repo.id, id).await?;
                let can_write_collab = matches!(perm.as_deref(), Some("write") | Some("admin"));
                if can_write_collab {
                    true
                } else if let Some(org_id) = repo.org_id {
                    if let Some(member) =
                        rg_db::ops::org_ops::find_org_member(db, org_id, id).await?
                    {
                        if member.role == "owner" || member.role == "admin" {
                            true
                        } else {
                            rg_db::ops::org_ops::is_member_of_write_team(db, org_id, id).await?
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
        }
        None => false,
    };

    set_perm_cache(db, repo.id, actor_id, true, result);
    Ok(result)
}

/// Check whether an actor may administer repository-scoped credentials and settings.
pub async fn can_admin_repo(
    db: &DatabaseConnection,
    repo: &rg_db::entities::repository::Model,
    actor_id: Option<i64>,
) -> Result<bool> {
    let Some(actor_id) = actor_id else {
        return Ok(false);
    };
    if actor_id == repo.owner_id {
        return Ok(true);
    }
    if rg_db::ops::repo_collaborator_ops::get_permission(db, repo.id, actor_id)
        .await?
        .as_deref()
        == Some("admin")
    {
        return Ok(true);
    }
    let Some(org_id) = repo.org_id else {
        return Ok(false);
    };
    let Some(member) = rg_db::ops::org_ops::find_org_member(db, org_id, actor_id).await? else {
        return Ok(false);
    };
    if matches!(member.role.as_str(), "owner" | "admin") {
        return Ok(true);
    }
    rg_db::ops::org_ops::is_member_of_admin_team(db, org_id, actor_id).await
}

/// Check whether `actor_id` can write to `owner/repo`.
/// Owner always has write. Collaborators with "write" or "admin" can write.
/// Org admins/members with write team permission can write.
///
/// Typed [`crate::error::NotFound`] for the absent repository, for the same
/// reason as [`can_read`].
pub async fn can_write(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    actor_id: Option<i64>,
) -> Result<bool> {
    let repo = find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| crate::error::not_found("repository"))?;
    can_write_repo(db, &repo, actor_id).await
}

/// Create a new repository (bare git init + DB record).
/// If org_id is Some, the repo belongs to the organization.
///
/// Legacy signature — kept for internal callers that don't need template options.
pub async fn create_repo(
    db: &DatabaseConnection,
    owner_id: i64,
    name: &str,
    description: Option<&str>,
    is_private: bool,
    repo_root: &std::path::Path,
    org_id: Option<i64>,
) -> Result<rg_db::entities::repository::Model> {
    create_repo_with_opts(
        db,
        CreateRepoOptions {
            owner_id,
            name: name.to_string(),
            description: description.map(str::to_string),
            is_private,
            org_id,
            default_branch: Some("main".to_string()),
            auto_init: false,
            gitignores: None,
            license: None,
            readme: None,
            issue_labels: None,
            owner_display_name: String::new(),
            git_author_name: None,
            git_author_email: None,
        },
        repo_root,
    )
    .await
}

/// Create a new repository with full template/auto-init support.
pub async fn create_repo_with_opts(
    db: &DatabaseConnection,
    opts: CreateRepoOptions,
    repo_root: &std::path::Path,
) -> Result<rg_db::entities::repository::Model> {
    create_repo_with_post_commit(db, opts, repo_root, || std::future::ready(())).await
}

/// Create the source row, then expose the historical post-commit FTS window to
/// a deterministic regression test. Production has no index write in that
/// window: source-table triggers are the sole owner of `repos_fts`.
async fn create_repo_with_post_commit<F, Fut>(
    db: &DatabaseConnection,
    opts: CreateRepoOptions,
    repo_root: &std::path::Path,
    after_source_commit: F,
) -> Result<rg_db::entities::repository::Model>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let default_branch = opts.default_branch.as_deref().unwrap_or("main");
    let owner_id = opts.owner_id;
    let name = &opts.name;

    // Validate repo name (prevents path traversal via repo name). Both this and
    // the name conflict below are the caller's to fix and carry
    // `InvalidRequest`; the git init, the `create_dir_all` and every query in
    // this function are ours and must not be reported as a bad request.
    crate::validate_repo_name(name).map_err(|error| {
        crate::error::invalid_request(format!("invalid repository name: {name} ({error})"))
    })?;

    // Check name conflict — in the namespace being created in, which is the
    // organization when `opts.org_id` names one and the owner's account
    // otherwise. Not `owner_id` alone: that is the same account for a user and
    // for every organization they own.
    ensure_repo_name_free(
        db,
        owner_id,
        opts.org_id,
        name,
        None,
        &format!("repository '{name}' already exists"),
    )
    .await?;

    // Determine path prefix: org name or user name.
    let path_prefix = repository_namespace_name(db, owner_id, opts.org_id).await?;

    // Create bare git repo on disk using gix
    let git_path = repo_root.join(format!("{}/{}.git", path_prefix, name));
    let namespace_dir = git_path
        .parent()
        .context("repository path has no namespace directory")?;
    // Create the parent first, then claim the final path with one non-recursive
    // create. `create_dir_all(git_path)` accepted an occupied directory and
    // left gix to turn the ordinary state conflict into an anonymous 500.
    std::fs::create_dir_all(namespace_dir).map_err(|error| {
        crate::platform::fs::path_error(
            "repository namespace directory",
            namespace_dir,
            &error,
            crate::platform::fs::REPO_ROOT_HINT,
        )
    })?;
    match std::fs::create_dir(&git_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(crate::error::conflict(format!(
                "repository storage for '{path_prefix}/{name}' is already occupied"
            )));
        }
        Err(error) => {
            return Err(crate::platform::fs::path_error(
                "repository directory",
                &git_path,
                &error,
                crate::platform::fs::REPO_ROOT_HINT,
            ));
        }
    }

    let init_result = gix::create::into(
        &git_path,
        gix::create::Kind::Bare,
        gix::create::Options::default(),
    )
    .with_context(|| format!("gix init --bare failed for {:?}", git_path));
    if let Err(error) = init_result {
        // This call atomically claimed the directory above, so unlike a
        // preflight `exists()` check it is safe to remove a partial gix init:
        // no concurrent creator could have owned these bytes first.
        discard_unreferenced_repo_dir(&git_path, &recreate_blocked_by(name));
        return Err(error);
    }

    let bare_repo = match gix::open(&git_path)
        .with_context(|| format!("failed to open newly-created bare repository {git_path:?}"))
    {
        Ok(repo) => repo,
        Err(error) => {
            discard_unreferenced_repo_dir(&git_path, &recreate_blocked_by(name));
            return Err(error);
        }
    };

    // `gix::create` inherits its HEAD from the gix template (currently
    // `refs/heads/main`), which is independent of the branch the caller asked
    // us to create. Set it before every later success path, including an empty
    // repository, so the database and Git agree about the default branch.
    if let Err(error) = set_bare_repo_head_to_branch(&bare_repo, default_branch) {
        discard_unreferenced_repo_dir(&git_path, &recreate_blocked_by(name));
        return Err(error);
    }

    // Auto-initialize with template files if requested
    if opts.auto_init {
        let init_result = auto_init_repo(
            &git_path,
            name,
            opts.description.as_deref().unwrap_or(""),
            default_branch,
            opts.gitignores.as_deref(),
            opts.license.as_deref(),
            opts.readme.as_deref(),
            &opts.owner_display_name,
            opts.git_author_name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| {
                    if opts.owner_display_name.trim().is_empty() {
                        "ForgeKeep"
                    } else {
                        opts.owner_display_name.as_str()
                    }
                }),
            opts.git_author_email
                .as_deref()
                .filter(|email| !email.trim().is_empty())
                .unwrap_or("forgekeep@example.invalid"),
        );

        if let Err(e) = &init_result {
            discard_unreferenced_repo_dir(&git_path, &recreate_blocked_by(name));
            bail!("auto-initialization failed: {}", e);
        }

        init_result?;
    }

    // Insert DB record
    let now = Utc::now();
    let model = RepoActiveModel {
        owner_id: Set(owner_id),
        name: Set(name.to_string()),
        description: Set(opts.description),
        is_private: Set(opts.is_private),
        default_branch: Set(default_branch.to_string()),
        stars_count: Set(0),
        forks_count: Set(0),
        org_id: Set(opts.org_id),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    // Same rollback as the auto-init branch above, for the same reason: the
    // bare repository is on disk and this row is what was supposed to point at
    // it. Leaving it makes the name permanently un-creatable.
    //
    // Losing the namespace-unique race is the same outcome `ensure_repo_name_free`
    // reports, reached a moment later: someone else claimed the name first. It
    // is answered in that check's words rather than as a server fault. Only
    // that one loss is folded; every other write failure stays an error, and
    // the directory is discarded either way.
    let repo = match repo_ops::create(db, model).await {
        Ok(repo) => repo,
        Err(error) => {
            discard_unreferenced_repo_dir(&git_path, &recreate_blocked_by(name));
            return Err(if rg_db::is_unique_violation_anyhow(&error) {
                crate::error::conflict(format!("repository '{name}' already exists"))
            } else {
                error
            });
        }
    };

    // The namespace was resolved several statements ago, and nothing since then
    // has been holding it: a `DELETE /orgs/{name}` or a
    // `DELETE /admin/users/{id}` could have claimed it for retirement,
    // inventoried its repositories and found none — all while this request was
    // initialising Git — and the row above would then be a live repository
    // whose owner is on its way out, with bytes no collector ever walks
    // (card_b6dd1fb60659, card_da1abc6074ac).
    //
    // Re-reading the claim *after* the row is committed is what closes that: if
    // the claim landed before this read, this request lost and undoes itself
    // here; if it lands after, the deleter's own re-inventory necessarily sees
    // this committed row and retires it. One of the two always happens, and the
    // argument rests on nothing stronger than "a committed write is visible to
    // a read that starts later", which every supported backend gives.
    if let Err(error) =
        namespace_still_accepts_repository(db, owner_id, opts.org_id, &path_prefix, name).await
    {
        // Undo in the order that leaves nothing dangling: the row first, so the
        // directory it named is unreferenced before it is discarded.
        if let Err(rollback_error) = repo_ops::delete_by_id(db, repo.id).await {
            tracing::error!(
                repo_id = repo.id,
                owner_id,
                org_id = opts.org_id,
                reason = %format!("{error:#}"),
                error = %format!("{rollback_error:#}"),
                "repository was created into a namespace that is being deleted, and removing the \
                 row failed — it now names an owner that is gone"
            );
            return Err(rollback_error);
        }
        discard_unreferenced_repo_dir(&git_path, &recreate_blocked_by(name));
        return Err(error);
    }

    after_source_commit().await;

    // Create default issue labels if requested
    if let Some(ref label_set) = opts.issue_labels {
        if label_set != "none" {
            if let Err(e) = create_default_labels(db, repo.id, label_set).await {
                tracing::warn!(repo_id = repo.id, label_set = %label_set, error = %format!("{e:#}"),
                    "failed to create default labels");
            }
        }
    }

    // Count the creation here (not in the HTTP handler) so the REST path and the
    // import subsystem (`resolve_or_create_target_repo`) both funnel through one
    // recording site.
    crate::metrics_hook::record_repo_created();

    Ok(repo)
}

/// Set the symbolic HEAD of a newly-created bare repository to its default branch.
///
/// The branch can be unborn; the symbolic reference still records the branch a
/// first push and a clone must use. This is intentionally fallible: persisting a
/// row after the Git repository rejected the requested reference would make a
/// successful create response lie about the repository's state.
fn set_bare_repo_head_to_branch(repo: &gix::Repository, branch: &str) -> Result<()> {
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    use gix::refs::{FullName, Target};

    let branch_ref: FullName = format!("refs/heads/{branch}")
        .try_into()
        .map_err(|error| anyhow::anyhow!("invalid default branch reference: {error}"))?;
    let head_name: FullName = "HEAD"
        .try_into()
        .map_err(|error| anyhow::anyhow!("invalid HEAD reference: {error}"))?;

    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "set default branch".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Symbolic(branch_ref),
        },
        name: head_name,
        deref: false,
    })
    .map_err(|error| anyhow::anyhow!("failed to set HEAD to refs/heads/{branch}: {error}"))?;

    Ok(())
}

/// Roll back a repository directory that a failed step left with no row
/// pointing at it.
///
/// Deliberately not [`discard_dir`]: its message is about a temporary working
/// tree and the remedy it names is `TMPDIR`, while these paths are the
/// canonical location every create checks before it does anything. A leftover
/// here is not disk noise — it is a repository name that can no longer be
/// created. The caller still has to report the failure that triggered the
/// rollback, so a failed rollback can only be logged, and `consequence` is what
/// makes that line worth reading.
fn discard_unreferenced_repo_dir(path: &std::path::Path, consequence: &str) {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            path = %path.display(),
            error = %error,
            "failed to roll back a repository directory that no row points at: {consequence}"
        ),
    }
}

/// What an operator sees next when the rollback above fails.
fn recreate_blocked_by(name: &str) -> String {
    format!(
        "creating {name} will keep failing with \"repository already exists\" until the directory \
         is removed by hand"
    )
}

/// Convert a local path to a git-compatible URL format.
/// On Windows, converts "D:\path\to\repo" to "file:///D:/path/to/repo".
/// On Unix, converts "/path/to/repo" to "file:///path/to/repo".
fn path_to_git_url(path: &std::path::Path) -> Result<String> {
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize path: {:?}", path))?;

    let path_str = canonical.to_string_lossy().to_string();

    // On Windows, convert "D:\path" to "file:///D:/path"
    // On Unix, convert "/path" to "file:///path"
    if cfg!(windows) {
        // Windows path like "D:\path\to\repo"
        // Step 1: Replace backslashes with forward slashes
        let with_forward_slash = path_str.replace('\\', "/");
        // Step 2: Ensure drive letter is followed by colon and slash
        // "D:/path/to/repo" -> "file:///D:/path/to/repo"
        Ok(format!("file:///{}", with_forward_slash))
    } else {
        // Unix path like "/path/to/repo"
        // "file:///path/to/repo"
        Ok(format!("file://{}", path_str))
    }
}

/// Absolute filesystem path for an entry that does **not exist yet** — the
/// destination of a `git clone`, say, which the clone itself creates.
///
/// [`std::fs::canonicalize`] requires every component of the path to exist, so
/// the final one cannot be resolved *before* the operation that creates it;
/// asking anyway is how forking answered 500 to everyone. Resolve the parent
/// instead — the caller has just created it — and re-append the name: symlinks
/// above the entry are still collapsed and the result is absolute regardless of
/// the process working directory.
fn path_for_new_entry(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let parent = path
        .parent()
        .with_context(|| format!("path has no parent directory: {:?}", path))?;
    let name = path
        .file_name()
        .with_context(|| format!("path has no final component: {:?}", path))?;
    let canonical_parent = std::fs::canonicalize(parent)
        .with_context(|| format!("failed to canonicalize parent directory: {:?}", parent))?;
    Ok(canonical_parent.join(name))
}

/// Auto-initialize a bare repo with initial files (README, LICENSE, .gitignore)
/// by creating a temp working tree, committing, and pushing to the bare repo.
// Wide by design: threads the full initial-commit context (paths, names, author identity).
#[allow(clippy::too_many_arguments)]
fn auto_init_repo(
    bare_path: &std::path::Path,
    repo_name: &str,
    description: &str,
    default_branch: &str,
    gitignores_key: Option<&str>,
    license_key: Option<&str>,
    readme_key: Option<&str>,
    owner_name: &str,
    git_author_name: &str,
    git_author_email: &str,
) -> Result<()> {
    // Canonicalize the bare repo path so git push works from any working directory
    let bare_path = std::fs::canonicalize(bare_path)
        .with_context(|| format!("bare repo path does not exist: {:?}", bare_path))?;

    // Create a temp directory for the working tree
    let tmp = std::env::temp_dir().join(format!("forgekeep-init-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)
        .map_err(|error| temp_tree_error("auto-init working tree", &tmp, &error))?;

    // One cleanup point behind the body, the shape `update_files_in_commit`
    // already uses. `tmp` is a whole working tree, and a `?` or `bail!` that
    // slipped past a per-branch `discard_dir` left it in `TMPDIR` for good —
    // under a UUID name nothing in the logs can tie back to a repository.
    let result = (|| -> Result<()> {
        // Init a non-bare repo in the temp dir
        let gateway = rg_git::cli_gateway::GitCommandGateway::new()
            .with_context(|| "git CLI not available")?;
        let output = gateway
            .run(&["init", "-b", default_branch], Some(&tmp))
            .with_context(|| format!("git init failed in {:?}", tmp))?;
        output
            .ensure_success()
            .with_context(|| format!("git init failed in {:?}", tmp))?;

        // Write README.md if specified
        let mut files_written = false;

        // Write .gitignore if specified
        if let Some(key) = gitignores_key {
            if !key.is_empty() {
                if let Some(tmpl) = templates::gitignore_content(key) {
                    std::fs::write(tmp.join(".gitignore"), tmpl.content)
                        .context("failed to write .gitignore")?;
                    files_written = true;
                }
            }
        }

        // Write LICENSE if specified (with year/author substitution)
        if let Some(key) = license_key {
            if !key.is_empty() {
                if let Some(tmpl) = templates::license_content(key) {
                    let year = Utc::now().format("%Y").to_string();
                    let content = tmpl
                        .content
                        .replace("{YEAR}", &year)
                        .replace("{AUTHOR}", owner_name);
                    std::fs::write(tmp.join("LICENSE"), content)
                        .context("failed to write LICENSE")?;
                    files_written = true;
                }
            }
        }

        // Write README.md if specified (default to "default" if auto_init but no template specified)
        let readme_key = readme_key.unwrap_or("default");
        if !readme_key.is_empty() {
            if let Some(content) = templates::readme_content(readme_key, repo_name, description) {
                std::fs::write(tmp.join("README.md"), content)
                    .context("failed to write README.md")?;
                files_written = true;
            }
        }

        // If no files were written, skip the commit — the tail still cleans up.
        if !files_written {
            tracing::info!(%repo_name, "auto_init: no template files to commit, skipping");
            return Ok(());
        }

        // git add all files
        let output = gateway
            .run(&["add", "-A"], Some(&tmp))
            .context("git add failed")?;
        output.ensure_success().context("git add failed")?;

        // git commit (identity env via gateway)
        let identity = git_identity_env(git_author_name, git_author_email);
        let output = gateway
            .run_with_env(&["commit", "-m", "Initial commit"], Some(&tmp), &identity)
            .context("git commit failed")?;
        if !output.success() {
            bail!("git commit failed: {}", output.stderr_str());
        }

        // git push to the bare repo
        let push_url =
            path_to_git_url(&bare_path).context("failed to convert bare repo path to git URL")?;
        let refspec = format!("{}:{}", default_branch, default_branch);

        let output = gateway
            .run(&["push", "--quiet", &push_url, &refspec], Some(&tmp))
            .context("git push failed")?;
        if !output.success() {
            bail!("git push to bare repo failed: {}", output.stderr_str());
        }

        tracing::info!(
            repo = %repo_name,
            branch = %default_branch,
            "auto-initialized repository with template files"
        );

        Ok(())
    })();
    discard_dir("auto-init working tree", &tmp);
    result
}

/// Build git identity env vars for commit commands run via `GitCommandGateway`.
///
/// Returns a borrowed array suitable for `gateway.run_with_env(...)`. Used
/// instead of setting env on a raw `Command` now that commits go through the
/// gateway rather than spawning git directly.
fn git_identity_env<'a>(name: &'a str, email: &'a str) -> [(&'a str, &'a str); 4] {
    [
        ("GIT_AUTHOR_NAME", name),
        ("GIT_AUTHOR_EMAIL", email),
        ("GIT_COMMITTER_NAME", name),
        ("GIT_COMMITTER_EMAIL", email),
    ]
}

/// Create default issue labels for a newly created repository.
async fn create_default_labels(
    db: &DatabaseConnection,
    repo_id: i64,
    label_set: &str,
) -> Result<()> {
    let labels = templates::default_labels(label_set);

    for label_def in &labels {
        let now = Utc::now();
        let model = rg_db::entities::label::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            name: Set(label_def.name.clone()),
            color: Set(label_def.color.clone()),
            description: Set(Some(label_def.description.clone())),
            created_at: Set(now),
            updated_at: Set(now),
        };
        rg_db::ops::label_ops::create(db, model).await?;
    }

    tracing::info!(
        repo_id = repo_id,
        count = labels.len(),
        "created default issue labels"
    );

    Ok(())
}

/// Star a repository. Returns true if newly starred, false if unstarred.
pub async fn toggle_star(db: &DatabaseConnection, user_id: i64, repo_id: i64) -> Result<bool> {
    let starred = rg_db::ops::repo_star_ops::toggle_star(db, user_id, repo_id).await?;
    // Refresh cache count field
    rg_db::ops::repo_ops::update_stars_count(db, repo_id).await?;
    Ok(starred)
}

/// Check if user has starred a repo.
pub async fn is_starred(db: &DatabaseConnection, user_id: i64, repo_id: i64) -> Result<bool> {
    rg_db::ops::repo_star_ops::is_starred(db, user_id, repo_id).await
}

/// List stargazers of a repo.
pub async fn list_stargazers(
    db: &DatabaseConnection,
    repo_id: i64,
    offset: u64,
    limit: u64,
) -> Result<(Vec<rg_db::entities::repo_star::Model>, i64)> {
    rg_db::ops::repo_star_ops::list_stargazers(db, repo_id, offset, limit).await
}

/// The three values `repo_watch.watch_state` is allowed to hold.
///
/// The column is a bare `String`, and the subscribe endpoint used to write the
/// request body into it unchecked: `{"state": "wathcing"}` answered `200 OK`
/// with `{"watch_state": "wathcing"}`, the row was written, and the user was
/// never notified about anything again — a typo silently unsubscribed them,
/// with the API reporting success. The delivery side reads the state through an
/// allowlist (see [`crate::notification`]), so anything outside these three is
/// not "some other subscription", it is a permanently dead row.
///
/// Kept as a parsed type rather than a `matches!` at the handler so both doors
/// — `PUT /watch` and any future caller of [`set_watch`] — go through the same
/// check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchState {
    /// Send me notifications for this repository.
    Watching,
    /// Subscribed to nothing — what `DELETE /watch` writes.
    NotWatching,
    /// Explicitly muted.
    Ignoring,
}

impl WatchState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Watching => "watching",
            Self::NotWatching => "not_watching",
            Self::Ignoring => "ignoring",
        }
    }

    /// Parse a client-supplied state, rejecting anything else as a 400.
    ///
    /// The message is a fixed description of the rule (H-05) and deliberately
    /// does not echo the offending value back to the client.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "watching" => Ok(Self::Watching),
            "not_watching" => Ok(Self::NotWatching),
            "ignoring" => Ok(Self::Ignoring),
            _ => Err(crate::error::invalid_request(
                "invalid watch state: expected one of watching, not_watching, ignoring",
            )),
        }
    }
}

/// Set watch state for a repo. Returns new watch_state.
///
/// Validates `state` against [`WatchState`] — an unrecognised state is a
/// rejected request, never a stored row.
pub async fn set_watch(
    db: &DatabaseConnection,
    user_id: i64,
    repo_id: i64,
    state: &str,
) -> Result<String> {
    let state = WatchState::parse(state)?;
    rg_db::ops::repo_watch_ops::set_watch_state(db, user_id, repo_id, state.as_str()).await
}

/// Get watch state.
pub async fn get_watch(
    db: &DatabaseConnection,
    user_id: i64,
    repo_id: i64,
) -> Result<Option<String>> {
    rg_db::ops::repo_watch_ops::get_watch_state(db, user_id, repo_id).await
}

#[derive(Debug)]
struct StagedBlobPrefix {
    live: BlobKey,
    staged: BlobKey,
}

/// The repository-shaped blob prefixes owned by a repository, one per feature.
///
/// The OCI registry is deliberately not among them even though its keys are
/// repository-shaped (`oci/<owner>/<repo>/...`): it may be configured onto a
/// backend of its own (`[server].oci_storage_path`), so staging its prefix
/// through *this* storage would move nothing on the instances that have one.
/// It is retired by
/// [`OciStorage::stage_repository_deletion`](crate::package_registry::oci::storage::OciStorage::stage_repository_deletion),
/// which also reaches the two things no `BlobKey` names — the chunked-upload
/// tree and the legacy on-disk layout.
///
/// The `packages` prefix likewise covers only the file rows whose
/// `storage_path` is a key: rows inherited from before the blob-storage
/// migration hold an absolute path and are retired by
/// [`legacy_package_file_paths`] instead.
fn repository_blob_prefixes(
    namespace: &str,
    repo: &rg_db::entities::repository::Model,
    deletion_id: &str,
) -> Result<Vec<StagedBlobPrefix>> {
    let repo_id = repo.id.to_string();
    let live = [
        (
            "packages",
            BlobKey::from_segments(["packages", namespace, repo.name.as_str()])?,
        ),
        (
            "lfs",
            BlobKey::from_segments(["lfs", namespace, repo.name.as_str()])?,
        ),
        (
            "releases",
            BlobKey::from_segments(["releases", namespace, repo.name.as_str()])?,
        ),
        (
            "attachments",
            BlobKey::from_segments(["attachments", repo_id.as_str()])?,
        ),
    ];

    live.into_iter()
        .map(|(kind, live)| {
            let staged = BlobKey::from_segments([
                "_deleted",
                "repositories",
                repo_id.as_str(),
                deletion_id,
                kind,
            ])?;
            Ok(StagedBlobPrefix { live, staged })
        })
        .collect()
}

/// The CI artifact prefixes owned by a repository, one per job.
///
/// Artifacts are keyed `artifacts/jobs/<job_id>/<object>`, so unlike packages,
/// LFS, releases and attachments they have no repository-shaped prefix: the
/// only way from a repository to its artifact bytes is through the job ids its
/// pipelines own. `job_ids` therefore comes from the database, never from a
/// string prefix over the storage keys — a repository named `demo` must not
/// decide the fate of objects it merely shares a spelling with.
///
/// Job ids are unique instance-wide and never reissued, so a job that uploaded
/// nothing simply has no directory: [`BlobStorage::move_prefix`] answers
/// `Ok(false)` for it and the prefix never enters the staged set.
fn artifact_blob_prefixes(
    job_ids: &[i64],
    repo_id: i64,
    deletion_id: &str,
) -> Result<Vec<StagedBlobPrefix>> {
    let repo_id = repo_id.to_string();
    job_ids
        .iter()
        .map(|job_id| {
            let job_id = job_id.to_string();
            Ok(StagedBlobPrefix {
                live: BlobKey::from_segments(["artifacts", "jobs", job_id.as_str()])?,
                staged: BlobKey::from_segments([
                    "_deleted",
                    "repositories",
                    repo_id.as_str(),
                    deletion_id,
                    "artifacts",
                    "jobs",
                    job_id.as_str(),
                ])?,
            })
        })
        .collect()
}

/// One repository-shaped blob namespace moved between two live owners.
struct TransferredBlobPrefix {
    source: BlobKey,
    destination: BlobKey,
}

/// The blob namespaces whose location changes when a repository is transferred.
///
/// Attachments are deliberately absent: they are keyed by immutable repository
/// id, not by the owner/repository spelling. OCI has its own transfer primitive
/// because it may be configured on a dedicated backend and also owns upload and
/// legacy-directory trees that are not `BlobKey`s.
fn repository_transfer_blob_prefixes(
    source_namespace: &str,
    destination_namespace: &str,
    repo_name: &str,
) -> Result<Vec<TransferredBlobPrefix>> {
    ["packages", "lfs", "releases"]
        .into_iter()
        .map(|kind| {
            Ok(TransferredBlobPrefix {
                source: BlobKey::from_segments([kind, source_namespace, repo_name])?,
                destination: BlobKey::from_segments([kind, destination_namespace, repo_name])?,
            })
        })
        .collect()
}

async fn restore_transferred_blob_prefixes(
    storage: &dyn BlobStorage,
    prefixes: &[TransferredBlobPrefix],
    repo_id: i64,
) {
    for prefix in prefixes.iter().rev() {
        match storage
            .move_prefix(&prefix.destination, &prefix.source)
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                repo_id,
                moved_to = %prefix.destination,
                belongs_at = %prefix.source,
                "failed to restore a repository blob prefix after transfer aborted — the active repository can no longer reach these blobs until the prefix is moved back by hand"
            ),
            Err(error) => tracing::warn!(
                repo_id,
                moved_to = %prefix.destination,
                belongs_at = %prefix.source,
                %error,
                "failed to restore a repository blob prefix after transfer aborted — the active repository can no longer reach these blobs until the prefix is moved back by hand"
            ),
        }
    }
}

async fn transfer_blob_prefixes(
    storage: &dyn BlobStorage,
    prefixes: Vec<TransferredBlobPrefix>,
    repo_id: i64,
) -> Result<Vec<TransferredBlobPrefix>> {
    let mut moved = Vec::new();
    for prefix in prefixes {
        match storage
            .move_prefix(&prefix.source, &prefix.destination)
            .await
        {
            Ok(true) => moved.push(prefix),
            Ok(false) => {}
            Err(error) => {
                restore_transferred_blob_prefixes(storage, &moved, repo_id).await;
                return Err(error).with_context(|| {
                    format!(
                        "failed to move repository blob prefix {} to {}",
                        prefix.source, prefix.destination
                    )
                });
            }
        }
    }
    Ok(moved)
}

/// An optional, historical on-disk namespace moved between two live owners.
struct TransferredRepositoryDirectory {
    source: std::path::PathBuf,
    destination: std::path::PathBuf,
    kind: &'static str,
}

fn transfer_optional_repository_directory(
    source: std::path::PathBuf,
    destination: std::path::PathBuf,
    kind: &'static str,
) -> Result<Option<TransferredRepositoryDirectory>> {
    match source.try_exists() {
        Ok(false) => return Ok(None),
        Ok(true) => {}
        Err(error) => {
            return Err(crate::platform::fs::path_error(
                kind,
                &source,
                &error,
                crate::platform::fs::BLOB_STORAGE_HINT,
            ));
        }
    }
    match destination.try_exists() {
        Ok(false) => {}
        Ok(true) => {
            return Err(crate::platform::fs::path_error(
                kind,
                &destination,
                &std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "destination directory already exists",
                ),
                crate::platform::fs::BLOB_STORAGE_HINT,
            ));
        }
        Err(error) => {
            return Err(crate::platform::fs::path_error(
                kind,
                &destination,
                &error,
                crate::platform::fs::BLOB_STORAGE_HINT,
            ));
        }
    }
    let parent = destination
        .parent()
        .context("repository transfer destination has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|error| {
        crate::platform::fs::path_error(
            kind,
            parent,
            &error,
            crate::platform::fs::BLOB_STORAGE_HINT,
        )
    })?;
    std::fs::rename(&source, &destination).map_err(|error| {
        crate::platform::fs::path_error(
            kind,
            &source,
            &error,
            crate::platform::fs::BLOB_STORAGE_HINT,
        )
    })?;
    Ok(Some(TransferredRepositoryDirectory {
        source,
        destination,
        kind,
    }))
}

fn restore_transferred_repository_directories(
    directories: &[TransferredRepositoryDirectory],
    repo_id: i64,
) {
    for directory in directories.iter().rev() {
        if let Err(error) = std::fs::rename(&directory.destination, &directory.source) {
            tracing::warn!(
                repo_id,
                storage = directory.kind,
                moved_to = %directory.destination.display(),
                belongs_at = %directory.source.display(),
                %error,
                "failed to restore a historical repository storage directory after transfer aborted — the active repository can no longer reach these bytes until the directory is moved back by hand"
            );
        }
    }
}

fn restore_transferred_repository_directory(
    moved_path: &std::path::Path,
    source_path: &std::path::Path,
    repo_id: i64,
) {
    if let Err(error) = std::fs::rename(moved_path, source_path) {
        tracing::warn!(
            repo_id,
            moved_to = %moved_path.display(),
            belongs_at = %source_path.display(),
            %error,
            "failed to restore a repository directory after transfer aborted — the active repository is unreachable until the directory is moved back by hand"
        );
    }
}

async fn restore_blob_prefixes(
    storage: &dyn BlobStorage,
    prefixes: &[StagedBlobPrefix],
    repo_id: i64,
) {
    for prefix in prefixes.iter().rev() {
        match storage.move_prefix(&prefix.staged, &prefix.live).await {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                repo_id,
                staged_prefix = %prefix.staged,
                live_prefix = %prefix.live,
                "failed to restore a repository blob prefix after deletion aborted — the staged prefix disappeared and the active repository may now reference missing blobs"
            ),
            Err(error) => tracing::warn!(
                repo_id,
                staged_prefix = %prefix.staged,
                live_prefix = %prefix.live,
                %error,
                "failed to restore a repository blob prefix after deletion aborted — the active repository can no longer reach these blobs until the prefix is moved back by hand"
            ),
        }
    }
}

async fn stage_blob_prefixes(
    storage: &dyn BlobStorage,
    prefixes: Vec<StagedBlobPrefix>,
    repo_id: i64,
) -> Result<Vec<StagedBlobPrefix>> {
    let mut staged = Vec::new();
    for prefix in prefixes {
        match storage.move_prefix(&prefix.live, &prefix.staged).await {
            Ok(true) => staged.push(prefix),
            Ok(false) => {}
            Err(error) => {
                restore_blob_prefixes(storage, &staged, repo_id).await;
                return Err(error).with_context(|| {
                    format!(
                        "failed to stage repository blob prefix {} at {}",
                        prefix.live, prefix.staged
                    )
                });
            }
        }
    }
    Ok(staged)
}

fn restore_repository_directory(
    staged_path: &std::path::Path,
    repo_path: &std::path::Path,
    repo_id: i64,
) {
    if let Err(rollback_error) = std::fs::rename(staged_path, repo_path) {
        tracing::warn!(
            repo_id,
            staged_at = %staged_path.display(),
            belongs_at = %repo_path.display(),
            error = %rollback_error,
            "failed to restore a repository directory after deletion aborted — the active row is now unreachable until the directory is moved back by hand"
        );
    }
}

/// A repository-owned directory that does not belong to a [`BlobStorage`]
/// backend and therefore has to participate in deletion through ordinary
/// filesystem renames.
struct RepositoryFilesystemDirectory {
    live: std::path::PathBuf,
    kind: &'static str,
    hint: &'static str,
}

/// One filesystem directory moved out of its live namespace before the
/// repository row is soft-deleted.
struct StagedRepositoryFilesystemDirectory {
    live: std::path::PathBuf,
    staged: std::path::PathBuf,
    kind: &'static str,
    hint: &'static str,
}

/// The absolute paths of this repository's pre-migration package files.
///
/// `package_files.storage_path` holds a [`BlobKey`] for everything ForgeKeep
/// itself writes, but rows inherited from before the blob-storage migration
/// hold an absolute filesystem path instead. Those bytes sit outside
/// `packages/<owner>/<repo>`, so the prefix move cannot reach them — and every
/// other door treats them as live storage: `PackageStorage::read_file`,
/// `file_path` and `delete_file` all carry a legacy branch, and a single
/// version delete already stages them by path. Left out of the repository
/// deletion they outlive the rows that named them, in the live namespace, with
/// no tombstone and no sweep that would ever find them again.
///
/// Ownership is read from the database rather than inferred from the shape of
/// the key, for the same reason CI artifacts are: a failed inventory has to be
/// a failed deletion, not a deletion that quietly keeps the files.
async fn legacy_package_file_paths(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<Vec<std::path::PathBuf>> {
    let storage_paths = rg_db::ops::package_file_ops::list_storage_paths_by_repo(db, repo_id)
        .await
        .with_context(|| {
            format!(
                "failed to inventory the package files of repository {repo_id} — its \
                 pre-migration package storage cannot be retired without them"
            )
        })?;
    let mut paths = std::collections::BTreeSet::new();
    for storage_path in storage_paths {
        if BlobKey::new(storage_path.as_str()).is_ok() {
            continue;
        }
        let path = std::path::PathBuf::from(&storage_path);
        // A path with no final component names no file, so there is nothing to
        // rename beside itself. Skipping it keeps a nonsense row from making
        // the repository undeletable, but it is a row an operator has to see.
        if path.file_name().is_none() {
            tracing::warn!(
                repo_id,
                storage_path,
                "package file row holds neither a blob key nor a path with a final component; \
                 repository deletion cannot stage it"
            );
            continue;
        }
        paths.insert(path);
    }
    Ok(paths.into_iter().collect())
}

fn repository_filesystem_directories(
    repo_root: &std::path::Path,
    namespace: &str,
    repo: &rg_db::entities::repository::Model,
    job_ids: &[i64],
    legacy_package_files: &[std::path::PathBuf],
) -> Vec<RepositoryFilesystemDirectory> {
    let mut directories = vec![
        RepositoryFilesystemDirectory {
            live: crate::lfs::service::lfs_root(repo_root, namespace, &repo.name),
            kind: "legacy LFS directory",
            hint: crate::platform::fs::LFS_STORAGE_HINT,
        },
        // Pre-migration release assets are still a live read path — the blob
        // store reporting the key missing is what sends `read_asset_bytes` to
        // this directory — and a transfer already moves it with the repository.
        // Left out of the deletion it outlives the row that owned it, with no
        // sweep anywhere that walks the filesystem to find it again.
        RepositoryFilesystemDirectory {
            live: crate::release::service::legacy_asset_root(repo_root, namespace, &repo.name),
            kind: "legacy release asset directory",
            hint: crate::platform::fs::REPO_ROOT_HINT,
        },
        RepositoryFilesystemDirectory {
            live: repo_root.join("_ci_cache").join(repo.id.to_string()),
            kind: "CI cache directory",
            hint: crate::platform::fs::CI_CACHE_DIR_HINT,
        },
    ];
    directories.extend(job_ids.iter().map(|job_id| {
        RepositoryFilesystemDirectory {
            live: repo_root
                .join("_artifacts")
                .join("jobs")
                .join(job_id.to_string()),
            kind: "legacy CI artifact directory",
            hint: crate::platform::fs::BLOB_STORAGE_HINT,
        }
    }));
    directories.extend(
        legacy_package_files
            .iter()
            .map(|path| RepositoryFilesystemDirectory {
                live: path.clone(),
                kind: "legacy package file",
                hint: crate::platform::fs::BLOB_STORAGE_HINT,
            }),
    );
    directories
}

fn restore_repository_filesystem_directories(
    directories: &[StagedRepositoryFilesystemDirectory],
    repo_id: i64,
) {
    for directory in directories.iter().rev() {
        if let Err(error) = std::fs::rename(&directory.staged, &directory.live) {
            tracing::warn!(
                repo_id,
                storage = directory.kind,
                staged_at = %directory.staged.display(),
                belongs_at = %directory.live.display(),
                %error,
                "failed to restore a repository filesystem directory after deletion aborted — the active repository can no longer reach these bytes until the directory is moved back by hand"
            );
        }
    }
}

fn stage_repository_filesystem_directories(
    directories: Vec<RepositoryFilesystemDirectory>,
    repo_id: i64,
    deletion_id: &str,
) -> Result<Vec<StagedRepositoryFilesystemDirectory>> {
    let directories = directories
        .into_iter()
        .map(|directory| {
            let file_name = directory
                .live
                .file_name()
                .context("repository filesystem path has no final component")?;
            let staged = directory.live.with_file_name(format!(
                "{}.deleted-{repo_id}-{deletion_id}",
                file_name.to_string_lossy()
            ));
            Ok((directory, staged))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut staged_directories = Vec::new();
    for (directory, staged) in directories {
        match std::fs::symlink_metadata(&directory.live) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                restore_repository_filesystem_directories(&staged_directories, repo_id);
                return Err(crate::platform::fs::path_error(
                    directory.kind,
                    &directory.live,
                    &error,
                    directory.hint,
                ))
                .with_context(|| {
                    format!(
                        "failed to inspect repository filesystem storage at {}",
                        directory.live.display()
                    )
                });
            }
        }

        match std::fs::symlink_metadata(&staged) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                restore_repository_filesystem_directories(&staged_directories, repo_id);
                return Err(crate::platform::fs::path_error(
                    directory.kind,
                    &staged,
                    &std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "repository deletion staging path already exists",
                    ),
                    directory.hint,
                ));
            }
            Err(error) => {
                restore_repository_filesystem_directories(&staged_directories, repo_id);
                return Err(crate::platform::fs::path_error(
                    directory.kind,
                    &staged,
                    &error,
                    directory.hint,
                ))
                .with_context(|| {
                    format!(
                        "failed to inspect repository deletion staging path {}",
                        staged.display()
                    )
                });
            }
        }

        if let Err(error) = std::fs::rename(&directory.live, &staged) {
            restore_repository_filesystem_directories(&staged_directories, repo_id);
            return Err(crate::platform::fs::path_error(
                directory.kind,
                &directory.live,
                &error,
                directory.hint,
            ))
            .with_context(|| {
                format!(
                    "failed to stage repository filesystem storage at {}",
                    staged.display()
                )
            });
        }
        staged_directories.push(StagedRepositoryFilesystemDirectory {
            live: directory.live,
            staged,
            kind: directory.kind,
            hint: directory.hint,
        });
    }
    Ok(staged_directories)
}

fn retire_repository_filesystem_directories(
    directories: Vec<StagedRepositoryFilesystemDirectory>,
    repo_id: i64,
) -> Result<()> {
    let mut cleanup_error = None;
    for directory in directories {
        let removal = match std::fs::symlink_metadata(&directory.staged) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                std::fs::remove_dir_all(&directory.staged)
            }
            Ok(_) => std::fs::remove_file(&directory.staged),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => Err(error),
        };
        match removal {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    repo_id,
                    storage = directory.kind,
                    staged_at = %directory.staged.display(),
                    live_path = %directory.live.display(),
                    %error,
                    "repository is deleted and the live filesystem path is free, but its staged data remains and must be removed by hand"
                );
                if cleanup_error.is_none() {
                    cleanup_error = Some(
                        crate::platform::fs::path_error(
                            directory.kind,
                            &directory.staged,
                            &error,
                            directory.hint,
                        )
                        .context(format!(
                            "failed to retire repository filesystem storage staged at {} from live path {}",
                            directory.staged.display(),
                            directory.live.display()
                        )),
                    );
                }
            }
        }
    }
    cleanup_error.map_or(Ok(()), Err)
}

async fn ensure_repository_deletion_is_quiescent(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<()> {
    let active = rg_db::ops::pipeline_ops::count_active_pipelines(db, repo_id)
        .await
        .context("failed to check active CI pipelines before repository deletion")?;
    if active > 0 {
        return Err(crate::error::conflict(format!(
            "repository has {active} active CI pipeline(s); cancel them or wait for them to finish before deleting the repository"
        )));
    }
    Ok(())
}

/// Delete a repository's Git/blob data and soft-delete its metadata row.
///
/// The filesystems and database cannot share a transaction. Rename every live
/// namespace out of the way first, then mutate the row. If any prepare step or
/// the database step fails, move the prepared namespaces back. Once the row is
/// deleted, the tombstones are unreachable and can be physically removed
/// without keeping the repository name occupied.
///
/// CI artifacts join that set through their jobs rather than through a
/// repository-shaped prefix (see [`artifact_blob_prefixes`]). The inventory
/// query runs before anything is moved, so a database that cannot answer
/// "which jobs are yours" aborts the deletion instead of reporting a success
/// that leaves artifact bytes live with no owner and no collector. The
/// artifact *rows* stay where every other child table stays — behind the
/// repository's own soft-delete — and the retention sweep that later reaches
/// one finds its object already gone, which
/// [`BlobStorage::delete`](crate::blob_storage::BlobStorage::delete) reports as
/// `Ok(false)`, not as a failure.
///
/// Pre-migration package files join it the same way and for the same reason
/// (see [`legacy_package_file_paths`]): their rows hold an absolute path, not a
/// key, so the `packages/<owner>/<repo>` prefix move passes them by.
///
/// The OCI registry joins the set through its own storage rather than through
/// `blob_storage`, because it may be configured onto a different backend and
/// because two of the three things it owns — the chunked-upload tree and the
/// legacy on-disk layout — are directories no `BlobKey` names. It obeys the
/// same rule: staged before the row is touched, restored if the row will not
/// move, discarded only afterwards.
pub async fn delete_repo(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    blob_storage: &dyn BlobStorage,
    oci_storage: &crate::package_registry::oci::storage::OciStorage,
    repo: &rg_db::entities::repository::Model,
) -> Result<()> {
    // `_ci_cache/<repo_id>` and `_artifacts/jobs/<job_id>` are writable by CI
    // workers outside this function. Refuse before the first rename rather
    // than moving a directory while a live job can immediately recreate it.
    ensure_repository_deletion_is_quiescent(db, repo.id).await?;
    let namespace = repository_namespace_name(db, repo.owner_id, repo.org_id).await?;
    let repo_path = repo_root.join(format!("{namespace}/{}.git", repo.name));
    let deletion_id = uuid::Uuid::new_v4().simple().to_string();
    // Validate every backend-neutral namespace before moving any live data. A
    // malformed historical name must fail without leaving Git staged aside.
    let mut prefixes = repository_blob_prefixes(&namespace, repo, &deletion_id)?;
    // Same rule for the artifact namespaces, which additionally need the
    // database to name them: a failed inventory is a failed deletion, not a
    // deletion that quietly keeps the artifacts.
    let job_ids = rg_db::ops::pipeline_ops::list_job_ids_by_repo(db, repo.id)
        .await
        .with_context(|| {
            format!(
                "failed to inventory the CI jobs of repository {} — its artifact storage cannot \
                 be retired without them",
                repo.id
            )
        })?;
    prefixes.extend(artifact_blob_prefixes(&job_ids, repo.id, &deletion_id)?);
    // Pre-migration package files are the third namespace the database has to
    // name: they are absolute paths, so no prefix over the storage keys reaches
    // them. Same rule again — inventory before the first rename.
    let legacy_package_files = legacy_package_file_paths(db, repo.id).await?;
    let filesystem_directories = repository_filesystem_directories(
        repo_root,
        &namespace,
        repo,
        &job_ids,
        &legacy_package_files,
    );
    let staged_path = repo_path.with_file_name(format!(
        "{}.deleted-{}-{}",
        repo_path
            .file_name()
            .context("repository path has no final component")?
            .to_string_lossy(),
        repo.id,
        deletion_id
    ));

    let staged = match repo_path.try_exists() {
        Ok(true) => {
            std::fs::rename(&repo_path, &staged_path)
                .map_err(|error| {
                    crate::platform::fs::path_error(
                        "repository directory",
                        &repo_path,
                        &error,
                        crate::platform::fs::REPO_ROOT_HINT,
                    )
                })
                .with_context(|| {
                    format!(
                        "failed to stage repository deletion at {}",
                        staged_path.display()
                    )
                })?;
            true
        }
        // The requested end state is already true on disk. Still soft-delete
        // the row so metadata and storage converge instead of making an
        // idempotent delete impossible to finish.
        Ok(false) => false,
        Err(error) => {
            return Err(crate::platform::fs::path_error(
                "repository directory",
                &repo_path,
                &error,
                crate::platform::fs::REPO_ROOT_HINT,
            ));
        }
    };

    let staged_blobs = match stage_blob_prefixes(blob_storage, prefixes, repo.id).await {
        Ok(staged_blobs) => staged_blobs,
        Err(error) => {
            if staged {
                restore_repository_directory(&staged_path, &repo_path, repo.id);
            }
            return Err(error);
        }
    };

    let staged_directories = match stage_repository_filesystem_directories(
        filesystem_directories,
        repo.id,
        &deletion_id,
    ) {
        Ok(staged_directories) => staged_directories,
        Err(error) => {
            restore_blob_prefixes(blob_storage, &staged_blobs, repo.id).await;
            if staged {
                restore_repository_directory(&staged_path, &repo_path, repo.id);
            }
            return Err(error);
        }
    };

    let staged_oci = match oci_storage
        .stage_repository_deletion(&namespace, &repo.name, repo.id, &deletion_id)
        .await
    {
        Ok(staged_oci) => staged_oci,
        Err(error) => {
            restore_repository_filesystem_directories(&staged_directories, repo.id);
            restore_blob_prefixes(blob_storage, &staged_blobs, repo.id).await;
            if staged {
                restore_repository_directory(&staged_path, &repo_path, repo.id);
            }
            return Err(error);
        }
    };

    // A trigger already past its repository lookup can race the first check.
    // Recheck after every storage namespace is staged; if it published work in
    // that window, restore everything and make the caller retry only after the
    // pipeline has settled.
    if let Err(error) = ensure_repository_deletion_is_quiescent(db, repo.id).await {
        oci_storage.restore_repository(staged_oci).await;
        restore_repository_filesystem_directories(&staged_directories, repo.id);
        restore_blob_prefixes(blob_storage, &staged_blobs, repo.id).await;
        if staged {
            restore_repository_directory(&staged_path, &repo_path, repo.id);
        }
        return Err(error);
    }

    if let Err(error) = rg_db::ops::repo_ops::soft_delete(db, repo.id).await {
        oci_storage.restore_repository(staged_oci).await;
        restore_repository_filesystem_directories(&staged_directories, repo.id);
        restore_blob_prefixes(blob_storage, &staged_blobs, repo.id).await;
        if staged {
            restore_repository_directory(&staged_path, &repo_path, repo.id);
        }
        return Err(error);
    }

    invalidate_perm_cache_repo(db, repo.id);

    let mut cleanup_error = None;
    if staged {
        match std::fs::remove_dir_all(&staged_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                repo_id = repo.id,
                path = %staged_path.display(),
                error = %error,
                "repository is deleted and its canonical name is free, but its old Git data \
                 remains on disk and must be removed by hand"
                );
                cleanup_error = Some(
                    crate::platform::fs::path_error(
                        "staged repository directory",
                        &staged_path,
                        &error,
                        crate::platform::fs::REPO_ROOT_HINT,
                    )
                    .context("failed to retire deleted repository Git data"),
                );
            }
        }
    }

    if let Err(error) = retire_repository_filesystem_directories(staged_directories, repo.id) {
        if cleanup_error.is_none() {
            cleanup_error = Some(error);
        }
    }

    for prefix in staged_blobs {
        if let Err(error) = blob_storage.delete_prefix(&prefix.staged).await {
            tracing::warn!(
                repo_id = repo.id,
                staged_prefix = %prefix.staged,
                live_prefix = %prefix.live,
                %error,
                "repository is deleted and its live blob namespace is free, but its staged blobs remain and must be removed by hand"
            );
            if cleanup_error.is_none() {
                cleanup_error = Some(anyhow::Error::new(error).context(format!(
                    "failed to retire staged repository blobs at {}",
                    prefix.staged
                )));
            }
        }
    }

    if let Err(error) = oci_storage.discard_repository(staged_oci).await {
        if cleanup_error.is_none() {
            cleanup_error =
                Some(error.context("failed to retire deleted repository registry data"));
        }
    }

    cleanup_error.map_or(Ok(()), Err)
}

/// Find repo by owner/name (skip soft-deleted).
pub async fn find_active_repo_by_owner_name(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<Option<rg_db::entities::repository::Model>> {
    // Reuse find_repo_by_owner_name logic but add deleted_at IS NULL filter
    // Actually existing find_repo_by_owner_name doesn't check deleted_at,
    // so we need to query via rg_db::ops and filter
    let repo = find_repo_by_owner_name(db, owner, repo_name).await?;
    Ok(repo.filter(|r| r.deleted_at.is_none()))
}

/// A repository that was just forked, and the account name it landed under.
///
/// The name is returned rather than re-derived by the caller because it is the
/// one that was actually used to build the directory on disk: an audit entry or
/// a response that spells the fork's path differently from where it lives is a
/// record of something that did not happen.
pub struct ForkedRepo {
    pub repo: rg_db::entities::repository::Model,
    /// The forker's account name — the `owner` half of the fork's `owner/name`.
    pub owner_username: String,
}

/// Fork `source_repo` into `user_id`'s namespace. Returns the forked repo.
///
/// **The read gate on the source is the caller's**, which is why the source
/// arrives as an already-resolved model rather than as a name to look up: the
/// HTTP route declares `RepoAuthRead` and the extractor hands the model over
/// having already asked `api::repo_access` whether this caller may read it.
/// This function used to re-decide that itself, off `can_read_repo` — a second
/// copy of the rule, living one crate away from the gate it was supposed to
/// mirror (card_b38bfb0f2b40).
pub async fn fork_repo(
    db: &DatabaseConnection,
    user_id: i64,
    owner: &str,
    source_repo: &rg_db::entities::repository::Model,
    repo_root: &std::path::Path,
) -> Result<ForkedRepo> {
    // Name already taken → 400. Each outcome carries its own type so the clone,
    // the `create_dir_all` and the queries in between keep their 5xx instead of
    // both answering 400.
    let repo_name = source_repo.name.as_str();

    // The forker id comes from a verified token, so a missing row here is our
    // inconsistency, not the caller's — it stays an untyped 500.
    let forker = user_ops::find_by_id(db, user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user not found"))?;

    // A fork lands in the forker's own account, so the namespace it has to be
    // free in is their personal one — never an organization they happen to own.
    ensure_repo_name_free(
        db,
        user_id,
        None,
        repo_name,
        None,
        &format!("repository '{repo_name}' already exists in your account"),
    )
    .await?;

    let source_path = repo_root.join(format!("{}/{}.git", owner, repo_name));
    let target_path = repo_root.join(format!("{}/{}.git", forker.username, repo_name));
    std::fs::create_dir_all(
        target_path
            .parent()
            .context("target path has no parent directory")?,
    )
    .with_context(|| format!("failed to create directory: {:?}", target_path.parent()))?;

    // TODO(gix): Local bare clone - gix doesn't support local bare clone via prepare_clone_bare
    // For now, use git CLI for local fork operations
    let git = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // The source is a clone *source*, so it goes through the URL form to avoid
    // Windows path issues. The target is a clone *destination*, which git reads
    // as a plain filesystem path and never as a URL: handed `file:///…` it
    // creates a literal `file:` directory under its own working directory
    // instead of the repository root. Canonicalizing it is impossible in any
    // case — the clone is what brings it into existence — so resolve the parent
    // `create_dir_all` just made and re-append the name.
    let source_url =
        path_to_git_url(&source_path).context("failed to convert source path to git URL")?;
    let target_dir =
        path_for_new_entry(&target_path).context("failed to resolve fork target directory")?;
    let target_arg = target_dir.to_string_lossy();

    let out = git
        .run(&["clone", "--bare", &source_url, &target_arg], None)
        .context("git clone --bare failed")?;
    out.ensure_success()?;

    let now = Utc::now();
    let model = RepoActiveModel {
        owner_id: Set(user_id),
        name: Set(repo_name.to_string()),
        description: Set(source_repo.description.clone()),
        is_private: Set(source_repo.is_private),
        default_branch: Set(source_repo.default_branch.clone()),
        fork_id: Set(None),
        stars_count: Set(0),
        forks_count: Set(0),
        org_id: Set(None),
        origin_repo_id: Set(Some(source_repo.id)),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        ..Default::default()
    };

    // The clone is already at the forker's canonical path and this row is what
    // was supposed to point at it. Without the rollback the retry passes the
    // database check (still no row) and dies in `git clone` on "destination
    // path already exists" — for good, since nothing ever removes that
    // directory.
    //
    // And losing the namespace-unique race is the same outcome the free-name
    // check above reports, reached a moment later — answered in its words, not
    // as a server fault. Every other write failure stays an error; the clone is
    // discarded either way.
    let forked = match repo_ops::create(db, model).await {
        Ok(forked) => forked,
        Err(error) => {
            discard_unreferenced_repo_dir(&target_path, &recreate_blocked_by(repo_name));
            return Err(if rg_db::is_unique_violation_anyhow(&error) {
                crate::error::conflict(format!(
                    "repository '{repo_name}' already exists in your account"
                ))
            } else {
                error
            });
        }
    };

    // A counter, not the fork: the row and the clone are both in place by now,
    // so failing the request here would report a fork that actually happened as
    // a server error — and the retry would be refused as a duplicate.
    if let Err(error) = repo_ops::update_forks_count(db, source_repo.id).await {
        tracing::warn!(
            source_repo_id = source_repo.id,
            fork_repo_id = forked.id,
            error = %format!("{error:#}"),
            "fork count not updated — the source repository now under-reports its forks"
        );
    }

    Ok(ForkedRepo {
        repo: forked,
        owner_username: forker.username,
    })
}

/// List forks of a repository.
pub async fn list_forks(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    offset: u64,
    limit: u64,
) -> Result<(Vec<rg_db::entities::repository::Model>, i64)> {
    let repo = find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository not found"))?;
    repo_ops::list_forks(db, repo.id, offset, limit).await
}

/// Transfer a repository to a new owner.
///
/// The check here is on the *source*: only its owner may give it away. Whether
/// the caller may put a repository into `new_owner/` at all is the destination
/// namespace's question, and it is answered by the route's `NamespaceCreate`
/// gate in `rg-http` — the same rule `create_repo` applies to its `org` field,
/// stated once there rather than a second time here.
#[allow(
    clippy::too_many_arguments,
    reason = "the service boundary receives the explicit database, source and destination namespaces, Git root, and independently configured blob and OCI backends so callers cannot accidentally transfer only one storage domain"
)]
pub async fn transfer_repo(
    db: &DatabaseConnection,
    user_id: i64,
    owner: &str,
    repo_name: &str,
    new_owner: &str,
    repo_root: &std::path::Path,
    blob_storage: &dyn BlobStorage,
    oci_storage: &crate::package_registry::oci::storage::OciStorage,
) -> Result<rg_db::entities::repository::Model> {
    let repo = find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| crate::error::not_found("repository"))?;

    if repo.owner_id != user_id {
        return Err(crate::error::forbidden(
            "only the repository owner can transfer it",
        ));
    }

    let (new_owner_id, new_org_id, new_owner_name) = resolve_owner(db, new_owner).await?;

    ensure_repo_name_free(
        db,
        new_owner_id,
        new_org_id,
        repo_name,
        // A transfer moves this row; it is not a second repository of that name.
        Some(repo.id),
        &format!("repository '{repo_name}' already exists at destination"),
    )
    .await?;

    // Build every BlobKey before moving Git. A malformed historical namespace
    // must fail as one untouched transfer, not after the repository directory
    // has already left its owner.
    let blob_prefixes = repository_transfer_blob_prefixes(owner, &new_owner_name, repo_name)?;
    let old_path = repo_root.join(format!("{}/{}.git", owner, repo_name));
    let new_path = repo_root.join(format!("{}/{}.git", new_owner_name, repo_name));
    std::fs::create_dir_all(
        new_path
            .parent()
            .context("new path has no parent directory")?,
    )
    .with_context(|| format!("failed to create directory: {:?}", new_path.parent()))?;
    std::fs::rename(&old_path, &new_path).with_context(|| {
        format!(
            "failed to move repository from {:?} to {:?}",
            old_path, new_path
        )
    })?;

    let moved_blobs = match transfer_blob_prefixes(blob_storage, blob_prefixes, repo.id).await {
        Ok(moved) => moved,
        Err(error) => {
            restore_transferred_repository_directory(&new_path, &old_path, repo.id);
            return Err(error);
        }
    };

    // New backend-neutral keys replaced the two paths below, but installations
    // that predate that migration can still read these directories. They are
    // just as namespace-bound as their blob counterparts and must move too.
    let legacy_paths = [
        (
            crate::lfs::service::lfs_root(repo_root, owner, repo_name),
            crate::lfs::service::lfs_root(repo_root, &new_owner_name, repo_name),
            "legacy LFS directory",
        ),
        (
            crate::release::service::legacy_asset_root(repo_root, owner, repo_name),
            crate::release::service::legacy_asset_root(repo_root, &new_owner_name, repo_name),
            "legacy release asset directory",
        ),
    ];
    let mut moved_directories = Vec::new();
    for (source, destination, kind) in legacy_paths {
        match transfer_optional_repository_directory(source, destination, kind) {
            Ok(Some(moved)) => moved_directories.push(moved),
            Ok(None) => {}
            Err(error) => {
                restore_transferred_blob_prefixes(blob_storage, &moved_blobs, repo.id).await;
                restore_transferred_repository_directory(&new_path, &old_path, repo.id);
                return Err(error);
            }
        }
    }

    let moved_oci = match oci_storage
        .transfer_repository(owner, repo_name, &new_owner_name, repo_name)
        .await
    {
        Ok(moved) => moved,
        Err(error) => {
            restore_transferred_repository_directories(&moved_directories, repo.id);
            restore_transferred_blob_prefixes(blob_storage, &moved_blobs, repo.id).await;
            restore_transferred_repository_directory(&new_path, &old_path, repo.id);
            return Err(error);
        }
    };

    if let Err(error) = repo_ops::transfer_owner(
        db,
        repo.id,
        new_owner_id,
        new_org_id,
        owner,
        &new_owner_name,
        repo_name,
    )
    .await
    {
        oci_storage.restore_repository_transfer(moved_oci).await;
        restore_transferred_repository_directories(&moved_directories, repo.id);
        restore_transferred_blob_prefixes(blob_storage, &moved_blobs, repo.id).await;
        restore_transferred_repository_directory(&new_path, &old_path, repo.id);
        // Same race as the create paths, one statement later: the free-name
        // check above passed, and someone else claimed the destination name
        // before this update landed. That is the caller's answer in the words
        // that check uses, not a server fault. Any other failure stays an error.
        return Err(if rg_db::is_unique_violation_anyhow(&error) {
            crate::error::conflict(format!(
                "repository '{repo_name}' already exists at destination"
            ))
        } else {
            error
        });
    }
    // Ownership (and thus who can read/write) changed — drop cached decisions.
    invalidate_perm_cache_repo(db, repo.id);

    // Read the moved row back from the namespace it landed in — the same
    // `(owner_id, org_id)` pair `update_owner` just wrote.
    let moved = match new_org_id {
        Some(org_id) => repo_ops::find_by_org_and_name(db, org_id, repo_name).await?,
        None => repo_ops::find_personal_by_owner_and_name(db, new_owner_id, repo_name).await?,
    };
    moved.ok_or_else(|| anyhow::anyhow!("repository not found after transfer"))
}

// ── Commit Status ──────────────────────────────────────────────────────

/// Create a commit status. Validates that state is one of: pending, success, failure, error.
// Wide by design: mirrors the commit_status column set.
#[allow(clippy::too_many_arguments)]
pub async fn create_commit_status(
    db: &DatabaseConnection,
    repo_id: i64,
    sha: &str,
    state: &str,
    context: &str,
    description: Option<&str>,
    target_url: Option<&str>,
    creator_id: i64,
) -> Result<rg_db::entities::commit_status::Model> {
    let valid_states = ["pending", "success", "failure", "error"];
    if !valid_states.contains(&state) {
        return Err(crate::error::invalid_request(format!(
            "invalid commit status state: '{state}', must be one of: {valid_states:?}"
        )));
    }

    let now = Utc::now();
    let model = rg_db::entities::commit_status::ActiveModel {
        repo_id: sea_orm::Set(repo_id),
        sha: sea_orm::Set(sha.to_string()),
        state: sea_orm::Set(state.to_string()),
        context: sea_orm::Set(context.to_string()),
        description: sea_orm::Set(description.map(str::to_string)),
        target_url: sea_orm::Set(target_url.map(str::to_string)),
        creator_id: sea_orm::Set(creator_id),
        created_at: sea_orm::Set(now),
        updated_at: sea_orm::Set(now),
        ..Default::default()
    };

    rg_db::ops::commit_status_ops::create_or_update(db, repo_id, sha, context, model).await
}

/// List all statuses for a commit SHA in a repository.
pub async fn list_commit_statuses(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    sha: &str,
) -> Result<Vec<rg_db::entities::commit_status::Model>> {
    let repo = find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository not found"))?;
    rg_db::ops::commit_status_ops::list_by_sha(db, repo.id, sha).await
}

/// Get the combined status for a commit SHA.
/// Returns "failure" if any failure, "pending" if any pending, "success" otherwise.
pub async fn get_combined_status(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    sha: &str,
) -> Result<serde_json::Value> {
    let repo = find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository not found"))?;

    let counts = rg_db::ops::commit_status_ops::get_combined_status(db, repo.id, sha).await?;
    let total: i64 = counts.iter().map(|(_, c)| c).sum();

    if total == 0 {
        return Ok(serde_json::json!({
            "state": "pending",
            "sha": sha,
            "total_count": 0,
            "statuses": []
        }));
    }

    let state_map: std::collections::HashMap<&str, i64> =
        counts.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let combined = if state_map.get("failure").is_some_and(|&c| c > 0)
        || state_map.get("error").is_some_and(|&c| c > 0)
    {
        "failure"
    } else if state_map.get("pending").is_some_and(|&c| c > 0) {
        "pending"
    } else {
        "success"
    };

    let statuses = rg_db::ops::commit_status_ops::list_by_sha(db, repo.id, sha).await?;

    Ok(serde_json::json!({
        "state": combined,
        "sha": sha,
        "total_count": total,
        "statuses": statuses
    }))
}

// ── Watch Notifications ────────────────────────────────────────────────

/// Notify watchers of a push event to a repository.
///
/// Called from [`crate::push_hooks::post_push_hooks`], so every transport that
/// runs the post-push hooks fans out to watchers. `pusher_name` is empty when
/// the transport authenticated nobody (open-access server): the body then omits
/// the actor instead of rendering a leading blank, and no recipient is excluded.
///
/// Detached onto `tracker` rather than awaited: the hook run is itself a tracked
/// background task, and one repository's subscriber walk has no business
/// delaying the CI trigger of the next ref in the same push.
pub fn notify_watchers_push(
    db: &DatabaseConnection,
    tracker: &crate::task_tracker::TaskTracker,
    repo_id: i64,
    repo_name: &str,
    pusher_name: &str,
    ref_name: &str,
) {
    let body = if pusher_name.is_empty() {
        format!("New push to {}", ref_name)
    } else {
        format!("{} pushed to {}", pusher_name, ref_name)
    };
    crate::notification::spawn_notify_watchers(
        db,
        tracker,
        crate::notification::WatchEvent {
            repo_id,
            author_name: pusher_name.to_string(),
            title: format!("New push to {}", repo_name),
            notification_type: "push".to_string(),
            body: Some(body),
        },
    );
}

/// Create or update a file in a repository.
///
/// This function:
/// 1. Creates a temp working directory
/// 2. Clones the bare repo
/// 3. Creates/updates the file
/// 4. Commits the change
/// 5. Pushes back to the bare repo
/// 6. Cleans up the temp directory
///
/// - `file_path`: Path within the repo (e.g., "README.md" or "docs/api.md")
/// - `content`: File content (UTF-8 string)
/// - `message`: Commit message
/// - `branch`: Target branch (default: repo's default branch)
/// - `sha`: Blob SHA of the file being updated (required for updates to prevent overwrites)
// Wide by design: threads the full write-a-commit context (repo identity, path, content, author).
#[allow(clippy::too_many_arguments)]
pub async fn create_or_update_file(
    _db: &DatabaseConnection,
    _repo_id: i64,
    owner: &str,
    repo_name: &str,
    file_path: &str,
    content: &str,
    message: &str,
    branch: &str,
    sha: Option<&str>,
    author_name: &str,
    author_email: &str,
    repo_root: &std::path::Path,
) -> Result<()> {
    validate_repo_file_path(file_path)?;

    let repo_path = repo_root.join(format!("{}/{}.git", owner, repo_name));

    // The repository row exists (the handler resolved it) but its bare tree does
    // not: a broken `repo_root`, not a broken request. Untyped on purpose, so it
    // stays a 5xx and the path reaches the operator log only.
    if !repo_path.exists() {
        bail!("repository path not found: {:?}", repo_path);
    }

    // Verify the file SHA if this is an update (not a create)
    if let Some(expected_sha) = sha {
        // Check if the file exists and its current SHA matches
        match get_file_sha(&repo_path, branch, file_path)? {
            None => return Err(crate::error::not_found("file")),
            Some(current) if current != expected_sha => {
                // Someone else wrote the file since the caller read it. A 409
                // says "re-read and retry"; a 400 would tell the client to fix
                // a request that was never malformed.
                return Err(crate::error::conflict(format!(
                    "file SHA mismatch: expected {expected_sha}, got {current}"
                )));
            }
            Some(_) => {}
        }
    } else {
        // This is a create operation - check if file already exists
        if get_file_sha(&repo_path, branch, file_path)?.is_some() {
            return Err(crate::error::conflict(format!(
                "file already exists: {file_path} (use update with sha)"
            )));
        }
    }

    // Create temp working directory
    let tmp = std::env::temp_dir().join(format!("forgekeep-file-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)
        .map_err(|error| temp_tree_error("file-edit working tree", &tmp, &error))?;

    // One cleanup point behind the body — see `auto_init_repo`. The per-branch
    // `discard_dir` calls this used to carry covered the `bail!`s and none of
    // the eleven `?`s between them, so a git CLI that was merely missing leaked
    // a full clone of the repository.
    let result = (|| -> Result<()> {
        // Clone the repo
        let clone_url =
            path_to_git_url(&repo_path).context("failed to convert repo path to git URL")?;
        let tmp_str = tmp.to_string_lossy();
        let gateway = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .map_err(|e| anyhow::anyhow!("git CLI not available: {e}"))?;

        // Try cloning with the target branch; fall back to --no-checkout for new repos
        // where the branch does not exist yet, then create the branch via checkout -b.
        let clone_out = gateway
            .run(&["clone", "-b", branch, &clone_url, &tmp_str], None)
            .context("git clone failed")?;
        if !clone_out.success() {
            let nc_out = gateway
                .run(&["clone", "--no-checkout", &clone_url, &tmp_str], None)
                .context("git clone (no-checkout) failed")?;
            if !nc_out.success() {
                bail!("git clone failed: {}", nc_out.stderr_str());
            }
            let co_out = gateway
                .run(&["checkout", "-b", branch], Some(&tmp))
                .context("git checkout failed")?;
            if !co_out.success() {
                bail!("git checkout failed: {}", co_out.stderr_str());
            }
        }

        // Write the file
        let full_path = tmp.join(file_path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| temp_tree_error("file-edit working tree", parent, &error))?;
        }
        std::fs::write(&full_path, content)
            .map_err(|error| temp_tree_error("edited file", &full_path, &error))?;

        // Git add
        // `--` before the client path: `file_path` is the caller's own string
        // and `validate_repo_file_path` lets a leading `-` through, so without
        // the separator git parses `-rf`/`--renormalize`/… as an option instead
        // of a pathspec — the same argument-injection class as the sibling on
        // the next commit (`add`/`push` both take the separator) and the one
        // closed in `download_archive`.
        let output = gateway
            .run(&["add", "--", file_path], Some(&tmp))
            .context("git add failed")?;
        if !output.success() {
            bail!("git add failed: {}", output.stderr_str());
        }

        // Git commit
        let identity = git_identity_env(author_name, author_email);
        let output = gateway
            .run_with_env(&["commit", "-m", message], Some(&tmp), &identity)
            .context("git commit failed")?;
        if !output.success() {
            bail!("git commit failed: {}", output.stderr_str());
        }

        // Git push
        let push_url =
            path_to_git_url(&repo_path).context("failed to convert repo path to git URL")?;

        // `--` before the client-chosen `branch`: a refspec beginning with `-`
        // is otherwise parsed as an option (`--receive-pack=<cmd>` reaches a
        // shell), so keep the separator even though a dash-leading branch is
        // rejected earlier by `checkout -b` — the guarantee comes from the
        // separator, not from that upstream check.
        let output = gateway
            .run(&["push", &push_url, "--", branch], Some(&tmp))
            .context("git push failed")?;
        if !output.success() {
            bail!("git push failed: {}", output.stderr_str());
        }

        tracing::info!(
            repo = %repo_name,
            file = %file_path,
            branch = %branch,
            "file created/updated successfully"
        );

        Ok(())
    })();
    discard_dir("file-edit working tree", &tmp);
    result
}

#[derive(Debug, Clone)]
pub struct FileUpdate {
    pub path: String,
    pub content: String,
    pub expected_blob_sha: String,
}

/// Look up a branch SHA without collapsing an absent ref into a repository
/// failure. Callers that hold a persisted branch name can report the former as
/// stale resource state while keeping an unreadable ref store as an operation
/// failure.
pub fn try_get_branch_sha(repo_path: &std::path::Path, branch: &str) -> Result<Option<String>> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    let ref_name = format!("refs/heads/{branch}");
    let Some(mut reference) = repo
        .try_find_reference(ref_name.as_str())
        .with_context(|| {
            format!(
                "failed to look up {ref_name} in repository: {:?}",
                repo_path
            )
        })?
    else {
        return Ok(None);
    };
    let id = reference.peel_to_id().with_context(|| {
        format!(
            "failed to resolve {ref_name} in repository: {:?}",
            repo_path
        )
    })?;
    Ok(Some(id.to_string()))
}

/// Confirm that a persisted branch still names the expected commit before an
/// operation starts a working clone. A missing ref is stale PR state; an error
/// opening or resolving the repository remains an operational failure.
fn ensure_expected_branch_head(
    repo_path: &std::path::Path,
    branch: &str,
    expected_head_sha: &str,
) -> Result<()> {
    let actual_head = try_get_branch_sha(repo_path, branch)?
        .ok_or_else(|| crate::error::conflict(format!("branch '{branch}' no longer exists")))?;
    if actual_head != expected_head_sha {
        return Err(crate::error::conflict(format!(
            "branch head changed: expected {expected_head_sha}, got {actual_head}"
        )));
    }
    Ok(())
}

/// Update multiple existing files and publish them as one commit.
///
/// The branch head and every touched blob are checked before writing. The
/// normal fast-forward push is the final concurrency guard if the branch moves
/// after those checks.
// Wide by design: threads the full write-a-commit context (repo identity, files, author).
#[allow(clippy::too_many_arguments)]
pub fn update_files_in_commit(
    owner: &str,
    repo_name: &str,
    branch: &str,
    expected_head_sha: &str,
    updates: &[FileUpdate],
    message: &str,
    author_name: &str,
    author_email: &str,
    repo_root: &std::path::Path,
) -> Result<String> {
    if updates.is_empty() {
        bail!("at least one file update is required");
    }
    let repo_path = repo_root.join(format!("{owner}/{repo_name}.git"));
    if !repo_path.exists() {
        bail!("repository path not found: {:?}", repo_path);
    }
    ensure_expected_branch_head(&repo_path, branch, expected_head_sha)?;

    let mut unique_paths = HashSet::new();
    for update in updates {
        let path = std::path::Path::new(&update.path);
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            bail!("invalid repository file path: {}", update.path);
        }
        if !unique_paths.insert(update.path.as_str()) {
            bail!("duplicate file update: {}", update.path);
        }
    }

    let tmp = std::env::temp_dir().join(format!("forgekeep-files-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)
        .map_err(|error| temp_tree_error("commit working tree", &tmp, &error))?;
    let result = (|| -> Result<String> {
        let clone_url = path_to_git_url(&repo_path)?;
        let tmp_str = tmp.to_string_lossy();
        let gateway = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .map_err(|error| anyhow::anyhow!("git CLI not available: {error}"))?;
        let clone = gateway.run(
            &[
                "clone",
                "--branch",
                branch,
                "--single-branch",
                "--",
                &clone_url,
                &tmp_str,
            ],
            None,
        )?;
        if !clone.success() {
            // A ref can disappear between the preflight check and clone. Check
            // it once more so that normal stale-PR state still reaches callers
            // as 409; a live ref means the clone failure is genuinely ours.
            ensure_expected_branch_head(&repo_path, branch, expected_head_sha)?;
            bail!("git clone failed: {}", clone.stderr_str());
        }

        let head = gateway.run(&["rev-parse", "HEAD"], Some(&tmp))?;
        head.ensure_success()?;
        let actual_head = head.stdout_str().trim().to_string();
        if actual_head != expected_head_sha {
            // Optimistic-concurrency loss, not a malformed request: someone
            // else pushed first. A 409 tells the caller to re-read and retry.
            return Err(crate::error::conflict(format!(
                "branch head changed: expected {expected_head_sha}, got {actual_head}"
            )));
        }

        for update in updates {
            let object = format!("HEAD:{}", update.path);
            let blob = gateway.run(&["rev-parse", &object], Some(&tmp))?;
            blob.ensure_success()?;
            let actual_blob = blob.stdout_str().trim().to_string();
            if actual_blob != update.expected_blob_sha {
                return Err(crate::error::conflict(format!(
                    "file SHA mismatch for {}: expected {}, got {}",
                    update.path, update.expected_blob_sha, actual_blob
                )));
            }

            let mut full_path = tmp.clone();
            for component in std::path::Path::new(&update.path).components() {
                let std::path::Component::Normal(component) = component else {
                    unreachable!("path was validated above")
                };
                full_path.push(component);
                if let Ok(metadata) = std::fs::symlink_metadata(&full_path) {
                    if metadata.file_type().is_symlink() {
                        bail!("refusing to update symlink path: {}", update.path);
                    }
                }
            }
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| temp_tree_error("commit working tree", parent, &error))?;
            }
            std::fs::write(&full_path, &update.content)
                .map_err(|error| temp_tree_error("committed file", &full_path, &error))?;
            let add = gateway.run(&["add", "--", &update.path], Some(&tmp))?;
            add.ensure_success()?;
        }

        let identity = git_identity_env(author_name, author_email);
        let commit = gateway.run_with_env(&["commit", "-m", message], Some(&tmp), &identity)?;
        commit.ensure_success()?;
        let commit_sha = gateway.run(&["rev-parse", "HEAD"], Some(&tmp))?;
        commit_sha.ensure_success()?;
        let commit_sha = commit_sha.stdout_str().trim().to_string();

        let push_url = path_to_git_url(&repo_path)?;
        let destination = format!("HEAD:refs/heads/{branch}");
        let push = gateway.run(&["push", &push_url, &destination], Some(&tmp))?;
        push.ensure_success()?;
        Ok(commit_sha)
    })();
    discard_dir("commit working tree", &tmp);
    result
}

/// Delete a file from a repository.
///
/// Similar to `create_or_update_file()`, but removes the file instead.
// Wide by design: threads the full write-a-commit context (repo identity, path, author).
#[allow(clippy::too_many_arguments)]
pub async fn delete_file(
    _db: &DatabaseConnection,
    _repo_id: i64,
    owner: &str,
    repo_name: &str,
    file_path: &str,
    message: &str,
    branch: &str,
    sha: &str,
    author_name: &str,
    author_email: &str,
    repo_root: &std::path::Path,
) -> Result<()> {
    validate_repo_file_path(file_path)?;

    let repo_path = repo_root.join(format!("{}/{}.git", owner, repo_name));

    // See `create_or_update_file`: a missing bare tree is ours, so it stays 5xx.
    if !repo_path.exists() {
        bail!("repository path not found: {:?}", repo_path);
    }

    // Verify the file SHA to prevent accidental deletes
    match get_file_sha(&repo_path, branch, file_path)? {
        None => return Err(crate::error::not_found("file")),
        Some(current) if current != sha => {
            return Err(crate::error::conflict(format!(
                "file SHA mismatch: expected {sha}, got {current}"
            )));
        }
        Some(_) => {}
    }

    // Create temp working directory
    let tmp = std::env::temp_dir().join(format!("forgekeep-file-del-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)
        .map_err(|error| temp_tree_error("file-delete working tree", &tmp, &error))?;

    // One cleanup point behind the body — see `auto_init_repo`.
    let result = (|| -> Result<()> {
        // Clone the repo
        let clone_url =
            path_to_git_url(&repo_path).context("failed to convert repo path to git URL")?;
        let tmp_str = tmp.to_string_lossy();
        let gateway = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .map_err(|e| anyhow::anyhow!("git CLI not available: {e}"))?;

        let output = gateway
            .run(&["clone", "-b", branch, &clone_url, &tmp_str], None)
            .context("git clone failed")?;
        if !output.success() {
            bail!("git clone failed: {}", output.stderr_str());
        }

        // Delete the file. `--` before the client path for the same reason as
        // the `add` in `create_or_update_file`: `validate_repo_file_path` lets
        // a leading `-` through, and without the separator git reads
        // `--cached`/`-r`/… as an option instead of a pathspec.
        let output = gateway
            .run(&["rm", "--", file_path], Some(&tmp))
            .context("git rm failed")?;
        if !output.success() {
            bail!("git rm failed: {}", output.stderr_str());
        }

        // Git commit
        let identity = git_identity_env(author_name, author_email);
        let output = gateway
            .run_with_env(&["commit", "-m", message], Some(&tmp), &identity)
            .context("git commit failed")?;
        if !output.success() {
            bail!("git commit failed: {}", output.stderr_str());
        }

        // Git push. `--` before `branch`: same argument-injection guard as the
        // push in `create_or_update_file`.
        let push_url =
            path_to_git_url(&repo_path).context("failed to convert repo path to git URL")?;

        let output = gateway
            .run(&["push", &push_url, "--", branch], Some(&tmp))
            .context("git push failed")?;
        if !output.success() {
            bail!("git push failed: {}", output.stderr_str());
        }

        tracing::info!(
            repo = %repo_name,
            file = %file_path,
            branch = %branch,
            "file deleted successfully"
        );

        Ok(())
    })();
    discard_dir("file-delete working tree", &tmp);
    result
}

/// Reject a repository-relative file path before it is joined onto a working
/// tree.
///
/// [`update_files_in_commit`] has always validated its paths this way; the
/// single-file write endpoints joined the client's path onto the temp clone
/// unchecked, and `PathBuf::join` happily leaves the tree for an absolute path
/// or a `..` component — so `..%2f..%2fetc%2fcron.d%2fx` reached
/// `std::fs::write` (H-02). The path is the caller's own input, so this is a
/// typed [`crate::error::InvalidRequest`]: a real `400`, not a 5xx.
fn validate_repo_file_path(file_path: &str) -> Result<()> {
    let path = std::path::Path::new(file_path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        // Fixed text on purpose: the message reaches the client verbatim, and
        // echoing the rejected path back would put a caller-chosen absolute
        // path (`/etc/...`, `/tmp/...`) into a response body that H-05 tests
        // read as a storage-path leak.
        return Err(crate::error::invalid_request(
            "invalid repository file path",
        ));
    }
    Ok(())
}

/// Get the blob SHA of a file at a given ref, or `None` when the path is not in
/// that commit.
///
/// The two outcomes have to stay apart at the type level: callers used to write
/// `get_file_sha(..).ok()`, which turned "this repository cannot be opened"
/// into "the file is not there" — the same collapse card_aa048c2956b1 fixed on
/// the read endpoints, one write endpoint over.
fn get_file_sha(
    repo_path: &std::path::Path,
    git_ref: &str,
    file_path: &str,
) -> Result<Option<String>> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let ref_name = if git_ref.starts_with("refs/") {
        git_ref.to_string()
    } else {
        format!("refs/heads/{git_ref}")
    };
    let Some(mut reference) = repo
        .try_find_reference(ref_name.as_str())
        .with_context(|| format!("failed to look up ref {ref_name}"))?
    else {
        // Creating the first file on an unborn branch is a normal absence, not
        // an object-store fault. This is the same explicit `Option` boundary as
        // a missing path below.
        return Ok(None);
    };

    let tree = reference
        .peel_to_id()
        .with_context(|| format!("failed to peel ref {ref_name}"))?
        .object()
        .with_context(|| format!("failed to read commit at ref {ref_name}"))?
        .peel_to_tree()
        .with_context(|| format!("failed to read tree at ref {ref_name}"))?;

    let Some(entry) = tree
        .lookup_entry_by_path(file_path)
        .with_context(|| format!("failed to look up {file_path} at ref {ref_name}"))?
    else {
        return Ok(None);
    };

    // Return the tree's SHA, but first force the object lookup: a dangling
    // entry is a storage fault, not a file the client may safely recreate.
    entry
        .object()
        .with_context(|| format!("failed to read {file_path} at ref {ref_name}"))?;
    Ok(Some(entry.object_id().to_string()))
}

#[cfg(test)]
mod path_diagnostic_tests {
    use super::*;

    /// The temp-tree path is `TMPDIR`-derived and generated per call, so the
    /// message has to carry both halves an operator needs: which directory
    /// failed, and the variable that moves it somewhere writable.
    #[test]
    fn temp_tree_error_names_the_directory_and_the_variable_that_moves_it() {
        let dir = tempfile::tempdir().unwrap();
        let occupied = dir.path().join("occupied");
        std::fs::write(&occupied, b"file").unwrap();
        let tmp = occupied.join("forgekeep-file-1");

        let error = std::fs::create_dir_all(&tmp).expect_err("a file cannot host a subdirectory");
        let rendered = temp_tree_error("file-edit working tree", &tmp, &error).to_string();

        assert!(rendered.contains(&tmp.display().to_string()), "{rendered}");
        assert!(rendered.contains("TMPDIR"), "{rendered}");
    }

    /// A permission failure trades the caller remedy for the uid diagnostic in
    /// [`describe_path_error`]; for a temp tree both matter, so the `TMPDIR`
    /// hint has to survive alongside it.
    #[cfg(unix)]
    #[test]
    fn temp_tree_error_keeps_the_tmpdir_hint_on_a_permission_failure() {
        let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let rendered = temp_tree_error(
            "commit working tree",
            std::path::Path::new("/tmp/forgekeep-files-1"),
            &error,
        )
        .to_string();

        assert!(rendered.contains("this process runs as uid="), "{rendered}");
        assert!(rendered.contains("TMPDIR"), "{rendered}");
    }

    /// A working tree that outlives its request is a whole clone of the
    /// repository, left in `TMPDIR` under a UUID no log can tie back to
    /// anything — so no error path may return without it being gone.
    ///
    /// `auto_init_repo` pushes into the bare path it is handed; pointing it at
    /// an ordinary directory fails that push after the tree has been built,
    /// which is the branch that used to `bail!` straight past the cleanup. A
    /// machine with no `git` at all fails earlier inside the same body, and the
    /// assertion holds either way — which is the point of one cleanup tail
    /// rather than a `discard_dir` per branch.
    ///
    /// `TMPDIR` is process-wide and `tempfile::tempdir()` reads it, so a test
    /// that pointed it into its own `TempDir` would delete the directory other
    /// tests in this binary were handed. Hence a plain directory that is never
    /// removed recursively, and an assertion scoped to this function's own
    /// `forgekeep-init-` prefix rather than to "the directory is empty".
    #[test]
    fn a_failed_auto_init_leaves_no_working_tree_behind() {
        let sandbox = tempfile::tempdir().expect("create sandbox");
        let private_tmp =
            std::env::temp_dir().join(format!("forgekeep-cleanup-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&private_tmp).expect("create private TMPDIR");
        let previous_tmpdir = std::env::var_os("TMPDIR");
        std::env::set_var("TMPDIR", &private_tmp);

        let not_a_repo = sandbox.path().join("bare");
        std::fs::create_dir_all(&not_a_repo).expect("create the push target");

        let outcome = auto_init_repo(
            &not_a_repo,
            "notes",
            "",
            "main",
            None,
            None,
            Some("default"),
            "alice",
            "Alice",
            "alice@example.com",
        );

        let leftovers: Vec<String> = std::fs::read_dir(&private_tmp)
            .expect("read the private TMPDIR")
            .map(|entry| {
                entry
                    .expect("TMPDIR entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.starts_with("forgekeep-init-"))
            .collect();

        // Put the environment back before asserting, so a failure here cannot
        // leave every later test staging into this directory.
        match previous_tmpdir {
            Some(value) => std::env::set_var("TMPDIR", value),
            None => std::env::remove_var("TMPDIR"),
        }
        // Non-recursive on purpose: if another test staged into this directory
        // while it was `TMPDIR`, this fails and leaves its files alone.
        drop(std::fs::remove_dir(&private_tmp));

        let error =
            outcome.expect_err("pushing into a directory that is not a repository must fail");
        assert!(
            leftovers.is_empty(),
            "the failed auto-init left a working tree behind: {leftovers:?} \
             (it failed with: {error:#})"
        );
    }

    /// The rollback of a repository directory is best-effort, and the operator
    /// only ever sees the failure that triggered it — so when the rollback
    /// fails too, the log line is the sole record of a name that can no longer
    /// be created, and it has to carry the path and that consequence.
    #[test]
    fn a_failed_repo_dir_rollback_names_the_path_and_what_it_blocks() {
        use std::sync::{Arc, Mutex};

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

        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let sandbox = tempfile::tempdir().expect("create sandbox");
        // A regular file is a directory removal that fails for a reason other
        // than "already gone" — no permission games, no root-vs-user surprise.
        let not_a_directory = sandbox.path().join("notes.git");
        std::fs::write(&not_a_directory, b"not a directory").expect("write the stand-in");

        discard_unreferenced_repo_dir(&not_a_directory, &recreate_blocked_by("notes"));

        let rendered = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert!(
            rendered.contains(&not_a_directory.display().to_string()),
            "{rendered}"
        );
        assert!(
            rendered.contains("creating notes will keep failing"),
            "{rendered}"
        );
        assert!(not_a_directory.exists(), "nothing was removed, as expected");
    }

    /// The common case is not a failure: a rollback of a directory that is
    /// already gone is the normal outcome of a step that failed before creating
    /// anything, and must stay silent rather than cry wolf.
    #[test]
    fn rolling_back_an_absent_repo_dir_says_nothing() {
        let sandbox = tempfile::tempdir().expect("create sandbox");
        discard_unreferenced_repo_dir(
            &sandbox.path().join("never-existed.git"),
            &recreate_blocked_by("never-existed"),
        );
    }
}

#[cfg(test)]
mod repository_fts_tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, ConnectionTrait, DatabaseBackend, EntityTrait, Statement};

    async fn fixture(name: &str) -> (tempfile::TempDir, DatabaseConnection, i64) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join(format!("{name}.db"));
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", db_path.display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            2,
        )
        .await
        .expect("connect sqlite");
        rg_db::run_migrations(&db).await.expect("run migrations");
        let user = rg_db::ops::user_ops::create_user(
            &db,
            name,
            &format!("{name}@example.invalid"),
            "",
            name,
        )
        .await
        .expect("create user");
        (dir, db, user.id)
    }

    fn create_options(owner_id: i64, name: &str, description: &str) -> CreateRepoOptions {
        CreateRepoOptions {
            owner_id,
            name: name.to_string(),
            description: Some(description.to_string()),
            is_private: false,
            org_id: None,
            default_branch: Some("main".to_string()),
            auto_init: false,
            gitignores: None,
            license: None,
            readme: None,
            issue_labels: None,
            owner_display_name: String::new(),
            git_author_name: None,
            git_author_email: None,
        }
    }

    async fn repo_fts_snapshot(db: &DatabaseConnection, repo_id: i64) -> Option<(String, String)> {
        db.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT name, description FROM repos_fts WHERE rowid = ?",
            [repo_id.into()],
        ))
        .await
        .expect("read repository FTS row")
        .map(|row| {
            (
                row.try_get("", "name").expect("decode FTS name"),
                row.try_get("", "description")
                    .expect("decode FTS description"),
            )
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn late_create_completion_cannot_overwrite_newer_repository_fts_snapshot() {
        let (dir, db, owner_id) = fixture("repoftsrace").await;
        let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let first_db = db.clone();
        let repo_root = dir.path().join("repos");
        let first = tokio::spawn(async move {
            create_repo_with_post_commit(
                &first_db,
                create_options(owner_id, "stale-repo", "stale alpha description"),
                &repo_root,
                move || async move {
                    committed_tx.send(()).expect("announce source commit");
                    release_rx.await.expect("release late create");
                },
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(10), committed_rx)
            .await
            .expect("repository create did not commit in time")
            .expect("repository create dropped its commit signal");
        let committed = repo_ops::find_personal_by_owner_and_name(&db, owner_id, "stale-repo")
            .await
            .expect("read committed repository")
            .expect("repository exists before rename");
        let repo_id = committed.id;
        let mut renamed: RepoActiveModel = committed.into();
        renamed.name = Set("fresh-repo".to_string());
        renamed.description = Set(Some("fresh beta description".to_string()));
        renamed.updated_at = Set(Utc::now());
        renamed
            .update(&db)
            .await
            .expect("rename repository source row");

        assert_eq!(
            repo_fts_snapshot(&db, repo_id).await,
            Some((
                "fresh-repo".to_string(),
                "fresh beta description".to_string()
            ))
        );
        release_tx.send(()).expect("release late repository create");
        first
            .await
            .expect("repository create task panicked")
            .expect("repository create succeeds");

        assert_eq!(
            repo_fts_snapshot(&db, repo_id).await,
            Some((
                "fresh-repo".to_string(),
                "fresh beta description".to_string()
            )),
            "a late create replayed its stale source snapshot into repository FTS"
        );
        assert!(
            repo_ops::find_personal_by_owner_and_name(&db, owner_id, "fresh-repo")
                .await
                .expect("read renamed repository")
                .is_some()
        );
    }

    #[tokio::test]
    async fn soft_delete_and_restore_are_owned_by_the_repository_trigger() {
        let (dir, db, owner_id) = fixture("repoftsdelete").await;
        let repo = create_repo_with_opts(
            &db,
            create_options(owner_id, "live-repo", "live description"),
            &dir.path().join("repos"),
        )
        .await
        .expect("create repository");
        assert!(repo_fts_snapshot(&db, repo.id).await.is_some());

        repo_ops::soft_delete(&db, repo.id)
            .await
            .expect("soft-delete repository source row");
        assert_eq!(
            repo_fts_snapshot(&db, repo.id).await,
            None,
            "soft-delete left the trigger-owned FTS row behind"
        );

        let deleted = rg_db::entities::repository::Entity::find_by_id(repo.id)
            .one(&db)
            .await
            .expect("read raw soft-deleted row")
            .expect("soft-deleted row still exists");
        let mut restored: RepoActiveModel = deleted.into();
        restored.deleted_at = Set(None);
        restored.name = Set("restored-repo".to_string());
        restored.description = Set(Some("restored description".to_string()));
        restored.updated_at = Set(Utc::now());
        restored
            .update(&db)
            .await
            .expect("restore repository source row");

        assert_eq!(
            repo_fts_snapshot(&db, repo.id).await,
            Some((
                "restored-repo".to_string(),
                "restored description".to_string()
            )),
            "restoring the source row did not recreate its FTS snapshot"
        );
    }
}

#[cfg(test)]
mod repository_deletion_tests {
    use super::*;
    use crate::blob_storage::{
        BlobMetadata, BlobStorageError, LocalBlobStorage, Result as BlobResult,
    };
    use crate::package_registry::oci::storage::OciStorage;
    use futures::future::BoxFuture;
    use sea_orm::{ConnectOptions, ConnectionTrait, Database};
    use std::sync::{Arc, Mutex};

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

    struct RestoreFailingStorage {
        inner: LocalBlobStorage,
    }

    impl BlobStorage for RestoreFailingStorage {
        fn backend_name(&self) -> &'static str {
            "restore-failing-test"
        }

        fn put<'a>(
            &'a self,
            key: &'a BlobKey,
            data: &'a [u8],
        ) -> BoxFuture<'a, BlobResult<BlobMetadata>> {
            self.inner.put(key, data)
        }

        fn put_file<'a>(
            &'a self,
            key: &'a BlobKey,
            source: &'a std::path::Path,
        ) -> BoxFuture<'a, BlobResult<BlobMetadata>> {
            self.inner.put_file(key, source)
        }

        fn get<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<Vec<u8>>> {
            self.inner.get(key)
        }

        fn metadata<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<BlobMetadata>> {
            self.inner.metadata(key)
        }

        fn exists<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<bool>> {
            self.inner.exists(key)
        }

        fn delete<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<bool>> {
            self.inner.delete(key)
        }

        fn list<'a>(
            &'a self,
            prefix: Option<&'a BlobKey>,
        ) -> BoxFuture<'a, BlobResult<Vec<BlobMetadata>>> {
            self.inner.list(prefix)
        }

        fn move_prefix<'a>(
            &'a self,
            source: &'a BlobKey,
            _destination: &'a BlobKey,
        ) -> BoxFuture<'a, BlobResult<bool>> {
            Box::pin(async move {
                Err(BlobStorageError::io(
                    "blob prefix restore",
                    std::path::PathBuf::from(source.as_str()),
                    std::io::Error::other("injected restore failure"),
                ))
            })
        }

        fn local_path(&self, key: &BlobKey) -> Option<std::path::PathBuf> {
            self.inner.local_path(key)
        }
    }

    /// The registry the deletion path is handed, in its default shape: the
    /// instance's own blob backend plus `_oci_uploads/` under `repo_root`,
    /// exactly as `rg_http::run` builds it when no dedicated OCI path is set.
    fn oci_storage_for(repo_root: &std::path::Path) -> OciStorage {
        OciStorage::from_backend(
            Arc::new(LocalBlobStorage::new(repo_root)),
            repo_root.join("_oci_uploads"),
        )
    }

    async fn setup_db() -> DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options)
            .await
            .expect("connect in-memory database");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db
    }

    /// One CI job of `repo_id`, with an artifact of its own on the blob
    /// backend. Returns the artifact's key.
    ///
    /// The walk pipeline → stage → job is the shortest one that makes a job id
    /// belong to a repository, and that ownership is the whole subject here:
    /// the artifact key carries the job, so nothing but this chain can say
    /// whose bytes these are.
    async fn seed_job_artifact(
        db: &DatabaseConnection,
        storage: &LocalBlobStorage,
        repo_id: i64,
        payload: &[u8],
    ) -> (BlobKey, i64) {
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            db,
            repo_id,
            "1234567890123456789012345678901234567890",
            "refs/heads/main",
            "manual",
            None,
        )
        .await
        .expect("create pipeline");
        let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
            .await
            .expect("create stage");
        let job = rg_db::ops::pipeline_ops::create_job(
            db, stage.id, "unit", "echo ok", None, None, None, None, None, false, None, None, None,
        )
        .await
        .expect("create job");
        let key = BlobKey::from_segments([
            "artifacts",
            "jobs",
            job.id.to_string().as_str(),
            "0e6c-report.txt",
        ])
        .expect("valid artifact key");
        storage
            .put(&key, payload)
            .await
            .expect("seed artifact blob");
        rg_db::ops::artifact_ops::create_artifact(
            db,
            job.id,
            "report.txt",
            key.as_str(),
            payload.len() as i64,
            None,
            None,
        )
        .await
        .expect("create artifact row");
        let finished_at = chrono::Utc::now().naive_utc();
        rg_db::ops::pipeline_ops::update_job_result(
            db,
            job.id,
            "success",
            Some(0),
            None,
            None,
            Some(finished_at),
        )
        .await
        .expect("settle artifact job");
        rg_db::ops::pipeline_ops::update_stage_status(
            db,
            stage.id,
            "success",
            None,
            Some(finished_at),
        )
        .await
        .expect("settle artifact stage");
        rg_db::ops::pipeline_ops::update_pipeline_status(
            db,
            pipeline.id,
            "success",
            None,
            Some(finished_at),
        )
        .await
        .expect("settle artifact pipeline");
        (key, job.id)
    }

    /// One pushed layer and the manifest that references it, stored the way a
    /// `docker push` leaves them. Returns the layer's backend key.
    ///
    /// Both are content-addressed under `oci/<owner>/<repo>/`, which is what
    /// makes them a deletion problem: the digest is the same for everyone, so
    /// only the namespace in the key says whose bytes these are — and that
    /// namespace is handed straight back when the repository is deleted.
    async fn seed_oci_repository(
        oci_storage: &OciStorage,
        owner: &str,
        name: &str,
        payload: &[u8],
    ) -> BlobKey {
        use sha2::Digest;

        let digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(payload)));
        let stored = oci_storage
            .store_blob(owner, name, &digest, payload)
            .await
            .expect("store an OCI layer");
        let manifest = format!(r#"{{"layers":[{{"digest":"{digest}"}}]}}"#);
        let manifest_digest = format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(manifest.as_bytes()))
        );
        oci_storage
            .store_manifest(owner, name, &manifest_digest, manifest.as_bytes())
            .await
            .expect("store an OCI manifest");
        BlobKey::new(stored).expect("the registry returns a valid backend key")
    }

    /// A repository's artifacts live under `artifacts/jobs/<job_id>`, which
    /// names no repository at all. Until the deletion path walked the job
    /// graph, `DELETE` reported a completed removal and left those bytes
    /// behind with no owner and no collector.
    ///
    /// The second repository is what makes the first assertion mean anything:
    /// a deletion that took every artifact directory it could find would pass
    /// a test that only looks at the deleted repository.
    #[tokio::test]
    async fn deleting_a_repository_retires_the_artifacts_of_its_ci_jobs() {
        let db = setup_db().await;
        let owner = user_ops::create_user(
            &db,
            "artifact-delete-owner",
            "artifact-delete@example.invalid",
            "unused",
            "Artifact Delete",
        )
        .await
        .expect("create owner");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        let blob_storage = LocalBlobStorage::new(&repo_root);
        let doomed = create_repo(
            &db,
            owner.id,
            "with-artifacts",
            None,
            false,
            &repo_root,
            None,
        )
        .await
        .expect("create repository");
        let neighbour = create_repo(&db, owner.id, "kept", None, false, &repo_root, None)
            .await
            .expect("create neighbouring repository");
        let (doomed_artifact, _) =
            seed_job_artifact(&db, &blob_storage, doomed.id, b"job output").await;
        let (kept_artifact, _) =
            seed_job_artifact(&db, &blob_storage, neighbour.id, b"other output").await;

        assert_eq!(
            blob_storage
                .get(&doomed_artifact)
                .await
                .expect("the artifact must exist before the deletion is asked to remove it"),
            b"job output"
        );

        delete_repo(
            &db,
            &repo_root,
            &blob_storage,
            &oci_storage_for(&repo_root),
            &doomed,
        )
        .await
        .expect("delete repository");

        assert!(
            matches!(
                blob_storage.get(&doomed_artifact).await,
                Err(BlobStorageError::NotFound(_))
            ),
            "the repository is gone but its CI artifact bytes are still readable at {doomed_artifact}"
        );
        let tombstones =
            BlobKey::from_segments(["_deleted", "repositories", doomed.id.to_string().as_str()])
                .expect("valid tombstone prefix");
        assert!(
            blob_storage
                .list(Some(&tombstones))
                .await
                .expect("inventory tombstones")
                .is_empty(),
            "the artifacts were staged aside but never retired"
        );
        assert_eq!(
            blob_storage
                .get(&kept_artifact)
                .await
                .expect("another repository's artifact must survive this deletion"),
            b"other output"
        );
    }

    /// One published package file of `repo_id` whose row holds an absolute
    /// path instead of a [`BlobKey`] — the shape rows written before the
    /// blob-storage migration still have.
    ///
    /// The file is seeded outside `repo_root` on purpose: that is exactly what
    /// makes it unreachable from the `packages/<owner>/<repo>` prefix, so a
    /// deletion that only moves prefixes leaves it behind.
    async fn seed_legacy_package_file(
        db: &DatabaseConnection,
        repo_id: i64,
        owner_id: i64,
        name: &str,
        storage_path: &std::path::Path,
    ) {
        let registry =
            match rg_db::ops::package_registry_ops::find_by_repo_and_type(db, repo_id, "generic")
                .await
                .expect("look up package registry")
            {
                Some(registry) => registry,
                None => rg_db::ops::package_registry_ops::create(db, repo_id, "generic")
                    .await
                    .expect("create package registry"),
            };
        let package =
            rg_db::ops::package_ops::create(db, registry.id, owner_id, name, None, None, None)
                .await
                .expect("create package");
        let version = rg_db::ops::package_version_ops::create(
            db, package.id, "1.0.0", None, None, 0, None, None,
        )
        .await
        .expect("create package version");
        rg_db::ops::package_file_ops::create(
            db,
            version.id,
            "payload.bin",
            0,
            rg_db::ops::package_file_ops::FileDigests::default(),
            &storage_path.to_string_lossy(),
        )
        .await
        .expect("create package file row");
    }

    /// card_3a9cfaf53d62: a `package_files` row written before the blob-storage
    /// migration holds an absolute path, so it sits outside every prefix the
    /// deletion moves — while `PackageStorage` still reads, serves and deletes
    /// through it. The repository deletion has to reach it through the database
    /// and stage it like every other repository-owned path, and a row whose
    /// file is already gone is the end state, not a refusal.
    #[tokio::test]
    async fn deleting_a_repository_retires_legacy_package_files() {
        let db = setup_db().await;
        let owner = user_ops::create_user(
            &db,
            "legacy-package-owner",
            "legacy-package@example.invalid",
            "unused",
            "Legacy Package",
        )
        .await
        .expect("create owner");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        let repo = create_repo(
            &db,
            owner.id,
            "with-legacy-packages",
            None,
            false,
            &repo_root,
            None,
        )
        .await
        .expect("create repository");
        let legacy_root = sandbox.path().join("legacy-packages");
        std::fs::create_dir_all(&legacy_root).expect("create legacy package root");
        let legacy_file = legacy_root.join("payload.bin");
        std::fs::write(&legacy_file, b"pre-migration package bytes")
            .expect("seed legacy package file");
        seed_legacy_package_file(&db, repo.id, owner.id, "demo", &legacy_file).await;
        // A row whose bytes an operator already removed by hand: the requested
        // end state is true for it, so it must not fail the deletion.
        seed_legacy_package_file(
            &db,
            repo.id,
            owner.id,
            "vanished",
            &legacy_root.join("already-gone.bin"),
        )
        .await;

        let blob_storage = LocalBlobStorage::new(&repo_root);
        delete_repo(
            &db,
            &repo_root,
            &blob_storage,
            &oci_storage_for(&repo_root),
            &repo,
        )
        .await
        .expect("delete repository owning legacy package files");

        assert!(
            !legacy_file.exists(),
            "DELETE left a pre-migration package file live at {}",
            legacy_file.display()
        );
        let leftovers: Vec<_> = std::fs::read_dir(&legacy_root)
            .expect("read the legacy package directory")
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "DELETE retired the live path but left the legacy package tombstone: {leftovers:?}"
        );
    }

    /// card_0ea3381a4f91, extended by card_ed203feab041 with the historical
    /// release-asset root: these directories sit under `repo_root`, but outside
    /// every BlobStorage namespace. A successful repository deletion owns all
    /// four and must leave neither their live names nor adjacent tombstones.
    #[tokio::test]
    async fn deleting_a_repository_retires_legacy_lfs_release_cache_and_artifact_directories() {
        let db = setup_db().await;
        let owner = user_ops::create_user(
            &db,
            "filesystem-delete-owner",
            "filesystem-delete@example.invalid",
            "unused",
            "Filesystem Delete",
        )
        .await
        .expect("create owner");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        let repo = create_repo(
            &db,
            owner.id,
            "with-local-storage",
            None,
            false,
            &repo_root,
            None,
        )
        .await
        .expect("create repository");
        let blob_storage = LocalBlobStorage::new(&repo_root);
        let (_, job_id) =
            seed_job_artifact(&db, &blob_storage, repo.id, b"portable artifact").await;
        let directories = [
            crate::lfs::service::lfs_root(
                &repo_root,
                "filesystem-delete-owner",
                "with-local-storage",
            ),
            crate::release::service::legacy_asset_root(
                &repo_root,
                "filesystem-delete-owner",
                "with-local-storage",
            ),
            repo_root.join("_ci_cache").join(repo.id.to_string()),
            repo_root
                .join("_artifacts")
                .join("jobs")
                .join(job_id.to_string()),
        ];
        for (index, directory) in directories.iter().enumerate() {
            std::fs::create_dir_all(directory).expect("create repository-owned directory");
            std::fs::write(
                directory.join(format!("marker-{index}")),
                format!("local payload {index}"),
            )
            .expect("seed repository-owned directory");
        }

        delete_repo(
            &db,
            &repo_root,
            &blob_storage,
            &oci_storage_for(&repo_root),
            &repo,
        )
        .await
        .expect("delete repository with local filesystem storage");

        for directory in directories {
            assert!(
                !directory.exists(),
                "DELETE left repository-owned storage at {}",
                directory.display()
            );
            let file_name = directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let tombstones: Vec<_> = std::fs::read_dir(directory.parent().unwrap())
                .expect("read local storage parent")
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| {
                    name.to_string_lossy()
                        .starts_with(&format!("{file_name}.deleted-{}-", repo.id))
                })
                .collect();
            assert!(
                tombstones.is_empty(),
                "DELETE retired the live path but left tombstones beside it: {tombstones:?}"
            );
        }
    }

    /// CI may recreate cache/artifact directories while a job is alive. The
    /// deletion path refuses before its first rename instead of reporting a
    /// completed delete over bytes a runner can still publish concurrently.
    #[tokio::test]
    async fn an_active_pipeline_blocks_repository_storage_staging() {
        let db = setup_db().await;
        let owner = user_ops::create_user(
            &db,
            "active-delete-owner",
            "active-delete@example.invalid",
            "unused",
            "Active Delete",
        )
        .await
        .expect("create owner");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        let repo = create_repo(&db, owner.id, "busy", None, false, &repo_root, None)
            .await
            .expect("create repository");
        rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            repo.id,
            "1234567890123456789012345678901234567890",
            "refs/heads/main",
            "push",
            Some(owner.id),
        )
        .await
        .expect("create active pipeline");
        let bare = repo_root.join("active-delete-owner/busy.git");
        let blob_storage = LocalBlobStorage::new(&repo_root);

        let error = delete_repo(
            &db,
            &repo_root,
            &blob_storage,
            &oci_storage_for(&repo_root),
            &repo,
        )
        .await
        .expect_err("active CI must make repository deletion retryable later");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("1 active CI pipeline"), "{rendered}");
        assert!(rendered.contains("cancel them or wait"), "{rendered}");
        assert!(bare.is_dir(), "the refused delete moved the Git directory");
        assert!(
            find_repo_by_owner_name(&db, "active-delete-owner", "busy")
                .await
                .expect("read repository after refused deletion")
                .is_some(),
            "the refused delete hid the repository row"
        );
    }

    /// A failure in a later plain-filesystem prepare step must restore the
    /// directories already renamed and the Git/blob namespaces staged before
    /// them. ENOTDIR is deterministic even when tests run as root.
    #[tokio::test]
    async fn a_filesystem_staging_failure_restores_git_blobs_and_earlier_directories() {
        let db = setup_db().await;
        let owner = user_ops::create_user(
            &db,
            "filesystem-rollback-owner",
            "filesystem-rollback@example.invalid",
            "unused",
            "Filesystem Rollback",
        )
        .await
        .expect("create owner");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        let repo = create_repo(&db, owner.id, "keep-me", None, false, &repo_root, None)
            .await
            .expect("create repository");
        let bare_marker = repo_root.join("filesystem-rollback-owner/keep-me.git/marker");
        std::fs::write(&bare_marker, b"git survives").expect("seed Git marker");
        let lfs = crate::lfs::service::lfs_root(&repo_root, "filesystem-rollback-owner", "keep-me");
        std::fs::create_dir_all(&lfs).expect("create legacy LFS directory");
        let lfs_marker = lfs.join("marker");
        std::fs::write(&lfs_marker, b"LFS survives").expect("seed LFS marker");
        std::fs::create_dir_all(&repo_root).expect("create repository root");
        std::fs::write(repo_root.join("_ci_cache"), b"not a directory")
            .expect("install deterministic cache-path blocker");
        let blob_storage = LocalBlobStorage::new(&repo_root);
        let blob = BlobKey::new(
            "packages/filesystem-rollback-owner/keep-me/generic/demo/1/objects/one/payload.bin",
        )
        .expect("valid package key");
        blob_storage
            .put(&blob, b"blob survives")
            .await
            .expect("seed package blob");

        let error = delete_repo(
            &db,
            &repo_root,
            &blob_storage,
            &oci_storage_for(&repo_root),
            &repo,
        )
        .await
        .expect_err("the cache path blocker must reject filesystem staging");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("_ci_cache"), "{rendered}");
        assert_eq!(std::fs::read(&bare_marker).unwrap(), b"git survives");
        assert_eq!(std::fs::read(&lfs_marker).unwrap(), b"LFS survives");
        assert_eq!(
            blob_storage.get(&blob).await.expect("blob prefix restored"),
            b"blob survives"
        );
        assert!(
            find_repo_by_owner_name(&db, "filesystem-rollback-owner", "keep-me")
                .await
                .expect("read repository after failed staging")
                .is_some(),
            "the failed prepare hid the repository row"
        );
    }

    /// Registry keys are `oci/<owner>/<repo>/...` and a repository's name is
    /// released the moment its row is soft-deleted, so a re-created
    /// `<owner>/<repo>` lands on exactly its predecessor's physical keys.
    /// `DELETE` reported a completed removal and left them there — not merely
    /// bytes nobody collects, but a trap: `HEAD /v2/{owner}/{repo}/blobs/
    /// {digest}` answers off storage and then demands the `oci_blob` row that
    /// went with the old repository, so the first `docker push` into the
    /// re-created namespace meets a `500` for a layer it never uploaded.
    ///
    /// Uploads in flight leak without any collision at all: the sessions that
    /// name them belong to the deleted repository, so nothing ever comes back
    /// for the directory.
    ///
    /// The second repository is what makes the assertions mean anything — a
    /// deletion that swept the whole registry would pass a test that only
    /// looks at the repository it deleted.
    #[tokio::test]
    async fn deleting_a_repository_retires_its_registry_data_and_uploads_in_flight() {
        let db = setup_db().await;
        let owner = user_ops::create_user(
            &db,
            "oci-delete-owner",
            "oci-delete@example.invalid",
            "unused",
            "Oci Delete",
        )
        .await
        .expect("create owner");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        let blob_storage = LocalBlobStorage::new(&repo_root);
        let oci_storage = oci_storage_for(&repo_root);
        let doomed = create_repo(&db, owner.id, "published", None, false, &repo_root, None)
            .await
            .expect("create repository");
        let neighbour = create_repo(&db, owner.id, "kept", None, false, &repo_root, None)
            .await
            .expect("create neighbouring repository");
        assert_eq!(neighbour.name, "kept");

        let doomed_layer = seed_oci_repository(
            &oci_storage,
            "oci-delete-owner",
            "published",
            b"doomed layer",
        )
        .await;
        let kept_layer =
            seed_oci_repository(&oci_storage, "oci-delete-owner", "kept", b"kept layer").await;
        let (_, doomed_upload) = oci_storage
            .create_upload("oci-delete-owner", "published")
            .await
            .expect("start a chunked upload");
        let (_, kept_upload) = oci_storage
            .create_upload("oci-delete-owner", "kept")
            .await
            .expect("start a neighbouring chunked upload");

        delete_repo(&db, &repo_root, &blob_storage, &oci_storage, &doomed)
            .await
            .expect("delete repository");

        let doomed_namespace =
            BlobKey::from_segments(["oci", "oci-delete-owner", "published"]).expect("valid prefix");
        assert!(
            blob_storage
                .list(Some(&doomed_namespace))
                .await
                .expect("inventory the deleted registry namespace")
                .is_empty(),
            "the repository is gone but its layers and manifests still occupy {doomed_namespace}, \
             which the next repository of that name will be handed"
        );
        assert!(
            matches!(
                blob_storage.get(&doomed_layer).await,
                Err(BlobStorageError::NotFound(_))
            ),
            "the deleted repository's layer is still readable at {doomed_layer}"
        );
        assert!(
            !std::path::Path::new(&doomed_upload).exists(),
            "the deleted repository left an upload in flight behind at {doomed_upload}"
        );
        let tombstones =
            BlobKey::from_segments(["_deleted", "repositories", doomed.id.to_string().as_str()])
                .expect("valid tombstone prefix");
        assert!(
            blob_storage
                .list(Some(&tombstones))
                .await
                .expect("inventory tombstones")
                .is_empty(),
            "the registry data was staged aside but never retired"
        );

        assert_eq!(
            blob_storage
                .get(&kept_layer)
                .await
                .expect("another repository's layer must survive this deletion"),
            b"kept layer"
        );
        assert!(
            std::path::Path::new(&kept_upload).is_file(),
            "another repository's upload in flight was swept away with this deletion: \
             {kept_upload}"
        );
    }

    /// The filesystem move necessarily happens before the database mutation.
    /// If that mutation fails, returning the error without compensation would
    /// leave a live row whose Git data has vanished under a staging name.
    #[tokio::test]
    async fn a_failed_soft_delete_restores_the_repository_directory() {
        let db = setup_db().await;
        let owner = user_ops::create_user(
            &db,
            "delete-rollback-owner",
            "delete-rollback@example.invalid",
            "unused",
            "Delete Rollback",
        )
        .await
        .expect("create owner");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        let repo = create_repo(&db, owner.id, "keep-me", None, false, &repo_root, None)
            .await
            .expect("create repository");
        let bare = repo_root.join("delete-rollback-owner/keep-me.git");
        let marker = bare.join("rollback-marker");
        std::fs::write(&marker, b"must survive").expect("write marker");
        let blob_storage = crate::blob_storage::LocalBlobStorage::new(&repo_root);
        let blob = crate::blob_storage::BlobKey::new(
            "packages/delete-rollback-owner/keep-me/generic/demo/1/objects/one/payload.bin",
        )
        .expect("valid package key");
        blob_storage
            .put(&blob, b"must survive too")
            .await
            .expect("seed package blob");
        // Staged last, restored first: the artifact prefixes are the tail of
        // the staged set, so a rollback that walked it in the wrong order — or
        // skipped the part the database had to name — shows up here.
        let (artifact, job_id) =
            seed_job_artifact(&db, &blob_storage, repo.id, b"and so must this").await;
        let legacy_lfs =
            crate::lfs::service::lfs_root(&repo_root, "delete-rollback-owner", "keep-me");
        let cache = repo_root.join("_ci_cache").join(repo.id.to_string());
        let legacy_artifacts = repo_root
            .join("_artifacts")
            .join("jobs")
            .join(job_id.to_string());
        for (directory, payload) in [
            (&legacy_lfs, b"legacy LFS survives".as_slice()),
            (&cache, b"CI cache survives".as_slice()),
            (&legacy_artifacts, b"legacy artifact survives".as_slice()),
        ] {
            std::fs::create_dir_all(directory).expect("create local rollback directory");
            std::fs::write(directory.join("rollback-marker"), payload)
                .expect("seed local rollback directory");
        }
        // Pre-migration package files are the tail of the filesystem set, so a
        // restore that stops at the directories it could name from `repo_root`
        // leaves this one staged aside while its row is still published.
        let legacy_package_root = sandbox.path().join("legacy-packages");
        std::fs::create_dir_all(&legacy_package_root).expect("create legacy package root");
        let legacy_package = legacy_package_root.join("payload.bin");
        std::fs::write(&legacy_package, b"legacy package survives")
            .expect("seed legacy package file");
        seed_legacy_package_file(&db, repo.id, owner.id, "demo", &legacy_package).await;
        // The registry is staged after every blob prefix, so it is restored
        // first — and it is the only part whose staging touches both a backend
        // key and a plain directory.
        let oci_storage = oci_storage_for(&repo_root);
        let layer = seed_oci_repository(
            &oci_storage,
            "delete-rollback-owner",
            "keep-me",
            b"and this layer",
        )
        .await;
        let upload = oci_storage
            .create_upload("delete-rollback-owner", "keep-me")
            .await
            .expect("start a chunked upload");

        db.execute_unprepared(&format!(
            "CREATE TRIGGER reject_repo_soft_delete \
             BEFORE UPDATE OF deleted_at ON repositories \
             WHEN NEW.id = {} \
             BEGIN SELECT RAISE(ABORT, 'forced soft-delete failure'); END",
            repo.id
        ))
        .await
        .expect("install failure trigger");

        let error = delete_repo(&db, &repo_root, &blob_storage, &oci_storage, &repo)
            .await
            .expect_err("soft-delete trigger must reject the update");
        assert!(
            format!("{error:#}").contains("forced soft-delete failure"),
            "unexpected failure: {error:#}"
        );
        assert_eq!(
            std::fs::read(&marker).expect("repository directory must be restored"),
            b"must survive"
        );
        assert_eq!(
            blob_storage
                .get(&blob)
                .await
                .expect("repository blob prefix must be restored"),
            b"must survive too"
        );
        assert_eq!(
            blob_storage
                .get(&artifact)
                .await
                .expect("the CI artifact prefix must be restored too"),
            b"and so must this"
        );
        for (directory, payload) in [
            (&legacy_lfs, b"legacy LFS survives".as_slice()),
            (&cache, b"CI cache survives".as_slice()),
            (&legacy_artifacts, b"legacy artifact survives".as_slice()),
        ] {
            assert_eq!(
                std::fs::read(directory.join("rollback-marker"))
                    .expect("repository filesystem directory must be restored"),
                payload
            );
        }
        assert_eq!(
            std::fs::read(&legacy_package)
                .expect("the legacy package file must be restored to its published path"),
            b"legacy package survives"
        );
        assert_eq!(
            rg_db::ops::package_file_ops::list_storage_paths_by_repo(&db, repo.id)
                .await
                .expect("read package file rows after rollback"),
            vec![legacy_package.to_string_lossy().into_owned()],
            "the failed deletion changed the rows that name the restored bytes"
        );
        assert_eq!(
            blob_storage
                .get(&layer)
                .await
                .expect("the OCI blob namespace must be restored too"),
            b"and this layer"
        );
        assert!(
            std::path::Path::new(&upload.1).is_file(),
            "the rollback left an upload in flight without the file it is being written to: {}",
            upload.1
        );
        assert!(
            find_repo_by_owner_name(&db, "delete-rollback-owner", "keep-me")
                .await
                .expect("read repository after rollback")
                .is_some(),
            "the failed soft-delete still hid the database row"
        );
        let leftovers: Vec<_> = std::fs::read_dir(bare.parent().unwrap())
            .expect("read namespace directory")
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with("keep-me.git.deleted-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "the rollback restored the live path but left staging directories: {leftovers:?}"
        );
        let tombstones = crate::blob_storage::BlobKey::from_segments([
            "_deleted",
            "repositories",
            repo.id.to_string().as_str(),
        ])
        .expect("valid tombstone prefix");
        assert!(
            blob_storage
                .list(Some(&tombstones))
                .await
                .expect("inventory tombstones")
                .is_empty(),
            "the rollback restored the live blob prefix but left staged objects"
        );
    }

    /// A transfer changes the namespace in every read key, not just the Git
    /// directory. Package rows and OCI rows retain portable paths too, so the
    /// test reads them through their production services after the move rather
    /// than only checking that a directory happened to be renamed.
    #[tokio::test]
    async fn transferring_a_repository_keeps_lfs_packages_and_oci_readable_and_frees_the_old_name()
    {
        use sha2::Digest;

        let db = setup_db().await;
        let source_owner = user_ops::create_user(
            &db,
            "transfer-source",
            "transfer-source@example.invalid",
            "unused",
            "Transfer Source",
        )
        .await
        .expect("create source owner");
        let destination_owner = user_ops::create_user(
            &db,
            "transfer-destination",
            "transfer-destination@example.invalid",
            "unused",
            "Transfer Destination",
        )
        .await
        .expect("create destination owner");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        let storage: Arc<dyn BlobStorage> = Arc::new(LocalBlobStorage::new(&repo_root));
        let oci_storage = OciStorage::from_backend(storage.clone(), repo_root.join("_oci_uploads"));
        let repository = create_repo(
            &db,
            source_owner.id,
            "portable",
            None,
            false,
            &repo_root,
            None,
        )
        .await
        .expect("create repository");
        let git_marker = repo_root.join("transfer-source/portable.git/transfer-marker");
        std::fs::write(&git_marker, b"git bytes").expect("seed bare repository");

        let lfs_payload = b"LFS bytes follow the repository";
        let oid = hex::encode(sha2::Sha256::digest(lfs_payload));
        let old_lfs =
            crate::lfs::service::lfs_object_key("transfer-source", "portable", &oid, false)
                .expect("valid LFS key");
        storage
            .put(&old_lfs, lfs_payload)
            .await
            .expect("seed LFS object");

        let package_storage =
            crate::package_registry::PackageStorage::from_backend(storage.clone());
        crate::package_registry::service::publish(
            &db,
            &package_storage,
            crate::package_registry::PublishInfo {
                owner: "transfer-source".to_string(),
                repo: "portable".to_string(),
                package_type: "generic".to_string(),
                name: "widget".to_string(),
                version: "1.0.0".to_string(),
                semver: Some("1.0.0".to_string()),
                metadata: None,
                description: None,
                homepage: None,
                repository_url: None,
                author_id: source_owner.id,
                files: vec![("widget.bin".to_string(), b"package bytes".to_vec())],
            },
        )
        .await
        .expect("publish package before transfer");

        let oci_payload = b"OCI layer follows the repository";
        let digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(oci_payload)));
        let old_oci_path = oci_storage
            .store_blob("transfer-source", "portable", &digest, oci_payload)
            .await
            .expect("store OCI layer");
        let oci_repo = rg_db::ops::oci_ops::find_or_create_repo(
            &db,
            repository.id,
            "transfer-source/portable",
            source_owner.id,
        )
        .await
        .expect("create OCI metadata");
        rg_db::ops::oci_ops::insert_blob(
            &db,
            oci_repo.id,
            &digest,
            "application/vnd.oci.image.layer.v1.tar",
            oci_payload.len() as i64,
            &old_oci_path,
        )
        .await
        .expect("record OCI layer");
        let (upload_id, old_upload) = oci_storage
            .create_upload("transfer-source", "portable")
            .await
            .expect("start an OCI upload");

        transfer_repo(
            &db,
            source_owner.id,
            "transfer-source",
            "portable",
            "transfer-destination",
            &repo_root,
            storage.as_ref(),
            &oci_storage,
        )
        .await
        .expect("transfer repository");

        assert_eq!(
            std::fs::read(repo_root.join("transfer-destination/portable.git/transfer-marker"))
                .expect("Git directory moved"),
            b"git bytes"
        );
        assert!(
            !git_marker.exists(),
            "old Git directory still owns the marker"
        );

        let lfs = crate::lfs::service::read_object_source(
            storage.as_ref(),
            &crate::lfs::service::lfs_root(&repo_root, "transfer-destination", "portable"),
            "transfer-destination",
            "portable",
            &oid,
        )
        .await
        .expect("read LFS object at destination");
        let lfs_bytes = match lfs {
            crate::lfs::service::LfsObjectSource::Local { path, .. } => {
                std::fs::read(path).expect("read local LFS object")
            }
            crate::lfs::service::LfsObjectSource::Bytes { data, .. } => data,
        };
        assert_eq!(lfs_bytes, lfs_payload);

        let (package, _, _) = crate::package_registry::service::download_file(
            &db,
            &package_storage,
            "transfer-destination",
            "portable",
            "generic",
            "widget",
            "1.0.0",
            "widget.bin",
        )
        .await
        .expect("read package at destination");
        assert_eq!(package, b"package bytes");

        assert_eq!(
            oci_storage
                .read_blob("transfer-destination", "portable", &digest)
                .await
                .expect("read OCI layer at destination"),
            oci_payload
        );
        let moved_oci_repo = rg_db::ops::oci_ops::find_repo_by_id(&db, repository.id)
            .await
            .expect("read OCI repository")
            .expect("OCI repository survives transfer");
        assert_eq!(moved_oci_repo.namespace, "transfer-destination/portable");
        assert_eq!(moved_oci_repo.owner_id, destination_owner.id);
        let moved_blob = rg_db::ops::oci_ops::find_blob(&db, moved_oci_repo.id, &digest)
            .await
            .expect("read OCI blob")
            .expect("OCI blob row survives transfer");
        assert!(
            moved_blob
                .storage_path
                .starts_with("oci/transfer-destination/portable/"),
            "OCI metadata still names the source path: {}",
            moved_blob.storage_path
        );
        let new_upload = repo_root
            .join("_oci_uploads/oci-uploads/transfer-destination/portable")
            .join(&upload_id)
            .join("data");
        assert!(
            new_upload.is_file(),
            "OCI upload did not move to destination"
        );
        assert!(
            !std::path::Path::new(&old_upload).exists(),
            "old OCI upload still occupies source namespace"
        );

        create_repo(
            &db,
            source_owner.id,
            "portable",
            None,
            false,
            &repo_root,
            None,
        )
        .await
        .expect("the old owner may create a repository with the released name");
        assert!(
            !storage.exists(&old_lfs).await.expect("probe old LFS key"),
            "the replacement repository inherited the transferred LFS object"
        );
        let old_package_prefix =
            BlobKey::from_segments(["packages", "transfer-source", "portable"])
                .expect("valid source package prefix");
        assert!(
            storage
                .list(Some(&old_package_prefix))
                .await
                .expect("inventory old package namespace")
                .is_empty(),
            "the replacement repository inherited the transferred package bytes"
        );
        assert!(
            !oci_storage
                .blob_exists("transfer-source", "portable", &digest)
                .await
                .expect("probe old OCI key"),
            "the replacement repository inherited the transferred OCI layer"
        );
    }

    /// `move_prefix` is the storage prepare boundary. A backend that refuses it
    /// must leave both the database owner and the Git directory at the source;
    /// returning success here would detach every later LFS/package/OCI read.
    #[tokio::test]
    async fn a_storage_transfer_failure_keeps_the_repository_at_its_source_owner() {
        let db = setup_db().await;
        let source_owner = user_ops::create_user(
            &db,
            "transfer-failure-source",
            "transfer-failure-source@example.invalid",
            "unused",
            "Transfer Failure Source",
        )
        .await
        .expect("create source owner");
        let _destination_owner = user_ops::create_user(
            &db,
            "transfer-failure-destination",
            "transfer-failure-destination@example.invalid",
            "unused",
            "Transfer Failure Destination",
        )
        .await
        .expect("create destination owner");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        let storage = Arc::new(RestoreFailingStorage {
            inner: LocalBlobStorage::new(&repo_root),
        });
        let oci_storage = OciStorage::from_backend(storage.clone(), repo_root.join("_oci_uploads"));
        create_repo(
            &db,
            source_owner.id,
            "must-stay",
            None,
            false,
            &repo_root,
            None,
        )
        .await
        .expect("create repository");
        let marker = repo_root.join("transfer-failure-source/must-stay.git/marker");
        std::fs::write(&marker, b"must remain at source").expect("seed Git marker");

        let error = transfer_repo(
            &db,
            source_owner.id,
            "transfer-failure-source",
            "must-stay",
            "transfer-failure-destination",
            &repo_root,
            storage.as_ref(),
            &oci_storage,
        )
        .await
        .expect_err("the injected prefix-move failure must reject the transfer");
        assert!(
            format!("{error:#}").contains("failed to move repository blob prefix"),
            "unexpected storage failure: {error:#}"
        );
        assert_eq!(
            std::fs::read(&marker).expect("Git directory restored to source"),
            b"must remain at source"
        );
        assert!(
            !repo_root
                .join("transfer-failure-destination/must-stay.git")
                .exists(),
            "the rejected transfer left Git under the destination owner"
        );
        assert!(
            find_repo_by_owner_name(&db, "transfer-failure-source", "must-stay")
                .await
                .expect("read source repository")
                .is_some(),
            "the rejected transfer rewrote the source owner in the database"
        );
        assert!(
            find_repo_by_owner_name(&db, "transfer-failure-destination", "must-stay")
                .await
                .expect("read destination repository")
                .is_none(),
            "the rejected transfer created a destination-owned row"
        );
    }

    /// A failed rollback cannot replace the original storage/DB error, so its
    /// only durable evidence is the warning. It must contain both namespaces
    /// and state the consequence, otherwise an operator cannot repair it.
    #[tokio::test]
    async fn failed_blob_compensation_logs_the_object_and_consequence() {
        let sandbox = tempfile::tempdir().expect("create blob root");
        let storage = RestoreFailingStorage {
            inner: LocalBlobStorage::new(sandbox.path()),
        };
        let live = BlobKey::new("packages/owner/repo").unwrap();
        let staged = BlobKey::new("_deleted/repositories/42/delete-id/packages").unwrap();
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        restore_blob_prefixes(
            &storage,
            &[StagedBlobPrefix {
                live: live.clone(),
                staged: staged.clone(),
            }],
            42,
        )
        .await;

        let rendered = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert!(rendered.contains(staged.as_str()), "{rendered}");
        assert!(rendered.contains(live.as_str()), "{rendered}");
        assert!(
            rendered.contains("active repository can no longer reach these blobs"),
            "{rendered}"
        );
    }

    /// Post-commit cleanup cannot be rolled back, so its 5xx and warning are
    /// the only operator-facing evidence. Both the staged and formerly-live
    /// paths must be present in that evidence.
    #[test]
    fn failed_filesystem_retirement_names_staged_and_live_paths() {
        let sandbox = tempfile::tempdir().expect("create filesystem root");
        let blocker = sandbox.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").expect("create ENOTDIR blocker");
        let staged = blocker.join("7.deleted-id");
        let live = sandbox.path().join("_ci_cache/7");
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let error = retire_repository_filesystem_directories(
            vec![StagedRepositoryFilesystemDirectory {
                live: live.clone(),
                staged: staged.clone(),
                kind: "CI cache directory",
                hint: crate::platform::fs::CI_CACHE_DIR_HINT,
            }],
            7,
        )
        .expect_err("ENOTDIR must make post-commit cleanup fail closed");

        let rendered_error = format!("{error:#}");
        assert!(
            rendered_error.contains(&staged.display().to_string()),
            "{rendered_error}"
        );
        assert!(
            rendered_error.contains(&live.display().to_string()),
            "{rendered_error}"
        );
        let rendered_log = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert!(
            rendered_log.contains(&staged.display().to_string()),
            "{rendered_log}"
        );
        assert!(
            rendered_log.contains(&live.display().to_string()),
            "{rendered_log}"
        );
        assert!(
            rendered_log.contains("staged data remains and must be removed by hand"),
            "{rendered_log}"
        );
    }
}

#[cfg(test)]
mod perm_cache_tests {
    use super::*;

    // These exercise the pure cache logic against a private `PermCache`, so they
    // neither perturb nor are perturbed by the process-global cache the rest of
    // the suite shares (all of which compiles into one test binary). No cross-test
    // serialization is needed — each test owns its instance.

    /// Stand-in for two databases open in one process.
    const DB_A: rg_db::InstanceId = 1;
    const DB_B: rg_db::InstanceId = 2;

    #[test]
    fn invalidate_user_drops_only_that_user_read_and_write() {
        let cache = PermCache::default();
        let (repo, user, other) = (910_001, 42, 43);
        cache.set(DB_A, repo, Some(user), false, true);
        cache.set(DB_A, repo, Some(user), true, true);
        cache.set(DB_A, repo, Some(other), false, true);

        cache.invalidate_user(DB_A, repo, user);

        assert_eq!(cache.check(DB_A, repo, Some(user), false), None);
        assert_eq!(cache.check(DB_A, repo, Some(user), true), None);
        // Other users on the same repo are untouched.
        assert_eq!(cache.check(DB_A, repo, Some(other), false), Some(true));
    }

    #[test]
    fn invalidate_repo_drops_all_entries_for_that_repo_only() {
        let cache = PermCache::default();
        let (repo, keep) = (910_002, 910_003);
        cache.set(DB_A, repo, None, false, true);
        cache.set(DB_A, repo, Some(7), true, true);
        cache.set(DB_A, keep, Some(7), true, true);

        cache.invalidate_repo(DB_A, repo);

        assert_eq!(cache.check(DB_A, repo, None, false), None);
        assert_eq!(cache.check(DB_A, repo, Some(7), true), None);
        assert_eq!(cache.check(DB_A, keep, Some(7), true), Some(true));
    }

    #[test]
    fn invalidate_all_clears_everything_of_that_database() {
        let cache = PermCache::default();
        cache.set(DB_A, 910_004, Some(1), false, true);
        cache.set(DB_A, 910_005, Some(2), true, true);
        cache.set(DB_B, 910_004, Some(1), false, true);

        cache.invalidate_all(DB_A);

        assert_eq!(cache.check(DB_A, 910_004, Some(1), false), None);
        assert_eq!(cache.check(DB_A, 910_005, Some(2), true), None);
        // Another database's entries are not this database's business.
        assert_eq!(cache.check(DB_B, 910_004, Some(1), false), Some(true));
    }

    /// The bug the instance component of the key exists to prevent: two
    /// databases whose row ids collide (which is every pair of them, since ids
    /// restart at 1) must not answer each other's questions.
    #[test]
    fn identical_ids_in_two_databases_are_separate_decisions() {
        let cache = PermCache::default();
        let (repo, actor) = (1, Some(2));

        // Same repo id, same actor id, opposite correct answers.
        cache.set(DB_A, repo, actor, false, false);
        cache.set(DB_B, repo, actor, false, true);

        assert_eq!(cache.check(DB_A, repo, actor, false), Some(false));
        assert_eq!(cache.check(DB_B, repo, actor, false), Some(true));

        // Nor may invalidating one reach into the other.
        cache.invalidate_user(DB_A, repo, 2);
        assert_eq!(cache.check(DB_A, repo, actor, false), None);
        assert_eq!(cache.check(DB_B, repo, actor, false), Some(true));
    }
}

/// DB-backed coverage of the access-control matrix: owner / anonymous /
/// collaborator (read·write·admin) / org role / write-team / admin-team, plus
/// the permission-cache invalidation contract (a revoked grant must actually
/// disappear, not linger behind the 30s TTL).
#[cfg(test)]
mod permission_matrix_tests {
    use super::*;
    use rg_db::entities::{repo_collaborator, repository};
    use rg_db::ops::{org_ops, repo_collaborator_ops};
    use sea_orm::{ConnectOptions, Database};

    // Each test opens its own in-memory database and every one of them reuses
    // the same small autoincrement ids (repo 1, user 1, …). What keeps these
    // tests out of each other's way is the instance component of the cache key:
    // a decision cached for one database is unreachable from another, so the
    // tests need neither a shared lock nor a cache flush on entry — including
    // the one below that deliberately reads a stale entry, which nothing else
    // in the process can now clear.

    async fn setup_db() -> DatabaseConnection {
        let mut opt = ConnectOptions::new("sqlite::memory:");
        opt.max_connections(1);
        let db = Database::connect(opt).await.expect("connect in-memory db");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db
    }

    async fn mk_user(db: &DatabaseConnection) -> i64 {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        user_ops::create_user(
            db,
            &format!("u_{tag}"),
            &format!("{tag}@example.test"),
            "",
            "",
        )
        .await
        .expect("create user")
        .id
    }

    async fn mk_repo(
        db: &DatabaseConnection,
        owner_id: i64,
        org_id: Option<i64>,
        is_private: bool,
    ) -> repository::Model {
        let now = Utc::now();
        let tag = uuid::Uuid::new_v4().simple().to_string();
        repo_ops::create(
            db,
            RepoActiveModel {
                owner_id: Set(owner_id),
                name: Set(format!("r_{tag}")),
                description: Set(None),
                is_private: Set(is_private),
                default_branch: Set("main".to_string()),
                org_id: Set(org_id),
                stars_count: Set(0),
                forks_count: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .expect("create repo")
    }

    async fn add_collab(db: &DatabaseConnection, repo_id: i64, user_id: i64, permission: &str) {
        repo_collaborator_ops::create(
            db,
            repo_collaborator::ActiveModel {
                id: sea_orm::NotSet,
                repo_id: Set(repo_id),
                user_id: Set(user_id),
                permission: Set(permission.to_string()),
                created_at: Set(Utc::now()),
            },
        )
        .await
        .expect("add collaborator");
    }

    #[tokio::test]
    async fn owner_has_full_access_others_denied_on_private() {
        let db = setup_db().await;
        let owner = mk_user(&db).await;
        let repo = mk_repo(&db, owner, None, true).await;

        // Owner: read + write + admin.
        assert!(can_read_repo(&db, &repo, Some(owner)).await.unwrap());
        assert!(can_write_repo(&db, &repo, Some(owner)).await.unwrap());
        assert!(can_admin_repo(&db, &repo, Some(owner)).await.unwrap());

        // Anonymous: nothing on a private repo.
        assert!(!can_read_repo(&db, &repo, None).await.unwrap());
        assert!(!can_write_repo(&db, &repo, None).await.unwrap());
        assert!(!can_admin_repo(&db, &repo, None).await.unwrap());

        // Unrelated authenticated user: nothing.
        let stranger = mk_user(&db).await;
        assert!(!can_read_repo(&db, &repo, Some(stranger)).await.unwrap());
        assert!(!can_write_repo(&db, &repo, Some(stranger)).await.unwrap());
        assert!(!can_admin_repo(&db, &repo, Some(stranger)).await.unwrap());
    }

    #[tokio::test]
    async fn public_repo_is_world_readable_but_write_stays_restricted() {
        let db = setup_db().await;
        let owner = mk_user(&db).await;
        let stranger = mk_user(&db).await;
        let repo = mk_repo(&db, owner, None, false).await; // public

        // Readable by anyone, including anonymous.
        assert!(can_read_repo(&db, &repo, None).await.unwrap());
        assert!(can_read_repo(&db, &repo, Some(stranger)).await.unwrap());

        // The public flag never grants write/admin.
        assert!(!can_write_repo(&db, &repo, Some(stranger)).await.unwrap());
        assert!(!can_admin_repo(&db, &repo, Some(stranger)).await.unwrap());
        assert!(can_write_repo(&db, &repo, Some(owner)).await.unwrap());
    }

    #[tokio::test]
    async fn collaborator_levels_grant_expected_access() {
        let db = setup_db().await;
        let owner = mk_user(&db).await;
        let repo = mk_repo(&db, owner, None, true).await;

        // read → read only.
        let reader = mk_user(&db).await;
        add_collab(&db, repo.id, reader, "read").await;
        assert!(can_read_repo(&db, &repo, Some(reader)).await.unwrap());
        assert!(!can_write_repo(&db, &repo, Some(reader)).await.unwrap());
        assert!(!can_admin_repo(&db, &repo, Some(reader)).await.unwrap());

        // write → read + write, not admin.
        let writer = mk_user(&db).await;
        add_collab(&db, repo.id, writer, "write").await;
        assert!(can_read_repo(&db, &repo, Some(writer)).await.unwrap());
        assert!(can_write_repo(&db, &repo, Some(writer)).await.unwrap());
        assert!(!can_admin_repo(&db, &repo, Some(writer)).await.unwrap());

        // admin → read + write + admin.
        let admin = mk_user(&db).await;
        add_collab(&db, repo.id, admin, "admin").await;
        assert!(can_read_repo(&db, &repo, Some(admin)).await.unwrap());
        assert!(can_write_repo(&db, &repo, Some(admin)).await.unwrap());
        assert!(can_admin_repo(&db, &repo, Some(admin)).await.unwrap());
    }

    #[tokio::test]
    async fn org_roles_and_team_permissions_control_access() {
        let db = setup_db().await;

        let org_owner = mk_user(&db).await;
        let org_name = format!("org_{}", uuid::Uuid::new_v4().simple());
        let org = org_ops::create_org(&db, &org_name, None, None, org_owner, "private")
            .await
            .unwrap();
        // Repo owned by the org (owner_id is the org owner, org_id set).
        let repo = mk_repo(&db, org_owner, Some(org.id), true).await;

        // Plain org member: read yes, write/admin no.
        let member = mk_user(&db).await;
        org_ops::add_org_member(&db, org.id, member, "member")
            .await
            .unwrap();
        assert!(can_read_repo(&db, &repo, Some(member)).await.unwrap());
        assert!(!can_write_repo(&db, &repo, Some(member)).await.unwrap());
        assert!(!can_admin_repo(&db, &repo, Some(member)).await.unwrap());

        // Org "admin" role: write + admin.
        let org_admin = mk_user(&db).await;
        org_ops::add_org_member(&db, org.id, org_admin, "admin")
            .await
            .unwrap();
        assert!(can_read_repo(&db, &repo, Some(org_admin)).await.unwrap());
        assert!(can_write_repo(&db, &repo, Some(org_admin)).await.unwrap());
        assert!(can_admin_repo(&db, &repo, Some(org_admin)).await.unwrap());

        // Member of a write-permission team: write yes, admin no.
        let team_writer = mk_user(&db).await;
        org_ops::add_org_member(&db, org.id, team_writer, "member")
            .await
            .unwrap();
        let write_team = org_ops::create_team(&db, org.id, "writers", None, "write")
            .await
            .unwrap();
        org_ops::add_team_member(&db, write_team.id, team_writer, "member")
            .await
            .unwrap();
        assert!(can_write_repo(&db, &repo, Some(team_writer)).await.unwrap());
        assert!(!can_admin_repo(&db, &repo, Some(team_writer)).await.unwrap());

        // Member of an admin-permission team: admin (and therefore write) yes.
        let team_admin = mk_user(&db).await;
        org_ops::add_org_member(&db, org.id, team_admin, "member")
            .await
            .unwrap();
        let admin_team = org_ops::create_team(&db, org.id, "admins", None, "admin")
            .await
            .unwrap();
        org_ops::add_team_member(&db, admin_team.id, team_admin, "member")
            .await
            .unwrap();
        assert!(can_admin_repo(&db, &repo, Some(team_admin)).await.unwrap());
        assert!(can_write_repo(&db, &repo, Some(team_admin)).await.unwrap());
    }

    /// The card's headline concern: after a collaborator is removed, the 30s-TTL
    /// cache keeps serving the stale "granted" decision until it is invalidated.
    /// This pins that contract so a future refactor can't silently let revoked
    /// access linger.
    #[tokio::test]
    async fn revoked_collaborator_access_clears_only_after_invalidation() {
        let db = setup_db().await;
        let owner = mk_user(&db).await;
        let repo = mk_repo(&db, owner, None, true).await;
        let collab = mk_user(&db).await;
        add_collab(&db, repo.id, collab, "write").await;

        // Warm the cache: the collaborator currently has read + write.
        assert!(can_read_repo(&db, &repo, Some(collab)).await.unwrap());
        assert!(can_write_repo(&db, &repo, Some(collab)).await.unwrap());

        // Revoke in the DB. The cache (30s TTL) still answers "yes".
        assert!(
            repo_collaborator_ops::delete_by_repo_and_user(&db, repo.id, collab)
                .await
                .unwrap(),
            "the revocation matched no row, so the rest of the test proves nothing"
        );
        assert!(
            can_read_repo(&db, &repo, Some(collab)).await.unwrap(),
            "stale cache should still serve the revoked read decision"
        );
        assert!(
            can_write_repo(&db, &repo, Some(collab)).await.unwrap(),
            "stale cache should still serve the revoked write decision"
        );

        // Invalidation is what actually makes the revocation take effect —
        // exactly what add/update/remove-collaborator call in production.
        invalidate_perm_cache_user(&db, repo.id, collab);
        assert!(!can_read_repo(&db, &repo, Some(collab)).await.unwrap());
        assert!(!can_write_repo(&db, &repo, Some(collab)).await.unwrap());
    }

    /// Two databases in one process, with colliding row ids and opposite correct
    /// answers for the very same `(repo_id, actor_id, read)` question — the
    /// shape that made the private-repo integration tests answer each other's
    /// question and fail in both directions.
    ///
    /// The ids are asserted to collide rather than assumed to: a fresh database
    /// hands out `repo 1` and `user 2` to both sides, and if that ever stopped
    /// being true this test would quietly stop testing anything.
    #[tokio::test]
    async fn a_decision_cached_for_one_database_does_not_answer_for_another() {
        // Database A: an outsider must NOT read the private repo.
        let db_a = setup_db().await;
        let owner_a = mk_user(&db_a).await;
        let repo_a = mk_repo(&db_a, owner_a, None, true).await;
        let outsider = mk_user(&db_a).await;

        // Database B: the same ids, but here the actor is a collaborator and
        // must read it.
        let db_b = setup_db().await;
        let owner_b = mk_user(&db_b).await;
        let repo_b = mk_repo(&db_b, owner_b, None, true).await;
        let collab = mk_user(&db_b).await;
        add_collab(&db_b, repo_b.id, collab, "read").await;

        assert_eq!(
            (repo_a.id, outsider),
            (repo_b.id, collab),
            "the two databases must hand out colliding ids for this to test anything"
        );

        // Poison first, in the order that used to break: the negative decision
        // is cached, then the other database asks the same key.
        assert!(!can_read_repo(&db_a, &repo_a, Some(outsider)).await.unwrap());
        assert!(
            can_read_repo(&db_b, &repo_b, Some(collab)).await.unwrap(),
            "database B's collaborator was answered with database A's refusal"
        );

        // And the other way round, now that B holds a positive decision.
        assert!(
            !can_read_repo(&db_a, &repo_a, Some(outsider)).await.unwrap(),
            "database A's outsider was let in by database B's grant"
        );
    }
}
