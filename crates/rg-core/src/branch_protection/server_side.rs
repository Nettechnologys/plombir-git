//! Push-policy adapter for commits created by Plombir Git itself.
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
    /// LFS locks someone other than the actor holds. receive-pack refuses a
    /// push that changes one of these paths, and a commit the server makes on
    /// the actor's behalf is held to the same lock (card_e486e8e09406).
    foreign_locks: Vec<rg_git::protocol::receive_pack::ForeignLock>,
}

impl ServerSideCommitPolicy {
    /// Load and apply the canonical branch/actor push-policy decision.
    pub async fn load(
        db: &DatabaseConnection,
        repo_id: i64,
        branch: &str,
        actor_id: i64,
    ) -> Result<Self> {
        // A pull mirror's next pass would overwrite this commit without a
        // trace, so it is refused before anything is built (card_97a2c0209056).
        crate::mirror::write_guard::refuse_mirrored_write(db, repo_id).await?;
        // The token behind the request may be narrower than its account: one
        // kept off protected branches is refused here, ahead of the rules that
        // would otherwise admit the account (card_60a80311d512).
        crate::auth::credential_context::refuse_protected_write(db, repo_id, branch).await?;
        let protections = rg_db::ops::protected_branch_ops::list_rules_by_repo(db, repo_id).await?;
        let signed_commit_required_refs = signed_commit_required_refs(&protections);
        let rejected_refs = branch_protection_rejected_refs(protections, Some(actor_id))?;
        let target_ref = format!("refs/heads/{branch}");

        if let Some((_, reason)) = rejected_refs.iter().find(|(pattern, _)| {
            rg_git::protocol::receive_pack::ref_matches_rejection_pattern(&target_ref, pattern)
        }) {
            return Err(crate::error::forbidden(reason.clone()));
        }
        let foreign_locks = crate::lfs::locks::held_by_others(db, repo_id, Some(actor_id))
            .await
            .context("load the LFS locks other people hold")?;

        Ok(Self {
            target_ref,
            signed_commit_required_refs,
            foreign_locks,
        })
    }

    /// Verify every commit introduced by the prepared ref move.
    ///
    /// An unsigned commit is a policy refusal and therefore typed as 403. A
    /// created commit that changes a path someone else has locked is `409`:
    /// the actor may write here, and the lock — state the holder can release —
    /// is what refuses it. Failure to enumerate or inspect commits is an
    /// operational error and deliberately remains untyped so HTTP maps it to
    /// 5xx instead of blaming the caller.
    ///
    /// `new_sha` is the one commit the server made on top of `old_sha` (or of
    /// the branch it started from); the lock half reads only what that commit
    /// changes.
    pub fn verify_created_commit(
        &self,
        repo_path: &Path,
        old_sha: Option<&str>,
        new_sha: &str,
    ) -> Result<()> {
        if let Some(lock) = rg_git::protocol::receive_pack::foreign_lock_changed_by_commit(
            repo_path,
            new_sha,
            &self.foreign_locks,
        )
        .with_context(|| format!("failed to check LFS locks for {}", self.target_ref))?
        {
            return Err(crate::error::conflict(lock.refusal()));
        }
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
            foreign_locks: Vec::new(),
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

    fn locked_by_alice(path: &str) -> ServerSideCommitPolicy {
        ServerSideCommitPolicy {
            target_ref: "refs/heads/main".to_string(),
            signed_commit_required_refs: Vec::new(),
            foreign_locks: vec![rg_git::protocol::receive_pack::ForeignLock {
                path: path.to_string(),
                owner: "alice".to_string(),
            }],
        }
    }

    /// `unsigned_commit` writes `README.md` in a root commit, the shape of a
    /// web edit in an empty repository; a second commit on top of it is the
    /// ordinary case.
    #[test]
    fn a_created_commit_changing_a_path_someone_else_locked_is_a_conflict() {
        let (repo, root) = unsigned_commit();
        let error = locked_by_alice("README.md")
            .verify_created_commit(repo.path(), None, &root)
            .expect_err("a root commit adding a locked path must be refused");
        assert!(error.downcast_ref::<crate::error::Conflict>().is_some());
        assert_eq!(
            error.to_string(),
            "path 'README.md' is locked by alice",
            "the writer must learn which path and who holds it"
        );

        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        std::fs::write(repo.path().join("notes.txt"), "free").unwrap();
        git.run(&["add", "notes.txt"], Some(repo.path()))
            .unwrap()
            .ensure_success()
            .unwrap();
        git.run(&["commit", "-m", "notes"], Some(repo.path()))
            .unwrap()
            .ensure_success()
            .unwrap();
        let notes = git
            .run(&["rev-parse", "HEAD"], Some(repo.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_string();
        // Only what this commit changes counts: README.md is locked and sits in
        // the tree, but the commit leaves it alone.
        locked_by_alice("README.md")
            .verify_created_commit(repo.path(), Some(&root), &notes)
            .expect("a commit that leaves the locked path alone must pass");
        let error = locked_by_alice("notes.txt")
            .verify_created_commit(repo.path(), Some(&root), &notes)
            .expect_err("a commit changing the locked path must be refused");
        assert!(error.downcast_ref::<crate::error::Conflict>().is_some());
    }
}
