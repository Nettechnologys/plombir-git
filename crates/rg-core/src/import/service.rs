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
//! For the same reason a failure reason goes through
//! [`mask_source_credentials`] before it is persisted or logged: the token
//! reaches `git`/the platform API, so it can come back inside their error text.
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
    transport_policy: &crate::import::trust::ImportTransportPolicy,
) -> Result<ImportStats> {
    let mut stats = ImportStats::default();

    // Defense at the final shared boundary: no API client or git subprocess may
    // receive content or a credential until transport confidentiality has been
    // decided.
    // Admission checks repeat this for fast feedback, but detached workers and
    // future direct callers must remain fail-closed on their own.
    transport_policy.require_confidential_credentials(&task.source_url, auth_token)?;

    // Keep worker admission DNS-free. API destinations resolve inside their
    // reqwest connector, while every repository/wiki clone resolves and binds
    // its own URL immediately before the git subprocess. An early lookup here
    // would only recreate the check/use gap while metadata import runs.
    trusted_origins.check_url_static(&task.source_url)?;

    let auth_token = auth_token.unwrap_or("");

    match task.platform.as_str() {
        "github" => {
            run_github_import(db, task, repo_root, auth_token, &mut stats, trusted_origins).await?
        }
        "gitlab" => {
            run_gitlab_import(db, task, repo_root, auth_token, &mut stats, trusted_origins).await?
        }
        "gitea" | "git" => {
            run_git_import(db, task, repo_root, auth_token, &mut stats, trusted_origins).await?
        }
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
    trusted_origins: &crate::import::trust::TrustedImportOrigins,
    repo_root: &Path,
    token: &str,
    progress_when_done: i32,
    stats: &mut ImportStats,
) -> Result<()> {
    clone_into_target_with_destination(
        db,
        task,
        repo_id,
        trusted_origins.git_destination(clone_url),
        repo_root,
        token,
        progress_when_done,
        stats,
    )
    .await
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn clone_into_target_for_test(
    db: &DatabaseConnection,
    task: &ImportTask,
    repo_id: i64,
    clone_url: &str,
    repo_root: &Path,
    token: &str,
    progress_when_done: i32,
    stats: &mut ImportStats,
) -> Result<()> {
    clone_into_target_with_destination(
        db,
        task,
        repo_id,
        std::future::ready(Ok(crate::net::GuardedGitRemote::unbound_for_test(
            clone_url,
        ))),
        repo_root,
        token,
        progress_when_done,
        stats,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn clone_into_target_with_destination(
    db: &DatabaseConnection,
    task: &ImportTask,
    repo_id: i64,
    destination: impl Future<Output = Result<crate::net::GuardedGitRemote>>,
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

    // Awaited only after the lifecycle recheck, then consumed by the very next
    // network-capable step. Metadata/API work cannot age this DNS decision.
    let destination = destination.await?;
    let outcome = clone_repo(
        &destination,
        repo_root,
        &task.target_owner,
        &task.target_name,
        source_credentials(&task.platform, &task.source_url, token).as_ref(),
    )
    .await?;
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

/// How many branch names the unborn-HEAD warning is willing to spell out. The
/// list is a diagnosis, not a listing — the same cap the read side puts on the
/// `409` it answers for this repository (`DESYNC_BRANCH_SAMPLE`).
const UNBORN_BRANCH_SAMPLE: usize = 10;

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
///
/// An unborn `HEAD` over branches that *do* exist is a different repository
/// altogether. `git clone --bare` copies a symbolic `HEAD` verbatim, so an
/// upstream whose own `HEAD` names a branch nobody created arrives here with a
/// full history behind an unresolvable `HEAD`. Adopting is still wrong — there
/// is no branch to adopt, only a guess between the ones that exist — but the
/// silence was: the column keeps the created-with name, and every read of the
/// repository afterwards answers `409` from `classify_repo_emptiness`
/// (card_9e11f76dddd1) with nothing in the import log to explain why.
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

    if advertisement.head_oid.is_none() {
        let mut branches = advertisement
            .refs
            .iter()
            .filter_map(|(_, refname)| refname.strip_prefix("refs/heads/"))
            .collect::<Vec<_>>();
        // No branches either: the clone really does carry no history, which is
        // the one unborn state that leaves without a word.
        if branches.is_empty() {
            return;
        }
        branches.sort_unstable();
        let branch_count = branches.len();
        let overflow = branch_count.saturating_sub(UNBORN_BRANCH_SAMPLE);
        let mut sample = branches
            .into_iter()
            .take(UNBORN_BRANCH_SAMPLE)
            .collect::<Vec<_>>()
            .join(", ");
        if overflow > 0 {
            sample.push_str(&format!(" and {overflow} more"));
        }
        tracing::warn!(
            repo_id,
            path = %repo_path.display(),
            head_target = advertisement.head_target.as_deref().unwrap_or("HEAD"),
            branch_count,
            branches = %sample,
            recorded_branch,
            "the imported repository's HEAD names a branch the upstream never created — its \
             default branch column still names the branch the repository was created with, and \
             every read of the repository answers 409 until HEAD points at a branch that exists"
        );
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
    trusted_origins: &crate::import::trust::TrustedImportOrigins,
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
            trusted_origins,
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
    trusted_origins: &crate::import::trust::TrustedImportOrigins,
) -> Result<()> {
    // Parse the repository identity and its API host together. Computing only
    // owner/repo here used to leave the client's optional base URL at `None`,
    // which sent a GHES token and every metadata request to api.github.com.
    let GitHubImportSource {
        owner: gh_owner,
        repo: gh_repo,
        api_base_url,
    } = parse_github_url(&task.source_url)?;
    let api_destination = trusted_origins.api_destination(&api_base_url)?;
    let client = GitHubClient::new(token.to_string(), api_destination)?;

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
            trusted_origins,
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
            let imported_reviews = import_github_pr(
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
            stats.pr_reviews_imported += imported_reviews;
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
            trusted_origins,
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
    let api_destination = trusted_origins.api_destination(&api_base_url)?;
    let client = GitLabClient::new(token.to_string(), api_destination)?;

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
        clone_into_target(
            db,
            task,
            repo_id,
            &project.http_url_to_repo,
            trusted_origins,
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
            trusted_origins,
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

/// Remove the skeleton a finished install replaced.
///
/// Only ever called once the clone holds the target path, which is what makes
/// this a tombstone rather than a rollback point: nothing names these bytes any
/// more.
fn discard_retired_skeleton(retired: &Path) {
    if let Err(error) = std::fs::remove_dir_all(retired) {
        tracing::warn!(
            path = %retired.display(),
            error = %error,
            "the empty repository the import replaced could not be removed and is now \
             unreferenced bytes under the repository root"
        );
    }
}

/// An install that did not happen, and whether it left the target path whole.
///
/// The second half is what the caller needs and an `anyhow::Error` cannot
/// carry: a rollback that itself failed is the one outcome where the skeleton
/// is still parked under `retired` and the repository's row names a path with
/// nothing on it — exactly the state the recovery journal exists to finish.
struct FailedInstall {
    error: anyhow::Error,
    skeleton_restored: bool,
}

/// Move a finished clone onto the target path, retiring the skeleton the
/// repository's creation left there.
///
/// Two renames inside one directory rather than "remove the skeleton, then
/// rename": the skeleton is discarded only once the clone is in its place, so a
/// failure in between is undone instead of leaving the repository's row
/// pointing at a path with nothing on it. Both renames stay within `parent`, so
/// neither can fail for crossing a filesystem boundary.
///
/// `occupied` is decided by the caller rather than re-asked here, so the
/// journal entry that declares this move and the move itself cannot disagree
/// about whether there is a skeleton to put back. Discarding the retired
/// skeleton is the caller's too — that step is what the commit marker
/// authorizes, and it must not run before the marker is written.
fn install_clone(
    staging: &Path,
    retired: &Path,
    target_dir: &Path,
    occupied: bool,
) -> std::result::Result<(), FailedInstall> {
    if occupied {
        if let Err(error) = std::fs::rename(target_dir, retired) {
            return Err(FailedInstall {
                error: path_error(
                    "the empty repository the import replaces",
                    target_dir,
                    &error,
                    REPO_ROOT_HINT,
                ),
                // Nothing moved, so nothing needs putting back.
                skeleton_restored: true,
            });
        }
    }

    if let Err(error) = std::fs::rename(staging, target_dir) {
        let failure = path_error(
            "the imported repository",
            target_dir,
            &error,
            REPO_ROOT_HINT,
        );
        let mut skeleton_restored = true;
        if occupied {
            if let Err(restore) = std::fs::rename(retired, target_dir) {
                skeleton_restored = false;
                tracing::error!(
                    path = %target_dir.display(),
                    retired = %retired.display(),
                    error = %restore,
                    "the import could not install its clone and could not put the repository \
                     it moved aside back — the target path is empty and the row still names it; \
                     the startup recovery pass will put it back"
                );
            }
        }
        return Err(FailedInstall {
            error: failure,
            skeleton_restored,
        });
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
async fn clone_repo(
    remote: &crate::net::GuardedGitRemote,
    repo_root: &Path,
    owner: &str,
    name: &str,
    credentials: Option<&GitCredentials>,
) -> Result<CloneOutcome> {
    // Final sink guard. `run_import` owns the worker boundary, but keeping the
    // invariant here prevents a future direct caller or derived clone URL from
    // reintroducing native plaintext Git.
    crate::import::trust::ImportTransportPolicy::require_confidential_transport(remote.url())?;

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
    //
    // The staging name is spelled by `rg_core::staging` rather than here: the
    // clone below is the longest step this import has, and a stop during it
    // leaves a bare repository the size of the upstream that only the startup
    // sweep will ever look at again — and that sweep finds it by recognising
    // this name. The retired skeleton is deliberately not named there: it is
    // declared in the deletion-recovery journal, and that pass owns it.
    let pass = uuid::Uuid::new_v4();
    let token = pass.simple().to_string();
    let staging = parent.join(crate::staging::import_clone_staging_name(name, pass));
    let retired = parent.join(format!(".{name}.git.replaced-{token}"));

    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let invocation = remote.bind_invocation(credential_invocation(credentials))?;
    let destination = staging.to_string_lossy();
    let cloned = invocation
        .run(git, &["clone", "--bare", remote.url(), &destination], None)
        .and_then(|output| output.ensure_success().context("git clone --bare"));
    if let Err(error) = cloned {
        discard_partial_clone(&staging);
        return Err(error);
    }

    // The skeleton is about to leave its live name by rename, and a rename is
    // only reversible by the process that made it. Declared one statement
    // before the move so the *next* process can reverse it instead: killed
    // between the two renames below, the repository's row is live, names
    // `<owner>/<name>.git`, and there is nothing on that path — every Git
    // operation fails while the owner sees an ordinary repository in the list.
    // `recover_stuck_imports` does not close this: it fails the import task row
    // and touches neither the directory nor the repository.
    let journal = crate::deletion_recovery::journal_at(repo_root);
    let occupied = target_dir.exists();
    if occupied {
        let declared = match crate::deletion_recovery::StagedBytes::path(&target_dir, &retired) {
            Ok(staged) => {
                crate::deletion_recovery::open(
                    &journal,
                    &token,
                    "the repository skeleton an import replaced",
                    vec![staged],
                )
                .await
            }
            Err(error) => Err(error),
        };
        // A move we cannot record is a move we do not make — the same rule the
        // journal states for a deletion. The clone goes with it rather than
        // being left as bytes no row names and no sweep retires.
        if let Err(error) = declared {
            discard_partial_clone(&staging);
            return Err(error);
        }
    }

    if let Err(failed) = install_clone(&staging, &retired, &target_dir, occupied) {
        discard_partial_clone(&staging);
        // Closed only when the skeleton is actually back on the target path. A
        // rollback that failed is precisely the state the entry is for, so
        // dropping it here would throw away the only record of it.
        if occupied && failed.skeleton_restored {
            crate::deletion_recovery::close(&journal, &token).await;
        }
        return Err(failed.error);
    }

    if occupied {
        // The clone holds the target path now, so the skeleton beside it is a
        // tombstone nothing names. Without the marker a startup pass would read
        // this entry as an install that never happened and try to put the
        // skeleton back on top of the import — it would refuse, correctly, and
        // leave both for an operator. Reported rather than fatal for the same
        // reason as everywhere else this marker is written.
        if let Err(error) = crate::deletion_recovery::mark_committed(&journal, &token).await {
            tracing::warn!(
                path = %retired.display(),
                error = %format!("{error:#}"),
                "the import installed its clone but could not mark the skeleton it replaced \
                 retired; the startup pass will report the entry rather than guess about it"
            );
            discard_retired_skeleton(&retired);
        } else {
            discard_retired_skeleton(&retired);
            crate::deletion_recovery::close(&journal, &token).await;
        }
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

/// Largest complete set of wiki pages retained for one import.
///
/// [`MAX_WIKI_PAGE_BYTES`] bounds one page and nothing else, while every page
/// [`collect_wiki_pages`] returns stays in the `Vec<SourceWikiPage>` it hands
/// back until the caller has written each one to the database. How many pages
/// a source wiki holds is chosen by whoever pushed to it, so a thousand pages
/// a byte under the per-file ceiling would cost a thousand times what that
/// number promises. The same reasoning, and the same figure, as
/// `MAX_WORKFLOW_TOTAL_BYTES` in `rg-ci` and `MAX_TEMPLATE_TOTAL_BYTES` in
/// `crate::issue_template`.
const MAX_WIKI_TOTAL_BYTES: u64 = 16 * 1024 * 1024;

/// Independent backstop for source wikis made of tiny or empty pages.
///
/// [`MAX_WIKI_TOTAL_BYTES`] is no bound at all against a directory of empty
/// pages: each costs nothing to hold and everything to walk, name and report.
/// This is what a set of them runs out of.
const MAX_WIKI_PAGE_COUNT: usize = 4096;

/// Ceiling on the raw `git ls-tree -r -l -z HEAD` listing that answers the
/// wiki's page discovery.
///
/// The set budget above bounds the page bodies discovery keeps, but the
/// listing itself is the input the budget is charged from — and a `-r`
/// listing includes every attachment and asset a wiki carries alongside its
/// pages, not just Markdown files. Size scales with what the source wiki
/// committed, chosen by whoever pushed to it. 16 MiB accommodates ~130 000
/// `-l` records — orders above `MAX_WIKI_PAGE_COUNT` plus any realistic
/// asset directory, and small enough that a wiki with millions of tiny
/// attachments is refused before its listing lands in heap.
const WIKI_LISTING_LIMIT_BYTES: u64 = 16 * 1024 * 1024;

/// What one import may still spend on the set of pages it is assembling.
///
/// Both halves are charged against the size in the `ls-tree -l` listing,
/// BEFORE any `cat-file blob` runs — a budget charged after the allocation
/// costs exactly the memory it was declared to save, which is the same rule
/// the neighbouring readers of a committed blob follow
/// (`crate::issue_template::TemplateSetBudget`,
/// `crate::review::codeowners::load_codeowners`).
///
/// Running out is fatal to the whole import: a set that outgrew its budget
/// cannot be reported as a complete one, and a partial `Ok(_)` carrying the
/// first N pages of a source wiki is a worse answer than a refusal naming the
/// limit — the acceptance the ticket for this bound spells out.
struct WikiPageBudget {
    pages_left: usize,
    bytes_left: u64,
}

impl WikiPageBudget {
    fn new() -> Self {
        Self {
            pages_left: MAX_WIKI_PAGE_COUNT,
            bytes_left: MAX_WIKI_TOTAL_BYTES,
        }
    }

    /// Charge one candidate page against the count backstop.
    fn charge_page(&mut self, path: &str) -> Result<()> {
        self.pages_left = self.pages_left.checked_sub(1).ok_or_else(|| {
            crate::error::invalid_request(format!(
                "the source wiki holds more than {MAX_WIKI_PAGE_COUNT} pages; limit reached at \
                 {path}"
            ))
        })?;
        Ok(())
    }

    /// Charge one candidate page's bytes against the aggregate budget.
    ///
    /// Spent BEFORE `cat-file blob` reads the object, so the read that this
    /// budget is here to bound never begins after it has been refused.
    fn charge_bytes(&mut self, path: &str, size: u64) -> Result<()> {
        self.bytes_left = self.bytes_left.checked_sub(size).ok_or_else(|| {
            crate::error::invalid_request(format!(
                "wiki pages exceed the {MAX_WIKI_TOTAL_BYTES}-byte total limit at {path}"
            ))
        })?;
        Ok(())
    }
}

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
/// can collide with `<name>.git` itself. Mirrors [`clone_repo`]'s staging,
/// including where the name comes from — a clone interrupted by a stop that
/// runs no destructors is retired by `rg_core::staging`'s startup sweep, which
/// recognises it by that name and by nothing else.
fn wiki_staging_dir(repo_root: &Path, owner: &str, name: &str) -> PathBuf {
    repo_root
        .join(owner)
        .join(crate::staging::import_wiki_clone_staging_name(
            name,
            uuid::Uuid::new_v4(),
        ))
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

/// What a clone of a source wiki turned out to hold.
enum SourceWikiClone {
    /// `HEAD` resolved, and these are the Markdown pages under it.
    Pages(Vec<SourceWikiPage>),
    /// Nothing to read: `HEAD` does not resolve, so there is no tree to list.
    /// *Why* it does not resolve cannot be answered from the clone — see
    /// [`report_wiki_without_pages`], which is the reason this is a variant of
    /// its own rather than an empty `Vec` indistinguishable from a wiki that
    /// genuinely holds no page.
    Nothing,
}

/// Read the Markdown pages a cloned wiki repository holds at `HEAD`.
///
/// A wiki that exists but was never written to is not an error: the platform
/// hands out a repository with no commit in it, and an unborn `HEAD` has no
/// tree to list.
fn collect_wiki_pages(staging: &Path) -> Result<SourceWikiClone> {
    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    if rg_git::ref_advertisement::collect(staging)
        .context("read the cloned source wiki")?
        .head_oid
        .is_none()
    {
        return Ok(SourceWikiClone::Nothing);
    }

    // The set budget below bounds what we keep across the discovery; this
    // ceiling bounds the RAW listing that answers it. A wiki committed with a
    // million-file `assets/` directory otherwise buffers hundreds of MiB in
    // this process before the budget ever sees the first size — the class the
    // whole `Committed Files Are Not a Memory Budget` phase exists to remove.
    let listing = git.run_bounded(
        &["ls-tree", "-r", "-l", "-z", "HEAD"],
        Some(staging),
        WIKI_LISTING_LIMIT_BYTES,
    )?;
    listing
        .ensure_success()
        .context("list the source wiki's pages")?;

    let mut pages = Vec::new();
    let mut foreign = Vec::new();
    // One budget for the whole listing: every page collect_wiki_pages returns
    // is held in the same `Vec<SourceWikiPage>` until the caller has written
    // it to the database, so bounding each page separately would bound
    // nothing (card_5e0f9bd8877c).
    let mut budget = WikiPageBudget::new();
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
        let page_size = size.parse::<u64>().unwrap_or(u64::MAX);
        if page_size > MAX_WIKI_PAGE_BYTES {
            tracing::warn!(
                path,
                size,
                limit = MAX_WIKI_PAGE_BYTES,
                "a source wiki page is larger than an import carries and was not imported"
            );
            continue;
        }

        // Charge every candidate this pass will hand back BEFORE the read that
        // holds it. Aggregate overflow is fatal to the whole import — see
        // [`WikiPageBudget`] — so `?` lets it out of the collector without
        // dropping the pages that came before it as a silent partial import.
        budget.charge_page(path)?;
        budget.charge_bytes(path, page_size)?;

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

    Ok(SourceWikiClone::Pages(pages))
}

/// Render a wiki clone failure for the log with the source token taken back out
/// of it — the same last-resort net [`mask_source_credentials`] is, for the same
/// reason: the token reaches `git` and the remote, so it can come back inside
/// their error text.
///
/// This one is a *log* line and keeps the whole chain: the split
/// [`failure_reason`] makes is for the task row the importer reads, and nothing
/// here is persisted.
fn wiki_failure_reason(error: &anyhow::Error, credentials: Option<&GitCredentials>) -> String {
    let reason = crate::net::mask_url_credentials(&format!("{error:#}"));
    match credentials.map(|credentials| credentials.password().to_string()) {
        Some(token) => crate::auth::encryption::mask_values(&reason, &[token]),
        None => reason,
    }
}

/// Say why a wiki that cloned cleanly carried no page at all.
///
/// `git clone --bare --depth 1` of a wiki whose `HEAD` names a branch nobody
/// created reports success and copies *nothing* — not even the branches that do
/// exist. On disk that is byte for byte a wiki nobody has ever written a page
/// into, so the clone cannot be asked which of the two happened; and one of the
/// two is every page the source had, dropped without a word. (The neighbouring
/// [`adopt_cloned_default_branch`] can tell them apart because its clone is not
/// shallow and keeps the branches behind the broken `HEAD`.)
///
/// The remote can be asked. `ls-remote` advertises the branches that are there:
/// branches on the remote against no page here is the broken `HEAD`, and no
/// branch at all is a wiki that is genuinely empty — the one shape of "no pages"
/// that is not a loss, and the one that stays silent.
fn report_wiki_without_pages(
    git: &rg_git::cli_gateway::GitCommandGateway,
    invocation: &rg_git::credentials::OutboundGitInvocation,
    repo_id: i64,
    wiki_url: &str,
    credentials: Option<&GitCredentials>,
) {
    let advertised = invocation
        .run(git, &["ls-remote", "--heads", wiki_url], None)
        .and_then(|output| {
            output
                .ensure_success()
                .context("git ls-remote --heads (wiki)")?;
            Ok(output.stdout_str())
        });
    let advertised = match advertised {
        Ok(advertised) => advertised,
        Err(error) => {
            tracing::warn!(
                repo_id,
                wiki_url = %crate::net::mask_url_credentials(wiki_url),
                reason = %wiki_failure_reason(&error, credentials),
                "the source wiki cloned without a single page and the remote could not be asked \
                 whether it holds any — the import carried no wiki pages"
            );
            return;
        }
    };

    // `<oid> TAB <refname>`, one branch per line.
    let mut branches = advertised
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter_map(|(_, refname)| refname.trim().strip_prefix("refs/heads/"))
        .collect::<Vec<_>>();
    // No branch on the remote either: the wiki was created and never written
    // to, which is the one unborn state that leaves without a word.
    if branches.is_empty() {
        return;
    }
    branches.sort_unstable();
    let branch_count = branches.len();
    let overflow = branch_count.saturating_sub(UNBORN_BRANCH_SAMPLE);
    let mut sample = branches
        .into_iter()
        .take(UNBORN_BRANCH_SAMPLE)
        .collect::<Vec<_>>()
        .join(", ");
    if overflow > 0 {
        sample.push_str(&format!(" and {overflow} more"));
    }
    tracing::warn!(
        repo_id,
        wiki_url = %crate::net::mask_url_credentials(wiki_url),
        branch_count,
        branches = %sample,
        "the source wiki has branches but its HEAD resolves to none of them — not one of its \
         pages was imported, and none will be until the source's wiki HEAD names a branch that \
         exists"
    );
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
/// A clone that *does* come back and still holds no page gets the same
/// treatment for the same reason, and takes one extra question to explain
/// itself: see [`report_wiki_without_pages`].
///
/// Everything after the clone stays fatal: a wiki we did clone and then could
/// not read, or could not write into the database, is our failure and is
/// reported as one.
async fn import_wiki_pages(
    db: &DatabaseConnection,
    repo_id: i64,
    wiki_url: &str,
    trusted_origins: &crate::import::trust::TrustedImportOrigins,
    staging: &Path,
    credentials: Option<&GitCredentials>,
    author_id: Option<i64>,
) -> Result<usize> {
    // A missing wiki is normally a non-fatal clone failure. Transport policy is
    // different: it must fail closed, before that compatibility path can turn a
    // refused plaintext transport into a successful zero-page import.
    crate::import::trust::ImportTransportPolicy::require_confidential_transport(wiki_url)?;
    let remote = trusted_origins.git_destination(wiki_url).await?;
    import_wiki_pages_from_destination(db, repo_id, &remote, staging, credentials, author_id).await
}

/// Import a wiki from an explicit local path for the filesystem integration
/// contract. Remote production imports cannot enter through this API: a URL is
/// not an absolute filesystem path and is refused by the destination type.
pub async fn import_wiki_pages_from_local_path(
    db: &DatabaseConnection,
    repo_id: i64,
    wiki_path: &Path,
    staging: &Path,
    credentials: Option<&GitCredentials>,
    author_id: Option<i64>,
) -> Result<usize> {
    let remote = crate::net::GuardedGitRemote::local_path(wiki_path)?;
    import_wiki_pages_from_destination(db, repo_id, &remote, staging, credentials, author_id).await
}

async fn import_wiki_pages_from_destination(
    db: &DatabaseConnection,
    repo_id: i64,
    remote: &crate::net::GuardedGitRemote,
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
    let invocation = remote.bind_invocation(credential_invocation(credentials))?;
    let destination = staging.to_string_lossy();
    // `--depth 1`: only the pages as they stand are imported. A ForgeKeep wiki
    // keeps its own revision history from the first edit onwards, and there is
    // nowhere to put the source's.
    let cloned = invocation
        .run(
            git,
            &[
                "clone",
                "--bare",
                "--depth",
                "1",
                remote.url(),
                &destination,
            ],
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
    let collected = collect_wiki_pages(staging);
    discard_partial_clone(staging);
    let pages = match collected? {
        SourceWikiClone::Pages(pages) => pages,
        // A clone that came back with nothing to read is the one answer this
        // step cannot interpret on its own, and the one it used to report as a
        // plain zero.
        SourceWikiClone::Nothing => {
            report_wiki_without_pages(git, &invocation, repo_id, remote.url(), credentials);
            return Ok(0);
        }
    };

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

fn parse_import_datetime(
    provider: &str,
    object: &str,
    external_id: i64,
    field: &str,
    value: &str,
) -> Result<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&Utc))
        .with_context(|| {
            format!("{provider} {object} {external_id} has invalid RFC3339 timestamp in `{field}`")
        })
}

fn parse_optional_import_datetime(
    provider: &str,
    object: &str,
    external_id: i64,
    field: &str,
    value: Option<&str>,
) -> Result<Option<chrono::DateTime<Utc>>> {
    value
        .map(|value| parse_import_datetime(provider, object, external_id, field, value))
        .transpose()
}

fn parse_required_import_datetime(
    provider: &str,
    object: &str,
    external_id: i64,
    field: &str,
    value: Option<&str>,
) -> Result<chrono::DateTime<Utc>> {
    let value = value.with_context(|| {
        format!("{provider} {object} {external_id} is missing required `{field}` timestamp")
    })?;
    parse_import_datetime(provider, object, external_id, field, value)
}

fn parse_optional_import_date(
    provider: &str,
    object: &str,
    external_id: i64,
    field: &str,
    value: Option<&str>,
) -> Result<Option<chrono::DateTime<Utc>>> {
    value
        .map(|value| {
            chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map(|date| {
                    date.and_hms_opt(0, 0, 0)
                        .expect("midnight is a valid time")
                        .and_utc()
                })
                .with_context(|| {
                    format!(
                        "{provider} {object} {external_id} has invalid YYYY-MM-DD date in `{field}`"
                    )
                })
        })
        .transpose()
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

        let due_date = parse_optional_import_datetime(
            "GitHub",
            "milestone",
            gm.number,
            "due_on",
            gm.due_on.as_deref(),
        )?;
        let created_at = parse_import_datetime(
            "GitHub",
            "milestone",
            gm.number,
            "created_at",
            &gm.created_at,
        )?;
        let updated_at = parse_import_datetime(
            "GitHub",
            "milestone",
            gm.number,
            "updated_at",
            &gm.updated_at,
        )?;

        let model = milestone::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            title: Set(gm.title.clone()),
            description: Set(gm.description.clone()),
            state: Set(state.as_str().to_string()),
            due_date: Set(due_date),
            created_at: Set(created_at),
            updated_at: Set(updated_at),
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

    let created_at = parse_import_datetime(
        "GitHub",
        "issue",
        issue.number,
        "created_at",
        &issue.created_at,
    )?;
    let updated_at = parse_import_datetime(
        "GitHub",
        "issue",
        issue.number,
        "updated_at",
        &issue.updated_at,
    )?;
    let closed_at = parse_optional_import_datetime(
        "GitHub",
        "issue",
        issue.number,
        "closed_at",
        issue.closed_at.as_deref(),
    )?;
    let comment_timestamps = comments
        .iter()
        .map(|comment| {
            Ok((
                parse_import_datetime(
                    "GitHub",
                    "issue comment",
                    comment.id,
                    "created_at",
                    &comment.created_at,
                )?,
                parse_import_datetime(
                    "GitHub",
                    "issue comment",
                    comment.id,
                    "updated_at",
                    &comment.updated_at,
                )?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
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
        updated_at: Set(updated_at),
        closed_at: Set(closed_at),
        deleted_at: Set(None),
    };

    let saved = create_imported_issue(db, repo_id, model, label_ids).await?;

    // Import comments
    for (comment, (created_at, updated_at)) in comments.iter().zip(comment_timestamps) {
        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id),
            author_id: Set(author_id),
            body: Set(comment.body.clone().unwrap_or_default()),
            created_at: Set(created_at),
            updated_at: Set(updated_at),
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
) -> Result<usize> {
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

    let created_at = parse_import_datetime(
        "GitHub",
        "pull request",
        pr.number,
        "created_at",
        &pr.created_at,
    )?;
    let updated_at = parse_import_datetime(
        "GitHub",
        "pull request",
        pr.number,
        "updated_at",
        &pr.updated_at,
    )?;
    let closed_at = parse_optional_import_datetime(
        "GitHub",
        "pull request",
        pr.number,
        "closed_at",
        pr.closed_at.as_deref(),
    )?;
    let merged_at = parse_optional_import_datetime(
        "GitHub",
        "pull request",
        pr.number,
        "merged_at",
        pr.merged_at.as_deref(),
    )?;
    let comment_timestamps = comments
        .iter()
        .map(|comment| {
            Ok((
                parse_import_datetime(
                    "GitHub",
                    "pull request comment",
                    comment.id,
                    "created_at",
                    &comment.created_at,
                )?,
                parse_import_datetime(
                    "GitHub",
                    "pull request comment",
                    comment.id,
                    "updated_at",
                    &comment.updated_at,
                )?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let review_timestamps: Vec<Option<chrono::DateTime<Utc>>> = reviews
        .iter()
        .map(|review| {
            match parse_required_import_datetime(
                "GitHub",
                "pull request review",
                review.id,
                "submitted_at",
                review.submitted_at.as_deref(),
            ) {
                Ok(timestamp) => Some(timestamp),
                Err(error) => {
                    tracing::warn!(
                        provider = "GitHub",
                        object = "pull request review",
                        external_id = review.id,
                        field = "submitted_at",
                        error = %format!("{error:#}"),
                        "skipping imported object with invalid required timestamp"
                    );
                    None
                }
            }
        })
        .collect();

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
        created_at: Set(created_at),
        updated_at: Set(updated_at),
        closed_at: Set(closed_at),
        merged_at: Set(merged_at),
    };

    let saved = crate::pull_request::service::insert_with_repo_number(db, repo_id, model).await?;

    // Import PR comments (general discussion)
    for (comment, (created_at, updated_at)) in comments.iter().zip(comment_timestamps) {
        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id), // issue_id is PR id in this context
            author_id: Set(author_id),
            body: Set(comment.body.clone().unwrap_or_default()),
            created_at: Set(created_at),
            updated_at: Set(updated_at),
        };

        if let Err(e) = issue_comment_ops::create(db, cm).await {
            tracing::warn!(pr_number = %pr.number, error = %format!("{e:#}"), "failed to import PR comment");
        }
    }

    // Import reviews
    let mut imported_reviews = 0;
    for (review, submitted_at) in reviews.iter().zip(review_timestamps) {
        let Some(submitted_at) = submitted_at else {
            continue;
        };
        let action = match review.state.as_str() {
            "APPROVED" => "approve",
            "CHANGES_REQUESTED" => "request_changes",
            "COMMENTED" => "comment",
            "DISMISSED" => "dismiss",
            _ => "comment",
        };

        // GitHub reports a withdrawn review as `DISMISSED` and does not say
        // which verdict it used to be, so the imported row keeps `"dismiss"`
        // as its action and carries the stamp as well. Both halves say the
        // same thing to `count_current_approvals` — this authorizes nothing —
        // which is the only safe reading of a verdict we cannot reconstruct
        // (card_dc0f5d58e5f4).
        let dismissed_at = (action == "dismiss").then_some(submitted_at);

        let rv = rg_db::entities::pr_review::ActiveModel {
            id: sea_orm::NotSet,
            pr_id: Set(saved.id),
            repo_id: Set(repo_id),
            reviewer_id: Set(author_id),
            action: Set(action.to_string()),
            body: Set(review.body.clone()),
            commit_id: Set(None),
            created_at: Set(submitted_at),
            dismissed_at: Set(dismissed_at),
            dismissed_by: Set(None),
        };

        if let Err(e) = pr_review_ops::create(db, rv).await {
            tracing::warn!(pr_number = %pr.number, error = %format!("{e:#}"), "failed to import PR review");
        } else {
            imported_reviews += 1;
        }
    }

    Ok(imported_reviews)
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

        let due_date = parse_optional_import_date(
            "GitLab",
            "milestone",
            gm.id,
            "due_date",
            gm.due_date.as_deref(),
        )?;
        let created_at =
            parse_import_datetime("GitLab", "milestone", gm.id, "created_at", &gm.created_at)?;
        let updated_at =
            parse_import_datetime("GitLab", "milestone", gm.id, "updated_at", &gm.updated_at)?;

        let model = milestone::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            title: Set(gm.title.clone()),
            description: Set(gm.description.clone()),
            state: Set(state.as_str().to_string()),
            due_date: Set(due_date),
            created_at: Set(created_at),
            updated_at: Set(updated_at),
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

    let created_at =
        parse_import_datetime("GitLab", "issue", issue.id, "created_at", &issue.created_at)?;
    let updated_at =
        parse_import_datetime("GitLab", "issue", issue.id, "updated_at", &issue.updated_at)?;
    let closed_at = parse_optional_import_datetime(
        "GitLab",
        "issue",
        issue.id,
        "closed_at",
        issue.closed_at.as_deref(),
    )?;
    let note_timestamps = notes
        .iter()
        .filter(|note| !note.system)
        .map(|note| {
            Ok((
                parse_import_datetime(
                    "GitLab",
                    "issue note",
                    note.id,
                    "created_at",
                    &note.created_at,
                )?,
                parse_import_datetime(
                    "GitLab",
                    "issue note",
                    note.id,
                    "updated_at",
                    &note.updated_at,
                )?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

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
        created_at: Set(created_at),
        updated_at: Set(updated_at),
        closed_at: Set(closed_at),
        deleted_at: Set(None),
    };

    let saved = create_imported_issue(db, repo_id, model, label_ids).await?;

    // Import notes (skip system notes)
    for (note, (created_at, updated_at)) in notes
        .iter()
        .filter(|note| !note.system)
        .zip(note_timestamps)
    {
        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id),
            author_id: Set(author_id),
            body: Set(note.body.clone().unwrap_or_default()),
            created_at: Set(created_at),
            updated_at: Set(updated_at),
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

    let created_at = parse_import_datetime(
        "GitLab",
        "merge request",
        mr.id,
        "created_at",
        &mr.created_at,
    )?;
    let updated_at = parse_import_datetime(
        "GitLab",
        "merge request",
        mr.id,
        "updated_at",
        &mr.updated_at,
    )?;
    let closed_at = parse_optional_import_datetime(
        "GitLab",
        "merge request",
        mr.id,
        "closed_at",
        mr.closed_at.as_deref(),
    )?;
    let merged_at = parse_optional_import_datetime(
        "GitLab",
        "merge request",
        mr.id,
        "merged_at",
        mr.merged_at.as_deref(),
    )?;
    let note_timestamps = notes
        .iter()
        .filter(|note| !note.system)
        .map(|note| {
            Ok((
                parse_import_datetime(
                    "GitLab",
                    "merge request note",
                    note.id,
                    "created_at",
                    &note.created_at,
                )?,
                parse_import_datetime(
                    "GitLab",
                    "merge request note",
                    note.id,
                    "updated_at",
                    &note.updated_at,
                )?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

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
        created_at: Set(created_at),
        updated_at: Set(updated_at),
        closed_at: Set(closed_at),
        merged_at: Set(merged_at),
    };

    let saved = crate::pull_request::service::insert_with_repo_number(db, repo_id, model).await?;

    // Import MR notes (skip system notes)
    for (note, (created_at, updated_at)) in notes
        .iter()
        .filter(|note| !note.system)
        .zip(note_timestamps)
    {
        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id),
            author_id: Set(author_id),
            body: Set(note.body.clone().unwrap_or_default()),
            created_at: Set(created_at),
            updated_at: Set(updated_at),
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

/// What a failed import is allowed to say in its `error` column.
///
/// That column is not operator-only. `GET /api/v1/imports/{id}` serialises the
/// task row as it stands and the progress page renders it, so whatever lands
/// here is read by the person who started the import — and anyone registered on
/// the instance can start one. The flattened chain is not fit for that: a failed
/// clone comes back through `GitOutput::ensure_success` as the whole command
/// line, `-C <server-side repository path>` included, with git's own stderr
/// after it, and the platform clients quote the source's response bodies
/// (card_4ec796295ae8, H-05).
///
/// So the same split the merge queue makes for the pull request timeline
/// (`merge_queue::merge_failure_reason`): a typed state of [`crate::error`]
/// carries a message written for the person who asked — a source that has no
/// such repository, a token the source refused, a rate limit — and each of
/// those types documents that its message reaches a client verbatim. Everything
/// else is ours, settles as [`UNSPECIFIED_IMPORT_FAILURE`], and stays whole in
/// the log beside the task id. The `stage` column, written all the way to the
/// failure, is what still says *where* it stopped.
///
/// Masking stays over the typed half. It closes a different vector and neither
/// subsumes the other: the token is handed to `git` and to the platform's HTTP
/// API, so it can come back inside a message this function is about to persist —
/// and a typed message quoting the source URL is exactly such a message, since a
/// task row written before the create-time split still carries `user:token@` in
/// it. It is the same last-resort net `mirror::service` puts in front of
/// `last_sync_error`.
fn failure_reason(error: &anyhow::Error, auth_token: Option<&str>) -> String {
    match crate::error::client_facing_message(error) {
        Some(message) => mask_source_credentials(&message, auth_token),
        None => UNSPECIFIED_IMPORT_FAILURE.to_string(),
    }
}

/// The `error` column of an import that failed for a reason of ours.
///
/// Fixed text on purpose — the detail it replaces is in the log, and the row's
/// own `stage` says how far the import got before it stopped.
const UNSPECIFIED_IMPORT_FAILURE: &str =
    "the import could not be completed; ask the instance operator to check the server log";

/// Take the source credential back out of a message about to be persisted or
/// logged, from either of the two places it can be written into one.
fn mask_source_credentials(message: &str, auth_token: Option<&str>) -> String {
    let message = crate::net::mask_url_credentials(message);
    match auth_token.filter(|token| !token.is_empty()) {
        Some(token) => crate::auth::encryption::mask_values(&message, &[token.to_string()]),
        None => message,
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
    transport_policy: &crate::import::trust::ImportTransportPolicy,
    repo_root: &Path,
) -> Result<ImportTask> {
    // The target name is the client's, whether they typed it or let it be
    // derived from the source URL, so it is refused here rather than inside the
    // worker. `create_repo` asks the same question later, but by then the
    // request has been answered `201` and the only place the refusal appears is
    // a failed task nobody is watching — and a name ending in `.git` is exactly
    // what a mirror URL suggests (card_a9a991c507e4).
    crate::validate_repo_name(&target_name).map_err(|error| {
        crate::error::invalid_request(format!(
            "invalid target repository name: {target_name} ({error})"
        ))
    })?;

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

    // This happens after URL-embedded credentials have joined the explicit
    // token, but before the first DB write or detached worker. Private-origin
    // trust is intentionally not consulted: reachability and confidentiality
    // are separate operator decisions.
    transport_policy.require_confidential_credentials(&source_url, auth_token.as_deref())?;

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
    let transport_policy = transport_policy.clone();
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
            &transport_policy,
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
                // The chain, not the reason: `failure_reason` keeps the command
                // line and the source's own text out of the row that the task's
                // owner reads, so this is the only place either survives.
                tracing::error!(
                    task_id = task_clone.id,
                    reason,
                    detail = %mask_source_credentials(&format!("{e:#}"), auth_token.as_deref()),
                    "import failed"
                );
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
    use crate::test_support::CapturedLogs;
    use rg_db::ops::{issue_label_ops, issue_ops};
    use sea_orm::{ConnectionTrait, Database, EntityTrait, Statement};

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

    fn github_pr() -> GitHubPR {
        let git_ref = |name: &str, sha: &str| crate::import::github_client::GitHubRef {
            ref_name: name.to_string(),
            sha: sha.to_string(),
            label: None,
            repo: None,
        };
        GitHubPR {
            number: 61,
            title: "GitHub pull request".to_string(),
            body: None,
            state: "open".to_string(),
            merged: Some(false),
            merged_at: None,
            draft: false,
            user: None,
            head: git_ref("feature", "1111111111111111111111111111111111111111"),
            base: git_ref("main", "2222222222222222222222222222222222222222"),
            labels: Vec::new(),
            milestone: None,
            created_at: "2024-01-03T00:00:00Z".to_string(),
            updated_at: "2024-01-03T01:00:00Z".to_string(),
            closed_at: None,
        }
    }

    fn gitlab_mr() -> GitLabMR {
        GitLabMR {
            id: 71,
            iid: 17,
            title: "GitLab merge request".to_string(),
            description: None,
            state: "opened".to_string(),
            merged_at: None,
            draft: false,
            author: None,
            source_branch: "feature".to_string(),
            target_branch: "main".to_string(),
            source_project_id: None,
            target_project_id: 1,
            labels: Vec::new(),
            milestone: None,
            created_at: "2024-01-04T00:00:00Z".to_string(),
            updated_at: "2024-01-04T01:00:00Z".to_string(),
            closed_at: None,
        }
    }

    fn github_comment(id: i64) -> GitHubComment {
        GitHubComment {
            id,
            body: Some("comment".to_string()),
            user: None,
            created_at: "2024-01-05T00:00:00Z".to_string(),
            updated_at: "2024-01-05T01:00:00Z".to_string(),
        }
    }

    fn gitlab_note(id: i64) -> GitLabNote {
        GitLabNote {
            id,
            body: Some("note".to_string()),
            author: None,
            system: false,
            created_at: "2024-01-06T00:00:00Z".to_string(),
            updated_at: "2024-01-06T01:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn github_issue_and_comment_reject_bad_timestamps_before_writing_the_issue() {
        let db = test_db().await;
        let mut issue = github_issue(GitHubLabel {
            id: 10,
            name: "bug".to_string(),
            color: "ee0701".to_string(),
            description: None,
        });
        issue.created_at = "not-a-timestamp".to_string();

        let error = import_github_issue(
            &db,
            1,
            "importer",
            "imported",
            &issue,
            &[],
            IMPORTER_ID,
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("a GitHub issue with a malformed created_at must fail");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("GitHub issue 41"), "{rendered}");
        assert!(rendered.contains("`created_at`"), "{rendered}");
        assert!(
            issue_ops::find_by_repo_and_number(&db, 1, 1)
                .await
                .unwrap()
                .is_none(),
            "the malformed issue must not be stored with an invented timestamp"
        );

        issue.created_at = "2024-01-01T00:00:00Z".to_string();
        let mut comment = github_comment(501);
        comment.updated_at = "still-not-a-timestamp".to_string();
        let error = import_github_issue(
            &db,
            1,
            "importer",
            "imported",
            &issue,
            &[comment],
            IMPORTER_ID,
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("a GitHub comment with a malformed updated_at must fail its issue item");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("GitHub issue comment 501"), "{rendered}");
        assert!(rendered.contains("`updated_at`"), "{rendered}");
        assert!(
            issue_ops::find_by_repo_and_number(&db, 1, 1)
                .await
                .unwrap()
                .is_none(),
            "child timestamps are validated before the parent issue is written"
        );
    }

    #[tokio::test]
    async fn github_pr_rejects_bad_timestamps_before_writing_the_pr() {
        let db = test_db().await;
        let mut pr = github_pr();
        pr.updated_at = "not-a-timestamp".to_string();

        let error = import_github_pr(&db, 1, &pr, &[], &[], IMPORTER_ID, &HashMap::new())
            .await
            .expect_err("a GitHub PR with a malformed updated_at must fail");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("GitHub pull request 61"), "{rendered}");
        assert!(rendered.contains("`updated_at`"), "{rendered}");
        assert!(
            rg_db::entities::pull_request::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty(),
            "the malformed PR must not be stored with an invented timestamp"
        );
    }

    #[tokio::test]
    async fn github_reviews_without_a_valid_submitted_at_are_skipped_and_reported() {
        let db = test_db().await;
        let (logs, _guard) = CapturedLogs::capture();
        let reviews = [
            GitHubReview {
                id: 801,
                user: None,
                state: "APPROVED".to_string(),
                body: None,
                submitted_at: None,
            },
            GitHubReview {
                id: 802,
                user: None,
                state: "COMMENTED".to_string(),
                body: Some("looks good".to_string()),
                submitted_at: Some("not-a-timestamp".to_string()),
            },
        ];

        let imported = import_github_pr(
            &db,
            1,
            &github_pr(),
            &[],
            &reviews,
            IMPORTER_ID,
            &HashMap::new(),
        )
        .await
        .unwrap();

        assert_eq!(imported, 0, "skipped reviews must not inflate import stats");
        assert!(
            rg_db::entities::pr_review::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty(),
            "neither absent nor malformed submitted_at may become the current time"
        );
        let rendered = logs.rendered();
        for expected in [
            "GitHub",
            "pull request review",
            "submitted_at",
            "801",
            "802",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?} in {rendered}"
            );
        }
    }

    #[tokio::test]
    async fn gitlab_issue_and_note_reject_bad_timestamps_before_writing_the_issue() {
        let db = test_db().await;
        let mut issue = gitlab_issue("bug");
        issue.updated_at = "not-a-timestamp".to_string();

        let error = import_gitlab_issue(
            &db,
            1,
            "importer",
            "imported",
            &issue,
            &[],
            IMPORTER_ID,
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("a GitLab issue with a malformed updated_at must fail");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("GitLab issue 52"), "{rendered}");
        assert!(rendered.contains("`updated_at`"), "{rendered}");
        assert!(issue_ops::find_by_repo_and_number(&db, 1, 1)
            .await
            .unwrap()
            .is_none());

        issue.updated_at = "2024-01-02T00:00:00Z".to_string();
        let mut note = gitlab_note(901);
        note.created_at = "still-not-a-timestamp".to_string();
        let error = import_gitlab_issue(
            &db,
            1,
            "importer",
            "imported",
            &issue,
            &[note],
            IMPORTER_ID,
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("a GitLab note with a malformed created_at must fail its issue item");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("GitLab issue note 901"), "{rendered}");
        assert!(rendered.contains("`created_at`"), "{rendered}");
        assert!(
            issue_ops::find_by_repo_and_number(&db, 1, 1)
                .await
                .unwrap()
                .is_none(),
            "child timestamps are validated before the parent issue is written"
        );
    }

    #[tokio::test]
    async fn gitlab_mr_rejects_bad_timestamps_before_writing_the_mr() {
        let db = test_db().await;
        let mut mr = gitlab_mr();
        mr.created_at = "not-a-timestamp".to_string();

        let error = import_gitlab_mr(&db, 1, &mr, &[], IMPORTER_ID, &HashMap::new())
            .await
            .expect_err("a GitLab MR with a malformed created_at must fail");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("GitLab merge request 71"), "{rendered}");
        assert!(rendered.contains("`created_at`"), "{rendered}");
        assert!(
            rg_db::entities::pull_request::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty(),
            "the malformed MR must not be stored with an invented timestamp"
        );
    }

    #[tokio::test]
    async fn imported_milestones_keep_source_timestamps_and_gitlab_date_only_due_dates() {
        let db = test_db().await;
        let mut milestone_map = HashMap::new();
        let github = GitHubMilestone {
            number: 31,
            title: "GitHub milestone".to_string(),
            description: None,
            state: "open".to_string(),
            due_on: Some("2024-06-30T12:34:56Z".to_string()),
            created_at: "2024-02-01T02:03:04Z".to_string(),
            updated_at: "2024-02-02T03:04:05Z".to_string(),
            closed_at: None,
        };
        let gitlab = GitLabMilestone {
            id: 32,
            iid: 32,
            title: "GitLab milestone".to_string(),
            description: None,
            state: "active".to_string(),
            due_date: Some("2024-07-31".to_string()),
            start_date: None,
            created_at: "2024-03-01T02:03:04Z".to_string(),
            updated_at: "2024-03-02T03:04:05Z".to_string(),
        };

        assert_eq!(
            import_github_milestones(&db, 1, &[github], &mut milestone_map)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            import_gitlab_milestones(&db, 1, &[gitlab], &mut milestone_map)
                .await
                .unwrap(),
            1
        );

        let stored = milestone_ops::list_by_repo(&db, 1, None).await.unwrap();
        let github = stored
            .iter()
            .find(|milestone| milestone.title == "GitHub milestone")
            .unwrap();
        assert_eq!(github.created_at.to_rfc3339(), "2024-02-01T02:03:04+00:00");
        assert_eq!(github.updated_at.to_rfc3339(), "2024-02-02T03:04:05+00:00");
        let gitlab = stored
            .iter()
            .find(|milestone| milestone.title == "GitLab milestone")
            .unwrap();
        assert_eq!(
            gitlab.due_date.unwrap().to_rfc3339(),
            "2024-07-31T00:00:00+00:00"
        );
        assert_eq!(gitlab.created_at.to_rfc3339(), "2024-03-01T02:03:04+00:00");
        assert_eq!(gitlab.updated_at.to_rfc3339(), "2024-03-02T03:04:05+00:00");
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

    async fn stored_task(
        db: &DatabaseConnection,
        repo_id: Option<i64>,
        platform: &str,
        source_url: &str,
        status: &str,
        import_repo: bool,
        import_wiki: bool,
    ) -> ImportTask {
        let now = Utc::now();
        import_task_ops::create(
            db,
            import_task::ActiveModel {
                user_id: Set(1),
                repo_id: Set(repo_id),
                platform: Set(platform.to_string()),
                source_url: Set(source_url.to_string()),
                target_owner: Set("importer".to_string()),
                target_name: Set("imported".to_string()),
                status: Set(status.to_string()),
                progress: Set(0),
                stage: Set(None),
                error: Set(None),
                import_repo: Set(import_repo),
                import_issues: Set(false),
                import_pull_requests: Set(false),
                import_wiki: Set(import_wiki),
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

    async fn running_task(db: &DatabaseConnection, repo_id: Option<i64>) -> ImportTask {
        stored_task(
            db,
            repo_id,
            "git",
            "https://example.invalid/importer/imported.git",
            "cloning",
            true,
            false,
        )
        .await
    }

    #[tokio::test]
    async fn a_stored_native_git_wiki_task_fails_at_the_worker_boundary() {
        let db = lifecycle_db().await;
        let task = stored_task(
            &db,
            Some(1),
            "github",
            "git://does-not-resolve.invalid/importer/imported.git",
            "pending",
            false,
            true,
        )
        .await;
        let directory = tempfile::tempdir().expect("temporary repository root");
        let repo_root = directory.path().join("repos");
        let http_origin = "http://does-not-resolve.invalid".to_string();
        let trusted_origins =
            crate::import::trust::TrustedImportOrigins::parse(std::slice::from_ref(&http_origin))
                .expect("HTTP reachability exception");
        let transport_policy =
            crate::import::trust::ImportTransportPolicy::parse(std::slice::from_ref(&http_origin))
                .expect("HTTP confidentiality exception");

        let error = run_import(
            &db,
            &task,
            &repo_root,
            None,
            &trusted_origins,
            &transport_policy,
        )
        .await
        .expect_err("a legacy native-Git task must fail before its wiki path");

        let typed = error
            .downcast_ref::<crate::error::InvalidRequest>()
            .expect("the worker reported transport policy, not a later API or git failure");
        assert!(typed.message.contains("git://"));
        assert!(typed.message.contains("HTTP-only"));
        let stored = import_task_ops::find_by_id(&db, task.id)
            .await
            .expect("re-read stored import task")
            .expect("stored import task still exists");
        assert_eq!(stored.status, "pending");
        assert_eq!(stored.progress, 0);
        assert_eq!(stored.stage, None);
        assert!(
            !repo_root.exists(),
            "the rejected legacy task reached repository or wiki staging"
        );
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
        let trusted_origins = crate::import::trust::TrustedImportOrigins::default();
        let error = clone_into_target(
            &db,
            &task,
            1,
            "https://example.invalid/importer/imported.git",
            &trusted_origins,
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
    use crate::test_support::CapturedLogs;
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

    pub(super) async fn importing_user() -> DatabaseConnection {
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

    pub(super) async fn task_for(
        db: &DatabaseConnection,
        source_url: &str,
        target_name: &str,
    ) -> ImportTask {
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
        clone_into_target_for_test(
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
        clone_into_target_for_test(
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
        clone_into_target_for_test(
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

    /// card_48a0f3d7b1f6: `git clone --bare` copies a symbolic `HEAD`
    /// verbatim, so an upstream whose `HEAD` names a branch nobody created
    /// arrives with a full history behind an unresolvable `HEAD`. There is no
    /// branch to adopt — but leaving without a word is what made the resulting
    /// `409` on every read of the repository unattributable to the import.
    #[tokio::test]
    async fn an_unborn_head_over_existing_branches_is_reported_not_passed_over() {
        let directory = tempfile::tempdir().expect("tempdir");
        let upstream = directory.path().join("upstream.git");
        upstream_with_a_commit_on(&upstream, "master");
        // The desync itself: HEAD names `main`, the history is on `master`.
        git(
            &["symbolic-ref", "HEAD", "refs/heads/main"],
            Some(&upstream),
        );
        let source_url = upstream.to_string_lossy().to_string();

        let db = importing_user().await;
        let repo_root = directory.path().join("repo_root");
        let task = task_for(&db, &source_url, "desynced").await;

        let repo_id = resolve_or_create_target_repo(&db, None, "importer", "desynced", &repo_root)
            .await
            .expect("the import creates the target it was accepted for");

        let mut stats = ImportStats::default();
        let (logs, guard) = CapturedLogs::capture();
        clone_into_target_for_test(
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
        .expect("the clone runs — the bytes arrived, only HEAD is unusable");
        drop(guard);

        let target = repo_root.join("importer/desynced.git");
        let advertisement =
            rg_git::ref_advertisement::collect(&target).expect("read the imported repository");
        assert!(
            advertisement.head_oid.is_none(),
            "the clone resolved HEAD after all, so this test no longer covers the unborn case"
        );
        assert_eq!(
            advertisement.head_target.as_deref(),
            Some("refs/heads/main"),
            "the clone did not bring the upstream's broken HEAD, so this test proves nothing"
        );

        assert_eq!(
            rg_db::ops::repo_ops::find_by_id(&db, repo_id)
                .await
                .expect("read the imported row")
                .expect("the target still exists")
                .default_branch,
            "main",
            "an unborn HEAD is no branch to adopt — the column must keep what creation wrote"
        );

        let rendered = logs.rendered();
        assert!(
            rendered.contains("refs/heads/main") && rendered.contains("master"),
            "the import passed over a repository it left unreadable without naming either \
             side of the desync: {rendered}"
        );
    }

    /// The upstream this branch exists for: nothing was ever pushed to it, so
    /// an unborn `HEAD` is the honest state and there is nothing to report.
    #[tokio::test]
    async fn an_upstream_with_no_refs_at_all_is_still_imported_in_silence() {
        let directory = tempfile::tempdir().expect("tempdir");
        let upstream = directory.path().join("upstream.git");
        let upstream_arg = upstream.to_str().expect("UTF-8 bare path");
        git(&["init", "-q", "--bare", "-b", "main", upstream_arg], None);
        let source_url = upstream.to_string_lossy().to_string();

        let db = importing_user().await;
        let repo_root = directory.path().join("repo_root");
        let task = task_for(&db, &source_url, "pristine").await;

        let repo_id = resolve_or_create_target_repo(&db, None, "importer", "pristine", &repo_root)
            .await
            .expect("the import creates the target it was accepted for");

        let mut stats = ImportStats::default();
        let (logs, guard) = CapturedLogs::capture();
        clone_into_target_for_test(
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
        .expect("cloning an empty upstream is not a failure");
        drop(guard);

        assert_eq!(
            rg_db::ops::repo_ops::find_by_id(&db, repo_id)
                .await
                .expect("read the imported row")
                .expect("the target still exists")
                .default_branch,
            "main"
        );
        let rendered = logs.rendered();
        assert!(
            rendered.is_empty(),
            "an upstream with no history at all was reported as a broken one: {rendered}"
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
        clone_into_target_for_test(
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
    #[tokio::test]
    async fn clone_repo_names_the_directory_it_could_not_create() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file cannot host `<owner>/`, so `create_dir_all` fails
        // before any git subprocess is spawned.
        let repo_root = dir.path().join("repo_root");
        std::fs::write(&repo_root, b"not a directory").unwrap();
        let remote = crate::net::GuardedGitRemote::unbound_for_test(
            "https://example.invalid/alice/site.git",
        );

        let error = clone_repo(&remote, &repo_root, "alice", "site", None)
            .await
            .expect_err("repo_root is a file");
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&repo_root.join("alice").display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("[server].repo_root"), "{rendered}");
    }

    /// card_970b360d707d: an import installs its clone with two renames inside
    /// one directory, and rolls the first back if the second fails. The
    /// rollback runs only in a process that lives to run it — killed in
    /// between, the `repos` row is live, names `<owner>/<name>.git`, and there
    /// is nothing on that path. `recover_stuck_imports` fails the import task
    /// row and touches neither the directory nor the repository, so the broken
    /// repository and the failed import are two independent facts and the owner
    /// sees only the second.
    #[tokio::test]
    async fn an_import_killed_between_its_two_renames_gets_the_repository_back() {
        let directory = tempfile::tempdir().expect("tempdir");
        let repo_root = directory.path().join("repo_root");
        let target_dir = repo_root.join("alice/site.git");
        let retired = repo_root.join("alice/.site.git.replaced-aaaaaaaaaaaa");
        std::fs::create_dir_all(&target_dir).expect("the skeleton the creation left");
        std::fs::write(target_dir.join("HEAD"), b"ref: refs/heads/main").expect("skeleton HEAD");

        // The state the kill leaves: the entry the import wrote before it moved
        // anything, and the first of its two renames.
        let token = "0123456789abcdef0123456789abcdef";
        let journal = crate::deletion_recovery::journal_at(&repo_root);
        crate::deletion_recovery::open(
            &journal,
            token,
            "the repository skeleton an import replaced",
            vec![
                crate::deletion_recovery::StagedBytes::path(&target_dir, &retired)
                    .expect("declare the skeleton"),
            ],
        )
        .await
        .expect("open the journal entry");
        std::fs::rename(&target_dir, &retired).expect("move the skeleton aside");
        assert!(
            !target_dir.exists(),
            "the fixture did not reproduce the state it is testing"
        );

        let report = crate::deletion_recovery::recover_interrupted_deletions_at(
            &repo_root,
            std::time::Duration::ZERO,
        )
        .await;

        assert_eq!(
            std::fs::read_to_string(target_dir.join("HEAD")).expect("the repository is back"),
            "ref: refs/heads/main",
            "the live row still names a path with nothing on it"
        );
        assert!(
            !retired.exists(),
            "the recovered skeleton was copied rather than moved back"
        );
        assert_eq!(report.restored, 1, "{report:?}");
    }

    /// The wiring, not the pass: an import that cannot record what it is about
    /// to move must refuse before the first rename and leave the repository
    /// exactly as it found it — otherwise the entry the test above acts on is
    /// one only a fixture ever writes.
    ///
    /// The fault is a file where the journal prefix has to be a directory, the
    /// one way to make the local backend refuse a write without reaching into
    /// production code.
    #[tokio::test]
    async fn an_import_that_cannot_be_recorded_leaves_the_repository_alone() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("source.git");
        let git = global_gateway().as_ref().expect("git");
        git.run_or_bail(&["init", "--bare", &source.to_string_lossy()], None)
            .expect("source repo");

        let repo_root = directory.path().join("repo_root");
        // A real ref-less bare skeleton, the shape `create_repo` leaves and the
        // only one `target_holds_history` lets an import clone over.
        let target_dir = repo_root.join("alice/site.git");
        git.run_or_bail(&["init", "--bare", &target_dir.to_string_lossy()], None)
            .expect("the skeleton the creation left");
        std::fs::write(target_dir.join("forgekeep-skeleton"), b"the skeleton")
            .expect("mark the skeleton");
        std::fs::create_dir_all(repo_root.join("_deleted")).expect("journal parent");
        std::fs::write(repo_root.join("_deleted/journal"), b"not a directory")
            .expect("block the journal prefix");

        let remote = crate::net::GuardedGitRemote::unbound_for_test(&source.to_string_lossy());
        let error = clone_repo(&remote, &repo_root, "alice", "site", None)
            .await
            .expect_err("an import that cannot record its move must not make it");

        assert_eq!(
            std::fs::read_to_string(target_dir.join("forgekeep-skeleton"))
                .expect("the skeleton is untouched"),
            "the skeleton",
            "the refused import moved the repository it had not recorded: {error:#}"
        );
        let leftovers: Vec<_> = std::fs::read_dir(repo_root.join("alice"))
            .expect("the owner directory")
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .filter(|name| name != "site.git")
            .collect();
        assert!(
            leftovers.is_empty(),
            "the refused import left its clone behind as bytes nothing names: {leftovers:?}"
        );
    }

    /// An import that installs its clone marks the skeleton it replaced retired
    /// *before* dropping the entry, and keeps the entry when it cannot.
    ///
    /// Without the marker, a restart between the second rename and the entry
    /// being cleared would have the startup pass read this as an install that
    /// never happened. It would refuse to overwrite the installed clone — which
    /// is the safe answer, not a correct one — and leave both for an operator.
    /// The marker write is faulted here rather than observed, because a
    /// successful import clears both halves and leaves nothing to look at.
    #[tokio::test]
    async fn an_import_marks_the_skeleton_retired_before_forgetting_it() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("source.git");
        let git = global_gateway().as_ref().expect("git");
        git.run_or_bail(&["init", "--bare", &source.to_string_lossy()], None)
            .expect("source repo");

        let repo_root = directory.path().join("repo_root");
        let target_dir = repo_root.join("alice/site.git");
        git.run_or_bail(&["init", "--bare", &target_dir.to_string_lossy()], None)
            .expect("the skeleton the creation left");
        std::fs::create_dir_all(repo_root.join("_deleted")).expect("journal parent");
        std::fs::write(repo_root.join("_deleted/committed"), b"not a directory")
            .expect("block the commit-marker prefix");

        let remote = crate::net::GuardedGitRemote::unbound_for_test(&source.to_string_lossy());
        let outcome = clone_repo(&remote, &repo_root, "alice", "site", None)
            .await
            .expect("a marker that cannot be written is reported, not fatal");
        assert_eq!(outcome, CloneOutcome::Cloned);

        // The clone is installed, and the entry is deliberately still open: an
        // import that could not say "the skeleton is retired" must not be the
        // one to say "there is nothing to look at either".
        let entries = std::fs::read_dir(repo_root.join("_deleted/journal"))
            .expect("the journal entry outlives an import that could not mark itself")
            .count();
        assert_eq!(
            entries, 1,
            "the import dropped the only record of a skeleton it could not mark retired"
        );
        assert!(
            std::fs::read_to_string(target_dir.join("config"))
                .expect("the clone is installed")
                .contains(&source.to_string_lossy().to_string()),
            "the clone did not reach the target path"
        );
    }

    #[tokio::test]
    async fn native_git_is_refused_before_the_clone_touches_its_destination() {
        let dir = tempfile::tempdir().expect("temporary repository root");
        let repo_root = dir.path().join("repo_root");
        let remote = crate::net::GuardedGitRemote::unbound_for_test(
            "git://does-not-resolve.invalid/alice/site.git",
        );

        let error = clone_repo(&remote, &repo_root, "alice", "site", None)
            .await
            .expect_err("the final clone sink must refuse native Git");

        assert!(
            error
                .downcast_ref::<crate::error::InvalidRequest>()
                .is_some(),
            "the sink failed for some reason other than transport policy: {error:#}"
        );
        assert!(
            !repo_root.exists(),
            "the rejected clone reached destination setup before transport policy"
        );
    }
}

#[cfg(test)]
mod clone_credential_tests {
    use super::*;
    use crate::test_support::{spawn_authenticating_remote, spawn_rebinding_git_remotes};
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
    #[tokio::test]
    async fn the_token_is_absent_from_the_cloned_repository_config() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("source.git");
        let git = global_gateway().as_ref().expect("git");
        git.run_or_bail(&["init", "--bare", &source.to_string_lossy()], None)
            .expect("source repo");

        let repo_root = directory.path().join("repo_root");
        let credentials =
            source_credentials("github", "https://github.com/o/r.git", TOKEN).expect("a token");
        let remote = crate::net::GuardedGitRemote::unbound_for_test(&source.to_string_lossy());
        clone_repo(&remote, &repo_root, "alice", "site", Some(&credentials))
            .await
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
    #[tokio::test]
    async fn a_private_source_receives_the_supplied_token() {
        let (address, _requests, seen) = spawn_authenticating_remote();
        let directory = tempfile::tempdir().expect("tempdir");
        let credentials =
            source_credentials("github", "https://github.com/o/r.git", TOKEN).expect("a token");
        let remote = crate::net::GuardedGitRemote::unbound_for_test(&format!(
            "http://{address}/upstream.git"
        ));

        let outcome = clone_repo(
            &remote,
            directory.path(),
            "alice",
            "site",
            Some(&credentials),
        )
        .await;
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
    #[tokio::test]
    async fn an_authenticating_source_fails_fast_instead_of_waiting_for_a_login() {
        let (address, _requests, _seen) = spawn_authenticating_remote();
        let directory = tempfile::tempdir().expect("tempdir");
        let remote = crate::net::GuardedGitRemote::unbound_for_test(&format!(
            "http://{address}/upstream.git"
        ));

        let started = std::time::Instant::now();
        let outcome = clone_repo(&remote, directory.path(), "alice", "site", None).await;
        let elapsed = started.elapsed();

        assert!(outcome.is_err(), "the stub remote demands authentication");
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "the clone waited {elapsed:?} — that is a prompt, not a refusal"
        );
    }

    #[tokio::test]
    async fn import_clone_connects_only_to_the_checked_dns_answer() {
        use std::sync::atomic::Ordering;

        let sinks = spawn_rebinding_git_remotes();
        let remote =
            crate::net::guard_git_url_with_addresses(&sinks.url, vec![sinks.checked_ip], |ip| {
                ip == "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
            })
            .expect("the public-answer stand-in is allowed");
        let directory = tempfile::tempdir().expect("tempdir");

        let outcome = clone_repo(&remote, directory.path(), "alice", "site", None).await;
        assert!(
            outcome.is_err(),
            "the checked sink deliberately returns 403"
        );
        assert!(
            sinks.checked_requests.load(Ordering::SeqCst) > 0,
            "git ignored the checked DNS answer"
        );
        assert_eq!(
            sinks.rebound_requests.load(Ordering::SeqCst),
            0,
            "git resolved localhost again and reached the rebound sink"
        );
    }
}

#[cfg(test)]
mod failure_reason_tests {
    use super::*;

    /// The half that must not survive: an import that failed for a reason of
    /// ours settles as fixed text.
    ///
    /// The error here is the real thing rather than a hand-written string — a
    /// `git clone` of a source that is not there, run through the same
    /// `ensure_success` the import path uses — so the first assertion is what
    /// gives this test its teeth: the chain really does carry `git -C` and the
    /// server-side path, and it is the split that keeps them out of the row the
    /// task's owner reads.
    #[tokio::test]
    async fn a_failed_clone_does_not_hand_its_command_line_to_the_task_owner() {
        let directory = tempfile::tempdir().expect("tempdir");
        let repo_root = directory.path().join("repo_root");
        let absent = directory.path().join("no-such-upstream.git");
        let source_url = absent.to_string_lossy().to_string();

        let db = super::clone_effect_tests::importing_user().await;
        let task = super::clone_effect_tests::task_for(&db, &source_url, "target").await;
        let repo_id = resolve_or_create_target_repo(&db, None, "importer", "target", &repo_root)
            .await
            .expect("the import creates the target it was accepted for");

        let mut stats = ImportStats::default();
        let error = clone_into_target_for_test(
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
        .expect_err("cloning a source that does not exist fails");

        // All three of these are what the chain must keep and the column must
        // not: the invocation `build_command_line` assembled, the server-side
        // destination path, and git's own stderr.
        let chain = format!("{error:#}");
        let server_path = repo_root.to_string_lossy().to_string();
        for kept in ["clone --bare", server_path.as_str(), "fatal:"] {
            assert!(
                chain.contains(kept),
                "the chain no longer carries {kept:?}, so this test proves nothing: {chain}"
            );
        }

        let reason = failure_reason(&error, None);
        assert_eq!(reason, UNSPECIFIED_IMPORT_FAILURE);
        for leaked in ["clone --bare", server_path.as_str(), "fatal:", &source_url] {
            assert!(
                !reason.contains(leaked),
                "the persisted reason handed {leaked:?} to the task owner: {reason}"
            );
        }
    }

    /// The half that must survive: a source that refused for a reason the
    /// importer can act on keeps its own words, or the progress page becomes a
    /// spinner that ends in "ask the operator" for every wrong URL.
    #[test]
    fn a_typed_refusal_still_reaches_the_task_owner() {
        let refusal = crate::import::source_api_refusal(
            "GitHub",
            reqwest::StatusCode::NOT_FOUND,
            r#"{"message":"Not Found","documentation_url":"https://docs.github.com/rest"}"#,
        );
        let reason = failure_reason(&refusal, None);

        assert!(
            reason.contains("no repository at that address"),
            "the source's refusal did not reach the task owner: {reason}"
        );
        // Ours, not the source's: the response body stays in the chain.
        assert!(!reason.contains("documentation_url"), "{reason}");
        assert!(
            format!("{refusal:#}").contains("documentation_url"),
            "the operator lost the source's own answer"
        );

        let refused_token = crate::import::source_api_refusal(
            "GitLab",
            reqwest::StatusCode::UNAUTHORIZED,
            "401 Unauthorized",
        );
        assert!(
            failure_reason(&refused_token, None).contains("refused the import token"),
            "a rejected token did not reach the task owner"
        );

        // A status with no class of its own is ours, like any other failure.
        let unclassified = crate::import::source_api_refusal(
            "GitHub",
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "upstream exploded",
        );
        assert_eq!(
            failure_reason(&unclassified, None),
            UNSPECIFIED_IMPORT_FAILURE
        );
    }

    /// The token no longer rides in the clone URL, but it still reaches the
    /// platform's API and `git`, and both quote what they were given back into
    /// their error text. Whatever it rode in on, it must not reach the `error`
    /// column — that column is served to the browser on every status poll.
    ///
    /// The split above is not this guarantee: it is the typed half that carries
    /// a message the source influenced, and that half is exactly where masking
    /// is the last net left.
    #[test]
    fn the_token_is_taken_back_out_of_a_failure() {
        let error = crate::error::invalid_request(
            "the source refused https://oauth2:glpat-SECRET-TOKEN@gitlab.com/a/b.git",
        );
        let reason = failure_reason(&error, Some("glpat-SECRET-TOKEN"));

        assert!(
            !reason.contains("glpat-SECRET-TOKEN"),
            "the source token survived into the persisted reason: {reason}"
        );
        // Masking, not swallowing: the rest of the message comes through.
        assert!(reason.contains("the source refused"), "{reason}");
        assert!(reason.contains("gitlab.com/a/b.git"), "{reason}");
    }

    /// An anonymous import has nothing to mask, and its reason must come
    /// through untouched — a `***` there would be a mystery, not a redaction.
    #[test]
    fn an_anonymous_import_keeps_its_reason_verbatim() {
        let error = crate::error::not_found("repository");
        assert_eq!(failure_reason(&error, None), "repository not found");
        assert_eq!(failure_reason(&error, Some("")), "repository not found");
    }
}

/// card_ea33f26fe2f6: a wiki clone that comes back holding nothing has two very
/// different causes, and only one of them is harmless. The clone alone cannot
/// tell them apart, so the two tests here are the pair that matters: the loss is
/// named, and the wiki that never had a page stays silent.
#[cfg(test)]
mod wiki_clone_emptiness_tests {
    use super::*;
    use crate::test_support::{spawn_rebinding_git_remotes, CapturedLogs};

    fn git(args: &[&str], cwd: Option<&Path>) {
        global_gateway()
            .as_ref()
            .expect("git gateway")
            .run(args, cwd)
            .expect("run git")
            .ensure_success()
            .expect("git command succeeds");
    }

    /// A bare wiki carrying a page on `master` whose `HEAD` names `main` — the
    /// branch nobody created. This is an upstream whose wiki default branch was
    /// renamed on the platform while `HEAD` was left behind, and the state the
    /// import used to report as "this wiki has no pages".
    fn wiki_with_a_page_behind_a_broken_head(bare: &Path) -> String {
        let bare_arg = bare.to_str().expect("UTF-8 wiki path");
        git(&["init", "-q", "--bare", "-b", "master", bare_arg], None);

        let worktree = tempfile::tempdir().expect("wiki worktree");
        let path = worktree.path();
        let path_arg = path.to_str().expect("UTF-8 worktree path");
        git(&["init", "-q", "-b", "master", path_arg], None);
        git(&["config", "user.name", "Wiki fixture"], Some(path));
        git(
            &["config", "user.email", "wiki-fixture@example.invalid"],
            Some(path),
        );
        std::fs::write(path.join("Home.md"), "the page that must not vanish\n")
            .expect("write the wiki page");
        git(&["add", "."], Some(path));
        git(&["commit", "-qm", "wiki page"], Some(path));
        git(&["remote", "add", "origin", bare_arg], Some(path));
        git(&["push", "-q", "origin", "master"], Some(path));

        // The break itself: HEAD names `main`, the page is on `master`.
        git(&["symbolic-ref", "HEAD", "refs/heads/main"], Some(bare));

        bare_arg.to_string()
    }

    #[tokio::test]
    async fn wiki_clone_connects_only_to_the_checked_dns_answer() {
        use std::sync::atomic::Ordering;

        let sinks = spawn_rebinding_git_remotes();
        let remote =
            crate::net::guard_git_url_with_addresses(&sinks.url, vec![sinks.checked_ip], |ip| {
                ip == "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
            })
            .expect("the public-answer stand-in is allowed");
        let directory = tempfile::tempdir().expect("tempdir");
        let staging = directory.path().join("wiki.git.importing");
        let db = crate::test_support::migrated_memory_database().await;

        let imported = import_wiki_pages_from_destination(&db, 7, &remote, &staging, None, None)
            .await
            .expect("a missing wiki remains non-fatal");
        assert_eq!(imported, 0);
        assert!(
            sinks.checked_requests.load(Ordering::SeqCst) > 0,
            "git ignored the checked DNS answer"
        );
        assert_eq!(
            sinks.rebound_requests.load(Ordering::SeqCst),
            0,
            "the wiki clone resolved localhost again and reached the rebound sink"
        );
    }

    #[tokio::test]
    async fn native_git_is_a_fatal_policy_refusal_before_wiki_staging() {
        let directory = tempfile::tempdir().expect("tempdir");
        let staging = directory
            .path()
            .join("repo_root/importer/target.git.wiki.importing");
        let db = crate::test_support::migrated_memory_database().await;
        let trusted_origins = crate::import::trust::TrustedImportOrigins::default();

        let error = import_wiki_pages(
            &db,
            7,
            "git://does-not-resolve.invalid/importer/target.wiki.git",
            &trusted_origins,
            &staging,
            None,
            None,
        )
        .await
        .expect_err("transport policy is not an optional missing-wiki outcome");

        assert!(
            error
                .downcast_ref::<crate::error::InvalidRequest>()
                .is_some(),
            "the wiki sink failed for some reason other than transport policy: {error:#}"
        );
        assert!(
            !staging.parent().expect("wiki staging parent").exists(),
            "the rejected wiki clone created its staging namespace"
        );
    }

    #[tokio::test]
    async fn a_wiki_whose_head_names_no_branch_does_not_lose_its_pages_in_silence() {
        let directory = tempfile::tempdir().expect("tempdir");
        let wiki_url = wiki_with_a_page_behind_a_broken_head(&directory.path().join("source.wiki"));
        let db = crate::test_support::migrated_memory_database().await;
        let staging = directory
            .path()
            .join("repo_root/importer/target.git.wiki.importing");

        let (logs, guard) = CapturedLogs::capture();
        let imported =
            import_wiki_pages_from_local_path(&db, 7, Path::new(&wiki_url), &staging, None, None)
                .await
                .expect("a wiki that hands over no page must not fail the import");
        drop(guard);

        assert_eq!(imported, 0, "there was no page to import from that HEAD");
        let rendered = logs.rendered();
        assert!(
            rendered.contains("master"),
            "the branch the pages are actually on was not named: {rendered}"
        );
        assert!(
            rendered.contains("HEAD"),
            "the warning does not say what is wrong with the source: {rendered}"
        );
        assert!(
            !staging.exists(),
            "the wiki clone was left behind: {}",
            staging.display()
        );
    }

    /// The other half of the pair: a wiki the platform created and nobody ever
    /// wrote to is not a loss and must not spend an operator's attention.
    #[tokio::test]
    async fn a_wiki_that_was_never_written_to_reports_nothing_at_all() {
        let directory = tempfile::tempdir().expect("tempdir");
        let wiki = directory.path().join("empty.wiki");
        let wiki_url = wiki.to_str().expect("UTF-8 wiki path").to_string();
        git(&["init", "-q", "--bare", "-b", "main", &wiki_url], None);
        let db = crate::test_support::migrated_memory_database().await;
        let staging = directory
            .path()
            .join("repo_root/importer/empty.git.wiki.importing");

        let (logs, guard) = CapturedLogs::capture();
        let imported =
            import_wiki_pages_from_local_path(&db, 8, Path::new(&wiki_url), &staging, None, None)
                .await
                .expect("an empty wiki must not fail the import");
        drop(guard);

        assert_eq!(imported, 0);
        assert_eq!(
            logs.rendered(),
            "",
            "a wiki that never had a page was reported as a problem"
        );
    }
}

/// card_5e0f9bd8877c: `collect_wiki_pages` used to bound each page against
/// [`MAX_WIKI_PAGE_BYTES`] and then push every page it read into one
/// `Vec<SourceWikiPage>` with no ceiling of its own. A thousand pages a byte
/// under the per-file cap cost a thousand times what that cap promised. Both
/// halves are asserted here: the budget refuses on aggregate and on count
/// with named messages, and the production loop still charges each candidate
/// against it BEFORE the `cat-file blob` that reads the object it bounds.
#[cfg(test)]
mod wiki_page_budget_tests {
    use super::*;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    #[test]
    fn the_aggregate_byte_budget_refuses_a_set_that_outgrows_it_with_a_named_limit() {
        let mut budget = WikiPageBudget::new();
        // Two pages that fit individually but cross the aggregate together.
        // Charging the whole aggregate on the first entry lets the second one
        // be the drop and lets the assertion be about the aggregate rather
        // than about how big one page is.
        budget
            .charge_bytes("Home.md", MAX_WIKI_TOTAL_BYTES)
            .expect("the first page must fit while the budget is untouched");
        let error = budget
            .charge_bytes("Getting-Started.md", 1)
            .expect_err("the second page crosses the aggregate byte budget");
        assert!(
            error
                .downcast_ref::<crate::error::InvalidRequest>()
                .is_some(),
            "the source wiki's shape is the client's to fix, so this is not a 5xx: {error:#}"
        );
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&format!("{MAX_WIKI_TOTAL_BYTES}-byte total limit")),
            "the refusal does not name the aggregate limit: {rendered}"
        );
        assert!(
            rendered.contains("Getting-Started.md"),
            "the refusal does not name the page that crossed the budget: {rendered}"
        );
    }

    #[test]
    fn the_page_count_backstop_refuses_a_set_of_empty_pages_with_a_named_limit() {
        // Empty pages cost nothing against the byte budget and everything
        // against the count backstop. Draining the whole count and one more
        // is what the file-count backstop is for.
        let mut budget = WikiPageBudget::new();
        for index in 0..MAX_WIKI_PAGE_COUNT {
            budget
                .charge_page(&format!("Page{index:04}.md"))
                .expect("every page inside the count backstop must fit");
        }
        let error = budget
            .charge_page("Overflow.md")
            .expect_err("the page after the count backstop must be refused");
        assert!(
            error
                .downcast_ref::<crate::error::InvalidRequest>()
                .is_some(),
            "the source wiki's shape is the client's to fix, so this is not a 5xx: {error:#}"
        );
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&format!("more than {MAX_WIKI_PAGE_COUNT} pages")),
            "the refusal does not name the count limit: {rendered}"
        );
        assert!(
            rendered.contains("Overflow.md"),
            "the refusal does not name the page that crossed the backstop: {rendered}"
        );
    }

    /// The ordering is the whole fix, and it is invisible to the two tests
    /// above: an aggregate refused after `cat-file blob` has already collected
    /// the blob costs exactly the memory the ceiling was declared to save.
    /// `cat-file` has no cap of its own, and neither has the gateway that
    /// collects its output, so the order is asserted where it lives.
    #[test]
    fn the_wiki_budget_is_spent_before_each_cat_file_blob() {
        let code = rust_source::production_rust_code_only(include_str!("service.rs"));
        let start = code
            .find("fn collect_wiki_pages(")
            .expect("`collect_wiki_pages` must still be the wiki reader");
        let body = &code[start..];
        let end = body[1..]
            .find("\nfn ")
            .or_else(|| body[1..].find("\nasync fn "))
            .or_else(|| body[1..].find("\npub "))
            .map(|offset| offset + 1)
            .unwrap_or(body.len());
        let body = &body[..end];

        let charge_page = body.find("charge_page(").expect(
            "`collect_wiki_pages` no longer charges the page-count backstop: nothing bounds \
             the number of pages held in memory at once",
        );
        let charge_bytes = body.find("charge_bytes(").expect(
            "`collect_wiki_pages` no longer charges the aggregate byte budget: nothing bounds \
             the total memory the returned `Vec<SourceWikiPage>` holds",
        );
        // String literals are blanked in the code-only view, so `"cat-file"`
        // cannot be found; the anchor is the `blob.ensure_success(` chain, an
        // identifier sequence that sits right after — and only after — the
        // `git.run(&["cat-file", "blob", …])` read this budget must precede.
        // The first `git.run(` in this function is the `ls-tree` listing that
        // FEEDS the budget, so it cannot serve as the anchor here.
        let cat_file_read = body.find("blob.ensure_success(").expect(
            "`collect_wiki_pages` no longer post-checks its `cat-file blob` read — the anchor \
             this ordering is asserted against has moved, so the assertion below proves nothing",
        );
        assert!(
            charge_page < cat_file_read && charge_bytes < cat_file_read,
            "`collect_wiki_pages` charges the wiki budget only after `git cat-file blob` has \
             collected the page: a set of pages 16 MiB over the aggregate is then materialised \
             in full and refused afterwards"
        );
    }
}
