//! Import pipeline service — orchestrates full repository migration.
//!
//! Supports importing from GitHub and GitLab, including:
//! - Repository cloning (git clone --bare) + ForgeKeep DB registration
//! - Labels and milestones
//! - Issues with comments
//! - Pull/Merge requests with reviews/comments
//! - Releases
//!
//! Generic Git and Gitea imports currently clone the repository only.
//!
//! The import runs asynchronously and updates progress in the
//! import_tasks database table.
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

use anyhow::{Context, Result};
use chrono::Utc;
use rg_git::cli_gateway::global_gateway;
use rg_git::credentials::{credential_invocation, GitCredentials};
use sea_orm::{ActiveValue::Set, DatabaseConnection};
use std::collections::HashMap;
use std::path::Path;

use rg_db::entities::import_task::{self, Model as ImportTask};
use rg_db::entities::{label, milestone};
use rg_db::ops::{
    import_task_ops, issue_comment_ops, issue_ops, label_ops, milestone_ops, org_ops,
    pr_review_ops, pull_request_ops, user_ops,
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
) -> Result<ImportStats> {
    let mut stats = ImportStats::default();

    // SSRF guard: the platform runners spawn `git clone` (and API calls) against
    // this user-supplied URL. Reject internal/loopback/metadata hosts and
    // non-git transports (`file://`, `ext::`, …) before any subprocess runs —
    // the git twin of the mirror-sync guard. For GitLab this validates the
    // project URL; the actual API-derived clone URL is guarded again below.
    crate::net::guard_git_url(&task.source_url).await?;

    let auth_token = auth_token.unwrap_or("");

    match task.platform.as_str() {
        "github" => run_github_import(db, task, repo_root, auth_token, &mut stats).await?,
        "gitlab" => run_gitlab_import(db, task, repo_root, auth_token, &mut stats).await?,
        "gitea" | "git" => run_git_import(db, task, repo_root, auth_token, &mut stats).await?,
        other => anyhow::bail!("unsupported platform: {other}"),
    }

    Ok(stats)
}

// ═══════════════════════════════════════════════════════════════════════
// Generic Git / Gitea import
// ═══════════════════════════════════════════════════════════════════════

async fn run_git_import(
    db: &DatabaseConnection,
    task: &ImportTask,
    repo_root: &Path,
    auth_token: &str,
    stats: &mut ImportStats,
) -> Result<()> {
    let repo_id =
        resolve_or_create_target_repo(db, &task.target_owner, &task.target_name, repo_root).await?;
    import_task_ops::set_repo_id(db, task.id, repo_id)
        .await
        .context("failed to link the import task to its target repository")?;

    if task.import_repo {
        update_stage(db, task.id, "cloning", 0, "Cloning repository...").await?;
        clone_repo(
            &task.source_url,
            repo_root,
            &task.target_owner,
            &task.target_name,
            source_credentials(&task.platform, auth_token).as_ref(),
        )?;
        stats.repo_cloned = true;
        update_stage(db, task.id, "importing", 90, "Repository cloned").await?;
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
    let client = GitHubClient::new(token.to_string(), None)?;

    // Parse owner/repo from source URL (https://github.com/owner/repo)
    let (gh_owner, gh_repo) = parse_github_url(&task.source_url)?;

    // Resolve (or create) the target repo in ForgeKeep DB
    let repo_id =
        resolve_or_create_target_repo(db, &task.target_owner, &task.target_name, repo_root).await?;

    // Update task with repo_id
    import_task_ops::set_repo_id(db, task.id, repo_id)
        .await
        .context("failed to link the import task to its target repository")?;

    // Step 1: Clone repository
    if task.import_repo {
        update_stage(db, task.id, "cloning", 0, "Cloning repository...").await?;
        clone_repo(
            &task.source_url,
            repo_root,
            &task.target_owner,
            &task.target_name,
            source_credentials(&task.platform, token).as_ref(),
        )?;
        stats.repo_cloned = true;
        update_stage(db, task.id, "importing", 10, "Repository cloned").await?;
    }

    // Build user mapping from all referenced users
    let user_map = build_github_user_map(&client, &gh_owner, &gh_repo, task).await?;

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
                &user_map,
                &milestone_map,
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
                &user_map,
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
        stats.releases_imported =
            import_github_releases(db, repo_id, &releases, &user_map, repo_root).await?;
        update_stage(
            db,
            task.id,
            "importing",
            90,
            &format!("Imported {} releases", stats.releases_imported),
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
) -> Result<()> {
    let client = GitLabClient::new(token.to_string(), None)?;

    // Extract project path from source URL (https://gitlab.com/group/project)
    let project_path = parse_gitlab_url(&task.source_url)?;

    // Resolve (or create) the target repo in ForgeKeep DB
    let repo_id =
        resolve_or_create_target_repo(db, &task.target_owner, &task.target_name, repo_root).await?;

    import_task_ops::set_repo_id(db, task.id, repo_id)
        .await
        .context("failed to link the import task to its target repository")?;

    // Step 1: Clone repository
    if task.import_repo {
        update_stage(db, task.id, "cloning", 0, "Cloning repository...").await?;
        let project = client.get_project(&project_path).await?;
        // The clone URL comes from the GitLab API response, not the user's
        // source_url — re-guard it (a malicious/compromised instance could point
        // `http_url_to_repo` at an internal host).
        crate::net::guard_git_url(&project.http_url_to_repo).await?;
        clone_repo(
            &project.http_url_to_repo,
            repo_root,
            &task.target_owner,
            &task.target_name,
            source_credentials(&task.platform, token).as_ref(),
        )?;
        stats.repo_cloned = true;
        update_stage(db, task.id, "importing", 10, "Repository cloned").await?;
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

    // Build user map
    let user_map = build_gitlab_user_map(&client, &project_path, task).await?;

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
                &user_map,
                &milestone_map,
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
            import_gitlab_mr(db, repo_id, mr, &notes, &user_map, &milestone_map).await?;
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
        stats.releases_imported =
            import_gitlab_releases(db, repo_id, &releases, &user_map, repo_root).await?;
        update_stage(
            db,
            task.id,
            "importing",
            90,
            &format!("Imported {} releases", stats.releases_imported),
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
async fn resolve_or_create_target_repo(
    db: &DatabaseConnection,
    target_owner: &str,
    target_name: &str,
    repo_root: &Path,
) -> Result<i64> {
    // Try to find existing repo via the repo service (handles user+org lookup)
    if let Some(repo) = crate::repo::service::find_repo_by_owner_name(db, target_owner, target_name)
        .await
        .unwrap_or(None)
    {
        tracing::info!(repo_id = repo.id, "Found existing target repo");
        return Ok(repo.id);
    }

    // Resolve owner: try user first, then org
    let (owner_id, org_id) =
        if let Some(user) = user_ops::find_by_username(db, target_owner).await? {
            (user.id, None)
        } else if let Some(org) = org_ops::get_org_by_name(db, target_owner).await? {
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
fn source_credentials(platform: &str, token: &str) -> Option<GitCredentials> {
    if token.is_empty() {
        return None;
    }
    let username = match platform {
        "github" => "x-access-token",
        _ => "oauth2",
    };
    Some(GitCredentials::token(username, token))
}

/// Clone a repository (bare) into the ForgeKeep repo root.
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
) -> Result<()> {
    let target_dir = repo_root.join(format!("{}/{}.git", owner, name));
    if target_dir.join("HEAD").exists() {
        tracing::info!(
            path = %target_dir.display(),
            "Repository already exists, skipping clone"
        );
        return Ok(());
    }

    let parent = target_dir
        .parent()
        .context("import target path has no parent directory")?;
    // The whole path is derived from `repo_root` inside this function, so a bare
    // `?` here hands the operator an `os error 13` that names neither the
    // directory the import tried to create nor the setting that moves it.
    std::fs::create_dir_all(parent)
        .map_err(|error| path_error("import target directory", parent, &error, REPO_ROOT_HINT))?;

    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let (credential_args, env) = credential_invocation(credentials);
    let destination = target_dir.to_string_lossy();
    let mut args: Vec<&str> = credential_args.iter().map(String::as_str).collect();
    args.extend(["clone", "--bare", source_url, &destination]);
    let env: Vec<(&str, &str)> = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    git.run_with_env(&args, None, &env)?
        .ensure_success()
        .context("git clone --bare")?;

    tracing::info!(path = %target_dir.display(), "Repository cloned");
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════
// URL parsing
// ═══════════════════════════════════════════════════════════════════════

fn parse_github_url(url: &str) -> Result<(String, String)> {
    let url = url.trim_end_matches('/').trim_end_matches(".git");
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() < 2 {
        anyhow::bail!("invalid GitHub URL: {url}");
    }
    let repo = parts[parts.len() - 1].to_string();
    let owner = parts[parts.len() - 2].to_string();
    Ok((owner, repo))
}

fn parse_gitlab_url(url: &str) -> Result<String> {
    let url = url.trim_end_matches('/').trim_end_matches(".git");
    if let Some(pos) = url.find("://") {
        let after_protocol = &url[pos + 3..];
        if let Some(slash_pos) = after_protocol.find('/') {
            let path = &after_protocol[slash_pos + 1..];
            return Ok(path.to_string());
        }
    }
    anyhow::bail!("invalid GitLab URL: {url}")
}

// ═══════════════════════════════════════════════════════════════════════
// User mapping
// ═══════════════════════════════════════════════════════════════════════

async fn build_github_user_map(
    client: &GitHubClient,
    owner: &str,
    repo: &str,
    task: &ImportTask,
) -> Result<HashMap<String, i64>> {
    let mut logins: std::collections::HashSet<String> = std::collections::HashSet::new();

    if task.import_issues {
        if let Ok(issues) = client.list_issues(owner, repo).await {
            for issue in &issues {
                if let Some(ref user) = issue.user {
                    logins.insert(user.login.clone());
                }
                for a in &issue.assignees {
                    logins.insert(a.login.clone());
                }
            }
        }
    }

    if task.import_pull_requests {
        if let Ok(prs) = client.list_pull_requests(owner, repo).await {
            for pr in &prs {
                if let Some(ref user) = pr.user {
                    logins.insert(user.login.clone());
                }
            }
        }
    }

    map_users(task, &logins)
}

async fn build_gitlab_user_map(
    client: &GitLabClient,
    project_id: &str,
    task: &ImportTask,
) -> Result<HashMap<String, i64>> {
    let mut usernames: std::collections::HashSet<String> = std::collections::HashSet::new();

    if let Ok(issues) = client.list_issues(project_id).await {
        for issue in &issues {
            if let Some(ref author) = issue.author {
                usernames.insert(author.username.clone());
            }
        }
    }

    if let Ok(mrs) = client.list_merge_requests(project_id).await {
        for mr in &mrs {
            if let Some(ref author) = mr.author {
                usernames.insert(author.username.clone());
            }
        }
    }

    map_users(task, &usernames)
}

/// Map external user logins/usernames to local ForgeKeep user IDs.
/// Falls back to the importing user if no match is found.
fn map_users(
    task: &ImportTask,
    external_users: &std::collections::HashSet<String>,
) -> Result<HashMap<String, i64>> {
    let mut mapping = HashMap::new();
    for user in external_users {
        mapping.entry(user.clone()).or_insert(task.user_id);
    }
    Ok(mapping)
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

        let state = match gm.state.as_str() {
            "open" => "open",
            "closed" => "closed",
            _ => "open",
        };

        let due_date = parse_opt_datetime(&gm.due_on);

        let model = milestone::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            title: Set(gm.title.clone()),
            description: Set(gm.description.clone()),
            state: Set(state.to_string()),
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
    user_map: &HashMap<String, i64>,
    milestone_map: &HashMap<String, i64>,
) -> Result<()> {
    // Resolve author
    let author_id = issue
        .user
        .as_ref()
        .and_then(|u| user_map.get(&u.login))
        .copied()
        .unwrap_or(1); // fallback to admin

    // Collect label names
    let label_names: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
    let labels_json = if label_names.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&label_names).unwrap_or_else(|_| "[]".into()))
    };

    // Resolve milestone
    let milestone_id = issue
        .milestone
        .as_ref()
        .and_then(|m| milestone_map.get(&m.title))
        .copied();

    let number = issue_ops::next_number(db, repo_id).await?;
    let created_at = parse_datetime_or_now(&issue.created_at);
    let closed_at = parse_opt_datetime(&issue.closed_at);
    let state = if issue.state == "closed" {
        "closed"
    } else {
        "open"
    };

    let model = rg_db::entities::issue::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo_id),
        number: Set(number),
        title: Set(issue.title.clone()),
        body: Set(issue.body.clone()),
        state: Set(state.to_string()),
        author_id: Set(author_id),
        assignee_id: Set(None),
        milestone_id: Set(milestone_id),
        labels: Set(labels_json),
        created_at: Set(created_at),
        updated_at: Set(parse_datetime_or_now(&issue.updated_at)),
        closed_at: Set(closed_at),
        deleted_at: Set(None),
    };

    let saved = issue_ops::create(db, model).await?;

    // Import comments
    for comment in comments {
        let comment_author = comment
            .user
            .as_ref()
            .and_then(|u| user_map.get(&u.login))
            .copied()
            .unwrap_or(author_id);

        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id),
            author_id: Set(comment_author),
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
    user_map: &HashMap<String, i64>,
    milestone_map: &HashMap<String, i64>,
) -> Result<()> {
    let author_id = pr
        .user
        .as_ref()
        .and_then(|u| user_map.get(&u.login))
        .copied()
        .unwrap_or(1);

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

    let number = pull_request_ops::next_number(db, repo_id).await?;
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
        number: Set(number),
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
        milestone_id: Set(milestone_id),
        labels: Set(labels_json),
        created_at: Set(parse_datetime_or_now(&pr.created_at)),
        updated_at: Set(parse_datetime_or_now(&pr.updated_at)),
        closed_at: Set(parse_opt_datetime(&pr.closed_at)),
        merged_at: Set(parse_opt_datetime(&pr.merged_at)),
    };

    let saved = pull_request_ops::create(db, model).await?;

    // Import PR comments (general discussion)
    for comment in comments {
        let comment_author = comment
            .user
            .as_ref()
            .and_then(|u| user_map.get(&u.login))
            .copied()
            .unwrap_or(author_id);

        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id), // issue_id is PR id in this context
            author_id: Set(comment_author),
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
        let reviewer_id = review
            .user
            .as_ref()
            .and_then(|u| user_map.get(&u.login))
            .copied()
            .unwrap_or(author_id);

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
            reviewer_id: Set(reviewer_id),
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
    _user_map: &HashMap<String, i64>,
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

        // GitLab uses "active" instead of "open"
        let state = match gm.state.as_str() {
            "active" => "open",
            "closed" => "closed",
            _ => "open",
        };

        let due_date = parse_opt_datetime(&gm.due_date);

        let model = milestone::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            title: Set(gm.title.clone()),
            description: Set(gm.description.clone()),
            state: Set(state.to_string()),
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
    user_map: &HashMap<String, i64>,
    milestone_map: &HashMap<String, i64>,
) -> Result<()> {
    let author_id = issue
        .author
        .as_ref()
        .and_then(|a| user_map.get(&a.username))
        .copied()
        .unwrap_or(1);

    // GitLab labels are plain strings
    let labels_json = if issue.labels.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&issue.labels).unwrap_or_else(|_| "[]".into()))
    };

    let milestone_id = issue
        .milestone
        .as_ref()
        .and_then(|m| milestone_map.get(&m.title))
        .copied();

    let number = issue_ops::next_number(db, repo_id).await?;
    let state = if issue.state == "closed" {
        "closed"
    } else {
        "open"
    };

    let model = rg_db::entities::issue::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo_id),
        number: Set(number),
        title: Set(issue.title.clone()),
        body: Set(issue.description.clone()),
        state: Set(state.to_string()),
        author_id: Set(author_id),
        assignee_id: Set(None),
        milestone_id: Set(milestone_id),
        labels: Set(labels_json),
        created_at: Set(parse_datetime_or_now(&issue.created_at)),
        updated_at: Set(parse_datetime_or_now(&issue.updated_at)),
        closed_at: Set(parse_opt_datetime(&issue.closed_at)),
        deleted_at: Set(None),
    };

    let saved = issue_ops::create(db, model).await?;

    // Import notes (skip system notes)
    for note in notes {
        if note.system {
            continue;
        }
        let note_author = note
            .author
            .as_ref()
            .and_then(|a| user_map.get(&a.username))
            .copied()
            .unwrap_or(author_id);

        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id),
            author_id: Set(note_author),
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
    user_map: &HashMap<String, i64>,
    milestone_map: &HashMap<String, i64>,
) -> Result<()> {
    let author_id = mr
        .author
        .as_ref()
        .and_then(|a| user_map.get(&a.username))
        .copied()
        .unwrap_or(1);

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

    let number = pull_request_ops::next_number(db, repo_id).await?;
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
        number: Set(number),
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
        milestone_id: Set(milestone_id),
        labels: Set(labels_json),
        created_at: Set(parse_datetime_or_now(&mr.created_at)),
        updated_at: Set(parse_datetime_or_now(&mr.updated_at)),
        closed_at: Set(parse_opt_datetime(&mr.closed_at)),
        merged_at: Set(parse_opt_datetime(&mr.merged_at)),
    };

    let saved = pull_request_ops::create(db, model).await?;

    // Import MR notes (skip system notes)
    for note in notes {
        if note.system {
            continue;
        }
        let note_author = note
            .author
            .as_ref()
            .and_then(|a| user_map.get(&a.username))
            .copied()
            .unwrap_or(author_id);

        let cm = rg_db::entities::issue_comment::ActiveModel {
            id: sea_orm::NotSet,
            issue_id: Set(saved.id),
            author_id: Set(note_author),
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
    _user_map: &HashMap<String, i64>,
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
fn failure_reason(error: &anyhow::Error, auth_token: Option<&str>) -> String {
    let reason = format!("{error:#}");
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
    repo_root: &Path,
) -> Result<ImportTask> {
    let now = Utc::now();
    let supports_metadata = matches!(platform.as_str(), "github" | "gitlab");

    let model = import_task::ActiveModel {
        user_id: Set(user_id),
        repo_id: Set(None),
        platform: Set(platform),
        source_url: Set(source_url),
        target_owner: Set(target_owner),
        target_name: Set(target_name),
        status: Set("pending".to_string()),
        progress: Set(0),
        stage: Set(None),
        error: Set(None),
        user_mapping: Set(None),
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

    // Spawn background task
    let db_clone = db.clone();
    let repo_root_clone = repo_root.to_path_buf();
    tokio::spawn(async move {
        // These two are the last writes the task will ever get — there is no
        // caller left to notice a failure and no later pass that revisits the
        // row. Losing one leaves the task in `running` forever, which the UI
        // renders as an import that never finishes.
        match run_import(
            &db_clone,
            &task_clone,
            &repo_root_clone,
            auth_token.as_deref(),
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
    });

    // Re-fetch to get the persisted record
    import_task_ops::find_by_id(db, task.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found after creation"))
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
        assert!(source_credentials("github", "").is_none());
        assert!(source_credentials("gitlab", "").is_none());
    }

    /// Basic auth has nowhere to put a lone token, so each platform's
    /// placeholder username has to be the one that platform actually accepts.
    #[test]
    fn each_platform_gets_the_username_it_documents() {
        let github = source_credentials("github", TOKEN).expect("a token is a credential");
        assert_eq!(github.password(), TOKEN);
        let (_, env) = credential_invocation(Some(&github));
        let env: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(
            env.get(rg_git::credentials::USERNAME_ENV)
                .map(String::as_str),
            Some("x-access-token")
        );

        let gitlab = source_credentials("gitlab", TOKEN).expect("a token is a credential");
        let (_, env) = credential_invocation(Some(&gitlab));
        let env: HashMap<_, _> = env.into_iter().collect();
        assert_eq!(
            env.get(rg_git::credentials::USERNAME_ENV)
                .map(String::as_str),
            Some("oauth2")
        );
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
        let credentials = source_credentials("github", TOKEN).expect("a token");
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
        let credentials = source_credentials("github", TOKEN).expect("a token");

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
