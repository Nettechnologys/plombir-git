//! Branches and tags created and deleted on a person's behalf — the branches
//! and tags API, a merge's "delete head branch", a release that needs its tag.
//!
//! Every one of these goes through [`rg_git::protocol::receive_pack::
//! apply_server_ref_updates`] under the policy a `git push` from the same
//! person meets ([`crate::branch_protection::push_rules::
//! load_receive_pack_policy`]). That is the whole point of the module: a ref
//! moved over HTTP, over SSH or through the API must be refused for the same
//! reasons, with the same words (card_2060696224ff).

use std::path::Path;

use anyhow::{Context, Result};
use rg_git::protocol::receive_pack::{apply_server_ref_updates, RefUpdate};
use sea_orm::DatabaseConnection;

/// The all-zero object id: "this ref had no value" / "remove this ref".
pub const NULL_SHA: &str = "0000000000000000000000000000000000000000";

/// The current value of `refname` — the object it names, unpeeled, so an
/// annotated tag answers with the tag object a deletion must name — or `None`
/// when there is no such ref.
pub fn ref_value(repo_path: &Path, refname: &str) -> Result<Option<String>> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {repo_path:?}"))?;
    let Some(reference) = repo
        .try_find_reference(refname)
        .with_context(|| format!("failed to look up {refname}"))?
    else {
        return Ok(None);
    };
    Ok(reference.target().try_id().map(|id| id.to_string()))
}

/// The commit `revision` names — a branch, a tag, or an object id — or a
/// typed [`crate::error::InvalidRequest`] naming what did not resolve.
pub fn resolve_commit(repo_path: &Path, revision: &str) -> Result<String> {
    if revision.is_empty() || revision.starts_with('-') || revision.contains("..") {
        return Err(crate::error::invalid_request(format!(
            "'{revision}' is not a branch, tag or commit in this repository"
        )));
    }
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {repo_path:?}"))?;
    let unresolved = || {
        crate::error::invalid_request(format!(
            "'{revision}' is not a branch, tag or commit in this repository"
        ))
    };
    let Ok(object) = repo.rev_parse_single(revision) else {
        return Err(unresolved());
    };
    let object = object.object().context("failed to read the named object")?;
    let commit = match object.peel_to_commit() {
        Ok(commit) => commit.id.to_string(),
        Err(_) => return Err(unresolved()),
    };
    Ok(commit)
}

/// Apply one ref change under the caller's push policy.
///
/// `actor_id` is the account the change is made for — the one whose
/// protection exemptions apply. A token kept off protected branches is held to
/// that here too, through the request's published credential.
///
/// A refusal is a typed [`crate::error::Conflict`] carrying the reason a
/// pusher would read in its `ng` line. The landed [`RefUpdate`] comes back for
/// the caller to hand to the post-push hooks — webhooks, CI cancellation, the
/// open-PR head refresh — exactly as a push does.
pub async fn change_ref(
    db: &DatabaseConnection,
    repo_path: &Path,
    repo_id: i64,
    actor_id: Option<i64>,
    refname: &str,
    old_sha: &str,
    new_sha: &str,
) -> Result<RefUpdate> {
    let token_kept_off_protected = crate::auth::credential_context::current()
        .is_some_and(|context| context.deny_protected_writes);
    let policy = crate::branch_protection::push_rules::load_receive_pack_policy(
        db,
        repo_id,
        actor_id,
        token_kept_off_protected,
    )
    .await
    .context("load push policy")?;
    let mut updates = apply_server_ref_updates(
        repo_path,
        vec![(
            old_sha.to_string(),
            new_sha.to_string(),
            refname.to_string(),
        )],
        &policy,
    )
    .await?;
    let update = updates.pop().context("ref change produced no outcome")?;
    if update.status != "ok" {
        return Err(crate::error::conflict(update.message));
    }
    Ok(update)
}

/// Create `refs/heads/<branch>` at the commit `from` names.
pub async fn create_branch(
    db: &DatabaseConnection,
    repo_path: &Path,
    repo_id: i64,
    actor_id: Option<i64>,
    branch: &str,
    from: &str,
) -> Result<RefUpdate> {
    crate::auth::credential_context::refuse_protected_write(db, repo_id, branch).await?;
    let refname = format!("refs/heads/{branch}");
    if ref_value(repo_path, &refname)?.is_some() {
        return Err(crate::error::conflict(format!(
            "branch '{branch}' already exists"
        )));
    }
    let target = resolve_commit(repo_path, from)?;
    change_ref(
        db, repo_path, repo_id, actor_id, &refname, NULL_SHA, &target,
    )
    .await
}

/// Create the lightweight tag `refs/tags/<tag>` at the commit `target` names.
pub async fn create_tag(
    db: &DatabaseConnection,
    repo_path: &Path,
    repo_id: i64,
    actor_id: Option<i64>,
    tag: &str,
    target: &str,
) -> Result<RefUpdate> {
    let refname = format!("refs/tags/{tag}");
    if ref_value(repo_path, &refname)?.is_some() {
        return Err(crate::error::conflict(format!(
            "tag '{tag}' already exists"
        )));
    }
    let commit = resolve_commit(repo_path, target)?;
    change_ref(
        db, repo_path, repo_id, actor_id, &refname, NULL_SHA, &commit,
    )
    .await
}

/// Delete `refs/heads/<branch>`, as `git push origin :<branch>` would.
pub async fn delete_branch(
    db: &DatabaseConnection,
    repo_path: &Path,
    repo_id: i64,
    actor_id: Option<i64>,
    branch: &str,
) -> Result<RefUpdate> {
    crate::auth::credential_context::refuse_protected_write(db, repo_id, branch).await?;
    delete_named_ref(
        db,
        repo_path,
        repo_id,
        actor_id,
        &format!("refs/heads/{branch}"),
        "branch",
    )
    .await
}

/// Delete `refs/tags/<tag>`, as `git push origin :refs/tags/<tag>` would.
pub async fn delete_tag(
    db: &DatabaseConnection,
    repo_path: &Path,
    repo_id: i64,
    actor_id: Option<i64>,
    tag: &str,
) -> Result<RefUpdate> {
    delete_named_ref(
        db,
        repo_path,
        repo_id,
        actor_id,
        &format!("refs/tags/{tag}"),
        "tag",
    )
    .await
}

async fn delete_named_ref(
    db: &DatabaseConnection,
    repo_path: &Path,
    repo_id: i64,
    actor_id: Option<i64>,
    refname: &str,
    kind: &'static str,
) -> Result<RefUpdate> {
    if rg_git::refname::validate_refname(refname).is_err() {
        return Err(crate::error::invalid_request(format!(
            "'{refname}' is not a valid {kind} name"
        )));
    }
    let Some(current) = ref_value(repo_path, refname)? else {
        return Err(crate::error::not_found(kind));
    };
    change_ref(
        db, repo_path, repo_id, actor_id, refname, &current, NULL_SHA,
    )
    .await
}

/// The head branch a merge just consumed, deleted — the "delete head branch
/// after merge" a person ticks on the merge form (card_2060696224ff).
///
/// Removed only while it still names `merged_sha`, the commit the merge took:
/// a push that landed on the branch after the merge must not be thrown away
/// with it. Kept, with the reason as a [`crate::error::Conflict`], when another
/// open pull request still reads the branch, when the person may not write to
/// the repository it lives in (a fork they do not own), or when a push rule
/// refuses the deletion — the same rules as `git push origin :<branch>`.
///
/// Returns the head repository's `(owner, name, path)` with the deletion, for
/// the caller's post-push hooks.
pub async fn delete_merged_head_branch(
    db: &DatabaseConnection,
    repo_root: &Path,
    pr: &rg_db::entities::pull_request::Model,
    actor_id: i64,
) -> Result<(String, String, std::path::PathBuf, RefUpdate)> {
    let head_repo_id = pr.head_repo_id.unwrap_or(pr.repo_id);
    let head_repo = rg_db::ops::repo_ops::find_by_id(db, head_repo_id)
        .await?
        .ok_or_else(|| crate::error::conflict("the head repository no longer exists"))?;
    if !crate::repo::service::can_write_repo(db, &head_repo, Some(actor_id)).await? {
        return Err(crate::error::conflict(
            "you may not delete branches in the head repository",
        ));
    }
    let Some(merged_sha) = pr.head_sha.as_deref() else {
        return Err(crate::error::conflict(
            "the merged head commit is not recorded",
        ));
    };
    let still_open =
        rg_db::ops::pull_request_ops::count_open_with_head(db, head_repo.id, &pr.head_branch)
            .await?;
    if still_open > 0 {
        return Err(crate::error::conflict(format!(
            "branch '{}' is the head of {still_open} other open pull request(s)",
            pr.head_branch
        )));
    }
    let owner =
        crate::repo::service::repository_namespace_name(db, head_repo.owner_id, head_repo.org_id)
            .await?;
    let path = repo_root.join(format!("{owner}/{}.git", head_repo.name));
    crate::auth::credential_context::refuse_protected_write(db, head_repo.id, &pr.head_branch)
        .await?;
    let update = change_ref(
        db,
        &path,
        head_repo.id,
        Some(actor_id),
        &format!("refs/heads/{}", pr.head_branch),
        merged_sha,
        NULL_SHA,
    )
    .await?;
    Ok((owner, head_repo.name, path, update))
}
