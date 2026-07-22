//! Import pipeline service — orchestrates full repository migration.
//!
//! Supports importing from GitHub and GitLab, including:
//! - Repository cloning (git clone --bare) + IronForge DB registration
//! - Labels and milestones
//! - Issues with comments
//! - Pull/Merge requests with reviews/comments
//! - Releases
//!
//! Generic Git and Gitea imports currently clone the repository only.
//!
//! The import runs asynchronously and updates progress in the
//! import_tasks database table.

use anyhow::{Context, Result};
use chrono::Utc;
use rg_git::cli_gateway::global_gateway;
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
pub async fn run_import(
    db: &DatabaseConnection,
    task: &ImportTask,
    repo_root: &Path,
) -> Result<ImportStats> {
    let mut stats = ImportStats::default();

    match task.platform.as_str() {
        "github" => run_github_import(db, task, repo_root, &mut stats).await?,
        "gitlab" => run_gitlab_import(db, task, repo_root, &mut stats).await?,
        "gitea" | "git" => run_git_import(db, task, repo_root, &mut stats).await?,
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
    stats: &mut ImportStats,
) -> Result<()> {
    let repo_id =
        resolve_or_create_target_repo(db, &task.target_owner, &task.target_name, repo_root).await?;
    let _ = import_task_ops::set_repo_id(db, task.id, repo_id).await;

    if task.import_repo {
        update_stage(db, task.id, "cloning", 0, "Cloning repository...").await?;
        clone_repo(
            &task.source_url,
            repo_root,
            &task.target_owner,
            &task.target_name,
            task.auth_token_encrypted.as_deref().unwrap_or(""),
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
    stats: &mut ImportStats,
) -> Result<()> {
    let token = task.auth_token_encrypted.as_deref().unwrap_or("");
    let client = GitHubClient::new(token.to_string(), None);

    // Parse owner/repo from source URL (https://github.com/owner/repo)
    let (gh_owner, gh_repo) = parse_github_url(&task.source_url)?;

    // Resolve (or create) the target repo in IronForge DB
    let repo_id =
        resolve_or_create_target_repo(db, &task.target_owner, &task.target_name, repo_root).await?;

    // Update task with repo_id
    let _ = import_task_ops::set_repo_id(db, task.id, repo_id).await;

    // Step 1: Clone repository
    if task.import_repo {
        update_stage(db, task.id, "cloning", 0, "Cloning repository...").await?;
        clone_repo(
            &task.source_url,
            repo_root,
            &task.target_owner,
            &task.target_name,
            token,
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
    stats: &mut ImportStats,
) -> Result<()> {
    let token = task.auth_token_encrypted.as_deref().unwrap_or("");
    let client = GitLabClient::new(token.to_string(), None);

    // Extract project path from source URL (https://gitlab.com/group/project)
    let project_path = parse_gitlab_url(&task.source_url)?;

    // Resolve (or create) the target repo in IronForge DB
    let repo_id =
        resolve_or_create_target_repo(db, &task.target_owner, &task.target_name, repo_root).await?;

    let _ = import_task_ops::set_repo_id(db, task.id, repo_id).await;

    // Step 1: Clone repository
    if task.import_repo {
        update_stage(db, task.id, "cloning", 0, "Cloning repository...").await?;
        let project = client.get_project(&project_path).await?;
        let clone_url = build_gitlab_clone_url(&project.http_url_to_repo, token);
        clone_repo(
            &clone_url,
            repo_root,
            &task.target_owner,
            &task.target_name,
            token,
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

/// Find the target repo in IronForge DB, or create it if it doesn't exist.
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
                "target owner '{}' not found (must be an existing IronForge user or organization)",
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

/// Clone a repository (bare) into the IronForge repo root.
fn clone_repo(
    source_url: &str,
    repo_root: &Path,
    owner: &str,
    name: &str,
    _token: &str,
) -> Result<()> {
    let target_dir = repo_root.join(format!("{}/{}.git", owner, name));
    if target_dir.join("HEAD").exists() {
        tracing::info!(
            path = %target_dir.display(),
            "Repository already exists, skipping clone"
        );
        return Ok(());
    }

    std::fs::create_dir_all(target_dir.parent().unwrap())?;

    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    git.run_or_bail(
        &["clone", "--bare", source_url, &target_dir.to_string_lossy()],
        None,
    )
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

fn build_gitlab_clone_url(http_url: &str, token: &str) -> String {
    if token.is_empty() {
        http_url.to_string()
    } else {
        http_url.replacen("://", &format!("://oauth2:{}@", token), 1)
    }
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

/// Map external user logins/usernames to local IronForge user IDs.
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
            tracing::warn!("Failed to create label '{}': {e}", gl.name);
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
                tracing::warn!("Failed to create milestone '{}': {e}", gm.title);
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
            tracing::warn!("Failed to import comment on issue #{}: {e}", issue.number);
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
            tracing::warn!("Failed to import PR comment on #{}: {e}", pr.number);
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
            tracing::warn!("Failed to import review on PR #{}: {e}", pr.number);
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
                tracing::warn!("Failed to import release '{}': {e}", release.tag_name);
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
            tracing::warn!("Failed to create label '{}': {e}", gl.name);
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
                tracing::warn!("Failed to create milestone '{}': {e}", gm.title);
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
            tracing::warn!("Failed to import note on issue !{}: {e}", issue.iid);
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
            tracing::warn!("Failed to import MR note on !{}: {e}", mr.iid);
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
                tracing::warn!("Failed to import release '{}': {e}", release.tag_name);
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

/// Create a new import task and start the background import process.
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
        auth_token_encrypted: Set(auth_token),
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
        match run_import(&db_clone, &task_clone, &repo_root_clone).await {
            Ok(stats) => {
                let stats_json = serde_json::to_string(&stats).unwrap_or_default();
                let _ =
                    import_task_ops::mark_completed(&db_clone, task_clone.id, &stats_json).await;
            }
            Err(e) => {
                let _ =
                    import_task_ops::mark_failed(&db_clone, task_clone.id, &format!("{e:#}")).await;
            }
        }
    });

    // Re-fetch to get the persisted record
    import_task_ops::find_by_id(db, task.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found after creation"))
}
