// File: Push-time enforcement of branch / tag protection rules.
//! Push-time enforcement of branch and tag protection rules.
//!
//! Shared by every git-push entry point (HTTP smart protocol in `rg-http`,
//! SSH in `rg-ssh`). Keeping the reject/allow logic in one place prevents the
//! two protocol paths from drifting — a divergence here would silently make
//! branch protection weaker over one protocol than the other.
//!
//! Every decision here reads an allow-list out of a stored JSON column. A
//! column that does not decode is a broken row, not an empty allow-list: the
//! functions are fallible so the caller answers with a server failure instead
//! of telling a listed pusher, in the transport's own words, that the rule
//! excludes them. `NULL` still means "no allow-list configured".

use anyhow::{Context, Result};
use rg_db::entities::{protected_branch, protected_tag};

/// Refs that must be rejected because a protected-branch rule forbids this push.
///
/// Returns `(ref_name, human_readable_reason)` pairs. A rule is skipped when the
/// actor is explicitly allowed to push directly to it. Fails when a rule's
/// stored allow-list cannot be decoded — see the module comment.
pub fn branch_protection_rejected_refs(
    protections: Vec<protected_branch::Model>,
    actor_id: Option<i64>,
) -> Result<Vec<(String, String)>> {
    let mut rejected = Vec::new();

    for protection in protections {
        if direct_push_allowed_by_rule(&protection, actor_id)? {
            continue;
        }

        let message = if protection.require_pr {
            format!(
                "push to protected branch '{}' is not allowed; open a pull request instead",
                protection.branch_name
            )
        } else if !protection.allow_force_push {
            format!(
                "force push to protected branch '{}' is not allowed",
                protection.branch_name
            )
        } else {
            continue;
        };

        rejected.push((format!("refs/heads/{}", protection.branch_name), message));
    }

    Ok(rejected)
}

/// Whether `actor_id` is on the protection rule's direct-push allow-list.
///
/// The column is decoded whether or not the push is authenticated: a row that
/// does not decode is broken for every caller, and letting an anonymous push
/// slip past the decode would make the fault visible only to some of them.
fn direct_push_allowed_by_rule(
    protection: &protected_branch::Model,
    actor_id: Option<i64>,
) -> Result<bool> {
    let Some(allowed_json) = protection.allowed_push_user_ids.as_deref() else {
        return Ok(false);
    };
    let allowed_ids: Vec<i64> = serde_json::from_str(allowed_json).with_context(|| {
        format!(
            "stored allowed_push_user_ids of protected branch '{}' is not a JSON array of user ids",
            protection.branch_name
        )
    })?;

    Ok(actor_id.is_some_and(|uid| allowed_ids.contains(&uid)))
}

/// Branch refs (`refs/heads/…`) that require every pushed commit to be signed.
pub fn signed_commit_required_refs(protections: &[protected_branch::Model]) -> Vec<String> {
    protections
        .iter()
        .filter(|rule| rule.require_signed_commits)
        .map(|rule| format!("refs/heads/{}", rule.branch_name))
        .collect()
}

/// Tag refs that must be rejected because a protected-tag pattern forbids this push.
///
/// Returns `(ref_name, human_readable_reason)` pairs. A pattern is skipped when
/// the actor is on its allow-list. Fails when a pattern's stored allow-list
/// cannot be decoded — see the module comment.
pub fn tag_protection_rejected_refs(
    protections: Vec<protected_tag::Model>,
    actor_id: Option<i64>,
) -> Result<Vec<(String, String)>> {
    let mut rejected = Vec::new();

    for protection in protections {
        if tag_push_allowed_by_rule(&protection, actor_id)? {
            continue;
        }

        rejected.push((
            format!("refs/tags/{}", protection.pattern),
            format!(
                "creation or update of protected tag pattern '{}' is not allowed",
                protection.pattern
            ),
        ));
    }

    Ok(rejected)
}

/// Whether `actor_id` is on the protected-tag pattern's allow-list. Decodes
/// unconditionally, for the reason [`direct_push_allowed_by_rule`] gives.
fn tag_push_allowed_by_rule(
    protection: &protected_tag::Model,
    actor_id: Option<i64>,
) -> Result<bool> {
    let Some(allowed_json) = protection.allowed_user_ids.as_deref() else {
        return Ok(false);
    };
    let allowed_ids: Vec<i64> = serde_json::from_str(allowed_json).with_context(|| {
        format!(
            "stored allowed_user_ids of protected tag pattern '{}' is not a JSON array of user ids",
            protection.pattern
        )
    })?;

    Ok(actor_id.is_some_and(|uid| allowed_ids.contains(&uid)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn branch_rule(allowed_push_user_ids: Option<&str>) -> protected_branch::Model {
        protected_branch::Model {
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
            allowed_push_user_ids: allowed_push_user_ids.map(str::to_string),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn tag_rule(allowed_user_ids: Option<&str>) -> protected_tag::Model {
        protected_tag::Model {
            id: 1,
            repo_id: 1,
            pattern: "v*".to_string(),
            allowed_user_ids: allowed_user_ids.map(str::to_string),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn a_listed_pusher_is_not_rejected() {
        let rejected =
            branch_protection_rejected_refs(vec![branch_rule(Some("[7]"))], Some(7)).unwrap();
        assert!(rejected.is_empty());

        let rejected = tag_protection_rejected_refs(vec![tag_rule(Some("[7]"))], Some(7)).unwrap();
        assert!(rejected.is_empty());
    }

    #[test]
    fn an_unlisted_pusher_is_rejected_by_the_rule() {
        let rejected =
            branch_protection_rejected_refs(vec![branch_rule(Some("[7]"))], Some(9)).unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].0, "refs/heads/main");

        let rejected = tag_protection_rejected_refs(vec![tag_rule(Some("[7]"))], Some(9)).unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].0, "refs/tags/v*");
    }

    #[test]
    fn a_null_allow_list_still_means_nobody_is_exempt() {
        let rejected = branch_protection_rejected_refs(vec![branch_rule(None)], Some(7)).unwrap();
        assert_eq!(rejected.len(), 1);

        let rejected = tag_protection_rejected_refs(vec![tag_rule(None)], Some(7)).unwrap();
        assert_eq!(rejected.len(), 1);
    }

    #[test]
    fn an_undecodable_allow_list_fails_instead_of_blaming_the_pusher() {
        let error =
            branch_protection_rejected_refs(vec![branch_rule(Some("{\"7\":true}"))], Some(7))
                .unwrap_err();
        assert!(
            format!("{error:#}").contains("allowed_push_user_ids"),
            "{error:#}"
        );

        let error =
            tag_protection_rejected_refs(vec![tag_rule(Some("not json"))], Some(7)).unwrap_err();
        assert!(
            format!("{error:#}").contains("allowed_user_ids"),
            "{error:#}"
        );
    }
}
