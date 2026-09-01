//! Push-policy adapter for commits created by ForgeKeep itself.
//!
//! HTTP and SSH pushes enter `rg-git` receive-pack, but the contents editor and
//! review suggestions create a local commit and push it over `file://`.  This
//! type makes those producers carry the same branch/actor decision and the
//! same commit-signature verification to the point where they move the ref.

use std::path::Path;

use anyhow::{Context, Result};
use sea_orm::DatabaseConnection;

use super::push_rules::{branch_protection_rejected_refs, signed_commit_required_refs};

const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// A completed push-policy decision for one server-side branch commit.
///
/// Fields stay private so production callers cannot manufacture an unchecked
/// permit.  Loading the value proves the actor/ref branch gate; presenting the
/// created commit to [`verify_created_commit`](Self::verify_created_commit)
/// proves the signature half immediately before the push.
#[derive(Debug)]
pub struct ServerSideCommitPolicy {
    target_ref: String,
    signed_commit_required_refs: Vec<String>,
}

impl ServerSideCommitPolicy {
    /// Load and apply the canonical branch/actor push-policy decision.
    pub async fn load(
        db: &DatabaseConnection,
        repo_id: i64,
        branch: &str,
        actor_id: i64,
    ) -> Result<Self> {
        let protections = rg_db::ops::protected_branch_ops::list_rules_by_repo(db, repo_id).await?;
        let signed_commit_required_refs = signed_commit_required_refs(&protections);
        let rejected_refs = branch_protection_rejected_refs(protections, Some(actor_id))?;
        let target_ref = format!("refs/heads/{branch}");

        if let Some((_, reason)) = rejected_refs.iter().find(|(pattern, _)| {
            rg_git::protocol::receive_pack::ref_matches_rejection_pattern(&target_ref, pattern)
        }) {
            return Err(crate::error::forbidden(reason.clone()));
        }

        Ok(Self {
            target_ref,
            signed_commit_required_refs,
        })
    }

    /// Verify every commit introduced by the prepared ref move.
    ///
    /// An unsigned commit is a policy refusal and therefore typed as 403.
    /// Failure to enumerate or cryptographically inspect commits is an
    /// operational error and deliberately remains untyped so HTTP maps it to
    /// 5xx instead of blaming the caller.
    pub fn verify_created_commit(
        &self,
        repo_path: &Path,
        old_sha: Option<&str>,
        new_sha: &str,
    ) -> Result<()> {
        let old_sha = old_sha.unwrap_or(ZERO_SHA);
        match rg_git::protocol::receive_pack::unsigned_commit_for_required_signature(
            repo_path,
            old_sha,
            new_sha,
            &self.target_ref,
            &self.signed_commit_required_refs,
        )
        .map_err(anyhow::Error::new)
        .with_context(|| {
            format!(
                "failed to verify server-side commit signatures for {}",
                self.target_ref
            )
        })? {
            Some(commit) => Err(crate::error::forbidden(format!(
                "commit {commit} does not have a cryptographically valid signature"
            ))),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_policy() -> ServerSideCommitPolicy {
        ServerSideCommitPolicy {
            target_ref: "refs/heads/main".to_string(),
            signed_commit_required_refs: vec!["refs/heads/main".to_string()],
        }
    }

    fn unsigned_commit() -> (tempfile::TempDir, String) {
        let temp = tempfile::tempdir().unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        git.run(&["init"], Some(temp.path()))
            .unwrap()
            .ensure_success()
            .unwrap();
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git.run(&["config", key, value], Some(temp.path()))
                .unwrap()
                .ensure_success()
                .unwrap();
        }
        std::fs::write(temp.path().join("README.md"), "unsigned").unwrap();
        git.run(&["add", "README.md"], Some(temp.path()))
            .unwrap()
            .ensure_success()
            .unwrap();
        git.run(&["commit", "-m", "unsigned"], Some(temp.path()))
            .unwrap()
            .ensure_success()
            .unwrap();
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_string();
        (temp, sha)
    }

    #[test]
    fn an_unsigned_created_commit_is_a_typed_policy_refusal() {
        let (repo, sha) = unsigned_commit();
        let error = signed_policy()
            .verify_created_commit(repo.path(), None, &sha)
            .expect_err("an unsigned commit must be rejected");
        assert!(error.downcast_ref::<crate::error::Forbidden>().is_some());
    }

    #[test]
    fn a_signature_verifier_failure_is_not_downgraded_to_a_policy_refusal() {
        let (repo, _) = unsigned_commit();
        let error = signed_policy()
            .verify_created_commit(repo.path(), None, "not-a-commit")
            .expect_err("an unreadable commit must fail verification");
        assert!(error.downcast_ref::<crate::error::Forbidden>().is_none());
        assert!(
            format!("{error:#}").contains("failed to enumerate commits"),
            "{error:#}"
        );
    }
}
