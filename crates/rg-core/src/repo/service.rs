//! Repository service — business logic for repo creation and access control.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{ActiveValue::Set, ConnectionTrait, DatabaseConnection};
use std::collections::{HashMap, HashSet};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use rg_db::{
    entities::repository::ActiveModel as RepoActiveModel,
    ops::{repo_ops, user_ops},
};

use super::templates;
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
    // Try user first
    if let Some(user) = user_ops::find_by_username(db, owner).await? {
        return Ok((user.id, None, user.username.clone()));
    }

    // Try organization
    if let Some(org) = rg_db::ops::org_ops::get_org_by_name(db, owner).await? {
        return Ok((org.owner_id, Some(org.id), org.name.clone()));
    }

    // The owner name is client input (a repo's target namespace), so naming one
    // that does not exist is a bad request, not a failed query.
    Err(crate::error::invalid_request(format!(
        "owner '{owner}' not found (neither user nor organization)"
    )))
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
        return Err(crate::error::invalid_request(taken_message.to_string()));
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

    // Determine path prefix: org name or user name
    let path_prefix = if let Some(oid) = opts.org_id {
        let org = rg_db::ops::org_ops::get_org(db, oid)
            .await?
            .ok_or_else(|| anyhow::anyhow!("organization not found"))?;
        org.name
    } else {
        let owner_user = rg_db::ops::user_ops::find_by_id(db, owner_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("owner not found"))?;
        owner_user.username
    };

    // Create bare git repo on disk using gix
    let git_path = repo_root.join(format!("{}/{}.git", path_prefix, name));
    // The path was already named here; what was missing on the deployment that
    // hits this first — a bind-mounted `repo_root` owned by another uid — is
    // which knob points elsewhere and which side of the mismatch is wrong.
    std::fs::create_dir_all(&git_path).map_err(|error| {
        crate::platform::fs::path_error(
            "repository directory",
            &git_path,
            &error,
            crate::platform::fs::REPO_ROOT_HINT,
        )
    })?;

    gix::create::into(
        &git_path,
        gix::create::Kind::Bare,
        gix::create::Options::default(),
    )
    .with_context(|| format!("gix init --bare failed for {:?}", git_path))?;

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
    let repo = match repo_ops::create(db, model).await {
        Ok(repo) => repo,
        Err(error) => {
            discard_unreferenced_repo_dir(&git_path, &recreate_blocked_by(name));
            return Err(error);
        }
    };

    // Keep the metadata FTS table in sync. Triggers also maintain it; use an
    // upsert so the explicit write is safe on SQLite, PostgreSQL and MySQL.
    let backend = db.get_database_backend();
    let fts_sql =
        crate::search::dialect::metadata_fts_upsert_sql(backend, "repos_fts", "name, description");
    let description = repo.description.as_deref().unwrap_or("");
    if let Err(e) = db
        .execute(sea_orm::Statement::from_sql_and_values(
            backend,
            rg_db::prepare_sql(backend, &fts_sql),
            [
                repo.id.into(),
                repo.name.as_str().into(),
                description.into(),
            ],
        ))
        .await
    {
        tracing::warn!(repo_id = repo.id, error = %format!("{e:#}"), "failed to update repos_fts index");
    }

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

        // Set HEAD in the bare repo to point to the default branch.
        // Use --git-dir (cannot combine with the gateway's `-C`, so repo_path=None).
        let head_ref = format!("refs/heads/{}", default_branch);
        let head_output = gateway
            .run(
                &["--git-dir", &push_url, "symbolic-ref", "HEAD", &head_ref],
                None,
            )
            .context("git symbolic-ref HEAD failed")?;
        if !head_output.success() {
            tracing::warn!(stderr = %head_output.stderr_str(), "failed to set HEAD in bare repo");
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

/// Soft-delete a repository.
pub async fn delete_repo(db: &DatabaseConnection, repo_id: i64) -> Result<()> {
    rg_db::ops::repo_ops::soft_delete(db, repo_id).await?;

    // Manually remove from the metadata FTS table as a defensive fallback.
    let backend = db.get_database_backend();
    if let Err(e) = db
        .execute(sea_orm::Statement::from_sql_and_values(
            backend,
            rg_db::prepare_sql(backend, "DELETE FROM repos_fts WHERE rowid = ?"),
            [repo_id.into()],
        ))
        .await
    {
        tracing::warn!(repo_id = repo_id, error = %format!("{e:#}"), "failed to remove repo from repos_fts index");
    }

    invalidate_perm_cache_repo(db, repo_id);
    Ok(())
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
    let forked = match repo_ops::create(db, model).await {
        Ok(forked) => forked,
        Err(error) => {
            discard_unreferenced_repo_dir(&target_path, &recreate_blocked_by(repo_name));
            return Err(error);
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
pub async fn transfer_repo(
    db: &DatabaseConnection,
    user_id: i64,
    owner: &str,
    repo_name: &str,
    new_owner: &str,
    repo_root: &std::path::Path,
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

    if let Err(error) = repo_ops::update_owner(db, repo.id, new_owner_id, new_org_id).await {
        // The directory has already moved, so a row left pointing at the old
        // owner breaks the repository for *both* sides: the old owner has a row
        // whose tree is gone, the new owner a tree no row names. Move it back,
        // and if even that fails say so — nothing else will ever repair it.
        if let Err(cleanup_error) = std::fs::rename(&new_path, &old_path) {
            tracing::warn!(
                repo_id = repo.id,
                moved_to = %new_path.display(),
                belongs_at = %old_path.display(),
                error = %cleanup_error,
                "failed to move a repository back after its ownership update failed — it is now \
                 unreachable for both the old and the new owner until the directory is moved back \
                 by hand"
            );
        }
        return Err(error);
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
        clone.ensure_success()?;

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

/// Get the blob SHA of a file at a given ref, or `None` when the ref/path does
/// not resolve.
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

    let target = format!("{}:{}", git_ref, file_path);
    Ok(repo
        .rev_parse_single(target.as_str())
        .ok()
        .map(|object_id| object_id.to_string()))
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
        repo_collaborator_ops::delete_by_repo_and_user(&db, repo.id, collab)
            .await
            .unwrap();
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
