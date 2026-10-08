// File: Push-time enforcement of branch / tag protection rules.
//! Push-time enforcement of branch and tag protection rules.
//!
//! Shared by every git-push entry point (HTTP smart protocol in `rg-http`,
//! SSH in `rg-ssh`). Keeping the reject/allow logic in one place prevents the
//! two protocol paths from drifting — a divergence here would silently make
//! branch protection weaker over one protocol than the other.
//!
//! Every decision here receives an allow-list already verified against its
//! normalized FK rows. Loading that pair is fallible; the pure decision below
//! therefore cannot accidentally treat a broken JSON mirror as an empty list.
//!
//! [`load_receive_pack_policy`] is the one loader both transports call: it
//! adds to the protection rules the repository's pull mirror and the LFS locks
//! other people hold, so a new rule reaches HTTP and SSH in the same change.

use anyhow::{Context, Result};
use rg_db::ops::{protected_branch_ops, protected_tag_ops};
use rg_git::protocol::receive_pack::{validate_tag_protection_pattern, PushPolicy};
use sea_orm::DatabaseConnection;

/// Everything `receive-pack` must hold one push to in `repo_id`, for the
/// account `actor_id` (`None` for a deploy key).
///
/// `token_kept_off_protected` is the credential's own narrowing — a personal
/// access token kept off protected branches is refused every one of them
/// first, whatever the rules would let its account do (card_60a80311d512).
///
/// In order, first match wins:
/// 1. an enabled pull mirror refuses every ref (card_97a2c0209056);
/// 2. the token narrowing;
/// 3. branch protection, then tag protection.
///
/// A rule whose stored allow-list does not decode fails the whole load — the
/// caller answers with a server error, because refusing the ref instead would
/// blame the pusher for a broken row.
pub async fn load_receive_pack_policy(
    db: &DatabaseConnection,
    repo_id: i64,
    actor_id: Option<i64>,
    token_kept_off_protected: bool,
) -> Result<PushPolicy> {
    let mut rejected_refs = Vec::new();
    if let Some(reason) = crate::mirror::write_guard::read_only_reason(db, repo_id).await? {
        rejected_refs.push(("*".to_string(), reason));
    }

    let protection_rules = protected_branch_ops::list_rules_by_repo(db, repo_id)
        .await
        .context("load branch protections")?;
    if token_kept_off_protected {
        rejected_refs.extend(protection_rules.iter().map(|rule| {
            (
                format!("refs/heads/{}", rule.protection.branch_name),
                format!(
                    "this token may not write to protected branch '{}'",
                    rule.protection.branch_name
                ),
            )
        }));
    }
    let require_signed_refs = signed_commit_required_refs(&protection_rules);
    let fast_forward_only_refs = branch_protection_fast_forward_refs(&protection_rules);
    rejected_refs.extend(branch_protection_rejected_refs(protection_rules, actor_id)?);

    let tag_protection_rules = protected_tag_ops::list_rules_by_repo(db, repo_id)
        .await
        .context("load tag protections")?;
    rejected_refs.extend(tag_protection_rejected_refs(
        tag_protection_rules,
        actor_id,
    )?);

    let foreign_locks = crate::lfs::locks::held_by_others(db, repo_id, actor_id)
        .await
        .context("load the LFS locks other people hold")?;

    Ok(PushPolicy {
        rejected_refs,
        require_signed_refs,
        fast_forward_only_refs,
        foreign_locks,
    })
}

/// Refs that must be rejected because a protected-branch rule forbids this push.
///
/// Returns `(ref_name, human_readable_reason)` pairs. Only `require_pr` refuses
/// a ref outright, and the direct-push allow-list is exactly the exception to
/// it. Force push is a different question — whether the update *rewrites*
/// history, which needs the objects — and is answered after the pack is read,
/// from [`branch_protection_fast_forward_refs`] (card_a5c343996db3). It used to
/// be answered here, by refusing the whole ref: "push directly, never rewrite"
/// refused every fast-forward, and the allow-list skipped the rule entirely.
///
/// Fails when a rule's stored allow-list cannot be decoded — see the module
/// comment.
pub fn branch_protection_rejected_refs(
    protections: Vec<protected_branch_ops::Rule>,
    actor_id: Option<i64>,
) -> Result<Vec<(String, String)>> {
    let mut rejected = Vec::new();

    for protection in protections {
        if direct_push_allowed_by_rule(&protection, actor_id)? {
            continue;
        }
        if !protection.protection.require_pr {
            continue;
        }

        rejected.push((
            format!("refs/heads/{}", protection.protection.branch_name),
            format!(
                "push to protected branch '{}' is not allowed; open a pull request instead",
                protection.protection.branch_name
            ),
        ));
    }

    Ok(rejected)
}

/// Protected branches whose history may only grow: every rule that does not
/// allow force push, for every pusher — the direct-push allow-list lets its
/// members skip the pull request, not rewrite the branch.
pub fn branch_protection_fast_forward_refs(
    protections: &[protected_branch_ops::Rule],
) -> Vec<(String, String)> {
    protections
        .iter()
        .filter(|rule| !rule.protection.allow_force_push)
        .map(|rule| {
            (
                format!("refs/heads/{}", rule.protection.branch_name),
                format!(
                    "force push to protected branch '{}' is not allowed",
                    rule.protection.branch_name
                ),
            )
        })
        .collect()
}

/// Whether `actor_id` is on the protection rule's direct-push allow-list.
///
/// The column is decoded whether or not the push is authenticated: a row that
/// does not decode is broken for every caller, and letting an anonymous push
/// slip past the decode would make the fault visible only to some of them.
fn direct_push_allowed_by_rule(
    protection: &protected_branch_ops::Rule,
    actor_id: Option<i64>,
) -> Result<bool> {
    Ok(actor_id.is_some_and(|uid| protection.allowed_push_user_ids.contains(&uid)))
}

/// Branch refs (`refs/heads/…`) that require every pushed commit to be signed.
pub fn signed_commit_required_refs(protections: &[protected_branch_ops::Rule]) -> Vec<String> {
    protections
        .iter()
        .filter(|rule| rule.protection.require_signed_commits)
        .map(|rule| format!("refs/heads/{}", rule.protection.branch_name))
        .collect()
}

/// Tag refs that must be rejected because a protected-tag pattern forbids this push.
///
/// Returns `(ref_name, human_readable_reason)` pairs. A pattern is skipped when
/// the actor is on its allow-list. Fails when a pattern's stored allow-list
/// cannot be decoded, or when a historical pattern cannot be executed by the
/// receive-pack matcher — see the module comment.
pub fn tag_protection_rejected_refs(
    protections: Vec<protected_tag_ops::Rule>,
    actor_id: Option<i64>,
) -> Result<Vec<(String, String)>> {
    let mut rejected = Vec::new();

    for protection in protections {
        validate_tag_protection_pattern(&protection.protection.pattern).with_context(|| {
            format!(
                "stored tag protection pattern {:?} cannot be enforced",
                protection.protection.pattern
            )
        })?;
        if tag_push_allowed_by_rule(&protection, actor_id)? {
            continue;
        }

        rejected.push((
            format!("refs/tags/{}", protection.protection.pattern),
            format!(
                "creation or update of protected tag pattern '{}' is not allowed",
                protection.protection.pattern
            ),
        ));
    }

    Ok(rejected)
}

/// Whether `actor_id` is on the protected-tag pattern's allow-list. Decodes
/// unconditionally, for the reason [`direct_push_allowed_by_rule`] gives.
fn tag_push_allowed_by_rule(
    protection: &protected_tag_ops::Rule,
    actor_id: Option<i64>,
) -> Result<bool> {
    Ok(actor_id.is_some_and(|uid| protection.allowed_user_ids.contains(&uid)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn branch_rule(allowed_push_user_ids: &[i64]) -> protected_branch_ops::Rule {
        protected_branch_ops::Rule {
            allowed_push_user_ids: allowed_push_user_ids.to_vec(),
            protection: rg_db::entities::protected_branch::Model {
                id: 1,
                repo_id: 1,
                branch_name: "main".to_string(),
                require_pr: true,
                require_status_check: false,
                required_status_checks: None,
                require_approval: false,
                required_approvals: None,
                allow_force_push: false,
                require_signed_commits: false,
                allowed_push_user_ids: Some(serde_json::to_string(allowed_push_user_ids).unwrap()),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        }
    }

    fn tag_rule(pattern: &str, allowed_user_ids: &[i64]) -> protected_tag_ops::Rule {
        protected_tag_ops::Rule {
            allowed_user_ids: allowed_user_ids.to_vec(),
            protection: rg_db::entities::protected_tag::Model {
                id: 1,
                repo_id: 1,
                pattern: pattern.to_string(),
                allowed_user_ids: Some(serde_json::to_string(allowed_user_ids).unwrap()),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        }
    }

    #[test]
    fn a_listed_pusher_is_not_rejected() {
        let rejected = branch_protection_rejected_refs(vec![branch_rule(&[7])], Some(7)).unwrap();
        assert!(rejected.is_empty());

        let rejected = tag_protection_rejected_refs(vec![tag_rule("v*", &[7])], Some(7)).unwrap();
        assert!(rejected.is_empty());
    }

    #[test]
    fn an_unlisted_pusher_is_rejected_by_the_rule() {
        let rejected = branch_protection_rejected_refs(vec![branch_rule(&[7])], Some(9)).unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].0, "refs/heads/main");

        let rejected = tag_protection_rejected_refs(vec![tag_rule("v*", &[7])], Some(9)).unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].0, "refs/tags/v*");
    }

    /// card_a5c343996db3: only `require_pr` refuses a ref outright, and only
    /// for someone off the allow-list. Forbidding force push never refuses a
    /// ref here — it marks it fast-forward-only, for every pusher, the
    /// allow-list included.
    #[test]
    fn force_push_is_a_fast_forward_rule_not_a_refusal() {
        let mut direct = branch_rule(&[7]);
        direct.protection.require_pr = false;
        for actor in [Some(7), Some(9), None] {
            assert!(
                branch_protection_rejected_refs(vec![direct.clone()], actor)
                    .unwrap()
                    .is_empty(),
                "{actor:?}: a rule without require_pr refused the whole ref"
            );
        }
        assert_eq!(
            branch_protection_fast_forward_refs(&[direct.clone()]),
            vec![(
                "refs/heads/main".to_string(),
                "force push to protected branch 'main' is not allowed".to_string()
            )]
        );

        // The allow-list skips the pull request, not the force-push rule.
        let pr_only = branch_rule(&[7]);
        assert!(
            branch_protection_rejected_refs(vec![pr_only.clone()], Some(7))
                .unwrap()
                .is_empty()
        );
        assert_eq!(branch_protection_fast_forward_refs(&[pr_only]).len(), 1);

        let mut force_ok = direct;
        force_ok.protection.allow_force_push = true;
        assert!(branch_protection_fast_forward_refs(&[force_ok]).is_empty());
    }

    #[test]
    fn a_null_allow_list_still_means_nobody_is_exempt() {
        let rejected = branch_protection_rejected_refs(vec![branch_rule(&[])], Some(7)).unwrap();
        assert_eq!(rejected.len(), 1);

        let rejected = tag_protection_rejected_refs(vec![tag_rule("v*", &[])], Some(7)).unwrap();
        assert_eq!(rejected.len(), 1);
    }

    #[test]
    fn a_historical_unhonourable_tag_pattern_fails_closed_for_every_actor() {
        for pattern in ["v1.?", "v[0-9]*", "release+"] {
            let error = tag_protection_rejected_refs(vec![tag_rule(pattern, &[7])], Some(7))
                .expect_err("an allow-listed actor must still see a broken stored rule");
            let message = format!("{error:#}");
            assert!(message.contains(pattern), "{message}");
            assert!(message.contains("only '*' is supported"), "{message}");
        }
    }
}
