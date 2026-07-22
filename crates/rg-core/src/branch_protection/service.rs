//! Branch protection service — protected branches + required status checks.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};

use rg_db::entities::protected_branch::{self, Model as ProtectedBranch};
use rg_db::entities::pull_request;
use rg_db::ops::{pipeline_ops, pr_review_ops, protected_branch_ops, repo_ops};

/// Create a branch protection rule.
#[allow(clippy::too_many_arguments)]
pub async fn create_protection(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    branch_name: String,
    require_pr: bool,
    require_status_check: bool,
    required_status_checks: Option<Vec<String>>,
    require_approval: bool,
    required_approvals: Option<i64>,
    allow_force_push: bool,
    require_signed_commits: bool,
    allowed_push_user_ids: Option<Vec<i64>>,
) -> Result<ProtectedBranch> {
    let repo = resolve_repo(db, owner, repo_name).await?;

    // Check if protection already exists
    if protected_branch_ops::find_by_repo_and_branch(db, repo.id, &branch_name)
        .await?
        .is_some()
    {
        bail!("branch '{}' is already protected", branch_name);
    }

    let model = protected_branch::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo.id),
        branch_name: Set(branch_name),
        require_pr: Set(require_pr),
        require_status_check: Set(require_status_check),
        required_status_checks: Set(
            required_status_checks.map(|v| serde_json::to_string(&v).unwrap_or_default())
        ),
        require_approval: Set(require_approval),
        required_approvals: Set(required_approvals),
        allow_force_push: Set(allow_force_push),
        require_signed_commits: Set(require_signed_commits),
        allowed_push_user_ids: Set(
            allowed_push_user_ids.map(|v| serde_json::to_string(&v).unwrap_or_default())
        ),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
    };

    protected_branch_ops::create(db, model).await
}

/// List all branch protection rules for a repo.
pub async fn list_protections(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<Vec<ProtectedBranch>> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    protected_branch_ops::list_by_repo(db, repo.id).await
}

/// Get a branch protection rule by ID.
pub async fn get_protection(
    db: &DatabaseConnection,
    protection_id: i64,
) -> Result<ProtectedBranch> {
    protected_branch_ops::find_by_id(db, protection_id)
        .await?
        .context("protection rule not found")
}

/// Get a branch protection rule by ID, scoped to a repository route.
pub async fn get_protection_for_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    protection_id: i64,
) -> Result<ProtectedBranch> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    let protection = get_protection(db, protection_id).await?;
    if protection.repo_id != repo.id {
        bail!("protection rule not found");
    }
    Ok(protection)
}

/// Update a branch protection rule.
#[allow(clippy::too_many_arguments)]
pub async fn update_protection(
    db: &DatabaseConnection,
    protection_id: i64,
    require_pr: Option<bool>,
    require_status_check: Option<bool>,
    required_status_checks: Option<Vec<String>>,
    require_approval: Option<bool>,
    required_approvals: Option<i64>,
    allow_force_push: Option<bool>,
    require_signed_commits: Option<bool>,
    allowed_push_user_ids: Option<Vec<i64>>,
) -> Result<ProtectedBranch> {
    let mut protection = protected_branch_ops::find_by_id(db, protection_id)
        .await?
        .context("protection rule not found")?;

    if let Some(v) = require_pr {
        protection.require_pr = v;
    }
    if let Some(v) = require_status_check {
        protection.require_status_check = v;
    }
    if let Some(v) = required_status_checks {
        protection.required_status_checks = Some(serde_json::to_string(&v).unwrap_or_default());
    }
    if let Some(v) = require_approval {
        protection.require_approval = v;
    }
    if let Some(v) = required_approvals {
        protection.required_approvals = Some(v);
    }
    if let Some(v) = allow_force_push {
        protection.allow_force_push = v;
    }
    if let Some(v) = require_signed_commits {
        protection.require_signed_commits = v;
    }
    if let Some(v) = allowed_push_user_ids {
        protection.allowed_push_user_ids = Some(serde_json::to_string(&v).unwrap_or_default());
    }
    protection.updated_at = Utc::now();

    let active: protected_branch::ActiveModel = protection.into();
    protected_branch_ops::update(db, active).await
}

/// Update a branch protection rule, scoped to a repository route.
#[allow(clippy::too_many_arguments)]
pub async fn update_protection_for_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    protection_id: i64,
    require_pr: Option<bool>,
    require_status_check: Option<bool>,
    required_status_checks: Option<Vec<String>>,
    require_approval: Option<bool>,
    required_approvals: Option<i64>,
    allow_force_push: Option<bool>,
    require_signed_commits: Option<bool>,
    allowed_push_user_ids: Option<Vec<i64>>,
) -> Result<ProtectedBranch> {
    get_protection_for_repo(db, owner, repo_name, protection_id).await?;
    update_protection(
        db,
        protection_id,
        require_pr,
        require_status_check,
        required_status_checks,
        require_approval,
        required_approvals,
        allow_force_push,
        require_signed_commits,
        allowed_push_user_ids,
    )
    .await
}

/// Delete a branch protection rule.
pub async fn delete_protection(db: &DatabaseConnection, protection_id: i64) -> Result<()> {
    protected_branch_ops::delete_by_id(db, protection_id).await
}

/// Delete a branch protection rule, scoped to a repository route.
pub async fn delete_protection_for_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    protection_id: i64,
) -> Result<()> {
    get_protection_for_repo(db, owner, repo_name, protection_id).await?;
    delete_protection(db, protection_id).await
}

/// Check if a push to a branch is allowed.
/// Returns Ok(()) if allowed, or Err with the reason if blocked.
pub async fn check_push_allowed(
    db: &DatabaseConnection,
    repo_id: i64,
    branch_name: &str,
    user_id: Option<i64>,
) -> Result<()> {
    let protection =
        protected_branch_ops::find_by_repo_and_branch(db, repo_id, branch_name).await?;

    let Some(protection) = protection else {
        // Not protected, push is allowed
        return Ok(());
    };

    // Check if user is in the allowed list
    if let Some(uid) = user_id {
        if let Some(allowed_json) = &protection.allowed_push_user_ids {
            if let Ok(allowed_ids) = serde_json::from_str::<Vec<i64>>(allowed_json) {
                if allowed_ids.contains(&uid) {
                    return Ok(());
                }
            }
        }
    }

    if protection.require_pr {
        bail!(
            "push to protected branch '{}' is not allowed; open a pull request instead",
            branch_name
        );
    }

    if !protection.allow_force_push {
        bail!(
            "force push to protected branch '{}' is not allowed",
            branch_name
        );
    }

    Ok(())
}

/// Check if a PR merge is allowed under branch protection rules.
pub async fn check_merge_allowed(
    db: &DatabaseConnection,
    repo_id: i64,
    target_branch: &str,
    pr_id: i64,
) -> Result<()> {
    let protection =
        protected_branch_ops::find_by_repo_and_branch(db, repo_id, target_branch).await?;

    let Some(protection) = protection else {
        return Ok(());
    };

    if protection.require_signed_commits {
        bail!(
            "branch '{}' requires cryptographically signed commits; server-side PR merge commits are not signed, so create and push a signed commit with an allowed identity",
            target_branch
        );
    }

    // Check required approvals
    if protection.require_approval {
        let required = protection.required_approvals.unwrap_or(1);
        let pr = pull_request::Entity::find_by_id(pr_id)
            .one(db)
            .await?
            .context("pull request not found")?;
        let approval_count =
            pr_review_ops::count_current_approvals(db, pr_id, pr.head_sha.as_deref()).await?;
        if approval_count < required {
            bail!(
                "merging into protected branch '{}' requires at least {} approval(s), got {}",
                target_branch,
                required,
                approval_count
            );
        }
    }

    // Check required status checks
    if protection.require_status_check {
        if let Some(checks_json) = &protection.required_status_checks {
            if let Ok(required_checks) = serde_json::from_str::<Vec<String>>(checks_json) {
                // Find the PR to get its head commit SHA
                let pr = pull_request::Entity::find()
                    .filter(pull_request::Column::Id.eq(pr_id))
                    .one(db)
                    .await
                    .context("db: find PR for status check")?;

                let head_sha = match pr.and_then(|p| p.head_sha) {
                    Some(s) if !s.is_empty() => s,
                    _ => {
                        bail!(
                            "branch '{}' requires status checks but no CI pipeline found for PR {}",
                            target_branch,
                            pr_id
                        );
                    }
                };

                // Find the latest pipeline for this commit
                let pipeline = pipeline_ops::find_latest_by_repo_and_commit(db, repo_id, &head_sha)
                    .await
                    .context("db: find pipeline for status check")?;

                match pipeline {
                    None => {
                        bail!(
                            "branch '{}' requires status checks to pass, but no CI pipeline has run for commit {}",
                            target_branch,
                            &head_sha[..8.min(head_sha.len())]
                        );
                    }
                    Some(p) if p.status != "success" => {
                        bail!(
                            "branch '{}' requires all status checks to pass, but pipeline #{} is {}",
                            target_branch,
                            p.id,
                            p.status
                        );
                    }
                    Some(p) => {
                        // All pipeline jobs must have passed — check job names match required list
                        let jobs = pipeline_ops::list_jobs_by_pipeline(db, p.id).await?;
                        let passed_jobs: std::collections::HashSet<_> = jobs
                            .iter()
                            .filter(|j| j.status == "success")
                            .map(|j| j.name.clone())
                            .collect();

                        let missing: Vec<_> = required_checks
                            .iter()
                            .filter(|name| !passed_jobs.contains(*name))
                            .collect();

                        if !missing.is_empty() {
                            bail!(
                                "branch '{}' requires status checks {:?} to pass, but {:?} are missing or failed",
                                target_branch,
                                required_checks,
                                missing
                            );
                        }

                        tracing::info!(
                            branch = %target_branch,
                            pipeline_id = %p.id,
                            checks = ?required_checks,
                            "Branch protection: all status checks passed"
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────

async fn resolve_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<rg_db::entities::repository::Model> {
    let user = rg_db::ops::user_ops::find_by_username(db, owner)
        .await?
        .context("owner not found")?;
    repo_ops::find_by_owner_and_name(db, user.id, repo_name)
        .await?
        .context("repository not found")
}
