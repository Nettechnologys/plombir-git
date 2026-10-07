//! Branch protection service — protected branches + required status checks.

use anyhow::{Context, Result};
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

/// Testable boundary after the repository-scoped read and before the
/// conditional rule/grant transaction.
#[allow(clippy::too_many_arguments)]
async fn update_protection_after_read<F, Fut>(
    db: &DatabaseConnection,
    mut protection: ProtectedBranch,
    require_pr: Option<bool>,
    require_status_check: Option<bool>,
    required_status_checks: Option<Vec<String>>,
    require_approval: Option<bool>,
    required_approvals: Option<i64>,
    allow_force_push: Option<bool>,
    require_signed_commits: Option<bool>,
    allowed_push_user_ids: Option<Vec<i64>>,
    after_read: F,
) -> Result<ProtectedBranch>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    after_read().await?;

    if let Some(v) = require_pr {
        protection.require_pr = v;
    }
    if let Some(v) = require_status_check {
        protection.require_status_check = v;
    }
    if let Some(v) = required_status_checks {
        // Same reasoning as the create path: `""` is not a list of no checks,
        // it is a value the merge gate cannot read.
        protection.required_status_checks =
            Some(serde_json::to_string(&v).context("serialize required_status_checks")?);
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
    protection.updated_at = Utc::now();

    match protected_branch_ops::update_with_push_grants(db, protection, allowed_push_user_ids)
        .await
        .map_err(classify_grant_write_error)?
    {
        Some(updated) => Ok(updated),
        None => Err(crate::error::not_found("protection rule")),
    }
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
    let protection = get_protection_for_repo(db, owner, repo_name, protection_id).await?;
    update_protection_after_read(
        db,
        protection,
        require_pr,
        require_status_check,
        required_status_checks,
        require_approval,
        required_approvals,
        allow_force_push,
        require_signed_commits,
        allowed_push_user_ids,
        || async { Ok(()) },
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

// `check_push_allowed` used to live here: a second, drifted copy of the push
// gate with no caller anywhere in the tree. The push path is
// `push_rules::branch_protection_rejected_refs`, called from `git_http.rs`, and
// it is the only one — deleted rather than kept "for later", because "for later"
// is exactly the mechanism by which a second dialect of a gate comes back
// (card_ab36709fa0c7, card_1d07a85117ac).

/// What a merge gate decided, together with the commit it decided it on.
///
/// A bare `Ok(())` sends the caller looking for the head by itself, and the
/// nearest column — `pr.head_sha` — is the very row this gate read, so the
/// caller cannot tell "the commit the rules were counted for" from "whatever
/// the row says now". They are the same value only until the post-push hook
/// that moves the row lands, and the whole defect this type closes lives in
/// that gap (card_9ff26bb95dc9, card_758aa42d9d22).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeVerdict {
    /// The head commit the protection rules were evaluated against, when a rule
    /// actually read one. `None` when nothing judged a head at all — an
    /// unprotected branch, or a rule set requiring neither approvals nor status
    /// checks — and there is then no commit for the merge to be pinned to.
    pub judged_head_sha: Option<String>,
}

/// The current approvals of a pull request, split into those a required
/// approval count may rest on and those it may not.
struct ApprovalTally {
    counted: i64,
    set_aside: usize,
}

/// Count the approvals branch protection accepts for the pull request's head.
///
/// "Requires N approvals" means N people other than the one asking to merge
/// looked at the change and had the standing to say yes. Three kinds of
/// approval are recorded but cannot carry that meaning (card_25220b69c295):
///
/// - **A bot's.** An agent's verdict is not a review by a person, and one
///   owner's two bots could otherwise approve each other's pull requests and
///   meet any requirement with nobody looking. A bot may still post an
///   `approve` — it reads as advice — it just never counts.
/// - **The author's side.** The author cannot approve their own pull request
///   (`submit_review` refuses it), and a bot's pull request is its owner's: the
///   bot acts for them, so the owner approving it is the author approving
///   themselves. A different person has to.
/// - **A non-writer's.** Reviewing takes only read access, which on a public
///   repository is every account on the instance — a second account of the
///   author's included. An approval counts only from someone who could have
///   pushed the change themselves, and is checked now, not when it was given,
///   so a collaborator removed since stops counting.
async fn tally_approvals(
    db: &DatabaseConnection,
    repo_id: i64,
    pr: &pull_request::Model,
) -> Result<ApprovalTally> {
    let approvers = pr_review_ops::current_approvers(db, pr.id, pr.head_sha.as_deref()).await?;
    if approvers.is_empty() {
        return Ok(ApprovalTally {
            counted: 0,
            set_aside: 0,
        });
    }
    let repo = rg_db::entities::repository::Entity::find_by_id(repo_id)
        .one(db)
        .await
        .context("db: load repository for approval count")?
        .ok_or_else(|| crate::error::not_found("repository"))?;
    let author_bot_owner = rg_db::ops::user_ops::find_by_id(db, pr.author_id)
        .await?
        .and_then(|author| author.bot_owner_id);

    let mut counted = 0;
    let mut set_aside = 0;
    for approver in approvers {
        let authors_side = approver.id == pr.author_id || Some(approver.id) == author_bot_owner;
        if approver.is_bot()
            || authors_side
            || !crate::repo::service::can_write_repo(db, &repo, Some(approver.id)).await?
        {
            set_aside += 1;
        } else {
            counted += 1;
        }
    }
    Ok(ApprovalTally { counted, set_aside })
}

/// Check if a PR merge is allowed under branch protection rules.
///
/// The `Ok` half carries [`MergeVerdict::judged_head_sha`]: approvals are
/// counted for one commit (`pr_review_ops::current_approvers`) and status
/// checks are looked up for one commit
/// (`pipeline_ops::find_latest_by_repo_and_commit`), so the answer "this merge
/// is allowed" is only ever true *of that commit*. Merging anything else —
/// including the branch tip, which is where `refs/heads/<head>` has moved on to
/// while the row lagged — would be a merge nothing here approved.
pub async fn check_merge_allowed(
    db: &DatabaseConnection,
    repo_id: i64,
    target_branch: &str,
    pr_id: i64,
) -> Result<MergeVerdict> {
    // Every merge path asks this — the REST merge, auto-merge, the merge queue
    // — and a merge into a pull mirror lasts only until its next pass
    // (card_97a2c0209056). A `409`, and the queue shows it as what it waits on.
    crate::mirror::write_guard::refuse_mirrored_write(db, repo_id).await?;
    let mut verdict = MergeVerdict::default();
    let target_ref = format!("refs/heads/{target_branch}");
    // Push checks every matching rule, including glob rules. Merge must do the
    // same: choosing just the exact row (or one of several matching globs)
    // would let a less restrictive rule mask another rule's requirements.
    for protection in protected_branch_ops::list_by_repo(db, repo_id).await? {
        let rule_ref = format!("refs/heads/{}", protection.branch_name);
        if !rg_git::protocol::receive_pack::ref_matches_rejection_pattern(&target_ref, &rule_ref) {
            continue;
        }
        check_matching_merge_rule(db, repo_id, target_branch, pr_id, &protection, &mut verdict)
            .await?;
    }

    Ok(verdict)
}

async fn check_matching_merge_rule(
    db: &DatabaseConnection,
    repo_id: i64,
    target_branch: &str,
    pr_id: i64,
    protection: &ProtectedBranch,
    verdict: &mut MergeVerdict,
) -> Result<()> {
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
        let tally = tally_approvals(db, repo_id, &pr).await?;
        // The approvals just counted are the ones recorded against this head;
        // an approval given for another commit is not one of them. That makes
        // the head part of the verdict, not a detail of how it was reached.
        verdict.judged_head_sha = pr.head_sha.filter(|sha| !sha.is_empty());
        if tally.counted < required {
            let mut message = format!(
                "merging into protected branch '{}' requires at least {} approval(s), got {}",
                target_branch, required, tally.counted
            );
            // The reviewer sees an approval on the pull request and a merge
            // refused for want of one; say why the two disagree.
            if tally.set_aside > 0 {
                message.push_str(&format!(
                    " ({} more approval(s) do not count: an approval counts only from a \
                     person with write access who is neither the author nor, for a bot's pull \
                     request, the bot's owner)",
                    tally.set_aside
                ));
            }
            return Err(crate::error::forbidden(message));
        }
    }

    // Check required status checks
    if protection.require_status_check {
        // The flag is the gate; the name list only narrows it. Both halves of
        // that sentence were once conditions on running the gate at all.
        //
        // A stored list that does not decode is a broken row, not "no checks
        // are required". This whole block used to sit inside an `if let
        // Ok(...)`, so a column that failed to parse skipped *everything*
        // below it — the head-sha lookup, the pipeline lookup, the job
        // comparison — and `check_merge_allowed` returned `Ok(())`. The rule
        // stayed `true` in the database and lit up in the UI while every merge
        // into the protected branch went through with no CI checked at all.
        //
        // The push path (`push_rules::branch_protection_rejected_refs`) fails
        // *closed* on the same shape of data — an unreadable allow-list
        // refuses the push — and that is the whole difference: a broken row
        // there costs someone an unexplained 403, here it costs the branch its
        // protection. `?` makes an unreadable rule a server error, and a merge
        // that cannot be checked does not happen.
        //
        // An *absent* list (`NULL`) used to skip the block for the same
        // structural reason — one `if let Some(..)` out — and that one the
        // product itself hands to the operator: the settings form sends
        // `required_status_checks` only when the names field is non-empty
        // (`web/src/routes/[owner]/[repo]/settings/branches/+page.svelte`,
        // `parseStringList` returns `undefined` for a blank field), so ticking
        // "require status checks" and naming nothing — the most natural way to
        // say "I want a green CI" — minted a rule shown as enabled that gated
        // nothing. Note that the same intent stored as `[]` already gated
        // correctly: it decodes to an empty vec and falls through to the
        // pipeline lookup. `NULL` and `[]` are the same operator instruction
        // and now have the same effect — the flag alone requires a pipeline on
        // the head commit that finished `success`, and the names, when given,
        // additionally pin which jobs must be among the ones that passed.
        let required_checks: Vec<String> = match &protection.required_status_checks {
            Some(checks_json) => serde_json::from_str(checks_json).with_context(|| {
                format!(
                    "stored required_status_checks of protected branch '{target_branch}' \
                     is not a JSON array of check names"
                )
            })?,
            None => Vec::new(),
        };

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

        // Same as the approval count above: the pipeline is looked up *for this
        // commit*, so the verdict is about it and about nothing else.
        verdict.judged_head_sha = Some(head_sha.clone());

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
                        target_branch, required_checks, missing
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

#[cfg(test)]
mod update_delete_tests {
    use super::*;
    use sea_orm::{ColumnTrait, EntityTrait, NotSet, PaginatorTrait, QueryFilter, Set};

    async fn fixture() -> (tempfile::TempDir, DatabaseConnection, ProtectedBranch, i64) {
        let directory = tempfile::tempdir().expect("tempdir");
        let db = rg_db::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("protected-branch-race.db").display()
            ),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            4,
        )
        .await
        .expect("connect sqlite");
        rg_db::run_migrations(&db).await.expect("run migrations");

        let owner = rg_db::ops::user_ops::create_user(
            &db,
            "protected-branch-race-owner",
            "protected-branch-race-owner@example.invalid",
            "",
            "Branch Owner",
        )
        .await
        .expect("create owner");
        let replacement = rg_db::ops::user_ops::create_user(
            &db,
            "protected-branch-race-replacement",
            "protected-branch-race-replacement@example.invalid",
            "",
            "Replacement Pusher",
        )
        .await
        .expect("create replacement pusher");
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("protected-branch-race-repo".to_string()),
                description: Set(None),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .expect("create repository");
        let protection = protected_branch_ops::create_with_push_grants(
            &db,
            protected_branch::ActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                branch_name: Set("main".to_string()),
                require_pr: Set(true),
                require_status_check: Set(false),
                required_status_checks: Set(None),
                require_approval: Set(false),
                required_approvals: Set(None),
                allow_force_push: Set(false),
                require_signed_commits: Set(false),
                allowed_push_user_ids: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            },
            Some(vec![owner.id]),
        )
        .await
        .expect("create protected branch");

        (directory, db, protection, replacement.id)
    }

    #[tokio::test]
    async fn delete_after_the_scoped_read_is_typed_not_found_and_replaces_no_grants() {
        let (_directory, db, protection, replacement_id) = fixture().await;
        let protection_id = protection.id;

        let error = update_protection_after_read(
            &db,
            protection,
            Some(false),
            None,
            None,
            None,
            None,
            None,
            Some(true),
            Some(vec![replacement_id]),
            || async {
                assert!(
                    protected_branch_ops::delete_by_id(&db, protection_id).await?,
                    "the injected DELETE must remove the protected branch"
                );
                Ok(())
            },
        )
        .await
        .expect_err("a DELETE that wins after the scoped read must abort the PATCH");

        let not_found = error
            .downcast_ref::<crate::error::NotFound>()
            .expect("the losing PATCH must stay classifiable as HTTP 404");
        assert_eq!(not_found.resource, "protection rule");
        assert!(
            protected_branch_ops::find_by_id(&db, protection_id)
                .await
                .expect("look for a resurrected protection")
                .is_none(),
            "the losing PATCH must not recreate the deleted protection"
        );
        assert_eq!(
            rg_db::entities::protected_branch_push_grant::Entity::find()
                .filter(
                    rg_db::entities::protected_branch_push_grant::Column::ProtectedBranchId
                        .eq(protection_id),
                )
                .count(&db)
                .await
                .expect("count grants left for the deleted protection"),
            0,
            "neither the old nor requested grant set may survive the DELETE"
        );
    }

    #[tokio::test]
    async fn a_successful_patch_commits_the_rule_and_both_grant_representations() {
        let (_directory, db, protection, replacement_id) = fixture().await;

        let updated = update_protection_after_read(
            &db,
            protection,
            Some(false),
            Some(true),
            Some(vec!["ci".to_string()]),
            Some(true),
            Some(2),
            Some(true),
            Some(true),
            Some(vec![replacement_id]),
            || async { Ok(()) },
        )
        .await
        .expect("update protected branch and grants");

        assert!(!updated.require_pr);
        assert!(updated.require_status_check);
        assert!(updated.require_approval);
        assert_eq!(updated.required_approvals, Some(2));
        assert!(updated.allow_force_push);
        assert!(updated.require_signed_commits);
        assert_eq!(
            rg_db::user_grants::load_verified(
                &db,
                rg_db::user_grants::Target::ProtectedBranch(updated.id),
                updated.allowed_push_user_ids.as_deref(),
            )
            .await
            .expect("load and cross-check both branch grant representations"),
            vec![replacement_id]
        );
    }
}
