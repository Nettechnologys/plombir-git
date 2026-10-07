//! What a repository's LFS store holds, and removing what nothing needs
//! (card_9e4dd3f8330c).
//!
//! An LFS object used to live until its repository was deleted. A force-push,
//! a rewritten history or an abandoned branch left bytes in the store that
//! nobody could see or remove. An administrator can now see the store's size
//! and every object in it, ask which objects no ref's history points at, and
//! remove those.
//!
//! ## What is never removed
//!
//! * An object the history of any ref points at — branches, tags, and the
//!   server's own refs (pull request heads among them), since each of those can
//!   be checked out.
//! * An object younger than [`ORPHAN_GRACE`]. `git lfs push` uploads the
//!   objects first and moves the ref afterwards, so a fresh object nothing
//!   points at yet is a push in flight, not garbage.
//! * An object whose publication is in progress (its row holds the lease).
//! * An object a push was told, within [`ORPHAN_GRACE`], is already stored
//!   (`last_claimed_at`). That push sends no bytes and moves its ref
//!   afterwards, so until then the object looks exactly like an old orphan.
//!
//! Nothing is removed on the strength of an old answer: [`prune`] reads the
//! refs again itself, after the administrator chose, and each row goes only
//! under a conditional `DELETE` that repeats the age and lease checks.
//!
//! ## Shared objects
//!
//! A fork and a merged pull request get their own row and their own storage
//! key for every object they share with another repository — on local storage
//! the bytes are a hard link. Removing one repository's object therefore
//! removes its row and its key and never another repository's copy.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use sea_orm::DatabaseConnection;
use serde::Serialize;

use crate::blob_storage::BlobStorage;
use crate::lfs::service::LfsRepository;
use rg_db::entities::lfs_object::Model as LfsObject;

/// How old an object nothing points at must be before it may be removed.
///
/// A push uploads its objects and then updates the ref; a day is far longer
/// than any push takes, and short enough that a mistaken upload does not
/// stay forever.
pub const ORPHAN_GRACE: Duration = Duration::hours(24);

/// The store's size, as the settings page shows it.
#[derive(Debug, Serialize, PartialEq)]
pub struct LfsUsage {
    pub object_count: u64,
    pub total_bytes: i64,
}

/// One object as the settings page lists it.
#[derive(Debug, Serialize, PartialEq)]
pub struct LfsObjectView {
    pub oid: String,
    pub size: i64,
    /// `false` for an object announced to the batch API whose bytes never
    /// arrived.
    pub uploaded: bool,
    pub created_at: DateTime<Utc>,
}

impl From<&LfsObject> for LfsObjectView {
    fn from(row: &LfsObject) -> Self {
        Self {
            oid: row.oid.clone(),
            size: row.size,
            uploaded: row.uploaded,
            created_at: row.created_at,
        }
    }
}

/// How many objects the store holds and how many bytes they declare.
pub async fn usage(db: &DatabaseConnection, repo_id: i64) -> Result<LfsUsage> {
    let (object_count, total_bytes) = rg_db::ops::lfs_object_ops::usage(db, repo_id).await?;
    Ok(LfsUsage {
        object_count,
        total_bytes,
    })
}

/// One page of the store, oldest first, and the cursor of the next page.
pub async fn list_objects(
    db: &DatabaseConnection,
    repo_id: i64,
    cursor: Option<&str>,
    limit: Option<u64>,
) -> Result<(Vec<LfsObjectView>, Option<String>)> {
    let limit = crate::lfs::locks::page_size(limit)?;
    let after_id = match cursor.filter(|cursor| !cursor.is_empty()) {
        Some(raw) => Some(crate::lfs::locks::parse_lock_id(raw, "cursor")?),
        None => None,
    };
    let (rows, more) = rg_db::ops::lfs_object_ops::list_page(db, repo_id, after_id, limit).await?;
    let next = more
        .then(|| rows.last().map(|row| row.id.to_string()))
        .flatten();
    Ok((rows.iter().map(LfsObjectView::from).collect(), next))
}

/// The objects nothing needs and that are old enough to go, as of `now`.
pub async fn find_orphans(
    db: &DatabaseConnection,
    repo_id: i64,
    repo_path: &Path,
    now: DateTime<Utc>,
) -> Result<Vec<LfsObjectView>> {
    let referenced = referenced_oids(repo_path).await?;
    let cutoff = now - ORPHAN_GRACE;
    Ok(rg_db::ops::lfs_object_ops::list_by_repo(db, repo_id)
        .await?
        .iter()
        .filter(|row| is_orphan(row, &referenced, cutoff))
        .map(LfsObjectView::from)
        .collect())
}

fn is_orphan(row: &LfsObject, referenced: &BTreeSet<String>, cutoff: DateTime<Utc>) -> bool {
    row.created_at < cutoff
        && !claimed_since(row, cutoff)
        && row.publisher_token.is_none()
        && !referenced.contains(&row.oid)
}

fn claimed_since(row: &LfsObject, cutoff: DateTime<Utc>) -> bool {
    row.last_claimed_at.is_some_and(|claimed| claimed >= cutoff)
}

async fn referenced_oids(repo_path: &Path) -> Result<BTreeSet<String>> {
    let path = repo_path.to_path_buf();
    crate::blocking::run_blocking_git("LFS reference scan", move || {
        crate::lfs::pointer::oids_reachable_from_refs(&path)
    })
    .await
    .context("failed to read which LFS objects the repository's refs point at")
}

/// What [`prune`] did with each object it was asked to remove.
#[derive(Debug, Default, Serialize, PartialEq)]
pub struct PruneOutcome {
    /// Removed: row and stored bytes.
    pub deleted: Vec<String>,
    /// Kept, each with the reason.
    pub kept: Vec<KeptObject>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct KeptObject {
    pub oid: String,
    pub reason: &'static str,
}

/// Remove the objects named in `oids` that are, as of `now`, orphans — and
/// keep, with a reason, every one that is not.
pub async fn prune(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_root: &Path,
    repo: LfsRepository<'_>,
    repo_path: &Path,
    oids: &[String],
    now: DateTime<Utc>,
) -> Result<PruneOutcome> {
    // Read now, after the administrator chose: the list they chose from is
    // minutes old, and a push may have pointed a ref at one of them since.
    let referenced = referenced_oids(repo_path).await?;
    let cutoff = now - ORPHAN_GRACE;
    let rows = rg_db::ops::lfs_object_ops::list_by_repo(db, repo.id).await?;
    let legacy_root = crate::lfs::service::lfs_root(repo_root, repo.owner, repo.name);

    let mut outcome = PruneOutcome::default();
    let wanted: BTreeSet<&str> = oids.iter().map(String::as_str).collect();
    for oid in wanted {
        let keep = |reason| KeptObject {
            oid: oid.to_string(),
            reason,
        };
        let Some(row) = rows.iter().find(|row| row.oid == oid) else {
            outcome
                .kept
                .push(keep("this repository has no such object"));
            continue;
        };
        if referenced.contains(oid) {
            outcome.kept.push(keep("a ref's history points at it"));
            continue;
        }
        if row.created_at >= cutoff {
            outcome.kept.push(keep(
                "uploaded too recently; a push may still point a ref at it",
            ));
            continue;
        }
        if claimed_since(row, cutoff) {
            outcome.kept.push(keep(
                "a push was recently told it is stored and may still point a ref at it",
            ));
            continue;
        }
        if !rg_db::ops::lfs_object_ops::delete_unused(db, repo.id, row.id, cutoff).await? {
            outcome
                .kept
                .push(keep("an upload or a push of it is in progress"));
            continue;
        }
        // The row is gone first, so a failure below leaves bytes nothing
        // claims — never a row that promises bytes which are not there.
        remove_stored_bytes(storage, &legacy_root, repo, oid).await;
        outcome.deleted.push(oid.to_string());
    }
    Ok(outcome)
}

/// Remove every place the object's bytes may be stored for this repository:
/// both blob keys and both legacy files. A failure is logged, not returned —
/// the row is already gone, and bytes nothing claims are a leak, not a fault a
/// caller could repair.
async fn remove_stored_bytes(
    storage: &dyn BlobStorage,
    legacy_root: &Path,
    repo: LfsRepository<'_>,
    oid: &str,
) {
    for compressed in [true, false] {
        let key = match crate::lfs::service::lfs_object_key(repo.owner, repo.name, oid, compressed)
        {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(oid, error = %format!("{error:#}"), "invalid LFS object key");
                continue;
            }
        };
        if let Err(error) = storage.delete(&key).await {
            tracing::warn!(
                repo_id = repo.id,
                oid,
                blob_key = %key,
                error = %format!("{error:#}"),
                "an unused LFS object's row is gone but its stored bytes could not be removed"
            );
        }
    }
    let legacy = legacy_root.join(&oid[..2]).join(oid);
    for path in [legacy.with_extension("zst"), legacy] {
        crate::platform::fs::discard_file_async("unused LFS object", &path).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(oid: &str, age_hours: i64, publishing: bool, now: DateTime<Utc>) -> LfsObject {
        LfsObject {
            id: 1,
            repo_id: 1,
            oid: oid.to_string(),
            size: 1,
            uploaded: true,
            created_at: now - Duration::hours(age_hours),
            publisher_token: publishing.then(|| "token".to_string()),
            publisher_since: publishing.then_some(now),
            last_claimed_at: None,
        }
    }

    #[test]
    fn only_an_old_unreferenced_idle_object_is_an_orphan() {
        let now = Utc::now();
        let cutoff = now - ORPHAN_GRACE;
        let referenced = BTreeSet::from(["a".repeat(64)]);
        assert!(!is_orphan(
            &row(&"a".repeat(64), 48, false, now),
            &referenced,
            cutoff
        ));
        assert!(!is_orphan(
            &row(&"b".repeat(64), 1, false, now),
            &referenced,
            cutoff
        ));
        assert!(!is_orphan(
            &row(&"b".repeat(64), 48, true, now),
            &referenced,
            cutoff
        ));
        assert!(is_orphan(
            &row(&"b".repeat(64), 48, false, now),
            &referenced,
            cutoff
        ));
    }
}
