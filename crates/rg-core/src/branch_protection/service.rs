//! Branch protection service — protected branches + required status checks.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};

use rg_db::entities::protected_branch::{self, Model as ProtectedBranch};
use rg_db::entities::pull_request;
use rg_db::ops::{pipeline_ops, pr_review_ops, protected_branch_ops};

fn classify_grant_write_error(error: anyhow::Error) -> anyhow::Error {
    match rg_db::user_grants::invalid_principal_message(&error) {
        Some(message) => crate::error::invalid_request(message),
        None => error,
    }
}

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

    // `Conflict`, not `InvalidRequest`: the branch name is well-formed and an
    // existing rule refuses it. The caller edits that rule or deletes it — the
    // request itself has nothing to fix. Same reading as `tag protection
    // pattern already exists`, its neighbour one route over, which already
    // answers 409.
    let already_protected =
        || crate::error::conflict(format!("branch '{branch_name}' is already protected"));

    // Check if protection already exists
    if protected_branch_ops::find_by_repo_and_branch(db, repo.id, &branch_name)
        .await?
        .is_some()
    {
        return Err(already_protected());
    }

    // Taken before `branch_name` moves into the model below, so the losing
    // insert can still name the branch it lost on.
    let duplicate = already_protected();
    let model = protected_branch::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo.id),
        branch_name: Set(branch_name),
        require_pr: Set(require_pr),
        require_status_check: Set(require_status_check),
        // `?`, not `unwrap_or_default()`: the default is `""`, which is not
        // JSON, and the reader of this column now refuses a merge it cannot
        // parse. Writing a value that arms that refusal would trade an
        // impossible-in-practice serialization failure for a branch nobody can
        // merge into. Serializing a `Vec<String>` does not fail, so this only
        // ever changes what happens if that stops being true.
        required_status_checks: Set(required_status_checks
            .map(|v| serde_json::to_string(&v))
            .transpose()
            .context("serialize required_status_checks")?),
        require_approval: Set(require_approval),
        required_approvals: Set(required_approvals),
        allow_force_push: Set(allow_force_push),
        require_signed_commits: Set(require_signed_commits),
        // The common grant writer below fills this compatibility mirror and
        // the normalized FK rows in the same transaction.
        allowed_push_user_ids: Set(None),
        created_at: Set(Utc::now()),
        updated_at: Set(Utc::now()),
    };

    protected_branch_ops::create_with_push_grants(db, model, allowed_push_user_ids)
        .await
        .map_err(|error| {
            if rg_db::is_unique_violation_anyhow(&error) {
                duplicate
            } else {
                classify_grant_write_error(error)
            }
        })
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
///
/// Deliberately **not** `pub`: a protection id is an instance-wide primary key,
/// so a caller outside this module holding one has no way to know which
/// repository it belongs to. The three unscoped primitives here exist only to
/// be wrapped by their `*_for_repo` siblings below, which re-anchor the id to
/// the repository the caller was actually authorized against. Making the
/// distinction a matter of module visibility rather than of naming discipline
/// means the wrong one is not merely discouraged from another crate — it is
/// invisible there.
async fn get_protection(db: &DatabaseConnection, protection_id: i64) -> Result<ProtectedBranch> {
    protected_branch_ops::find_by_id(db, protection_id)
        .await?
        .ok_or_else(|| crate::error::not_found("protection rule"))
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
        return Err(crate::error::not_found("protection rule"));
    }
    Ok(protection)
}

/// Update a branch protection rule. Unscoped — see [`get_protection`].
#[allow(clippy::too_many_arguments)]
async fn update_protection(
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
    let protection = protected_branch_ops::find_by_id(db, protection_id)
        .await?
        .ok_or_else(|| crate::error::not_found("protection rule"))?;

    // `From<Model> for ActiveModel` marks every field `Unchanged`, so mutating
    // the model first and converting afterwards produced an update with no SET
    // clause: the call echoed the old row back and wrote nothing, while the
    // operator who just turned on `require_signed_commits` read that 200 as
    // "the branch is protected now". Each field the request actually carries
    // has to be `Set` on the ActiveModel itself; the ones it omits stay
    // `Unchanged` and are left alone.
    let mut active: protected_branch::ActiveModel = protection.into();
    if let Some(v) = require_pr {
        active.require_pr = Set(v);
    }
    if let Some(v) = require_status_check {
        active.require_status_check = Set(v);
    }
    if let Some(v) = required_status_checks {
        // Same reasoning as the create path: `""` is not a list of no checks,
        // it is a value the merge gate cannot read.
        active.required_status_checks = Set(Some(
            serde_json::to_string(&v).context("serialize required_status_checks")?,
        ));
    }
    if let Some(v) = require_approval {
        active.require_approval = Set(v);
    }
    if let Some(v) = required_approvals {
        active.required_approvals = Set(Some(v));
    }
    if let Some(v) = allow_force_push {
        active.allow_force_push = Set(v);
    }
    if let Some(v) = require_signed_commits {
        active.require_signed_commits = Set(v);
    }
    active.updated_at = Set(Utc::now());

    protected_branch_ops::update_with_push_grants(db, active, allowed_push_user_ids)
        .await
        .map_err(classify_grant_write_error)
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

/// Delete a branch protection rule. Unscoped — see [`get_protection`].
///
/// The caller's scope check and this `DELETE` are two statements, so a
/// concurrent delete can empty the row out from under it; zero rows reports
/// `not_found` rather than confirming a deletion this call did not perform.
async fn delete_protection(db: &DatabaseConnection, protection_id: i64) -> Result<()> {
    if protected_branch_ops::delete_by_id(db, protection_id).await? {
        Ok(())
    } else {
        Err(crate::error::not_found("branch protection rule"))
    }
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
        protected_branch_ops::find_rule_by_repo_and_branch(db, repo_id, branch_name).await?;

    let Some(protection) = protection else {
        // Not protected, push is allowed
        return Ok(());
    };

    if user_id.is_some_and(|uid| protection.allowed_push_user_ids.contains(&uid)) {
        return Ok(());
    }

    let protection = protection.protection;

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
        return Err(crate::error::forbidden(format!(
            "branch '{}' requires cryptographically signed commits; server-side PR merge commits are not signed, so create and push a signed commit with an allowed identity",
            target_branch
        )));
    }

    // Check required approvals
    if protection.require_approval {
        let required = protection.required_approvals.unwrap_or(1);
        let pr = pull_request::Entity::find_by_id(pr_id)
            .one(db)
            .await?
            .ok_or_else(|| crate::error::not_found("pull request"))?;
        let approval_count =
            pr_review_ops::count_current_approvals(db, pr_id, pr.head_sha.as_deref()).await?;
        if approval_count < required {
            return Err(crate::error::forbidden(format!(
                "merging into protected branch '{}' requires at least {} approval(s), got {}",
                target_branch, required, approval_count
            )));
        }
    }

    // Check required status checks
    if protection.require_status_check {
        if let Some(checks_json) = &protection.required_status_checks {
            // A stored list that does not decode is a broken row, not "no
            // checks are required". This whole block used to sit inside an
            // `if let Ok(...)`, so a column that failed to parse skipped
            // *everything* below it — the head-sha lookup, the pipeline
            // lookup, the job comparison — and `check_merge_allowed`
            // returned `Ok(())`. The rule stayed `true` in the database and
            // lit up in the UI while every merge into the protected branch
            // went through with no CI checked at all.
            //
            // Its sibling in `check_push_allowed` fails *closed* on the same
            // shape of data — an unreadable allow-list refuses the push — and
            // that is the whole difference: a broken row there costs someone
            // an unexplained 403, here it costs the branch its protection.
            // `?` makes an unreadable rule a server error, and a merge that
            // cannot be checked does not happen.
            let required_checks: Vec<String> =
                serde_json::from_str(checks_json).with_context(|| {
                    format!(
                        "stored required_status_checks of protected branch '{target_branch}' \
                         is not a JSON array of check names"
                    )
                })?;

            // Find the PR to get its head commit SHA
            let pr = pull_request::Entity::find()
                .filter(pull_request::Column::Id.eq(pr_id))
                .one(db)
                .await
                .context("db: find PR for status check")?;

            let head_sha = match pr.and_then(|p| p.head_sha) {
                Some(s) if !s.is_empty() => s,
                _ => {
                    return Err(crate::error::forbidden(format!(
                        "branch '{}' requires status checks but no CI pipeline found for PR {}",
                        target_branch, pr_id
                    )));
                }
            };

            // Find the latest pipeline for this commit
            let pipeline = pipeline_ops::find_latest_by_repo_and_commit(db, repo_id, &head_sha)
                .await
                .context("db: find pipeline for status check")?;

            match pipeline {
                None => {
                    return Err(crate::error::forbidden(format!(
                        "branch '{}' requires status checks to pass, but no CI pipeline has run for commit {}",
                        target_branch,
                        &head_sha[..8.min(head_sha.len())]
                    )));
                }
                Some(p) if p.status != "success" => {
                    return Err(crate::error::forbidden(format!(
                        "branch '{}' requires all status checks to pass, but pipeline #{} is {}",
                        target_branch, p.id, p.status
                    )));
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
                        return Err(crate::error::forbidden(format!(
                            "branch '{}' requires status checks {:?} to pass, but {:?} are missing or failed",
                            target_branch,
                            required_checks,
                            missing
                        )));
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

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────

async fn resolve_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<rg_db::entities::repository::Model> {
    crate::repo::service::find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| crate::error::not_found("repository"))
}
