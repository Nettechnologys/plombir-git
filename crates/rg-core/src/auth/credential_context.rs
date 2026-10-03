//! What the credential behind the current request is allowed to do beyond its
//! account's own rights — the narrowing a Personal Access Token carries.
//!
//! An account's permissions are answered by the repository and branch gates.
//! A token can be narrower than its account: confined to a few repositories,
//! to a few MCP tools, or kept off protected branches (card_60a80311d512). The
//! first two are decided at the HTTP edge, where the route and the tool are
//! known. The third is not: a protected branch is moved by a merge, by a
//! server-side commit from the contents editor or a review suggestion, by the
//! merge queue — all of them deep in this crate, behind signatures that know
//! the acting *account* and nothing about the credential it presented.
//!
//! So the HTTP layer that resolved the token publishes what it allows here, for
//! the duration of the request, and [`refuse_protected_write`] is asked where a
//! write is *initiated* by the caller: [`crate::branch_protection::server_side::
//! ServerSideCommitPolicy::load`], which every server-side commit (contents
//! editor, review suggestions) passes through, and the REST routes that merge
//! or schedule a merge (merge, auto-merge, the merge queue). Not in
//! `check_merge_allowed`: that check also runs for merges the server performs on
//! other people's behalf inside the same request — the auto-merge an approval
//! completes, a queue pass — and a narrowing meant for this caller must not
//! fail those. Work detached from the request (a spawned task) runs outside the
//! scope and sees no narrowing at all, which is why the routes that *schedule*
//! a later merge refuse a narrowed token up front.
//!
//! The same context is what [`crate::audit::record`] stamps on every audit row
//! written during the request, so a mutation an agent made says which token —
//! and which MCP tool — it came through.

use std::future::Future;

use anyhow::Result;
use sea_orm::DatabaseConnection;

tokio::task_local! {
    static CREDENTIAL: Option<CredentialContext>;
}

/// The narrowing published for one request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CredentialContext {
    /// The account the credential belongs to.
    pub user_id: i64,
    /// The Personal Access Token presented, when it was one.
    pub token_id: Option<i64>,
    /// The MCP tool this request is serving, when it is an inner call made by
    /// this instance's MCP endpoint on the agent's behalf.
    pub mcp_tool: Option<String>,
    /// Whether every write landing on a protected branch is refused.
    pub deny_protected_writes: bool,
}

/// Run `future` with `context` published as the current request's credential.
pub async fn scope<F: Future>(context: CredentialContext, future: F) -> F::Output {
    CREDENTIAL.scope(Some(context), future).await
}

/// The credential published for the current request, if any.
pub fn current() -> Option<CredentialContext> {
    CREDENTIAL.try_with(Clone::clone).ok().flatten()
}

/// Refuse a write to `branch` of `repo_id` when the current credential is kept
/// off protected branches and a protection rule covers that branch.
///
/// A rule covers a branch by the same matching the push path uses, so a token
/// cannot reach over a server-side path what it could not reach with `git
/// push`. The refusal is a typed `Forbidden` (403) and is written to the audit
/// log as `agent.scope_denied`; a rule lookup that fails propagates as the
/// server's own error.
pub async fn refuse_protected_write(
    db: &DatabaseConnection,
    repo_id: i64,
    branch: &str,
) -> Result<()> {
    let Some(context) = current().filter(|context| context.deny_protected_writes) else {
        return Ok(());
    };
    let rules = rg_db::ops::protected_branch_ops::list_rules_by_repo(db, repo_id).await?;
    let target = format!("refs/heads/{branch}");
    let protected = rules.iter().any(|rule| {
        rg_git::protocol::receive_pack::ref_matches_rejection_pattern(
            &target,
            &format!("refs/heads/{}", rule.protection.branch_name),
        )
    });
    if !protected {
        return Ok(());
    }

    let actor = crate::audit::AuditActor::resolve_after_the_fact(db, context.user_id).await;
    crate::audit::record(
        db,
        &actor,
        SCOPE_DENIED_ACTION,
        Some("repo"),
        Some(repo_id),
        None,
        None,
        Some(serde_json::json!({
            "reason": "protected_branch",
            "branch": branch,
        })),
    )
    .await;
    Err(crate::error::forbidden(format!(
        "this token may not write to protected branch '{branch}'"
    )))
}

/// The audit action of every refusal a token's narrowing produces.
pub const SCOPE_DENIED_ACTION: &str = "agent.scope_denied";

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nothing_is_published_outside_a_scope() {
        assert_eq!(current(), None);
    }

    #[tokio::test]
    async fn a_scope_publishes_its_context_and_a_spawned_task_does_not_inherit_it() {
        let context = CredentialContext {
            user_id: 7,
            token_id: Some(3),
            mcp_tool: Some("create_issue".to_string()),
            deny_protected_writes: true,
        };
        scope(context.clone(), async {
            assert_eq!(current(), Some(context.clone()));
            // Detached work — post-push hooks, deliveries — is not the
            // request's, and must not run under its narrowing.
            let detached = tokio::spawn(async { current() }).await.unwrap();
            assert_eq!(detached, None);
        })
        .await;
        assert_eq!(current(), None);
    }
}
