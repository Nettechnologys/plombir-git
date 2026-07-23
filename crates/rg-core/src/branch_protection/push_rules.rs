// File: Push-time enforcement of branch / tag protection rules.
//! Push-time enforcement of branch and tag protection rules.
//!
//! Shared by every git-push entry point (HTTP smart protocol in `rg-http`,
//! SSH in `rg-ssh`). Keeping the reject/allow logic in one place prevents the
//! two protocol paths from drifting — a divergence here would silently make
//! branch protection weaker over one protocol than the other.

use rg_db::entities::{protected_branch, protected_tag};

/// Refs that must be rejected because a protected-branch rule forbids this push.
///
/// Returns `(ref_name, human_readable_reason)` pairs. A rule is skipped when the
/// actor is explicitly allowed to push directly to it.
pub fn branch_protection_rejected_refs(
    protections: Vec<protected_branch::Model>,
    actor_id: Option<i64>,
) -> Vec<(String, String)> {
    protections
        .into_iter()
        .filter_map(|protection| {
            if direct_push_allowed_by_rule(&protection, actor_id) {
                return None;
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
                return None;
            };

            Some((format!("refs/heads/{}", protection.branch_name), message))
        })
        .collect()
}

/// Whether `actor_id` is on the protection rule's direct-push allow-list.
fn direct_push_allowed_by_rule(
    protection: &protected_branch::Model,
    actor_id: Option<i64>,
) -> bool {
    if let Some(uid) = actor_id {
        if let Some(allowed_json) = &protection.allowed_push_user_ids {
            if let Ok(allowed_ids) = serde_json::from_str::<Vec<i64>>(allowed_json) {
                if allowed_ids.contains(&uid) {
                    return true;
                }
            }
        }
    }

    false
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
/// the actor is on its allow-list.
pub fn tag_protection_rejected_refs(
    protections: Vec<protected_tag::Model>,
    actor_id: Option<i64>,
) -> Vec<(String, String)> {
    protections
        .into_iter()
        .filter_map(|protection| {
            let allowed = actor_id.is_some_and(|uid| {
                protection
                    .allowed_user_ids
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Vec<i64>>(json).ok())
                    .is_some_and(|ids| ids.contains(&uid))
            });
            (!allowed).then(|| {
                (
                    format!("refs/tags/{}", protection.pattern),
                    format!(
                        "creation or update of protected tag pattern '{}' is not allowed",
                        protection.pattern
                    ),
                )
            })
        })
        .collect()
}
