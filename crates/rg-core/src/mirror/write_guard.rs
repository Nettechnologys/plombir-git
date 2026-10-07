//! A pull mirror's branches and tags are its upstream's, so nothing else may
//! write them while the mirror is on (card_97a2c0209056).
//!
//! Every pass force-updates `refs/heads/*` and `refs/tags/*` to the upstream's
//! and prunes the rest (`service::publish_mirrored_refs`). A push the server
//! accepted with `ok` therefore lasted until the next pass and then vanished
//! without a trace — and so did a web edit, an applied suggestion, or a merged
//! pull request. GitHub and Gitea make a pull mirror read-only for exactly this
//! reason, and so does this module, at the three gates every writer already
//! passes through:
//!
//! * `receive-pack` over HTTP and SSH — the push policy
//!   (`branch_protection::push_rules::load_receive_pack_policy`) refuses every
//!   ref of the push before the pack is read;
//! * commits the server makes on a user's behalf — the web editor and review
//!   suggestions load `branch_protection::server_side::ServerSideCommitPolicy`;
//! * merges — the REST merge, auto-merge and the merge queue all ask
//!   `branch_protection::service::check_merge_allowed`.
//!
//! A mirror switched off (`status = inactive`) runs no passes and lifts the
//! refusal. A mirror whose last pass failed (`error`) is still retried by the
//! sweep and still overwrites, so it stays read-only.
//!
//! The refusal does not name the upstream: the merge-queue page shows a waiting
//! reason to everyone who can read the repository, and the mirror's settings —
//! where the URL lives — are deliberately visible to writers only.

use anyhow::Result;
use rg_db::entities::mirror::STATUS_INACTIVE;
use sea_orm::DatabaseConnection;

/// Why `repo_id`'s branches and tags may not be written, when they may not.
pub async fn read_only_reason(db: &DatabaseConnection, repo_id: i64) -> Result<Option<String>> {
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id).await?;
    Ok(mirror
        .filter(|mirror| mirror.status != STATUS_INACTIVE)
        .map(|_| {
            "this repository is a pull mirror: its branches and tags come from the upstream and \
             are read-only while the mirror is enabled — push to the upstream instead, or switch \
             the mirror off in the repository settings"
                .to_string()
        }))
}

/// Refuse a server-side write to `repo_id`'s branches or tags with `409` while
/// the repository is an enabled pull mirror.
///
/// `409` rather than `403`: the caller may write here, and nothing about the
/// request is wrong — the repository's own state is what refuses it, and
/// switching the mirror off is what changes the answer.
pub async fn refuse_mirrored_write(db: &DatabaseConnection, repo_id: i64) -> Result<()> {
    match read_only_reason(db, repo_id).await? {
        Some(reason) => Err(crate::error::conflict(reason)),
        None => Ok(()),
    }
}
