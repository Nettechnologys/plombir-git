//! Import pipeline service — orchestrates full repository migration.
//!
//! Supports importing from GitHub and GitLab, including:
//! - Repository cloning (git clone --bare) + ForgeKeep DB registration
//! - Labels and milestones
//! - Issues with comments
//! - Pull/Merge requests with reviews/comments
//! - Releases
//! - Wiki pages, cloned from the source's `<repo>.wiki.git`
//!
//! Generic Git and Gitea imports currently clone the repository only.
//!
//! The import runs asynchronously and updates progress in the
//! import_tasks database table.
//!
//! ## Imported content belongs to the account that imported it
//!
//! Every issue, comment, review and merge request this module writes is
//! attributed to `task.user_id`. There is no mapping from a source-platform
//! login to a local account, and the code no longer pretends otherwise: it used
//! to walk the source's issues and PRs to collect logins, hand them to a
//! `map_users` whose whole body was `mapping.entry(login).or_insert(task.user_id)`,
//! and then look each author up in the constant map it had just built
//! (card_dd6ae4f40206). The lookups that missed fell through to a hardcoded
//! user id `1`, which is not "the admin" on any instance where account 1 was
//! renamed, deleted, or never an admin to begin with.
//!
//! Attributing by name match is not the obvious fix it looks like. The source
//! platform's `alice` and this instance's `alice` are unrelated accounts, so a
//! name match would let anyone who can import a repository publish issues and
//! reviews under a colleague's name. A real mapping needs the importer to state
//! it — there is no API or UI to state one, so there is no mapping to store,
//! and `import_tasks.user_mapping` is gone rather than left NULL forever.
//!
//! ## The source platform's access token
//!
//! The PAT the user hands us for the source platform lives in memory only, for
//! exactly as long as the import runs: [`start_import`] passes it straight to
//! the worker it spawns and never writes it to the task row. It is not stored
//! because nothing would ever read it back — an interrupted import is failed by
//! the watchdog, never resumed — and a stored copy is a copy that outlives its
//! purpose, in the DB and in every polled status response. See the note on
//! `rg_db::entities::import_task`.
//!
//! For the same reason a failure reason is masked with [`failure_reason`]
//! before it is persisted: the token reaches `git`/the platform API, so it can
//! come back inside their error text.
//!
//! On the way to `git` the token travels through the **environment**, read back
//! by an inline credential helper — never through argv and never through the
//! clone URL, which git copies verbatim into the new repository's
//! `remote.origin.url`. See [`rg_git::credentials::credential_invocation`],
//! which mirror sync shares: both hand a secret to a user-supplied remote.
//!
//! ## A token typed into the source URL
//!
//! `https://user:token@host/repo.git` would put that same token in
//! `import_tasks.source_url`, which *is* stored and *is* returned by every
//! status poll. So [`start_import`] takes it back out
//! ([`crate::net::split_url_credentials`]) and treats it as the token it is —
//! in memory, for the life of the import. The login half stays in the URL: it
//! is not a secret, this table has no column for it, and `git` needs it to
//! authenticate. Rows written before that are stripped at startup by
//! [`strip_legacy_source_url_credentials`].

use anyhow::{Context, Result};
use chrono::Utc;
use rg_git::cli_gateway::global_gateway;
use rg_git::credentials::{credential_invocation, GitCredentials};
use sea_orm::{ActiveValue::Set, DatabaseConnection};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use rg_db::entities::import_task::{self, Model as ImportTask};
use rg_db::entities::{issue, label, milestone};
use rg_db::ops::{
    import_task_ops, issue_comment_ops, label_ops, milestone_ops, org_ops, pr_review_ops, user_ops,
};

use crate::import::github_client::{
    GitHubClient, GitHubComment, GitHubIssue, GitHubLabel, GitHubMilestone, GitHubPR,
    GitHubRelease, GitHubReview,
};
use crate::import::gitlab_client::{
    GitLabClient, GitLabIssue, GitLabLabel, GitLabMR, GitLabMilestone, GitLabNote, GitLabRelease,
};
use crate::platform::fs::{path_error, REPO_ROOT_HINT};

/// Statistics collected during import.
#[derive(Debug, Default, serde::Serialize)]
pub struct ImportStats {
    pub repo_cloned: bool,
    pub labels_imported: usize,
    pub milestones_imported: usize,
    pub issues_imported: usize,
    pub issue_comments_imported: usize,
    pub prs_imported: usize,
    pub pr_reviews_imported: usize,
    pub releases_imported: usize,
    pub wiki_pages_imported: usize,
}

/// Live import workers owned by one server instance.
///
/// The database row describes progress, but it cannot cancel the future that
/// owns the work. Keeping the cancellation edge in application state lets the
/// DELETE route stop that future and wait until it can no longer publish repo
/// bytes or metadata before deleting the row the worker reports through.
#[derive(Clone, Default)]
pub struct ImportWorkerRegistry {
    workers: Arc<Mutex<HashMap<i64, WorkerControl>>>,
}

#[derive(Clone)]
struct WorkerControl {
    cancellation: CancellationToken,
    finished: CancellationToken,
}

struct WorkerGuard {
    registry: ImportWorkerRegistry,
    task_id: i64,
    finished: CancellationToken,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        // Unlike a one-shot notification, a cancelled token remembers the
        // signal, so DELETE cannot miss a worker that finished just before it
        // began waiting.
        self.finished.cancel();

        // A panic in the worker must still release a DELETE waiting on it.
        // Recovering the map after poison is safe here: this drop path removes
        // one entry and never relies on an invariant guarded by the mutex.
        let mut workers = self
            .registry
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        workers.remove(&self.task_id);
    }
}

impl ImportWorkerRegistry {
    fn spawn<F>(&self, task_id: i64, worker: F) -> Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let cancellation = CancellationToken::new();
        let finished = CancellationToken::new();
        let control = WorkerControl {
            cancellation: cancellation.clone(),
            finished: finished.clone(),
        };

        {
            let mut workers = self
                .workers
                .lock()
                .map_err(|_| anyhow::anyhow!("import worker registry lock poisoned"))?;
            if workers.contains_key(&task_id) {
                anyhow::bail!("import worker already registered: {task_id}");
            }
            workers.insert(task_id, control);
        }

        let guard = WorkerGuard {
            registry: self.clone(),
            task_id,
            finished,
        };
        tokio::spawn(async move {
            let _guard = guard;
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    tracing::info!(task_id, "import worker canceled");
                }
                _ = worker => {}
            }
        });

        Ok(())
    }

    /// Cancel a live worker, if this process owns one, and wait for it to stop.
    ///
    /// `Ok(false)` means the worker already finished (or belonged to a previous
    /// process and was recovered by the watchdog). In either case there is no
    /// live future in this process that can publish after this method returns.
    pub async fn cancel_and_wait(&self, task_id: i64) -> Result<bool> {
        let control = self
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("import worker registry lock poisoned"))?
            .get(&task_id)
            .cloned();
        let Some(control) = control else {
            return Ok(false);
        };

        control.cancellation.cancel();
        control.finished.cancelled().await;
        Ok(true)
    }
}

/// Run a full import pipeline and update the import task as it progresses.
///
/// `auth_token` is the source platform's credential as the user supplied it.
/// It is a parameter rather than a column of `task` on purpose — see the module
/// note.
pub async fn run_import(
    db: &DatabaseConnection,
    task: &ImportTask,
    repo_root: &Path,
    auth_token: Option<&str>,
    trusted_origins: &crate::import::trust::TrustedImportOrigins,
) -> Result<ImportStats> {
    let mut stats = ImportStats::default();

    // SSRF guard: the platform runners spawn `git clone` (and API calls) against
    // this user-supplied URL. Reject internal/loopback/metadata hosts and
    // non-git transports (`file://`, `ext::`, …) before any subprocess runs —
    // the git twin of the mirror-sync guard. For GitLab this validates the
    // project URL; the actual API-derived clone URL is guarded again below.
    trusted_origins.guard_url(&task.source_url).await?;

    let auth_token = auth_token.unwrap_or("");

    match task.platform.as_str() {
        "github" => run_github_import(db, task, repo_root, auth_token, &mut stats).await?,
        "gitlab" => {
            run_gitlab_import(db, task, repo_root, auth_token, &mut stats, trusted_origins).await?
        }
        "gitea" | "git" => run_git_import(db, task, repo_root, auth_token, &mut stats).await?,
        other => anyhow::bail!("unsupported platform: {other}"),
    }

    Ok(stats)
}

// ═══════════════════════════════════════════════════════════════════════
// Generic Git / Gitea import
// ═══════════════════════════════════════════════════════════════════════

/// The clone step of an import pass, shared by every platform runner.
///
/// Shared rather than repeated so the lifecycle recheck below cannot be added
/// to one runner and forgotten on the other two: the three had carried three
/// copies of this block, and a guard that has to be remembered three times is
/// a guard that will be added twice.
///
/// The recheck sits as close to the subprocess as the code allows.
/// [`resolve_or_create_target_repo`] runs once, at the top of the pass, and
/// `clone_repo` then decides what to do from what is on disk alone: a
/// repository deleted in between has had `<owner>/<name>.git` retired, so the
/// missing directory reads as "nothing here yet" and the whole upstream is
/// written back under the canonical name (card_a3ce6a2363a7). The deletion
/// quiescence gate refuses while an import is in a running status; this closes
/// the window *between* that query and this `git`, which no single query can.
#[allow(clippy::too_many_arguments)]
async fn clone_into_target(
    db: &DatabaseConnection,
    task: &ImportTask,
    repo_id: i64,
    clone_url: &str,
    repo_root: &Path,
    token: &str,
    progress_when_done: i32,
    stats: &mut ImportStats,
) -> Result<()> {
    update_stage(db, task.id, "cloning", 0, "Cloning repository...").await?;

    let Some(target_row) = rg_db::ops::repo_ops::find_by_id(db, repo_id)
        .await
        .context("failed to re-read the import target repository before cloning")?
    else {
        anyhow::bail!(
            "the target repository '{}/{}' was deleted while this import was running; nothing \
             was cloned",
            task.target_owner,
            task.target_name
        );
    };

    let outcome = clone_repo(
        clone_url,
        repo_root,
        &task.target_owner,
        &task.target_name,
        source_credentials(&task.platform, &task.source_url, token).as_ref(),
    )?;
    // The fact, not the intention. Set unconditionally, this reported the one
    // case where the clone deliberately does nothing exactly like a clone that
    // transferred the whole upstream — and the status response is all a user
    // has to tell an imported repository from an empty one.
    stats.repo_cloned = outcome == CloneOutcome::Cloned;

    // Only a clone that ran replaced `HEAD`. A skipped one left the target's own
    // history — and its own default branch — in place, and the column already
    // describes it.
    if outcome == CloneOutcome::Cloned {
        adopt_cloned_default_branch(
            db,
            repo_id,
            &repo_root.join(format!("{}/{}.git", task.target_owner, task.target_name)),
            &target_row.default_branch,
        )
        .await;
    }

    update_stage(
        db,
        task.id,
        "importing",
        progress_when_done,
        match outcome {
            CloneOutcome::Cloned => "Repository cloned",
            CloneOutcome::Skipped => "Target already holds a repository — clone skipped",
        },
    )
    .await?;

    Ok(())
}

/// Make `repositories.default_branch` name the branch the freshly cloned
/// repository's `HEAD` actually points at.
///
/// `create_repo` writes that column from what the *request* asked for — `main`
/// unless told otherwise — and sets the bare repository's `HEAD` to match. An
/// import then replaces the repository wholesale, and `git clone --bare` brings
/// the upstream's `HEAD` with it. For an upstream on `master` (or `trunk`, or
/// `devel`) the two disagree from that moment on, and nothing else ever writes
/// the column: `resolve_content_ref` is handed `main`, finds neither
/// `refs/heads/main` nor `refs/tags/main`, and the repository page of a
/// repository holding the entire upstream history answers `404` — while
/// `classify_repo_emptiness` says `NotEmpty`, so not even the empty-repository view renders
/// (card_0e4d6e7fcdb2).
///
/// Deliberately not fatal. The bytes are in place and the import succeeded; a
/// `HEAD` that cannot be read afterwards leaves the column exactly as stale as
/// it was before this function existed, which is worth a warning naming the
/// repository, not an import reported as failed after it transferred everything.
///
/// An unborn clone is skipped rather than adopted. With no refs at all there is
/// no branch to describe, and the `HEAD` git wrote in that case can come from
/// *this host's* `init.defaultBranch` rather than from the upstream — adopting
/// it would replace a correct column with the server's local git config.
async fn adopt_cloned_default_branch(
    db: &DatabaseConnection,
    repo_id: i64,
    repo_path: &Path,
    recorded_branch: &str,
) {
    let advertisement = match rg_git::ref_advertisement::collect(repo_path) {
        Ok(advertisement) => advertisement,
        Err(error) => {
            tracing::warn!(
                repo_id,
                path = %repo_path.display(),
                error = %format!("{error:#}"),
                recorded_branch,
                "could not read HEAD of the imported repository — its default branch column still \
                 names the branch the repository was created with, which the upstream need not have"
            );
            return;
        }
    };

    // Unborn: the clone carries no history, so there is no branch to adopt.
    if advertisement.head_oid.is_none() {
        return;
    }

    let Some(branch) = advertisement
        .head_target
        .as_deref()
        .and_then(|target| target.strip_prefix("refs/heads/"))
    else {
        // Detached, or a symbolic HEAD pointing outside `refs/heads/`. Neither
        // is a branch name the column can carry.
        tracing::warn!(
            repo_id,
            path = %repo_path.display(),
            head_target = ?advertisement.head_target,
            "the imported repository's HEAD names no branch — leaving the default branch column"
        );
        return;
    };

    if branch == recorded_branch {
        return;
    }

    match rg_db::ops::repo_ops::set_default_branch(db, repo_id, branch).await {
        Ok(true) => tracing::info!(
            repo_id,
            from = recorded_branch,
            to = branch,
            "adopted the imported repository's default branch"
        ),
        // The repository was deleted while the clone ran. `clone_into_target`'s
        // own recheck reports that case; there is nothing to correct here.
        Ok(false) => {}
        Err(error) => tracing::warn!(
            repo_id,
            from = recorded_branch,
            to = branch,
            error = %format!("{error:#}"),
            "could not record the imported repository's default branch — its page will resolve a \
             branch the repository does not have"
        ),
    }
}

async fn run_git_import(
    db: &DatabaseConnection,
    task: &ImportTask,
    repo_root: &Path,
    auth_token: &str,
    stats: &mut ImportStats,
) -> Result<()> {
    let repo_id = resolve_or_create_target_repo(
        db,
        task.repo_id,
        &task.target_owner,
        &task.target_name,
        repo_root,
    )
    .await?;
    import_task_ops::set_repo_id(db, task.id, repo_id)
        .await
        .context("failed to link the import task to its target repository")?;

    if task.import_repo {
        clone_into_target(
            db,
            task,
            repo_id,
            &task.source_url,
            repo_root,
            auth_token,
            90,
            stats,
        )
        .await?;
    }

    Ok(())
}

async fn load_milestone_map(db: &DatabaseConnection, repo_id: i64) -> Result<HashMap<String, i64>> {
    let existing = milestone_ops::list_by_repo(db, repo_id, None).await?;
    let mut map = HashMap::with_capacity(existing.len());
    for ms in existing {
        map.insert(ms.title.clone(), ms.id);
    }
    Ok(map)
}

async fn load_label_map(db: &DatabaseConnection, repo_id: i64) -> Result<HashMap<String, i64>> {
    let existing = label_ops::list_by_repo(db, repo_id).await?;
    Ok(existing
        .into_iter()
        .map(|label| (label.name, label.id))
        .collect())
}

fn resolve_imported_label_ids(
    label_map: Option<&HashMap<String, i64>>,
    names: &[String],
) -> Result<Vec<i64>> {
    // `import_labels = false` means exactly that: issue import must not smuggle
    // label metadata back in through a denormalized field.
    let Some(label_map) = label_map else {
        return Ok(Vec::new());
    };

    let mut seen = HashSet::new();
    names
        .iter()
        .filter(|name| seen.insert((*name).clone()))
        .map(|name| {
            label_map.get(name).copied().ok_or_else(|| {
                anyhow::anyhow!(
                    "source issue references label {name:?}, but that label was not imported"
                )
            })
        })
        .collect()
}

async fn create_imported_issue(
    db: &DatabaseConnection,
    repo_id: i64,
    model: issue::ActiveModel,
    label_ids: Vec<i64>,
) -> Result<issue::Model> {
    // The same allocator as an ordinary create: an import running next to live
    // traffic (or next to a second import of the same repository) must not lose
    // a correct row to a number someone else took between the read and the
    // write.
    crate::issue::service::insert_with_repo_number(db, repo_id, model, Some(label_ids)).await
}

// ═══════════════════════════════════════════════════════════════════════
// GitHub import
// ═══════════════════════════════════════════════════════════════════════

async fn run_github_import(
    db: &DatabaseConnection,
    task: &ImportTask,
    repo_root: &Path,
    token: &str,
    stats: &mut ImportStats,
) -> Result<()> {
    // Parse the repository identity and its API host together. Computing only
    // owner/repo here used to leave the client's optional base URL at `None`,
    // which sent a GHES token and every metadata request to api.github.com.
    let GitHubImportSource {
        owner: gh_owner,
        repo: gh_repo,
        api_base_url,
    } = parse_github_url(&task.source_url)?;
    let client = GitHubClient::new(token.to_string(), api_base_url)?;

    // Resolve (or create) the target repo in ForgeKeep DB
    let repo_id = resolve_or_create_target_repo(
        db,
        task.repo_id,
        &task.target_owner,
        &task.target_name,
        repo_root,
    )
    .await?;

    // Update task with repo_id
    import_task_ops::set_repo_id(db, task.id, repo_id)
        .await
        .context("failed to link the import task to its target repository")?;

    // Step 1: Clone repository
    if task.import_repo {
        clone_into_target(
            db,
            task,
            repo_id,
            &task.source_url,
            repo_root,
            token,
            10,
            stats,
        )
        .await?;
    }

    // Build milestone cache (existing in target repo + newly imported ones when enabled)
    let mut milestone_map = load_milestone_map(db, repo_id).await?;

    // Step 2: Labels

    if task.import_labels {
        update_stage(db, task.id, "importing", 15, "Importing labels...").await?;
        let labels = client.list_labels(&gh_owner, &gh_repo).await?;
        stats.labels_imported = import_github_labels(db, repo_id, &labels).await?;
        update_stage(
            db,
            task.id,
            "importing",
            20,
            &format!("Imported {} labels", stats.labels_imported),
        )
        .await?;
    }

    let label_map = if task.import_labels {
        Some(load_label_map(db, repo_id).await?)
    } else {
        None
    };

    // Step 3: Milestones
    if task.import_milestones {
        update_stage(db, task.id, "importing", 25, "Importing milestones...").await?;
        let milestones = client.list_milestones(&gh_owner, &gh_repo).await?;
        stats.milestones_imported =
            import_github_milestones(db, repo_id, &milestones, &mut milestone_map).await?;
        update_stage(
            db,
            task.id,
            "importing",
            30,
            &format!("Imported {} milestones", stats.milestones_imported),
        )
        .await?;
    }

    // Step 4: Issues
    if task.import_issues {
        update_stage(db, task.id, "importing", 35, "Importing issues...").await?;
        let issues = client.list_issues(&gh_owner, &gh_repo).await?;
        let total = issues.len();

        for (i, issue) in issues.iter().enumerate() {
            let comments = client
                .list_issue_comments(&gh_owner, &gh_repo, issue.number)
                .await?;
            import_github_issue(
                db,
                repo_id,
                &task.target_owner,
                &task.target_name,
                issue,
                &comments,
                task.user_id,
                &milestone_map,
                label_map.as_ref(),
            )
            .await?;
            stats.issues_imported += 1;
            stats.issue_comments_imported += comments.len();

            let pct = 35 + (i as f64 / total.max(1) as f64 * 20.0) as i32;
            update_stage(
                db,
                task.id,
                "importing",
                pct,
                &format!("Importing issues ({}/{})", i + 1, total),
            )
            .await?;
        }
    }

    // Step 5: Pull Requests
    if task.import_pull_requests {
        update_stage(db, task.id, "importing", 60, "Importing pull requests...").await?;
        let prs = client.list_pull_requests(&gh_owner, &gh_repo).await?;
        let total = prs.len();

        for (i, pr) in prs.iter().enumerate() {
            let comments = client
                .list_issue_comments(&gh_owner, &gh_repo, pr.number)
                .await?;
            let reviews = client
                .list_pr_reviews(&gh_owner, &gh_repo, pr.number)
                .await?;
            import_github_pr(
                db,
                repo_id,
                pr,
                &comments,
                &reviews,
                task.user_id,
                &milestone_map,
            )
            .await?;
            stats.prs_imported += 1;
            stats.pr_reviews_imported += reviews.len();
            stats.issue_comments_imported += comments.len();

            let pct = 60 + (i as f64 / total.max(1) as f64 * 15.0) as i32;
            update_stage(
                db,
                task.id,
                "importing",
                pct,
                &format!("Importing PRs ({}/{})", i + 1, total),
            )
            .await?;
        }
    }

    // Step 6: Releases
    if task.import_releases {
        update_stage(db, task.id, "importing", 80, "Importing releases...").await?;
        let releases = client.list_releases(&gh_owner, &gh_repo).await?;
        stats.releases_imported = import_github_releases(db, repo_id, &releases, repo_root).await?;
        update_stage(
            db,
            task.id,
            "importing",
            90,
            &format!("Imported {} releases", stats.releases_imported),
        )
        .await?;
    }

    // Step 7: Wiki
    if task.import_wiki {
        update_stage(db, task.id, "importing", 92, "Importing wiki...").await?;
        stats.wiki_pages_imported = import_wiki_pages(
            db,
            repo_id,
            &wiki_clone_url(&task.source_url),
            &wiki_staging_dir(repo_root, &task.target_owner, &task.target_name),
            source_credentials(&task.platform, &task.source_url, token).as_ref(),
            Some(task.user_id),
        )
        .await?;
        update_stage(
            db,
            task.id,
            "importing",
            95,
            &format!("Imported {} wiki pages", stats.wiki_pages_imported),
        )
        .await?;
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════
// GitLab import
// ═══════════════════════════════════════════════════════════════════════

async fn run_gitlab_import(
    db: &DatabaseConnection,
    task: &ImportTask,
    repo_root: &Path,
    token: &str,
    stats: &mut ImportStats,
    trusted_origins: &crate::import::trust::TrustedImportOrigins,
) -> Result<()> {
    // Parse the project identity and its API host together. Computing only the
    // path here used to leave the client's optional base URL at `None`, which
    // sent a self-hosted token and every metadata request to gitlab.com.
    let GitLabImportSource {
        project_path,
        api_base_url,
    } = parse_gitlab_url(&task.source_url)?;
    let client = GitLabClient::new(token.to_string(), api_base_url)?;

    // Resolve (or create) the target repo in ForgeKeep DB
    let repo_id = resolve_or_create_target_repo(
        db,
        task.repo_id,
        &task.target_owner,
        &task.target_name,
        repo_root,
    )
    .await?;

    import_task_ops::set_repo_id(db, task.id, repo_id)
        .await
        .context("failed to link the import task to its target repository")?;

    // Step 1: Clone repository
    if task.import_repo {
        // The API round-trip below belongs to the cloning stage: without this
        // the task would sit in `pending` for its duration, which is a status
        // the UI renders as "not started yet".
        update_stage(db, task.id, "cloning", 0, "Resolving source project...").await?;
        let project = client.get_project(&project_path).await?;
        // The clone URL comes from the GitLab API response and receives the
        // user's credential. It must remain on the source's exact origin; a
        // second allowlisted origin is not authority to move this PAT there.
        crate::import::trust::require_same_origin(&task.source_url, &project.http_url_to_repo)?;
        trusted_origins.guard_url(&project.http_url_to_repo).await?;
        clone_into_target(
            db,
            task,
            repo_id,
            &project.http_url_to_repo,
            repo_root,
            token,
            10,
            stats,
        )
        .await?;
    }

    // Build milestone cache (existing in target repo + newly imported ones when enabled)
    let mut milestone_map = load_milestone_map(db, repo_id).await?;

    // Step 2: Labels

    if task.import_labels {
        update_stage(db, task.id, "importing", 15, "Importing labels...").await?;
        let labels = client.list_labels(&project_path).await?;
        stats.labels_imported = import_gitlab_labels(db, repo_id, &labels).await?;
        update_stage(
            db,
            task.id,
            "importing",
            20,
            &format!("Imported {} labels", stats.labels_imported),
        )
        .await?;
    }

    let label_map = if task.import_labels {
        Some(load_label_map(db, repo_id).await?)
    } else {
        None
    };

    // Step 3: Milestones
    if task.import_milestones {
        update_stage(db, task.id, "importing", 25, "Importing milestones...").await?;
        let milestones = client.list_milestones(&project_path).await?;
        stats.milestones_imported =
            import_gitlab_milestones(db, repo_id, &milestones, &mut milestone_map).await?;
        update_stage(
            db,
            task.id,
            "importing",
            30,
            &format!("Imported {} milestones", stats.milestones_imported),
        )
        .await?;
    }

    // Step 4: Issues
    if task.import_issues {
        update_stage(db, task.id, "importing", 35, "Importing issues...").await?;
        let issues = client.list_issues(&project_path).await?;
        let total = issues.len();

        for (i, issue) in issues.iter().enumerate() {
            let notes = client.list_issue_notes(&project_path, issue.iid).await?;
            import_gitlab_issue(
                db,
                repo_id,
                &task.target_owner,
                &task.target_name,
                issue,
                &notes,
                task.user_id,
                &milestone_map,
                label_map.as_ref(),
            )
            .await?;
            stats.issues_imported += 1;
            stats.issue_comments_imported += notes.len();

            let pct = 35 + (i as f64 / total.max(1) as f64 * 20.0) as i32;
            update_stage(
                db,
                task.id,
                "importing",
                pct,
                &format!("Importing issues ({}/{})", i + 1, total),
            )
            .await?;
        }
    }

    // Step 5: Merge Requests
    if task.import_pull_requests {
        update_stage(db, task.id, "importing", 60, "Importing merge requests...").await?;
        let mrs = client.list_merge_requests(&project_path).await?;
        let total = mrs.len();

        for (i, mr) in mrs.iter().enumerate() {
            let notes = client.list_mr_notes(&project_path, mr.iid).await?;
            import_gitlab_mr(db, repo_id, mr, &notes, task.user_id, &milestone_map).await?;
            stats.prs_imported += 1;
            stats.issue_comments_imported += notes.len();

            let pct = 60 + (i as f64 / total.max(1) as f64 * 15.0) as i32;
            update_stage(
                db,
                task.id,
                "importing",
                pct,
                &format!("Importing MRs ({}/{})", i + 1, total),
            )
            .await?;
        }
    }

    // Step 6: Releases
    if task.import_releases {
        update_stage(db, task.id, "importing", 80, "Importing releases...").await?;
        let releases = client.list_releases(&project_path).await?;
        stats.releases_imported = import_gitlab_releases(db, repo_id, &releases, repo_root).await?;
        update_stage(
            db,
            task.id,
            "importing",
            90,
            &format!("Imported {} releases", stats.releases_imported),
        )
        .await?;
    }

    // Step 7: Wiki
    //
    // Derived from `task.source_url`, not from the project the API described:
    // GitLab's `http_url_to_repo` is only read on the cloning path, and the
    // wiki lives at the same address the user typed with one suffix added.
    if task.import_wiki {
        update_stage(db, task.id, "importing", 92, "Importing wiki...").await?;
        stats.wiki_pages_imported = import_wiki_pages(
            db,
            repo_id,
            &wiki_clone_url(&task.source_url),
            &wiki_staging_dir(repo_root, &task.target_owner, &task.target_name),
            source_credentials(&task.platform, &task.source_url, token).as_ref(),
            Some(task.user_id),
        )
        .await?;
        update_stage(
            db,
            task.id,
            "importing",
            95,
            &format!("Imported {} wiki pages", stats.wiki_pages_imported),
        )
        .await?;
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════
// Target repo resolution
// ═══════════════════════════════════════════════════════════════════════

/// Find the target repo in ForgeKeep DB, or create it if it doesn't exist.
/// Returns the repo_id for use in all subsequent import operations.
///
/// `anchored_repo_id` is the repository the import was accepted for, recorded
/// by [`start_import`] when it already existed. It makes "the target is gone"
/// distinguishable from "the target was never there": without it a repository
/// deleted after the import started simply falls out of the lookup below, the
/// create branch runs, and the import quietly rebuilds the namespace the
/// deletion retired — under the canonical `<owner>/<name>` the next repository
/// of that name will claim (card_a3ce6a2363a7). An import that was accepted
/// for a *new* name still creates it; only an anchored one refuses.
async fn resolve_or_create_target_repo(
    db: &DatabaseConnection,
    anchored_repo_id: Option<i64>,
    target_owner: &str,
    target_name: &str,
    repo_root: &Path,
) -> Result<i64> {
    if let Some(repo_id) = anchored_repo_id {
        return match rg_db::ops::repo_ops::find_by_id(db, repo_id)
            .await
            .context("failed to re-read the target repository this import was accepted for")?
        {
            Some(repo) => Ok(repo.id),
            None => anyhow::bail!(
                "the target repository '{target_owner}/{target_name}' was deleted after this \
                 import started; nothing was cloned"
            ),
        };
    }

    // Try to find existing repo via the repo service (handles user+org lookup)
    if let Some(repo) = crate::repo::service::find_repo_by_owner_name(db, target_owner, target_name)
        .await
        .context("failed to look up existing target repository")?
    {
        tracing::info!(repo_id = repo.id, "Found existing target repo");
        return Ok(repo.id);
    }

    // Resolve owner: try user first, then org
    let (owner_id, org_id) =
        if let Some(user) = user_ops::find_active_by_username(db, target_owner).await? {
            (user.id, None)
        } else if let Some(org) = org_ops::find_active_org_by_name(db, target_owner).await? {
            (org.owner_id, Some(org.id))
        } else {
            anyhow::bail!(
                "target owner '{}' not found (must be an existing ForgeKeep user or organization)",
                target_owner
            );
        };

    // Create the repo via the repo service
    let repo = crate::repo::service::create_repo(
        db,
        owner_id,
        target_name,
        None,  // no description — imported repo
        false, // public by default
        repo_root,
        org_id,
    )
    .await?;

    tracing::info!(repo_id = repo.id, "Created target repo for import");
    Ok(repo.id)
}

// ═══════════════════════════════════════════════════════════════════════
// Git helpers
// ═══════════════════════════════════════════════════════════════════════

/// How a source platform expects a personal access token to arrive.
///
/// The token always travels as the HTTP Basic *password*; the username is the
/// fixed placeholder the platform documents — `x-access-token` for GitHub,
/// `oauth2` for GitLab — because Basic auth has nowhere to put a lone token.
/// Gitea (and a plain git remote fronted by it) checks the password against its
/// access tokens and ignores the username, so the GitLab placeholder serves
/// those too.
///
/// An empty token means the user described a public source: the clone stays
/// anonymous rather than offering an empty password.
///
/// `source_url` is read for one thing only: the login the operator left in it
/// (`https://user@host/repo.git`, what remains after [`start_import`] lifted the
/// token out of `user:token@`). A self-hosted remote that checks the username
/// gets the one that was typed instead of a placeholder that would not match;
/// a URL without one falls back to the platform's.
fn source_credentials(platform: &str, source_url: &str, token: &str) -> Option<GitCredentials> {
    if token.is_empty() {
        return None;
    }
    let from_url = crate::net::strip_url_credentials(source_url).username;
    let username = match from_url {
        Some(username) => username,
        None => match platform {
            "github" => "x-access-token".to_string(),
            _ => "oauth2".to_string(),
        },
    };
    Some(GitCredentials::token(&username, token))
}

/// What the clone step did with `<owner>/<name>.git`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloneOutcome {
    /// The upstream was transferred and is installed at the target path.
    Cloned,
    /// The target already held a repository with refs of its own. Nothing was
    /// cloned, and nothing was overwritten.
    Skipped,
}

/// Whether `<owner>/<name>.git` already holds a repository of its own.
///
/// Fail-closed on purpose: a target that cannot be opened or whose refs cannot
/// be read counts as holding history, so a repository the import cannot
/// understand is never cloned over. Only the two states an import may write
/// into answer `false` — nothing on disk at all, and the ref-less bare skeleton
/// [`crate::repo::service::create_repo`] leaves behind.
fn target_holds_history(target_dir: &Path) -> bool {
    if !target_dir.exists() {
        return false;
    }
    match rg_git::ref_advertisement::collect(target_dir) {
        Ok(advertisement) => !advertisement.refs.is_empty(),
        Err(error) => {
            tracing::warn!(
                path = %target_dir.display(),
                error = %format!("{error:#}"),
                "the import target could not be read — treating it as an existing repository \
                 rather than cloning over it"
            );
            true
        }
    }
}

/// Remove a clone that never made it onto the target path.
fn discard_partial_clone(staging: &Path) {
    if !staging.exists() {
        return;
    }
    if let Err(error) = std::fs::remove_dir_all(staging) {
        tracing::warn!(
            path = %staging.display(),
            error = %error,
            "a partial import clone could not be removed and is now unreferenced bytes under \
             the repository root"
        );
    }
}

/// Move a finished clone onto the target path, retiring the skeleton the
/// repository's creation left there.
///
/// Two renames inside one directory rather than "remove the skeleton, then
/// rename": the skeleton is discarded only once the clone is in its place, so a
/// failure in between is undone instead of leaving the repository's row
/// pointing at a path with nothing on it. Both renames stay within `parent`, so
/// neither can fail for crossing a filesystem boundary.
fn install_clone(staging: &Path, retired: &Path, target_dir: &Path) -> Result<()> {
    let occupied = target_dir.exists();
    if occupied {
        std::fs::rename(target_dir, retired).map_err(|error| {
            path_error(
                "the empty repository the import replaces",
                target_dir,
                &error,
                REPO_ROOT_HINT,
            )
        })?;
    }

    if let Err(error) = std::fs::rename(staging, target_dir) {
        let failure = path_error(
            "the imported repository",
            target_dir,
            &error,
            REPO_ROOT_HINT,
        );
        if occupied {
            if let Err(restore) = std::fs::rename(retired, target_dir) {
                tracing::error!(
                    path = %target_dir.display(),
                    retired = %retired.display(),
                    error = %restore,
                    "the import could not install its clone and could not put the repository \
                     it moved aside back — the target path is empty and the row still names it"
                );
            }
        }
        return Err(failure);
    }

    if occupied {
        if let Err(error) = std::fs::remove_dir_all(retired) {
            tracing::warn!(
                path = %retired.display(),
                error = %error,
                "the empty repository the import replaced could not be removed and is now \
                 unreferenced bytes under the repository root"
            );
        }
    }
    Ok(())
}

/// Clone a repository (bare) into the ForgeKeep repo root.
///
/// ## Why this does not simply clone into the target path
///
/// By the time an import reaches `git`, `<owner>/<name>.git` usually already
/// exists: on the main path the import creates its own target, and
/// [`crate::repo::service::create_repo`] writes a bare skeleton — `HEAD`,
/// `refs/`, `objects/` — before returning. `git clone --bare` refuses a
/// destination that exists, so "does the path exist" cannot be the question.
/// Asking it (as a `HEAD`-file check) made the clone a no-op for precisely the
/// scenario the feature exists for: the import returned success having never
/// spawned `git`, and the user was handed the empty skeleton back.
///
/// So the question is whether the target holds any *history*, and the clone
/// lands beside it and is moved into place. A clone that dies half-way leaves
/// the skeleton — and the repository the row points at — untouched.
///
/// ## The token
///
/// The source's token never enters `source_url` and never enters argv — it is
/// handed to the subprocess through the environment by
/// [`credential_invocation`]. A URL with the token in it would be copied
/// verbatim by git into the new repository's `remote.origin.url`, leaving a
/// plaintext PAT on disk long after the import finished, and would show up in
/// `ps`, in the gateway's error text, and in its trace span.
fn clone_repo(
    source_url: &str,
    repo_root: &Path,
    owner: &str,
    name: &str,
    credentials: Option<&GitCredentials>,
) -> Result<CloneOutcome> {
    let target_dir = repo_root.join(format!("{}/{}.git", owner, name));
    if target_holds_history(&target_dir) {
        tracing::info!(
            path = %target_dir.display(),
            "Import target already holds a repository, skipping clone"
        );
        return Ok(CloneOutcome::Skipped);
    }

    let parent = target_dir
        .parent()
        .context("import target path has no parent directory")?;
    // The whole path is derived from `repo_root` inside this function, so a bare
    // `?` here hands the operator an `os error 13` that names neither the
    // directory the import tried to create nor the setting that moves it.
    std::fs::create_dir_all(parent)
        .map_err(|error| path_error("import target directory", parent, &error, REPO_ROOT_HINT))?;

    // A per-pass token, so neither working path can collide with the target,
    // with a repository sitting next to it, or with another pass importing the
    // same name. The leading dot only keeps them out of the way visually.
    let token = uuid::Uuid::new_v4().simple().to_string();
    let staging = parent.join(format!(".{name}.git.importing-{token}"));
    let retired = parent.join(format!(".{name}.git.replaced-{token}"));

    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let invocation = credential_invocation(credentials);
    let destination = staging.to_string_lossy();
    let cloned = invocation
        .run(git, &["clone", "--bare", source_url, &destination], None)
        .and_then(|output| output.ensure_success().context("git clone --bare"));
    if let Err(error) = cloned {
        discard_partial_clone(&staging);
        return Err(error);
    }

    if let Err(error) = install_clone(&staging, &retired, &target_dir) {
        discard_partial_clone(&staging);
        return Err(error);
    }

    tracing::info!(path = %target_dir.display(), "Repository cloned");
    Ok(CloneOutcome::Cloned)
}

// ═══════════════════════════════════════════════════════════════════════
// Wiki import
// ═══════════════════════════════════════════════════════════════════════

/// The file extensions an imported wiki page may carry.
///
/// Both platforms keep a wiki as a gollum repository, and gollum renders
/// textile, rdoc, org, creole and rst besides Markdown. A ForgeKeep wiki page
/// is Markdown — that is what `wiki_pages.content` holds and what the page view
/// renders — so a page written in one of the others would be stored as text
/// this wiki then renders as something it is not. Those are counted and named
/// in a warning rather than carried in under a format they are not written in.
const WIKI_PAGE_EXTENSIONS: [&str; 2] = ["md", "markdown"];

/// Page formats gollum renders and a ForgeKeep wiki page cannot hold.
///
/// Only used to tell "this repository holds pages we left behind" from "this
/// repository holds the images its pages link to", which is the ordinary case
/// and not worth a word.
const FOREIGN_WIKI_PAGE_EXTENSIONS: [&str; 9] = [
    "adoc",
    "asciidoc",
    "creole",
    "mediawiki",
    "org",
    "pod",
    "rdoc",
    "rst",
    "textile",
];

/// The largest wiki page this import reads into memory and into a row.
///
/// A wiki repository also holds whatever its authors attached to the pages, and
/// `ls-tree -l` reports each object's size before anything is read — so an
/// oversized page is named in a warning instead of travelling through the
/// import process's heap.
const MAX_WIKI_PAGE_BYTES: u64 = 1024 * 1024;

/// The wiki repository that sits beside a source repository.
///
/// GitHub and GitLab both publish a repository's wiki as a *second* git
/// repository one path suffix away — `<repo>.wiki.git`, same host, same
/// credential — and neither serves page content through its REST API. So a wiki
/// is imported the way a repository is: by cloning it.
///
/// Derived from the source URL the import was accepted for, which [`run_import`]
/// has already put through [`crate::net::guard_git_url`]. Only the path changes
/// here, so the host that guard approved is the host this clone reaches.
fn wiki_clone_url(source_url: &str) -> String {
    let base = source_url.trim_end_matches('/').trim_end_matches(".git");
    format!("{base}.wiki.git")
}

/// Where a wiki clone is staged: beside the repository it belongs to, under a
/// per-pass name, so two passes over one repository cannot share it and neither
/// can collide with `<name>.git` itself. Mirrors [`clone_repo`]'s staging.
fn wiki_staging_dir(repo_root: &Path, owner: &str, name: &str) -> PathBuf {
    let token = uuid::Uuid::new_v4().simple().to_string();
    repo_root.join(format!("{owner}/.{name}.wiki.git.importing-{token}"))
}

/// A page as the source wiki holds it.
struct SourceWikiPage {
    title: String,
    content: String,
}

/// The title a wiki file is served under, or `None` when the file is not a page.
///
/// A page named `Foo Bar` is stored by both platforms as `Foo-Bar.md` and
/// served at `/wiki/Foo-Bar`; `wiki_pages.title` is that same slug — it is what
/// `/{owner}/{repo}/wiki/{title}` matches on. So the file's stem is kept
/// verbatim rather than un-hyphenated into a display title: every `[[Foo-Bar]]`
/// already written inside the imported pages keeps naming a page that exists.
///
/// A page in a subdirectory keeps its stem alone. The title is one URL segment
/// and the unique key of a page within a repository, with nowhere to put the
/// directory — so two pages that flatten onto one title are reported by the
/// caller, never merged.
fn wiki_page_title(path: &str) -> Option<&str> {
    let file_name = path.rsplit('/').next()?;
    let (stem, extension) = file_name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    WIKI_PAGE_EXTENSIONS
        .iter()
        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        .then_some(stem)
}

/// Whether a file is a wiki page this import cannot carry, as opposed to an
/// image or attachment the pages link to.
fn is_foreign_wiki_page(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .and_then(|file_name| file_name.rsplit_once('.'))
        .is_some_and(|(stem, extension)| {
            !stem.is_empty()
                && FOREIGN_WIKI_PAGE_EXTENSIONS
                    .iter()
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

/// Read the Markdown pages a cloned wiki repository holds at `HEAD`.
///
/// A wiki that exists but was never written to is not an error: the platform
/// hands out a repository with no commit in it, and an unborn `HEAD` has no
/// tree to list.
fn collect_wiki_pages(staging: &Path) -> Result<Vec<SourceWikiPage>> {
    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    if rg_git::ref_advertisement::collect(staging)
        .context("read the cloned source wiki")?
        .head_oid
        .is_none()
    {
        return Ok(Vec::new());
    }

    let listing = git.run(&["ls-tree", "-r", "-l", "-z", "HEAD"], Some(staging))?;
    listing
        .ensure_success()
        .context("list the source wiki's pages")?;

    let mut pages = Vec::new();
    let mut foreign = Vec::new();
    for record in listing.stdout.split(|byte| *byte == 0) {
        // `<mode> SP <type> SP <oid> SP <size> TAB <path>`: `-z` turns off the
        // quoting that would otherwise mangle a path, and `-l` reports the size
        // before the blob is read.
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let (meta, path) = record.split_at(tab);
        let meta = String::from_utf8_lossy(meta);
        let mut fields = meta.split_whitespace();
        let (Some(_mode), Some(kind), Some(_oid), Some(size)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if kind != "blob" {
            continue;
        }

        let Ok(path) = std::str::from_utf8(&path[1..]) else {
            tracing::warn!(
                "a file of the source wiki has a path that is not UTF-8 and was not imported"
            );
            continue;
        };
        let Some(title) = wiki_page_title(path) else {
            if is_foreign_wiki_page(path) {
                foreign.push(path.to_string());
            }
            continue;
        };

        // An unparseable size is treated as too large: the guard exists so that
        // nothing unbounded is read, and a field we cannot read is not a reason
        // to read one.
        if size.parse::<u64>().unwrap_or(u64::MAX) > MAX_WIKI_PAGE_BYTES {
            tracing::warn!(
                path,
                size,
                limit = MAX_WIKI_PAGE_BYTES,
                "a source wiki page is larger than an import carries and was not imported"
            );
            continue;
        }

        // `cat-file blob` rather than `show`: the bytes as committed, with no
        // filter of the host's able to rewrite them on the way out.
        let blob = git.run(
            &["cat-file", "blob", &format!("HEAD:{path}")],
            Some(staging),
        )?;
        blob.ensure_success()
            .with_context(|| format!("read the source wiki page {path}"))?;
        let Ok(content) = String::from_utf8(blob.stdout) else {
            tracing::warn!(
                path,
                "a source wiki page is not UTF-8 text and was not imported"
            );
            continue;
        };

        pages.push(SourceWikiPage {
            title: title.to_string(),
            content,
        });
    }

    if !foreign.is_empty() {
        tracing::warn!(
            count = foreign.len(),
            pages = %foreign.join(", "),
            "the source wiki holds pages written in a markup a ForgeKeep wiki page does not \
             render — they were not imported"
        );
    }

    Ok(pages)
}

/// Render a wiki clone failure for the log with the source token taken back out
/// of it — the same last-resort net [`failure_reason`] is, for the same reason:
/// the token reaches `git` and the remote, so it can come back inside their
/// error text.
fn wiki_failure_reason(error: &anyhow::Error, credentials: Option<&GitCredentials>) -> String {
    let reason = crate::net::mask_url_credentials(&format!("{error:#}"));
    match credentials.map(|credentials| credentials.password().to_string()) {
        Some(token) => crate::auth::encryption::mask_values(&reason, &[token]),
        None => reason,
    }
}

/// Clone a source wiki and create its pages in `repo_id`'s ForgeKeep wiki.
/// Returns the number of pages created.
///
/// `staging` is a directory that does not exist yet: the wiki is cloned into it
/// and it is removed again before this returns, on every path. It is the
/// caller's to choose because the only place an import may write is the
/// repository root it was handed.
///
/// ## A source that has no wiki
///
/// This is the one step of an import whose failure is not fatal. Both platforms
/// answer a wiki that was never written with a plain clone failure, and there is
/// no field to ask beforehand: GitHub's `has_wiki` is true for every repository
/// whose wiki feature is merely *enabled*, which is the default, so it says
/// nothing about whether `<repo>.wiki.git` exists. Failing the pass over that
/// would mean ticking the box breaks the import of every repository that never
/// wrote a wiki page — including the repository itself, which by then is already
/// on disk. So a clone that does not come back is a warning naming the reason,
/// and the import finishes reporting zero wiki pages.
///
/// Everything after the clone stays fatal: a wiki we did clone and then could
/// not read, or could not write into the database, is our failure and is
/// reported as one.
pub async fn import_wiki_pages(
    db: &DatabaseConnection,
    repo_id: i64,
    wiki_url: &str,
    staging: &Path,
    credentials: Option<&GitCredentials>,
    author_id: Option<i64>,
) -> Result<usize> {
    let parent = staging
        .parent()
        .context("wiki staging path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| path_error("wiki import directory", parent, &error, REPO_ROOT_HINT))?;

    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let invocation = credential_invocation(credentials);
    let destination = staging.to_string_lossy();
    // `--depth 1`: only the pages as they stand are imported. A ForgeKeep wiki
    // keeps its own revision history from the first edit onwards, and there is
    // nowhere to put the source's.
    let cloned = invocation
        .run(
            git,
            &["clone", "--bare", "--depth", "1", wiki_url, &destination],
            None,
        )
        .and_then(|output| output.ensure_success().context("git clone --bare (wiki)"));
    if let Err(error) = cloned {
        discard_partial_clone(staging);
        tracing::warn!(
            repo_id,
            reason = %wiki_failure_reason(&error, credentials),
            "the source wiki could not be cloned — the import carried no wiki pages"
        );
        return Ok(0);
    }

    // Read first, then put the clone away whatever the read did: the staging
    // directory is unreferenced bytes under the repository root from the moment
    // the pages are in hand.
    let pages = collect_wiki_pages(staging);
    discard_partial_clone(staging);
    let pages = pages?;

    let mut imported = 0usize;
    for page in &pages {
        match crate::wiki::service::create_page(
            db,
            repo_id,
            &page.title,
            &page.content,
            Some("Imported from the source wiki"),
            author_id,
        )
        .await
        {
            Ok(_) => imported += 1,
            // The target already holds a page under that title — either one of
            // its own, or a second source file that flattens onto the same
            // title. Neither is a reason to overwrite what is there, and both
            // are worth naming.
            Err(error) if error.downcast_ref::<crate::error::Conflict>().is_some() => {
                tracing::warn!(
                    repo_id,
                    title = %page.title,
                    "the target repository already holds a wiki page under this title — the \
                     source page was not imported over it"
                );
            }
            Err(error) => {
                return Err(error).with_context(|| format!("import the wiki page '{}'", page.title))
            }
        }
    }

    Ok(imported)
}

// ═══════════════════════════════════════════════════════════════════════
// URL parsing
// ═══════════════════════════════════════════════════════════════════════

struct GitHubImportSource {
    owner: String,
    repo: String,
    api_base_url: String,
}

struct GitLabImportSource {
    project_path: String,
    api_base_url: String,
}

fn parse_github_url(raw: &str) -> Result<GitHubImportSource> {
    let source = parse_api_source_url(raw, "GitHub")?;
    let (owner, repo) = {
        let mut segments = source
            .path_segments()
            .ok_or_else(|| anyhow::anyhow!("invalid GitHub URL: repository path is missing"))?
            .filter(|segment| !segment.is_empty());
        let repo = segments
            .next_back()
            .map(|segment| segment.strip_suffix(".git").unwrap_or(segment))
            .filter(|segment| !segment.is_empty())
            .ok_or_else(|| anyhow::anyhow!("invalid GitHub URL: repository name is missing"))?;
        let owner = segments
            .next_back()
            .filter(|segment| !segment.is_empty())
            .ok_or_else(|| anyhow::anyhow!("invalid GitHub URL: repository owner is missing"))?;
        (owner.to_string(), repo.to_string())
    };

    let api_base_url = if source.host_str().is_some_and(|host| {
        host.trim_end_matches('.')
            .eq_ignore_ascii_case("github.com")
    }) {
        "https://api.github.com".to_string()
    } else {
        api_base_url(source, "/api/v3", "GitHub")?
    };

    Ok(GitHubImportSource {
        owner,
        repo,
        api_base_url,
    })
}

fn parse_gitlab_url(raw: &str) -> Result<GitLabImportSource> {
    let source = parse_api_source_url(raw, "GitLab")?;
    let project_path = {
        let mut segments = source
            .path_segments()
            .ok_or_else(|| anyhow::anyhow!("invalid GitLab URL: project path is missing"))?
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let last = segments
            .pop()
            .map(|segment| segment.strip_suffix(".git").unwrap_or(segment))
            .filter(|segment| !segment.is_empty())
            .ok_or_else(|| anyhow::anyhow!("invalid GitLab URL: project path is missing"))?;
        segments.push(last);
        segments.join("/")
    };
    let api_base_url = api_base_url(source, "/api/v4", "GitLab")?;

    Ok(GitLabImportSource {
        project_path,
        api_base_url,
    })
}

fn parse_api_source_url(raw: &str, platform: &str) -> Result<reqwest::Url> {
    let source = reqwest::Url::parse(raw).with_context(|| format!("invalid {platform} URL"))?;
    if !matches!(source.scheme(), "http" | "https") {
        anyhow::bail!(
            "invalid {platform} URL: scheme '{}' cannot identify an HTTP API host",
            source.scheme()
        );
    }
    if source.host_str().is_none() {
        anyhow::bail!("invalid {platform} URL: host is missing");
    }
    Ok(source)
}

fn api_base_url(mut source: reqwest::Url, path: &str, platform: &str) -> Result<String> {
    source.set_username("").map_err(|()| {
        anyhow::anyhow!("invalid {platform} URL: user information cannot be removed")
    })?;
    source.set_password(None).map_err(|()| {
        anyhow::anyhow!("invalid {platform} URL: user information cannot be removed")
    })?;
    source.set_path(path);
    source.set_query(None);
    source.set_fragment(None);
    Ok(source.to_string().trim_end_matches('/').to_string())
}

#[cfg(test)]
mod import_source_url_tests {
    use super::*;

    #[test]
    fn github_com_uses_the_public_api_host() {
        let source = parse_github_url("https://github.com/acme/widgets.git/")
            .expect("a GitHub repository URL");

        assert_eq!(source.owner, "acme");
        assert_eq!(source.repo, "widgets");
        assert_eq!(source.api_base_url, "https://api.github.com");
    }

    #[test]
    fn ghes_uses_the_source_origin_and_never_its_userinfo() {
        let source = parse_github_url(
            "http://git@github.acme.example:8443/acme/widgets.git?view=source#readme",
        )
        .expect("a GHES repository URL");

        assert_eq!(source.owner, "acme");
        assert_eq!(source.repo, "widgets");
        assert_eq!(
            source.api_base_url,
            "http://github.acme.example:8443/api/v3"
        );
    }

    #[test]
    fn self_hosted_gitlab_uses_the_source_origin_and_nested_project_path() {
        let source = parse_gitlab_url(
            "https://git@gitlab.acme.example:9443/teams/platform/widgets.git?ref=main#readme",
        )
        .expect("a self-hosted GitLab repository URL");

        assert_eq!(source.project_path, "teams/platform/widgets");
        assert_eq!(
            source.api_base_url,
            "https://gitlab.acme.example:9443/api/v4"
        );
    }

    #[test]
    fn gitlab_com_uses_its_v4_api() {
        let source = parse_gitlab_url("https://gitlab.com/acme/widgets")
            .expect("a GitLab.com repository URL");

        assert_eq!(source.project_path, "acme/widgets");
        assert_eq!(source.api_base_url, "https://gitlab.com/api/v4");
    }

    #[test]
    fn api_backed_imports_reject_a_non_http_source() {
        let error = parse_github_url("git://github.example/acme/widgets.git")
            .err()
            .expect("a git transport cannot identify an HTTP API endpoint");

        assert!(format!("{error:#}").contains("cannot identify an HTTP API host"));
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Date/time helpers
// ═══════════════════════════════════════════════════════════════════════

fn parse_opt_datetime(s: &Option<String>) -> Option<chrono::DateTime<Utc>> {
    s.as_ref().and_then(|v| {
        chrono::DateTime::parse_from_rfc3339(v)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    })
}

fn parse_datetime_or_now(s: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

// ═══════════════════════════════════════════════════════════════════════
// Import helpers — GitHub
// ═══════════════════════════════════════════════════════════════════════

async fn import_github_labels(
    db: &DatabaseConnection,
    repo_id: i64,
    labels: &[GitHubLabel],
) -> Result<usize> {
    let existing = label_ops::list_by_repo(db, repo_id).await?;
    let now = Utc::now();
    let mut count = 0;

    for gl in labels {
        // Skip if label with same name already exists
        if existing.iter().any(|l| l.name == gl.name) {
            continue;
        }
        // GitHub colors are "ff0000" without # prefix
        let color = if gl.color.starts_with('#') {
            gl.color.clone()
        } else {
            format!("#{}", gl.color)
        };

        let model = label::ActiveModel {
            id: sea_orm::NotSet, // sea_orm NotSet via Default
            repo_id: Set(repo_id),
            name: Set(gl.name.clone()),
            color: Set(color),
            description: Set(gl.description.clone()),
            created_at: Set(now),
            updated_at: Set(now),
        };

        if let Err(e) = label_ops::create(db, model).await {
            tracing::warn!(label = %gl.name, error = %format!("{e:#}"), "failed to create label");
        } else {
            count += 1;
        }
    }

    Ok(count)
}

async fn import_github_milestones(
    db: &DatabaseConnection,
    repo_id: i64,
    milestones: &[GitHubMilestone],
    milestone_map: &mut HashMap<String, i64>,
) -> Result<usize> {
    let existing = milestone_ops::list_by_repo(db, repo_id, None).await?;
    let now = Utc::now();
    let mut count = 0;

    for gm in milestones {
        // Skip if milestone with same title already exists
        if existing.iter().any(|m| m.title == gm.title) {
            continue;
        }

        // A foreign vocabulary, so an unknown word falls back rather than
        // failing the import — but what lands in the column is named by the
        // type that owns it, not by a literal (card_09b2665584ed).
        let state = match gm.state.as_str() {
            "closed" => crate::issue::MilestoneState::Closed,
            _ => crate::issue::MilestoneState::Open,
        };

        let due_date = parse_opt_datetime(&gm.due_on);

        let model = milestone::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            title: Set(gm.title.clone()),
            description: Set(gm.description.clone()),
            state: Set(state.as_str().to_string()),
            due_date: Set(due_date),
            created_at: Set(now),
            updated_at: Set(now),
        };

        match milestone_ops::create(db, model).await {
            Ok(ms) => {
                milestone_map.insert(gm.title.clone(), ms.id);
                count += 1;
            }
            Err(e) => {
                tracing::warn!(milestone = %gm.title, error = %format!("{e:#}"), "failed to create milestone");
            }
        }
    }

    Ok(count)
}

#[allow(clippy::too_many_arguments)]
async fn import_github_issue(
    db: &DatabaseConnection,
    repo_id: i64,
    _target_owner: &str,
    _target_name: &str,
    issue: &GitHubIssue,
    comments: &[GitHubComment],
    author_id: i64,
    milestone_map: &HashMap<String, i64>,
    label_map: Option<&HashMap<String, i64>>,
) -> Result<()> {
    // Collect label names
    let label_names: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
    let label_ids = resolve_imported_label_ids(label_map, &label_names)?;

    // Resolve milestone
    let milestone_id = issue
        .milestone
        .as_ref()
        .and_then(|m| milestone_map.get(&m.title))
        .copied();

    let created_at = parse_datetime_or_now(&issue.created_at);
    let closed_at = parse_opt_datetime(&issue.closed_at);
    let state = if issue.state == "closed" {
        "closed"
    } else {
        "open"
    };

    let model = issue::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo_id),
        // Allocated against the UNIQUE key by `create_imported_issue`.
        number: sea_orm::NotSet,
        title: Set(issue.title.clone()),
        body: Set(issue.body.clone()),
        state: Set(state.to_string()),
        author_id: Set(author_id),
        assignee_id: Set(None),
        milestone_id: Set(milestone_id),
        created_at: Set(created_at),
        updated_at: Set(parse_datetime_or_now(&issue.updated_at)),
        closed_at: Set(closed_at),
        deleted_at: Set(None),
    };

    let saved = create_imported_issue(db, repo_id, model, label_ids).await?;

    // Import comments
    for comment in comments {
        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id),
            author_id: Set(author_id),
            body: Set(comment.body.clone().unwrap_or_default()),
            created_at: Set(parse_datetime_or_now(&comment.created_at)),
            updated_at: Set(parse_datetime_or_now(&comment.updated_at)),
        };

        if let Err(e) = issue_comment_ops::create(db, cm).await {
            tracing::warn!(issue_number = %issue.number, error = %format!("{e:#}"), "failed to import issue comment");
        }
    }

    Ok(())
}

async fn import_github_pr(
    db: &DatabaseConnection,
    repo_id: i64,
    pr: &GitHubPR,
    comments: &[GitHubComment],
    reviews: &[GitHubReview],
    author_id: i64,
    milestone_map: &HashMap<String, i64>,
) -> Result<()> {
    let label_names: Vec<String> = pr.labels.iter().map(|l| l.name.clone()).collect();
    let labels_json = if label_names.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&label_names).unwrap_or_else(|_| "[]".into()))
    };

    let milestone_id = pr
        .milestone
        .as_ref()
        .and_then(|m| milestone_map.get(&m.title))
        .copied();

    let state = if pr.merged.unwrap_or(false) {
        "merged"
    } else if pr.state == "closed" {
        "closed"
    } else {
        "open"
    };

    let model = rg_db::entities::pull_request::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo_id),
        // Allocated against the UNIQUE key by the insert below.
        number: sea_orm::NotSet,
        title: Set(pr.title.clone()),
        body: Set(pr.body.clone()),
        state: Set(state.to_string()),
        is_draft: Set(pr.draft),
        auto_merge_enabled: Set(false),
        auto_merge_strategy: Set(None),
        auto_merge_enabled_by_id: Set(None),
        auto_merge_enabled_at: Set(None),
        author_id: Set(author_id),
        reviewer_id: Set(None),
        head_branch: Set(pr.head.ref_name.clone()),
        base_branch: Set(pr.base.ref_name.clone()),
        head_sha: Set(Some(pr.head.sha.clone())),
        merge_strategy: Set(None),
        merge_commit_sha: Set(None),
        head_repo_id: Set(None),
        ci_approved_sha: Set(None),
        ci_approved_by: Set(None),
        ci_approved_at: Set(None),
        milestone_id: Set(milestone_id),
        labels: Set(labels_json),
        created_at: Set(parse_datetime_or_now(&pr.created_at)),
        updated_at: Set(parse_datetime_or_now(&pr.updated_at)),
        closed_at: Set(parse_opt_datetime(&pr.closed_at)),
        merged_at: Set(parse_opt_datetime(&pr.merged_at)),
    };

    let saved = crate::pull_request::service::insert_with_repo_number(db, repo_id, model).await?;

    // Import PR comments (general discussion)
    for comment in comments {
        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id), // issue_id is PR id in this context
            author_id: Set(author_id),
            body: Set(comment.body.clone().unwrap_or_default()),
            created_at: Set(parse_datetime_or_now(&comment.created_at)),
            updated_at: Set(parse_datetime_or_now(&comment.updated_at)),
        };

        if let Err(e) = issue_comment_ops::create(db, cm).await {
            tracing::warn!(pr_number = %pr.number, error = %format!("{e:#}"), "failed to import PR comment");
        }
    }

    // Import reviews
    for review in reviews {
        let action = match review.state.as_str() {
            "APPROVED" => "approve",
            "CHANGES_REQUESTED" => "request_changes",
            "COMMENTED" => "comment",
            "DISMISSED" => "dismiss",
            _ => "comment",
        };

        let rv = rg_db::entities::pr_review::ActiveModel {
            id: sea_orm::NotSet,
            pr_id: Set(saved.id),
            repo_id: Set(repo_id),
            reviewer_id: Set(author_id),
            action: Set(action.to_string()),
            body: Set(review.body.clone()),
            commit_id: Set(None),
            created_at: Set(parse_datetime_or_now(
                review.submitted_at.as_deref().unwrap_or(""),
            )),
        };

        if let Err(e) = pr_review_ops::create(db, rv).await {
            tracing::warn!(pr_number = %pr.number, error = %format!("{e:#}"), "failed to import PR review");
        }
    }

    Ok(())
}

async fn import_github_releases(
    db: &DatabaseConnection,
    repo_id: i64,
    releases: &[GitHubRelease],
    repo_root: &Path,
) -> Result<usize> {
    let mut count = 0;

    for release in releases {
        let author_id = 1; // GitHub releases API doesn't expose author in the list endpoint
                           // In a full implementation, we'd fetch release details

        let title = release
            .name
            .clone()
            .unwrap_or_else(|| release.tag_name.clone());
        let is_draft = release.draft;
        let is_prerelease = release.prerelease;

        match crate::release::service::create_release(
            db,
            repo_id,
            author_id,
            &release.tag_name,
            &title,
            release.body.as_deref(),
            &release.tag_name, // target_commitish defaults to tag
            is_draft,
            is_prerelease,
            repo_root,
        )
        .await
        {
            Ok(_) => count += 1,
            Err(e) => {
                tracing::warn!(tag = %release.tag_name, error = %format!("{e:#}"), "failed to import release");
            }
        }
    }

    Ok(count)
}

// ═══════════════════════════════════════════════════════════════════════
// Import helpers — GitLab
// ═══════════════════════════════════════════════════════════════════════

async fn import_gitlab_labels(
    db: &DatabaseConnection,
    repo_id: i64,
    labels: &[GitLabLabel],
) -> Result<usize> {
    let existing = label_ops::list_by_repo(db, repo_id).await?;
    let now = Utc::now();
    let mut count = 0;

    for gl in labels {
        if existing.iter().any(|l| l.name == gl.name) {
            continue;
        }
        // GitLab colors are "#FF0000" with # prefix already
        let color = if gl.color.starts_with('#') {
            gl.color.clone()
        } else {
            format!("#{}", gl.color)
        };

        let model = label::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            name: Set(gl.name.clone()),
            color: Set(color),
            description: Set(gl.description.clone()),
            created_at: Set(now),
            updated_at: Set(now),
        };

        if let Err(e) = label_ops::create(db, model).await {
            tracing::warn!(label = %gl.name, error = %format!("{e:#}"), "failed to create label");
        } else {
            count += 1;
        }
    }

    Ok(count)
}

async fn import_gitlab_milestones(
    db: &DatabaseConnection,
    repo_id: i64,
    milestones: &[GitLabMilestone],
    milestone_map: &mut HashMap<String, i64>,
) -> Result<usize> {
    let existing = milestone_ops::list_by_repo(db, repo_id, None).await?;
    let now = Utc::now();
    let mut count = 0;

    for gm in milestones {
        if existing.iter().any(|m| m.title == gm.title) {
            continue;
        }

        // GitLab uses "active" instead of "open"; same fallback rule as the
        // GitHub importer above.
        let state = match gm.state.as_str() {
            "closed" => crate::issue::MilestoneState::Closed,
            _ => crate::issue::MilestoneState::Open,
        };

        let due_date = parse_opt_datetime(&gm.due_date);

        let model = milestone::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            title: Set(gm.title.clone()),
            description: Set(gm.description.clone()),
            state: Set(state.as_str().to_string()),
            due_date: Set(due_date),
            created_at: Set(now),
            updated_at: Set(now),
        };

        match milestone_ops::create(db, model).await {
            Ok(ms) => {
                milestone_map.insert(gm.title.clone(), ms.id);
                count += 1;
            }
            Err(e) => {
                tracing::warn!(milestone = %gm.title, error = %format!("{e:#}"), "failed to create milestone");
            }
        }
    }

    Ok(count)
}

#[allow(clippy::too_many_arguments)]
async fn import_gitlab_issue(
    db: &DatabaseConnection,
    repo_id: i64,
    _target_owner: &str,
    _target_name: &str,
    issue: &GitLabIssue,
    notes: &[GitLabNote],
    author_id: i64,
    milestone_map: &HashMap<String, i64>,
    label_map: Option<&HashMap<String, i64>>,
) -> Result<()> {
    // GitLab labels are plain strings.
    let label_ids = resolve_imported_label_ids(label_map, &issue.labels)?;

    let milestone_id = issue
        .milestone
        .as_ref()
        .and_then(|m| milestone_map.get(&m.title))
        .copied();

    let state = if issue.state == "closed" {
        "closed"
    } else {
        "open"
    };

    let model = issue::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo_id),
        // Allocated against the UNIQUE key by `create_imported_issue`.
        number: sea_orm::NotSet,
        title: Set(issue.title.clone()),
        body: Set(issue.description.clone()),
        state: Set(state.to_string()),
        author_id: Set(author_id),
        assignee_id: Set(None),
        milestone_id: Set(milestone_id),
        created_at: Set(parse_datetime_or_now(&issue.created_at)),
        updated_at: Set(parse_datetime_or_now(&issue.updated_at)),
        closed_at: Set(parse_opt_datetime(&issue.closed_at)),
        deleted_at: Set(None),
    };

    let saved = create_imported_issue(db, repo_id, model, label_ids).await?;

    // Import notes (skip system notes)
    for note in notes {
        if note.system {
            continue;
        }
        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id),
            author_id: Set(author_id),
            body: Set(note.body.clone().unwrap_or_default()),
            created_at: Set(parse_datetime_or_now(&note.created_at)),
            updated_at: Set(parse_datetime_or_now(&note.updated_at)),
        };

        if let Err(e) = issue_comment_ops::create(db, cm).await {
            tracing::warn!(issue_iid = %issue.iid, error = %format!("{e:#}"), "failed to import issue note");
        }
    }

    Ok(())
}

async fn import_gitlab_mr(
    db: &DatabaseConnection,
    repo_id: i64,
    mr: &GitLabMR,
    notes: &[GitLabNote],
    author_id: i64,
    milestone_map: &HashMap<String, i64>,
) -> Result<()> {
    let labels_json = if mr.labels.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&mr.labels).unwrap_or_else(|_| "[]".into()))
    };

    let milestone_id = mr
        .milestone
        .as_ref()
        .and_then(|m| milestone_map.get(&m.title))
        .copied();

    let state = if mr.merged_at.is_some() {
        "merged"
    } else if mr.state == "closed" {
        "closed"
    } else {
        "open"
    };

    let model = rg_db::entities::pull_request::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo_id),
        // Allocated against the UNIQUE key by the insert below.
        number: sea_orm::NotSet,
        title: Set(mr.title.clone()),
        body: Set(mr.description.clone()),
        state: Set(state.to_string()),
        is_draft: Set(mr.draft),
        auto_merge_enabled: Set(false),
        auto_merge_strategy: Set(None),
        auto_merge_enabled_by_id: Set(None),
        auto_merge_enabled_at: Set(None),
        author_id: Set(author_id),
        reviewer_id: Set(None),
        head_branch: Set(mr.source_branch.clone()),
        base_branch: Set(mr.target_branch.clone()),
        head_sha: Set(None),
        merge_strategy: Set(None),
        merge_commit_sha: Set(None),
        head_repo_id: Set(mr.source_project_id),
        ci_approved_sha: Set(None),
        ci_approved_by: Set(None),
        ci_approved_at: Set(None),
        milestone_id: Set(milestone_id),
        labels: Set(labels_json),
        created_at: Set(parse_datetime_or_now(&mr.created_at)),
        updated_at: Set(parse_datetime_or_now(&mr.updated_at)),
        closed_at: Set(parse_opt_datetime(&mr.closed_at)),
        merged_at: Set(parse_opt_datetime(&mr.merged_at)),
    };

    let saved = crate::pull_request::service::insert_with_repo_number(db, repo_id, model).await?;

    // Import MR notes (skip system notes)
    for note in notes {
        if note.system {
            continue;
        }
        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id),
            author_id: Set(author_id),
            body: Set(note.body.clone().unwrap_or_default()),
            created_at: Set(parse_datetime_or_now(&note.created_at)),
            updated_at: Set(parse_datetime_or_now(&note.updated_at)),
        };

        if let Err(e) = issue_comment_ops::create(db, cm).await {
            tracing::warn!(mr_iid = %mr.iid, error = %format!("{e:#}"), "failed to import MR note");
        }
    }

    Ok(())
}

async fn import_gitlab_releases(
    db: &DatabaseConnection,
    repo_id: i64,
    releases: &[GitLabRelease],
    repo_root: &Path,
) -> Result<usize> {
    let mut count = 0;

    for release in releases {
        let title = release
            .name
            .clone()
            .unwrap_or_else(|| release.tag_name.clone());

        match crate::release::service::create_release(
            db,
            repo_id,
            1, // GitLab releases don't expose author in list; default to admin
            &release.tag_name,
            &title,
            release.description.as_deref(),
            &release.tag_name,
            false, // is_draft — GitLab doesn't have drafts
            false, // is_prerelease — could parse from tag name, simplified
            repo_root,
        )
        .await
        {
            Ok(_) => count += 1,
            Err(e) => {
                tracing::warn!(tag = %release.tag_name, error = %format!("{e:#}"), "failed to import release");
            }
        }
    }

    Ok(count)
}

// ═══════════════════════════════════════════════════════════════════════
// Progress helpers
// ═══════════════════════════════════════════════════════════════════════

async fn update_stage(
    db: &DatabaseConnection,
    task_id: i64,
    status: &str,
    progress: i32,
    stage: &str,
) -> Result<()> {
    import_task_ops::update_progress(db, task_id, status, progress, Some(stage)).await?;
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════
// Public API: start import task
// ═══════════════════════════════════════════════════════════════════════

/// Render an import failure for the `error` column (and the log line), with the
/// source token taken back out of it.
///
/// The token is handed to `git` and to the platform's HTTP API, so it can come
/// back inside their error text — a clone URL echoed by the git gateway, an API
/// error quoting the request. That text is persisted on the task and served to
/// the user on every status poll, which is exactly the path this module refuses
/// to put the token on. Masking is the same last-resort net `mirror::service`
/// puts in front of `last_sync_error`.
///
/// The URL userinfo is masked as well: a task row written before the
/// create-time split still carries `user:token@` in its source URL, and the
/// message quoting it is the same message.
fn failure_reason(error: &anyhow::Error, auth_token: Option<&str>) -> String {
    let reason = crate::net::mask_url_credentials(&format!("{error:#}"));
    match auth_token.filter(|token| !token.is_empty()) {
        Some(token) => crate::auth::encryption::mask_values(&reason, &[token.to_string()]),
        None => reason,
    }
}

/// Create a new import task and start the background import process.
///
/// `auth_token` is handed to the spawned worker and to nothing else: it is not
/// written to the task row, so it cannot outlive the import nor come back out
/// of a status response. See the module note.
#[allow(clippy::too_many_arguments)]
pub async fn start_import(
    db: &DatabaseConnection,
    workers: &ImportWorkerRegistry,
    user_id: i64,
    platform: String,
    source_url: String,
    target_owner: String,
    target_name: String,
    auth_token: Option<String>,
    import_repo: bool,
    import_issues: bool,
    import_pull_requests: bool,
    import_wiki: bool,
    import_releases: bool,
    import_labels: bool,
    import_milestones: bool,
    trusted_origins: &crate::import::trust::TrustedImportOrigins,
    repo_root: &Path,
) -> Result<ImportTask> {
    let now = Utc::now();
    let supports_metadata = matches!(platform.as_str(), "github" | "gitlab");

    // A token pasted into the URL is a token, not part of the address: take it
    // out before the row is written, and run the import with it. The login half
    // stays in the URL — `import_tasks` has no column for it, and it is what
    // `git` pairs the token with (see the module note).
    let source = crate::net::split_url_credentials(&source_url).context("invalid source URL")?;
    let source_url = source.url_without_secret;
    let auth_token = auth_token
        .filter(|token| !token.is_empty())
        .or(source.password);

    // Anchor the task to the repository it was accepted for, when that
    // repository already exists. Two things follow from writing it here rather
    // than after the worker's own lookup: the deletion quiescence gate can see
    // the import before it has resolved anything, and the worker knows it is
    // continuing an import into an *existing* repository, so a target that
    // disappears mid-flight stops the pass instead of being re-created under
    // the name the deletion just freed (card_a3ce6a2363a7).
    //
    // A lookup failure fails the request: an import that starts unanchored is
    // one this gate cannot see.
    let existing_target =
        crate::repo::service::find_repo_by_owner_name(db, &target_owner, &target_name)
            .await
            .context("failed to look up the import target repository")?;

    let model = import_task::ActiveModel {
        user_id: Set(user_id),
        repo_id: Set(existing_target.map(|repo| repo.id)),
        platform: Set(platform),
        source_url: Set(source_url),
        target_owner: Set(target_owner),
        target_name: Set(target_name),
        status: Set("pending".to_string()),
        progress: Set(0),
        stage: Set(None),
        error: Set(None),
        import_repo: Set(import_repo),
        import_issues: Set(supports_metadata && import_issues),
        import_pull_requests: Set(supports_metadata && import_pull_requests),
        import_wiki: Set(supports_metadata && import_wiki),
        import_releases: Set(supports_metadata && import_releases),
        import_labels: Set(supports_metadata && import_labels),
        import_milestones: Set(supports_metadata && import_milestones),
        stats: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let task = import_task_ops::create(db, model).await?;
    let task_clone = task.clone();

    // Spawn the background task behind the lifecycle registry. The worker owns
    // its terminal writes too: cancellation drops the whole future, so DELETE
    // never removes the row underneath a late `mark_completed`/`mark_failed`.
    let db_clone = db.clone();
    let repo_root_clone = repo_root.to_path_buf();
    let trusted_origins = trusted_origins.clone();
    if let Err(error) = workers.spawn(task.id, async move {
        // These two are the last writes the task will ever get — there is no
        // caller left to notice a failure and no later pass that revisits the
        // row. Losing one leaves the task in `running` forever, which the UI
        // renders as an import that never finishes.
        match run_import(
            &db_clone,
            &task_clone,
            &repo_root_clone,
            auth_token.as_deref(),
            &trusted_origins,
        )
        .await
        {
            Ok(stats) => {
                let stats_json = serde_json::to_string(&stats).unwrap_or_default();
                if let Err(error) =
                    import_task_ops::mark_completed(&db_clone, task_clone.id, &stats_json).await
                {
                    tracing::error!(
                        task_id = task_clone.id,
                        error = %format!("{error:#}"),
                        "import finished but could not be marked completed; \
                         the task is stuck in `running`"
                    );
                }
            }
            Err(e) => {
                let reason = failure_reason(&e, auth_token.as_deref());
                tracing::warn!(task_id = task_clone.id, reason, "import failed");
                if let Err(error) =
                    import_task_ops::mark_failed(&db_clone, task_clone.id, &reason).await
                {
                    tracing::error!(
                        task_id = task_clone.id,
                        error = %format!("{error:#}"),
                        "import failed and could not be marked failed either; \
                         the task is stuck in `running` and its reason is lost"
                    );
                }
            }
        }
    }) {
        if let Err(cleanup_error) = import_task_ops::delete_by_id(db, task.id).await {
            tracing::error!(
                task_id = task.id,
                error = %format!("{cleanup_error:#}"),
                "import worker registration failed and its task row could not be removed"
            );
        }
        return Err(error.context("failed to register import worker"));
    }

    // Re-fetch to get the persisted record
    import_task_ops::find_by_id(db, task.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found after creation"))
}

/// Take a token back out of every `import_tasks.source_url` that still has one.
///
/// The twin of `mirror::service::lift_legacy_url_credentials`, run from the
/// same point in the boot — but there is nowhere here to *lift* the secret to:
/// this table deliberately has no credential column (see the module note), and
/// an import is one-shot, so a token in a finished task's URL is a copy that
/// has already outlived its purpose. It is dropped, and the login half of the
/// userinfo stays. Returns how many rows it rewrote.
///
/// Idempotent: a URL with no password in it is left alone.
pub async fn strip_legacy_source_url_credentials(db: &DatabaseConnection) -> Result<usize> {
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    };

    let mut pages = import_task::Entity::find()
        .filter(import_task::Column::SourceUrl.contains("@"))
        .order_by_asc(import_task::Column::Id)
        .paginate(db, 500);

    let mut stripped = 0_usize;
    while let Some(tasks) = pages
        .fetch_and_next()
        .await
        .context("read import source URLs")?
    {
        for task in tasks {
            let lifted = crate::net::strip_url_credentials(&task.source_url);
            if lifted.password.is_none() {
                // An `@` in the path, an scp-like remote, or a bare login — no
                // secret to take out.
                continue;
            }
            let id = task.id;
            let mut model: import_task::ActiveModel = task.into();
            model.source_url = Set(lifted.url_without_secret);
            model
                .update(db)
                .await
                .with_context(|| format!("rewrite the source URL of import task {id}"))?;
            tracing::warn!(
                task_id = id,
                "import task {id} carried a token in its source URL; it was removed — re-run \
                 the import with the token in its own field if it is still needed"
            );
            stripped += 1;
        }
    }

    if stripped > 0 {
        tracing::info!(count = stripped, "removed tokens from import source URLs");
    }
    Ok(stripped)
}

#[cfg(test)]
mod imported_issue_label_tests {
    use super::*;
    use rg_db::ops::{issue_label_ops, issue_ops};
    use sea_orm::{ConnectionTrait, Database, Statement};

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// The account these fixtures import as. Imported content is attributed to
    /// the importer and to nobody else — see the module docs.
    const IMPORTER_ID: i64 = 1;

    async fn test_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(1, 'importer', 'importer@example.com', 'x', 1, 1, '2024-01-01', '2024-01-01')"
                .to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO repositories(id, owner_id, name, is_private, default_branch, stars_count, forks_count, created_at, updated_at) \
             VALUES(1, 1, 'imported', 0, 'main', 0, 0, '2024-01-01', '2024-01-01')"
                .to_string(),
        ))
        .await
        .unwrap();
        db
    }

    async fn create_label(db: &DatabaseConnection, id: i64, name: &str) -> label::Model {
        label_ops::create(
            db,
            label::ActiveModel {
                id: Set(id),
                repo_id: Set(1),
                name: Set(name.to_string()),
                color: Set("#ee0701".to_string()),
                description: Set(None),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
            },
        )
        .await
        .unwrap()
    }

    fn github_user(id: i64, login: &str) -> crate::import::github_client::GitHubUser {
        crate::import::github_client::GitHubUser {
            id,
            login: login.to_string(),
            email: None,
            avatar_url: None,
            user_type: None,
        }
    }

    fn github_issue(label: GitHubLabel) -> GitHubIssue {
        GitHubIssue {
            number: 41,
            title: "GitHub labelled issue".to_string(),
            body: None,
            state: "open".to_string(),
            labels: vec![label],
            milestone: None,
            user: None,
            assignees: Vec::new(),
            comments: 0,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            closed_at: None,
            pull_request: None,
        }
    }

    fn gitlab_issue(label: &str) -> GitLabIssue {
        GitLabIssue {
            id: 52,
            iid: 52,
            title: "GitLab labelled issue".to_string(),
            description: None,
            state: "opened".to_string(),
            labels: vec![label.to_string()],
            milestone: None,
            author: None,
            assignees: Vec::new(),
            user_notes_count: 0,
            created_at: "2024-01-02T00:00:00Z".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
            closed_at: None,
            merge_request_count: None,
            has_tasks: None,
        }
    }

    /// card_dd6ae4f40206: imported content belongs to the account that
    /// imported it, and to that account only.
    ///
    /// What this replaces was worse than a missing feature. The author was
    /// looked up in a "user map" built by `map_users`, whose entire body was
    /// `mapping.entry(login).or_insert(task.user_id)` — so every login resolved
    /// to the importer anyway — and a login the walk had not collected fell
    /// through to a hardcoded `1`. On an instance where account 1 is not an
    /// admin, or not the importer, or has been deleted, that is an issue filed
    /// under a name that never wrote it.
    #[tokio::test]
    async fn imported_issues_and_their_comments_belong_to_the_importer() {
        let db = test_db().await;
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(7, 'second', 'second@example.com', 'x', 0, 1, '2024-01-01', '2024-01-01')"
                .to_string(),
        ))
        .await
        .unwrap();

        // The payload names a source-platform author, and it is *not* the
        // importer. Nothing about that login may reach the stored rows.
        let mut issue = github_issue(GitHubLabel {
            id: 10,
            name: "bug".to_string(),
            color: "ee0701".to_string(),
            description: None,
        });
        issue.user = Some(github_user(999, "importer"));

        let comments = vec![GitHubComment {
            id: 1,
            body: Some("from somebody else entirely".to_string()),
            user: Some(github_user(1000, "stranger")),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        }];

        import_github_issue(
            &db,
            1,
            "importer",
            "imported",
            &issue,
            &comments,
            7,
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

        let imported = issue_ops::find_by_repo_and_number(&db, 1, 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            imported.author_id, 7,
            "the issue belongs to the account that ran the import, not to a \
             login that merely matches one"
        );

        let stored_comments = rg_db::ops::issue_comment_ops::list_by_issue(&db, imported.id)
            .await
            .unwrap();
        assert_eq!(stored_comments.len(), 1);
        assert_eq!(
            stored_comments[0].author_id, 7,
            "a comment must not fall through to user id 1 either"
        );
    }

    /// The source's issues and merge requests are listed **once**, by the step
    /// that imports them.
    ///
    /// A second listing call is exactly how this went wrong: `build_github_user_map`
    /// and `build_gitlab_user_map` walked those lists before the import steps to
    /// collect logins for a map that was a constant. The GitLab one did it with
    /// no flag at all, so importing nothing but labels paginated the whole
    /// issue list of the source project and spent its rate limit on it.
    ///
    /// A count rather than a block-structure check on purpose: the defect's
    /// shape is an *extra* call site, and a count says so without pretending to
    /// parse Rust with a line scanner.
    fn source_list_call_count(source: &str, method: &str) -> usize {
        let name = format!("client.{method}");
        rust_source::production_call_sites(source, &[&name]).len()
    }

    fn source_list_contract(source: &str) -> Result<(), String> {
        for (method, expected, step) in [
            ("list_issues", 2, "one GitHub step and one GitLab step"),
            ("list_pull_requests", 1, "the GitHub step"),
            ("list_merge_requests", 1, "the GitLab step"),
        ] {
            let name = format!("client.{method}");
            let calls = source_list_call_count(source, method);
            if calls != expected {
                return Err(format!(
                    "{name}( appears {calls} times in production; it belongs to {step} and \
                     nowhere else. Listing the source's issues or merge requests outside the \
                     step that imports them is how a label-only import came to paginate the \
                     entire issue list of the source project."
                ));
            }
        }
        Ok(())
    }

    fn without_one_production_list_call(source: &str, method: &str) -> String {
        let name = format!("client.{method}");
        let call = rust_source::production_call_sites(source, &[&name])
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("mutation target `{name}` must exist"));
        let name_at = source[..call.open_paren]
            .rfind(&name)
            .expect("call name must precede its opening parenthesis");
        let mut mutated = source.to_owned();
        mutated.replace_range(name_at..name_at + name.len(), &" ".repeat(name.len()));
        mutated
    }

    #[test]
    fn the_source_issue_and_merge_request_lists_are_fetched_once() {
        let source = include_str!("service.rs");
        source_list_contract(source).unwrap_or_else(|error| panic!("{error}"));

        for method in ["list_issues", "list_pull_requests", "list_merge_requests"] {
            let mutated = without_one_production_list_call(source, method);
            assert!(
                source_list_contract(&mutated).is_err(),
                "removing one production `client.{method}` call must fail the census"
            );
        }
    }

    #[test]
    fn source_list_census_ignores_non_code_and_test_only_decoys() {
        const SOURCE: &str = r####"
fn import() {
    // client.list_issues();
    /* client.list_issues(); */
    let normal = "client.list_issues()";
    let raw = r#"client.list_issues()"#;
    let bytes = b"client.list_issues()";
    let raw_bytes = br##"client.list_issues()"##;
    client.list_issues();
}

#[cfg(test)]
mod tests {
    fn decoy() {
        client.list_issues();
    }
}
"####;

        assert_eq!(source_list_call_count(SOURCE, "list_issues"), 1);
    }

    /// Both platform paths must populate the store read by label filtering and
    /// by `GET /issues/{number}/labels`, not a response-only copy.
    #[tokio::test]
    async fn github_and_gitlab_imports_write_the_canonical_label_junction() {
        let db = test_db().await;
        let bug = create_label(&db, 10, "bug").await;
        let triage = create_label(&db, 11, "triage").await;
        let label_map = load_label_map(&db, 1).await.unwrap();
        let empty_milestones = HashMap::new();

        import_github_issue(
            &db,
            1,
            "importer",
            "imported",
            &github_issue(GitHubLabel {
                id: 10,
                name: bug.name.clone(),
                color: "ee0701".to_string(),
                description: None,
            }),
            &[],
            IMPORTER_ID,
            &empty_milestones,
            Some(&label_map),
        )
        .await
        .unwrap();
        import_gitlab_issue(
            &db,
            1,
            "importer",
            "imported",
            &gitlab_issue(&triage.name),
            &[],
            IMPORTER_ID,
            &empty_milestones,
            Some(&label_map),
        )
        .await
        .unwrap();

        for label in [&bug, &triage] {
            let (issue_ids, total) =
                issue_label_ops::find_issues_with_all_labels(&db, 1, &[label.id], None, 0, 10)
                    .await
                    .unwrap();
            assert_eq!(
                total, 1,
                "filter did not find imported label {}",
                label.name
            );
            assert_eq!(issue_ids.len(), 1);

            let labels = crate::label::service::get_issue_labels(&db, issue_ids[0])
                .await
                .unwrap();
            assert_eq!(labels.len(), 1);
            assert_eq!(labels[0].name, label.name);
        }
    }

    #[tokio::test]
    async fn disabling_label_import_does_not_smuggle_labels_in_through_issues() {
        let db = test_db().await;
        let bug = create_label(&db, 10, "bug").await;

        import_github_issue(
            &db,
            1,
            "importer",
            "imported",
            &github_issue(GitHubLabel {
                id: 10,
                name: bug.name,
                color: "ee0701".to_string(),
                description: None,
            }),
            &[],
            IMPORTER_ID,
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

        let imported = issue_ops::find_by_repo_and_number(&db, 1, 1)
            .await
            .unwrap()
            .unwrap();
        assert!(crate::label::service::get_issue_labels(&db, imported.id)
            .await
            .unwrap()
            .is_empty());
    }
}

#[cfg(test)]
mod target_repo_resolution_tests {
    use super::*;

    /// The first lookup is the branch decision: its failure must not be
    /// rewritten as `None` and followed by the owner-resolution/create path.
    ///
    /// A closed real SQLite pool exercises the production query stack. The
    /// first-lookup context is the distinguishing assertion: the old
    /// `.unwrap_or(None)` implementation swallowed it, retried the owner query,
    /// and returned that later error instead.
    #[tokio::test]
    async fn a_failed_existing_repo_lookup_does_not_enter_the_create_branch() {
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("open SQLite pool");
        let closed_db = db.clone();
        db.close().await.expect("close SQLite pool");
        let repo_root = tempfile::tempdir().expect("temporary repository root");

        let error =
            resolve_or_create_target_repo(&closed_db, None, "alice", "widgets", repo_root.path())
                .await
                .expect_err("the failed existence check must abort target resolution");

        assert_eq!(
            error.to_string(),
            "failed to look up existing target repository"
        );
        assert!(
            !repo_root.path().join("alice/widgets.git").exists(),
            "a failed existence check entered the repository creation branch"
        );
    }
}

/// card_a3ce6a2363a7: an import is a detached worker that resolves its target
/// once and reaches `git` later. Deleting the repository underneath it must not
/// leave the upstream cloned back under `<owner>/<name>.git` — the canonical
/// name the next repository of that name will claim.
///
/// The two guards are asserted separately because they close different windows:
/// the anchor stops the pass from resolving a *second* time and re-creating the
/// target, and the pre-`git` recheck stops the clone when the repository goes
/// away after resolution. The third test is the one that keeps the pair honest
/// — an import accepted for a name that never existed still creates it.
#[cfg(test)]
mod import_target_lifecycle_tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, Statement};

    /// One user, `importer`, owning one repository, `importer/imported`.
    async fn lifecycle_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("open SQLite pool");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(1, 'importer', 'importer@example.com', 'x', 0, 1, '2024-01-01 00:00:00', '2024-01-01 00:00:00')"
                .to_string(),
        ))
        .await
        .expect("seed the importing user");
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO repositories(id, owner_id, name, is_private, default_branch, stars_count, forks_count, created_at, updated_at) \
             VALUES(1, 1, 'imported', 0, 'main', 0, 0, '2024-01-01 00:00:00', '2024-01-01 00:00:00')"
                .to_string(),
        ))
        .await
        .expect("seed the target repository");
        db
    }

    async fn running_task(db: &DatabaseConnection, repo_id: Option<i64>) -> ImportTask {
        let now = Utc::now();
        import_task_ops::create(
            db,
            import_task::ActiveModel {
                user_id: Set(1),
                repo_id: Set(repo_id),
                platform: Set("git".to_string()),
                source_url: Set("https://example.invalid/importer/imported.git".to_string()),
                target_owner: Set("importer".to_string()),
                target_name: Set("imported".to_string()),
                status: Set("cloning".to_string()),
                progress: Set(0),
                stage: Set(None),
                error: Set(None),
                import_repo: Set(true),
                import_issues: Set(false),
                import_pull_requests: Set(false),
                import_wiki: Set(false),
                import_releases: Set(false),
                import_labels: Set(false),
                import_milestones: Set(false),
                stats: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .expect("seed the import task")
    }

    #[tokio::test]
    async fn an_anchored_import_does_not_recreate_a_target_deleted_after_it_started() {
        let db = lifecycle_db().await;
        let repo_root = tempfile::tempdir().expect("temporary repository root");
        rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(
            &db,
            1,
            Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
        )
        .await
        .expect("delete the target repository");

        let error =
            resolve_or_create_target_repo(&db, Some(1), "importer", "imported", repo_root.path())
                .await
                .expect_err("an anchored import must not resolve its target a second time");

        assert!(
            error
                .to_string()
                .contains("was deleted after this import started"),
            "the pass failed for some other reason: {error:#}"
        );
        assert!(
            !repo_root.path().join("importer").exists(),
            "the import rebuilt the namespace the deletion retired"
        );
    }

    #[tokio::test]
    async fn the_clone_step_refuses_a_target_deleted_between_resolution_and_git() {
        let db = lifecycle_db().await;
        let repo_root = tempfile::tempdir().expect("temporary repository root");
        let task = running_task(&db, Some(1)).await;
        rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(
            &db,
            1,
            Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
        )
        .await
        .expect("delete the target repository mid-pass");

        let mut stats = ImportStats::default();
        let error = clone_into_target(
            &db,
            &task,
            1,
            "https://example.invalid/importer/imported.git",
            repo_root.path(),
            "",
            90,
            &mut stats,
        )
        .await
        .expect_err("the clone must not run for a repository that no longer exists");

        assert!(
            error
                .to_string()
                .contains("was deleted while this import was running"),
            "the clone step failed for some other reason: {error:#}"
        );
        assert!(
            !stats.repo_cloned,
            "the pass reported a clone it never made"
        );
        // `clone_repo` creates the namespace directory before it spawns `git`,
        // so an untouched repository root is the evidence that the recheck ran
        // *before* the subprocess and not merely that the bogus URL failed.
        assert!(
            !repo_root.path().join("importer").exists(),
            "the clone reached `git` for a repository that no longer exists"
        );
    }

    /// The anchor only closes the window if it is written when the import is
    /// accepted: a task that first learns its `repo_id` from its own worker has
    /// nothing to compare against by the time the target could be gone.
    ///
    /// `file://` is refused by the worker's SSRF guard before it resolves
    /// anything, so the detached pass cannot race the row this test reads.
    #[tokio::test]
    async fn start_import_anchors_the_task_to_a_target_that_already_exists() {
        let db = lifecycle_db().await;
        let repo_root = tempfile::tempdir().expect("temporary repository root");

        let existing = start_import(
            &db,
            &Default::default(),
            1,
            "git".to_string(),
            "file:///srv/upstream.git".to_string(),
            "importer".to_string(),
            "imported".to_string(),
            None,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            &Default::default(),
            repo_root.path(),
        )
        .await
        .expect("start an import into an existing repository");
        assert_eq!(
            existing.repo_id,
            Some(1),
            "the import was accepted unanchored, so no later check can tell \
             'the target is gone' from 'it was never there'"
        );

        let fresh = start_import(
            &db,
            &Default::default(),
            1,
            "git".to_string(),
            "file:///srv/upstream.git".to_string(),
            "importer".to_string(),
            "brand-new".to_string(),
            None,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            &Default::default(),
            repo_root.path(),
        )
        .await
        .expect("start an import into a name that does not exist yet");
        assert_eq!(
            fresh.repo_id, None,
            "an import accepted for a name that does not exist yet must stay \
             free to create it"
        );
    }

    #[tokio::test]
    async fn an_unanchored_import_still_creates_the_target_it_was_accepted_for() {
        let db = lifecycle_db().await;
        let repo_root = tempfile::tempdir().expect("temporary repository root");

        let repo_id =
            resolve_or_create_target_repo(&db, None, "importer", "fresh", repo_root.path())
                .await
                .expect("an import accepted for a name that does not exist yet creates it");

        assert_ne!(
            repo_id, 1,
            "a new name must not resolve to the existing repo"
        );
        assert!(
            repo_root.path().join("importer/fresh.git/HEAD").exists(),
            "the create branch no longer initialises the target repository"
        );
    }
}

/// card_309ae53b1f5a: the import's whole reason for existing is that the
/// upstream's objects end up in the target repository. Every test above this
/// one asserts that a clone *did not* run; these assert that it did, against a
/// real local bare source rather than a mock, because the defect was that no
/// `git` ever ran and a mock would have hidden exactly that.
#[cfg(test)]
mod clone_effect_tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, Statement};

    fn git(args: &[&str], cwd: Option<&Path>) {
        global_gateway()
            .as_ref()
            .expect("git gateway")
            .run(args, cwd)
            .expect("run git")
            .ensure_success()
            .expect("git command succeeds");
    }

    /// A bare repository holding one commit on `main`, standing in for the
    /// upstream an import is pointed at.
    fn upstream_with_a_commit(bare_path: &Path) -> String {
        upstream_with_a_commit_on(bare_path, "main")
    }

    /// The same fixture on a named default branch. Most upstreams are on
    /// `main`, which is also what `create_repo` writes into the row — so a
    /// fixture that only ever uses `main` cannot tell a column that tracks the
    /// clone from one that never moved.
    fn upstream_with_a_commit_on(bare_path: &Path, branch: &str) -> String {
        let bare_arg = bare_path.to_str().expect("UTF-8 bare path");
        git(&["init", "-q", "--bare", "-b", branch, bare_arg], None);

        let worktree = tempfile::tempdir().expect("upstream worktree");
        let path = worktree.path();
        let path_arg = path.to_str().expect("UTF-8 worktree path");
        git(&["init", "-q", "-b", branch, path_arg], None);
        git(&["config", "user.name", "Import fixture"], Some(path));
        git(
            &["config", "user.email", "import-fixture@example.invalid"],
            Some(path),
        );
        std::fs::write(path.join("README.md"), "upstream\n").expect("write upstream file");
        git(&["add", "."], Some(path));
        git(&["commit", "-qm", "upstream commit"], Some(path));
        git(&["remote", "add", "origin", bare_arg], Some(path));
        git(&["push", "-q", "origin", branch], Some(path));

        let head = global_gateway()
            .as_ref()
            .expect("git gateway")
            .run(&["rev-parse", "HEAD"], Some(path))
            .expect("read the upstream commit");
        head.ensure_success().expect("rev-parse succeeds");
        head.stdout_str().trim().to_string()
    }

    async fn importing_user() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("open SQLite pool");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(1, 'importer', 'importer@example.com', 'x', 0, 1, '2024-01-01 00:00:00', '2024-01-01 00:00:00')"
                .to_string(),
        ))
        .await
        .expect("seed the importing user");
        db
    }

    async fn task_for(db: &DatabaseConnection, source_url: &str, target_name: &str) -> ImportTask {
        let now = Utc::now();
        import_task_ops::create(
            db,
            import_task::ActiveModel {
                user_id: Set(1),
                repo_id: Set(None),
                platform: Set("git".to_string()),
                source_url: Set(source_url.to_string()),
                target_owner: Set("importer".to_string()),
                target_name: Set(target_name.to_string()),
                status: Set("cloning".to_string()),
                progress: Set(0),
                stage: Set(None),
                error: Set(None),
                import_repo: Set(true),
                import_issues: Set(false),
                import_pull_requests: Set(false),
                import_wiki: Set(false),
                import_releases: Set(false),
                import_labels: Set(false),
                import_milestones: Set(false),
                stats: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .expect("seed the import task")
    }

    /// The main import path end to end: a target name that does not exist yet,
    /// which the pass creates itself and then has to clone into. This is the
    /// scenario the `HEAD`-file check turned into a no-op — the pass returned
    /// success, `repo_cloned: true`, and an empty bare skeleton.
    #[tokio::test]
    async fn an_import_that_creates_its_own_target_still_transfers_the_upstream() {
        let directory = tempfile::tempdir().expect("tempdir");
        let upstream = directory.path().join("upstream.git");
        let expected_commit = upstream_with_a_commit(&upstream);
        let source_url = upstream.to_string_lossy().to_string();

        let db = importing_user().await;
        let repo_root = directory.path().join("repo_root");
        let task = task_for(&db, &source_url, "fresh").await;

        let repo_id = resolve_or_create_target_repo(&db, None, "importer", "fresh", &repo_root)
            .await
            .expect("the import creates the target it was accepted for");

        let mut stats = ImportStats::default();
        clone_into_target(
            &db,
            &task,
            repo_id,
            &source_url,
            &repo_root,
            "",
            90,
            &mut stats,
        )
        .await
        .expect("the clone runs");

        assert!(
            stats.repo_cloned,
            "the pass cloned the upstream but reported that it had not"
        );

        let target = repo_root.join("importer/fresh.git");
        let advertisement =
            rg_git::ref_advertisement::collect(&target).expect("read the imported repository");
        assert_eq!(
            advertisement.head_oid.as_deref(),
            Some(expected_commit.as_str()),
            "the import left a repository without the upstream's commit — refs: {:?}",
            advertisement.refs
        );

        // The staging and retired working paths are internal; leaving either
        // behind would be unreferenced bytes under the repository root.
        let leftovers: Vec<_> = std::fs::read_dir(repo_root.join("importer"))
            .expect("read the namespace directory")
            .map(|entry| entry.expect("read a directory entry").file_name())
            .filter(|name| name != "fresh.git")
            .collect();
        assert!(
            leftovers.is_empty(),
            "the import left working directories behind: {leftovers:?}"
        );
    }

    /// The other half of the honesty requirement: a target that already holds a
    /// repository is left alone, and the pass says so instead of claiming a
    /// clone. Nothing above this test would notice `repo_cloned` being wired to
    /// a constant `true` again.
    #[tokio::test]
    async fn a_target_that_already_holds_a_repository_is_not_cloned_over() {
        let directory = tempfile::tempdir().expect("tempdir");
        let upstream = directory.path().join("upstream.git");
        upstream_with_a_commit(&upstream);
        let source_url = upstream.to_string_lossy().to_string();

        let db = importing_user().await;
        let repo_root = directory.path().join("repo_root");
        let task = task_for(&db, &source_url, "occupied").await;

        let repo_id = resolve_or_create_target_repo(&db, None, "importer", "occupied", &repo_root)
            .await
            .expect("create the target");

        // Give the target a history of its own, so it is no longer the empty
        // skeleton the creation left.
        let target = repo_root.join("importer/occupied.git");
        let existing = upstream_with_a_commit(&directory.path().join("existing.git"));
        git(
            &[
                "fetch",
                "-q",
                &directory.path().join("existing.git").to_string_lossy(),
                "main:refs/heads/main",
            ],
            Some(&target),
        );

        let mut stats = ImportStats::default();
        clone_into_target(
            &db,
            &task,
            repo_id,
            &source_url,
            &repo_root,
            "",
            90,
            &mut stats,
        )
        .await
        .expect("the pass completes");

        assert!(
            !stats.repo_cloned,
            "the pass skipped the clone but reported one"
        );
        let advertisement =
            rg_git::ref_advertisement::collect(&target).expect("read the target repository");
        assert_eq!(
            advertisement.refs,
            vec![(existing, "refs/heads/main".to_string())],
            "the import overwrote a repository that already had a history"
        );
    }

    /// card_0e4d6e7fcdb2: the clone replaces `HEAD` along with everything else,
    /// so the column that names the default branch has to follow it. Left
    /// behind, it says `main` for a repository whose only branch is `master`,
    /// and the repository's own page resolves that name and answers `404`.
    #[tokio::test]
    async fn an_import_adopts_the_upstreams_default_branch() {
        let directory = tempfile::tempdir().expect("tempdir");
        let upstream = directory.path().join("upstream.git");
        upstream_with_a_commit_on(&upstream, "master");
        let source_url = upstream.to_string_lossy().to_string();

        let db = importing_user().await;
        let repo_root = directory.path().join("repo_root");
        let task = task_for(&db, &source_url, "legacy").await;

        let repo_id = resolve_or_create_target_repo(&db, None, "importer", "legacy", &repo_root)
            .await
            .expect("the import creates the target it was accepted for");
        assert_eq!(
            rg_db::ops::repo_ops::find_by_id(&db, repo_id)
                .await
                .expect("read the fresh row")
                .expect("the target exists")
                .default_branch,
            "main",
            "the fixture no longer starts from the mismatch this test is about"
        );

        let mut stats = ImportStats::default();
        clone_into_target(
            &db,
            &task,
            repo_id,
            &source_url,
            &repo_root,
            "",
            90,
            &mut stats,
        )
        .await
        .expect("the clone runs");

        let target = repo_root.join("importer/legacy.git");
        let advertisement =
            rg_git::ref_advertisement::collect(&target).expect("read the imported repository");
        assert_eq!(
            advertisement.head_target.as_deref(),
            Some("refs/heads/master"),
            "the clone did not bring the upstream's HEAD, so this test proves nothing"
        );
        assert_eq!(
            rg_db::ops::repo_ops::find_by_id(&db, repo_id)
                .await
                .expect("read the imported row")
                .expect("the target still exists")
                .default_branch,
            "master",
            "the row still names the branch the target was created with — the repository page \
             will resolve a ref that does not exist"
        );
    }

    /// The other edge: a clone that was skipped changed nothing on disk, so the
    /// column must keep describing the history the target already had. Adopting
    /// unconditionally would rewrite it from an upstream that was never pulled.
    #[tokio::test]
    async fn a_skipped_clone_leaves_the_default_branch_alone() {
        let directory = tempfile::tempdir().expect("tempdir");
        let upstream = directory.path().join("upstream.git");
        upstream_with_a_commit_on(&upstream, "master");
        let source_url = upstream.to_string_lossy().to_string();

        let db = importing_user().await;
        let repo_root = directory.path().join("repo_root");
        let task = task_for(&db, &source_url, "occupied").await;

        let repo_id = resolve_or_create_target_repo(&db, None, "importer", "occupied", &repo_root)
            .await
            .expect("create the target");

        // A history of its own on `main`, which is what the row already says.
        let target = repo_root.join("importer/occupied.git");
        let existing_source = directory.path().join("existing.git");
        upstream_with_a_commit(&existing_source);
        git(
            &[
                "fetch",
                "-q",
                &existing_source.to_string_lossy(),
                "main:refs/heads/main",
            ],
            Some(&target),
        );

        let mut stats = ImportStats::default();
        clone_into_target(
            &db,
            &task,
            repo_id,
            &source_url,
            &repo_root,
            "",
            90,
            &mut stats,
        )
        .await
        .expect("the pass completes");

        assert!(!stats.repo_cloned, "the fixture stopped skipping the clone");
        assert_eq!(
            rg_db::ops::repo_ops::find_by_id(&db, repo_id)
                .await
                .expect("read the row")
                .expect("the target exists")
                .default_branch,
            "main",
            "a skipped clone rewrote the default branch from an upstream it never pulled"
        );
    }
}

#[cfg(test)]
mod clone_path_tests {
    use super::*;

    /// An import into an unusable `repo_root` is the failure an operator meets
    /// first, and until this test it reported the errno alone — the directory
    /// the clone tried to create was computed here and never left the function.
    #[test]
    fn clone_repo_names_the_directory_it_could_not_create() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file cannot host `<owner>/`, so `create_dir_all` fails
        // before any git subprocess is spawned.
        let repo_root = dir.path().join("repo_root");
        std::fs::write(&repo_root, b"not a directory").unwrap();

        let error = clone_repo(
            "https://example.invalid/alice/site.git",
            &repo_root,
            "alice",
            "site",
            None,
        )
        .expect_err("repo_root is a file");
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&repo_root.join("alice").display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("[server].repo_root"), "{rendered}");
    }
}

#[cfg(test)]
mod clone_credential_tests {
    use super::*;
    use crate::test_support::spawn_authenticating_remote;
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    const TOKEN: &str = "ghp-SECRET-TOKEN";

    /// A source with no token is public: offering git an empty password would
    /// turn an anonymous clone into a refused authenticated one.
    #[test]
    fn a_public_source_is_cloned_anonymously() {
        assert!(source_credentials("github", "https://github.com/o/r.git", "").is_none());
        assert!(source_credentials("gitlab", "https://gitlab.com/o/r.git", "").is_none());
    }

    /// Basic auth has nowhere to put a lone token, so each platform's
    /// placeholder username has to be the one that platform actually accepts.
    #[test]
    fn each_platform_gets_the_username_it_documents() {
        let github = source_credentials("github", "https://github.com/o/r.git", TOKEN)
            .expect("a token is a credential");
        assert_eq!(github.password(), TOKEN);
        assert_eq!(github.username(), Some("x-access-token"));

        let gitlab = source_credentials("gitlab", "https://gitlab.com/o/r.git", TOKEN)
            .expect("a token is a credential");
        assert_eq!(gitlab.username(), Some("oauth2"));
    }

    /// The acceptance check of card_64918e1184ff, on-disk half: git copies the
    /// URL it was handed verbatim into the clone's `remote.origin.url`. When the
    /// token rode inside that URL, the import left a plaintext PAT in
    /// `<repo>.git/config` — readable long after the import finished.
    #[test]
    fn the_token_is_absent_from_the_cloned_repository_config() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("source.git");
        let git = global_gateway().as_ref().expect("git");
        git.run_or_bail(&["init", "--bare", &source.to_string_lossy()], None)
            .expect("source repo");

        let repo_root = directory.path().join("repo_root");
        let credentials =
            source_credentials("github", "https://github.com/o/r.git", TOKEN).expect("a token");
        clone_repo(
            &source.to_string_lossy(),
            &repo_root,
            "alice",
            "site",
            Some(&credentials),
        )
        .expect("clone of a local source");

        let config = std::fs::read_to_string(repo_root.join("alice/site.git/config"))
            .expect("the clone has a config");
        assert!(
            !config.contains(TOKEN),
            "the token was written to the cloned repository's config:\n{config}"
        );
        assert!(
            config.contains(&source.to_string_lossy().to_string()),
            "the remote URL is missing entirely, so this test proves nothing:\n{config}"
        );
    }

    /// The acceptance check of card_64918e1184ff, authentication half: a private
    /// GitHub source must actually get as far as authenticating. The token used
    /// to be accepted by `clone_repo` as `_token` and ignored, so every private
    /// GitHub import answered 401 — the same "stored but unused" half the
    /// mirrors had.
    #[test]
    fn a_private_source_receives_the_supplied_token() {
        let (address, seen) = spawn_authenticating_remote();
        let directory = tempfile::tempdir().expect("tempdir");
        let credentials =
            source_credentials("github", "https://github.com/o/r.git", TOKEN).expect("a token");

        let outcome = clone_repo(
            &format!("http://{address}/upstream.git"),
            directory.path(),
            "alice",
            "site",
            Some(&credentials),
        );
        assert!(
            outcome.is_err(),
            "the stub remote refuses everyone — the clone cannot succeed"
        );

        let seen = seen.lock().expect("lock");
        let expected = format!(
            "Basic {}",
            STANDARD.encode(format!("x-access-token:{TOKEN}"))
        );
        assert!(
            seen.contains(&expected),
            "the remote never received the supplied token; it saw {seen:?}"
        );
    }

    /// An import runs with no terminal behind it: a source that asks for a login
    /// has to end the clone, not park it until the gateway's 120-second timeout
    /// while the user watches "Cloning…".
    ///
    /// Honest about its own reach: a test binary has no tty either, so this
    /// stays green even without `GIT_TERMINAL_PROMPT=0` — it guards the path,
    /// not the flag. The flag itself is pinned in
    /// `rg_git::credentials`, where removing it turns two tests red.
    #[test]
    fn an_authenticating_source_fails_fast_instead_of_waiting_for_a_login() {
        let (address, _seen) = spawn_authenticating_remote();
        let directory = tempfile::tempdir().expect("tempdir");

        let started = std::time::Instant::now();
        let outcome = clone_repo(
            &format!("http://{address}/upstream.git"),
            directory.path(),
            "alice",
            "site",
            None,
        );
        let elapsed = started.elapsed();

        assert!(outcome.is_err(), "the stub remote demands authentication");
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "the clone waited {elapsed:?} — that is a prompt, not a refusal"
        );
    }
}

#[cfg(test)]
mod failure_reason_tests {
    use super::*;

    /// The token no longer rides in the clone URL, but it still reaches the
    /// platform's API and `git`, and both quote what they were given back into
    /// their error text. Whatever it rode in on, it must not reach the `error`
    /// column — that column is served to the browser on every status poll.
    #[test]
    fn the_token_is_taken_back_out_of_a_failure() {
        let error = anyhow::anyhow!(
            "git clone --bare https://oauth2:glpat-SECRET-TOKEN@gitlab.com/a/b.git failed"
        );
        let reason = failure_reason(&error, Some("glpat-SECRET-TOKEN"));

        assert!(
            !reason.contains("glpat-SECRET-TOKEN"),
            "the source token survived into the persisted reason: {reason}"
        );
        // Masking, not swallowing: the operator still learns what failed.
        assert!(reason.contains("git clone"), "{reason}");
        assert!(reason.contains("gitlab.com/a/b.git"), "{reason}");
    }

    /// An anonymous import has nothing to mask, and its reason must come
    /// through untouched — a `***` there would be a mystery, not a redaction.
    #[test]
    fn an_anonymous_import_keeps_its_reason_verbatim() {
        let error = anyhow::anyhow!("repository not found");
        assert_eq!(failure_reason(&error, None), "repository not found");
        assert_eq!(failure_reason(&error, Some("")), "repository not found");
    }
}
