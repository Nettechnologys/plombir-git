//! Git LFS File Locking — `git lfs lock`, `unlock`, `locks` and the
//! `locks/verify` call a client makes before every push (card_e8afcaf3edf6).
//!
//! A lock records that one person is editing a file nobody can merge, and the
//! stock client enforces it: before a push it asks `locks/verify` which locks
//! are its own and which are someone else's, and refuses to push a change to a
//! path in the second list — but only when it is configured with
//! `lfs.locksverify = true`. Without that setting it warns and pushes anyway,
//! so the server enforces the lock too: receive-pack refuses a ref whose new
//! commits change a path someone else holds ([`held_by_others`] feeds that
//! check, card_4a40b70a6796), over HTTP and SSH alike.
//!
//! A lock covers a path on every branch. The client sends the ref it is on,
//! and it is kept for display, but a lock scoped to one branch would let the
//! same file be edited on another and then merged over the locked work — the
//! exact thing a lock exists to prevent.
//!
//! Who may do what is the HTTP layer's decision, made with the repository gate
//! before anything here runs: listing needs read access, locking, verifying and
//! unlocking need write access, and unlocking someone else's lock needs `force`
//! from an administrator of the repository.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use serde::Serialize;
use std::collections::HashMap;

use rg_db::entities::lfs_lock::Model as LfsLock;
use rg_db::ops::lfs_lock_ops::{self, LockAttempt, LockQuery};

/// The longest path a lock can name — the width of `lfs_locks.path`, chosen
/// to fit the unique index into every supported database.
pub const MAX_LOCK_PATH_CHARS: usize = 512;

/// The page size when a client names none, and the largest it may ask for.
pub const DEFAULT_PAGE_SIZE: u64 = 100;
pub const MAX_PAGE_SIZE: u64 = 1000;

/// A lock as the LFS API spells it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct LockView {
    pub id: String,
    pub path: String,
    pub locked_at: DateTime<Utc>,
    pub owner: LockOwner,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct LockOwner {
    pub name: String,
}

/// What `POST /locks` did.
pub enum CreateOutcome {
    Created(LockView),
    /// The path is already locked — by this lock, which the client shows.
    AlreadyLocked(LockView),
}

/// One page of locks and the cursor of the next, empty when there is none.
pub struct LockPage {
    pub locks: Vec<LockView>,
    pub next_cursor: Option<String>,
}

/// One page of `locks/verify`.
pub struct VerifyPage {
    pub ours: Vec<LockView>,
    pub theirs: Vec<LockView>,
    pub next_cursor: Option<String>,
}

/// Check a path a client asked to lock, and return it as it will be stored.
///
/// Refused here rather than stored: an empty or absolute path, one with a
/// `NUL`, and one longer than the column — each is a request no lock could
/// honestly answer.
pub fn normalize_path(path: &str) -> Result<String> {
    let path = path.trim_start_matches("./");
    if path.is_empty() {
        return Err(crate::error::invalid_request("`path` must name a file"));
    }
    if path.starts_with('/') || path.contains('\0') || path.contains('\\') {
        return Err(crate::error::invalid_request(
            "`path` must be a path inside the repository, with `/` between its parts",
        ));
    }
    if path.split('/').any(|part| part == ".." || part.is_empty()) {
        return Err(crate::error::invalid_request(
            "`path` must not contain `..` or empty parts",
        ));
    }
    if path.chars().count() > MAX_LOCK_PATH_CHARS {
        return Err(crate::error::invalid_request(format!(
            "`path` is longer than the {MAX_LOCK_PATH_CHARS} characters a lock can name"
        )));
    }
    Ok(path.to_string())
}

/// The page size a client asked for, within bounds.
pub fn page_size(requested: Option<u64>) -> Result<u64> {
    match requested {
        None => Ok(DEFAULT_PAGE_SIZE),
        Some(0) => Err(crate::error::invalid_request("`limit` must be at least 1")),
        Some(limit) => Ok(limit.min(MAX_PAGE_SIZE)),
    }
}

/// A cursor or lock id the client sent back, which is always one this server
/// handed out: a decimal lock id.
pub fn parse_lock_id(raw: &str, field: &str) -> Result<i64> {
    raw.parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| crate::error::invalid_request(format!("`{field}` is not a lock id")))
}

/// Lock `path` for `owner_id`.
pub async fn create_lock(
    db: &DatabaseConnection,
    repo_id: i64,
    owner_id: i64,
    path: &str,
    ref_name: Option<&str>,
) -> Result<CreateOutcome> {
    let path = normalize_path(path)?;
    let attempt = lfs_lock_ops::create(db, repo_id, &path, ref_name, owner_id, Utc::now()).await?;
    Ok(match attempt {
        LockAttempt::Created(lock) => CreateOutcome::Created(view_one(db, lock).await?),
        LockAttempt::Held(lock) => CreateOutcome::AlreadyLocked(view_one(db, lock).await?),
    })
}

/// Which locks `GET /locks` asks for.
#[derive(Default)]
pub struct LockFilter<'a> {
    pub path: Option<&'a str>,
    pub id: Option<&'a str>,
    pub cursor: Option<&'a str>,
    pub limit: Option<u64>,
}

/// One page of the locks of `repo_id` matching `filter`.
pub async fn list_locks(
    db: &DatabaseConnection,
    repo_id: i64,
    filter: LockFilter<'_>,
) -> Result<LockPage> {
    let limit = page_size(filter.limit)?;
    let path = filter.path.filter(|path| !path.is_empty());
    let id = match filter.id.filter(|id| !id.is_empty()) {
        Some(raw) => Some(parse_lock_id(raw, "id")?),
        None => None,
    };
    let after_id = cursor(filter.cursor)?;
    let query = LockQuery { path, id, after_id };
    let (locks, more) = lfs_lock_ops::list(db, repo_id, &query, limit).await?;
    let next_cursor = next_cursor(&locks, more);
    Ok(LockPage {
        locks: view_all(db, locks).await?,
        next_cursor,
    })
}

/// One page of `locks/verify` for `holder`: the holder's own locks and
/// everyone else's, each walked by the same cursor. With no holder — a deploy
/// key, which cannot hold a lock — every lock is someone else's.
pub async fn verify_locks(
    db: &DatabaseConnection,
    repo_id: i64,
    holder: Option<i64>,
    cursor_raw: Option<&str>,
    limit: Option<u64>,
) -> Result<VerifyPage> {
    let limit = page_size(limit)?;
    let after_id = cursor(cursor_raw)?;
    let (locks, more) = lfs_lock_ops::list(
        db,
        repo_id,
        &LockQuery {
            after_id,
            ..Default::default()
        },
        limit,
    )
    .await?;
    let next_cursor = next_cursor(&locks, more);
    let (ours, theirs): (Vec<_>, Vec<_>) = locks
        .into_iter()
        .partition(|lock| Some(lock.owner_id) == holder);
    Ok(VerifyPage {
        ours: view_all(db, ours).await?,
        theirs: view_all(db, theirs).await?,
        next_cursor,
    })
}

/// The locks in `repo_id` someone other than `pusher` holds, in the shape the
/// receive-pack check enforces (card_4a40b70a6796). With no pusher — a deploy
/// key, which cannot hold a lock — every lock is someone else's.
pub async fn held_by_others(
    db: &DatabaseConnection,
    repo_id: i64,
    pusher: Option<i64>,
) -> Result<Vec<rg_git::protocol::receive_pack::ForeignLock>> {
    let locks = lfs_lock_ops::list_held_by_others(db, repo_id, pusher).await?;
    if locks.is_empty() {
        return Ok(Vec::new());
    }
    Ok(view_all(db, locks)
        .await?
        .into_iter()
        .map(|lock| rg_git::protocol::receive_pack::ForeignLock {
            path: lock.path,
            owner: lock.owner.name,
        })
        .collect())
}

/// What `POST /locks/{id}/unlock` found.
pub enum UnlockOutcome {
    Unlocked(LockView),
    NotFound,
    /// The lock is someone else's and the caller did not say `force`.
    OwnedByAnother(LockView),
}

/// Remove lock `id` for `actor_id`. With `force`, a lock held by someone else
/// goes too — the caller has already established that `actor_id` administers
/// the repository; without it such a lock is reported, not removed.
pub async fn unlock(
    db: &DatabaseConnection,
    repo_id: i64,
    actor_id: i64,
    id: i64,
    force: bool,
) -> Result<UnlockOutcome> {
    let Some(lock) = lfs_lock_ops::find_by_id(db, repo_id, id).await? else {
        return Ok(UnlockOutcome::NotFound);
    };
    let view = view_one(db, lock.clone()).await?;
    if lock.owner_id != actor_id && !force {
        return Ok(UnlockOutcome::OwnedByAnother(view));
    }
    // An owner's unlock is conditional on still owning it; a forced one is
    // not, because taking another person's lock away is what it is for.
    let condition = (!force).then_some(actor_id);
    if lfs_lock_ops::delete(db, repo_id, id, condition).await? {
        Ok(UnlockOutcome::Unlocked(view))
    } else {
        Ok(UnlockOutcome::NotFound)
    }
}

fn cursor(raw: Option<&str>) -> Result<Option<i64>> {
    match raw.filter(|raw| !raw.is_empty()) {
        Some(raw) => parse_lock_id(raw, "cursor").map(Some),
        None => Ok(None),
    }
}

fn next_cursor(locks: &[LfsLock], more: bool) -> Option<String> {
    more.then(|| locks.last().map(|lock| lock.id.to_string()))
        .flatten()
}

async fn view_one(db: &DatabaseConnection, lock: LfsLock) -> Result<LockView> {
    Ok(view_all(db, vec![lock])
        .await?
        .pop()
        .expect("one lock in, one view out"))
}

/// The API's view of `locks`, owner names read in one query.
async fn view_all(db: &DatabaseConnection, locks: Vec<LfsLock>) -> Result<Vec<LockView>> {
    let mut owners: Vec<i64> = locks.iter().map(|lock| lock.owner_id).collect();
    owners.sort_unstable();
    owners.dedup();
    let names: HashMap<i64, String> = rg_db::ops::user_ops::find_by_ids(db, &owners)
        .await
        .context("failed to read the owners of LFS locks")?
        .into_iter()
        .map(|user| (user.id, user.username))
        .collect();
    Ok(locks
        .into_iter()
        .map(|lock| LockView {
            id: lock.id.to_string(),
            owner: LockOwner {
                // The foreign key cascades, so a lock's owner exists; an empty
                // name is what a row read mid-deletion gets.
                name: names.get(&lock.owner_id).cloned().unwrap_or_default(),
            },
            path: lock.path,
            locked_at: lock.locked_at,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_path_is_one_inside_the_repository() {
        assert_eq!(normalize_path("art/hero.psd").unwrap(), "art/hero.psd");
        assert_eq!(normalize_path("./art/hero.psd").unwrap(), "art/hero.psd");
        for refused in [
            "",
            "/etc/passwd",
            "art/../secret",
            "art//hero.psd",
            "art\\hero.psd",
            "nul\0byte",
        ] {
            assert!(normalize_path(refused).is_err(), "{refused:?} was accepted");
        }
        assert!(normalize_path(&"a".repeat(MAX_LOCK_PATH_CHARS)).is_ok());
        assert!(normalize_path(&"a".repeat(MAX_LOCK_PATH_CHARS + 1)).is_err());
    }

    #[test]
    fn a_page_size_is_bounded_and_never_zero() {
        assert_eq!(page_size(None).unwrap(), DEFAULT_PAGE_SIZE);
        assert_eq!(page_size(Some(5)).unwrap(), 5);
        assert_eq!(page_size(Some(1_000_000)).unwrap(), MAX_PAGE_SIZE);
        assert!(page_size(Some(0)).is_err());
    }
}
