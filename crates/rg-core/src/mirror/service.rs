//! Mirror service — business logic for repository mirroring.
//!
//! Supports creating a mirror of an external Git repository, periodic
//! sync via cron-like scheduling, and manual sync triggers.
//!
//! ## The remote's credential
//!
//! `mirrors.password_encrypted` holds the password/token for the upstream
//! remote. It is AES-256-GCM ciphertext, keyed the same way as every other
//! secret at rest in Plombir Git (`derive_key(encryption_key)` — see
//! `crate::auth::encryption`), and it is decrypted for exactly the duration of
//! one sync. That is why every entry point here takes `encryption_key`.
//!
//! On the way to `git` the plaintext travels through the **environment**, never
//! through argv and never through the remote URL: argv is world-readable on a
//! shared box (`ps`), and a URL with credentials in it is what git writes
//! verbatim into `.git/config` on disk. See
//! [`rg_git::credentials::credential_invocation`], which the import pipeline
//! shares — both carry a secret to a user-supplied remote.
//!
//! ## A credential typed into the URL
//!
//! `https://user:token@host/repo.git` puts the same secret in `mirrors.url`,
//! which is a plaintext column — past the encryption `password_encrypted`
//! provides, and back out through `MirrorResponse.url` and
//! `last_sync_error`. So the credential is taken out of the URL on the way in
//! ([`crate::net::split_url_credentials`]) and put where it belongs: the login
//! in `username`, the secret in `password_encrypted`. Rows written before that
//! are converted at startup by
//! [`crate::mirror::service::lift_legacy_url_credentials`], and
//! `mask_credential` is the last net in front of anything persisted or logged.

use super::transport::MirrorTransportPolicy;
use anyhow::{Context, Result};
use chrono::Utc;
use rg_db::entities::mirror::{
    ActiveModel, Model as Mirror, STATUS_ACTIVE, STATUS_ERROR, STATUS_INACTIVE,
};
use rg_db::entities::repository;
use rg_git::cli_gateway::global_gateway;
use rg_git::credentials::{credential_invocation, GitCredentials};
use sea_orm::ActiveValue::Set;
use sea_orm::{DatabaseConnection, EntityTrait};
use std::path::Path;

/// The shortest sync interval a mirror may be given.
///
/// Chosen to match the scheduler's own polling granularity rather than the
/// hour the settings form offers: a mirror cannot be refreshed more often than
/// the sweep runs, so anything below this is a number the server could not
/// honour anyway — while an hour-long floor in the API would forbid legitimate
/// frequent mirroring that the form simply does not offer.
pub const MIN_SYNC_INTERVAL_SECONDS: i64 = 60;

/// Reject an interval the scheduler cannot turn into a schedule.
///
/// `next_sync_at` is written as `now + sync_interval_seconds` and
/// [`rg_db::ops::mirror_ops::list_due_sync`] selects on `next_sync_at <= now`,
/// so a zero or negative interval makes the row *permanently* due: every tick
/// of the sweep picks it up, every tick spawns a `git remote update` against a
/// third-party host, and the pass moves the schedule forward by nothing at all.
/// A handful of such rows also fills the sweep's batch, so correctly configured
/// mirrors never reach the queue behind them (card_3d4c7b8b27c8).
///
/// `MirrorSyncConfig::validate` already refuses `poll_interval_secs = 0` for
/// exactly this reason — but that guards the knob an instance admin edits in a
/// config file, while this one is set over HTTP by any repository owner.
fn check_sync_interval(seconds: i64) -> Result<()> {
    if seconds < MIN_SYNC_INTERVAL_SECONDS {
        return Err(crate::error::invalid_request(format!(
            "`sync_interval_seconds` must be at least {MIN_SYNC_INTERVAL_SECONDS} (a zero or \
             negative interval is a busy loop, not a schedule, and the sweep cannot refresh a \
             mirror more often than it runs); set `status` to `{STATUS_INACTIVE}` to switch this \
             mirror off"
        )));
    }
    Ok(())
}

/// The interval a pass actually schedules by — the floor again, on the way out.
///
/// [`check_sync_interval`] stops a bad interval from being *written*, but a row
/// stored before that check existed — or written by any future path that
/// forgets it — would still compute `now + 0` and stay permanently due, which
/// is the whole failure. The pass is where the schedule is set, so this is the
/// last place that can guarantee a mirror leaves its own selection.
fn effective_sync_interval(mirror: &Mirror) -> i64 {
    let interval = mirror.sync_interval_seconds.max(MIN_SYNC_INTERVAL_SECONDS);
    if interval != mirror.sync_interval_seconds {
        tracing::warn!(
            repo_id = mirror.repo_id,
            mirror_id = mirror.id,
            stored = mirror.sync_interval_seconds,
            used = interval,
            "this mirror's stored sync interval is below the minimum and would keep it \
             permanently due; scheduling the next pass at the minimum instead"
        );
    }
    interval
}

/// Create a new mirror for a repository.
///
/// `password` is the plaintext credential as the operator typed it; it is
/// encrypted here and never stored as given. An empty string means "no
/// credential", the same as `None`.
///
/// A credential written into `url` itself is lifted out into the same two
/// fields, so it is stored the same way wherever it was typed. An explicit
/// `username` / `password` wins over the URL's — the form is the place the
/// operator meant it, and the URL is the place they pasted it.
#[allow(clippy::too_many_arguments)]
pub async fn create_mirror(
    db: &DatabaseConnection,
    repo_id: i64,
    url: String,
    username: Option<String>,
    password: Option<String>,
    sync_interval_seconds: i64,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
) -> Result<Mirror> {
    // Ensure the repository exists
    let repo = repository::Entity::find_by_id(repo_id)
        .one(db)
        .await
        .context("check repo exists")?;
    if repo.is_none() {
        return Err(crate::error::not_found("repository"));
    }

    check_sync_interval(sync_interval_seconds)?;

    // Reject an obviously-internal / non-git-transport remote at registration
    // for immediate operator feedback; sync re-checks with DNS resolution.
    transport_policy.validate_url_static(&url)?;

    // Take a credential out of the URL before anything stores it. `url` is a
    // plaintext column; `password_encrypted` is not.
    let remote = crate::net::split_url_credentials(&url).context("invalid mirror URL")?;
    let url = remote.url;
    let username = username
        .filter(|value| !value.is_empty())
        .or(remote.username);
    let password = password
        .filter(|value| !value.is_empty())
        .or(remote.password);

    // Check for existing mirror. This read is the fast path only — the row can
    // still appear between here and the insert below, which is why the insert
    // classifies its own failure rather than trusting this answer.
    if rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .is_some()
    {
        return Err(mirror_already_exists());
    }

    let now = Utc::now();
    let next_sync = now + chrono::Duration::seconds(sync_interval_seconds);

    let model = ActiveModel {
        repo_id: Set(repo_id),
        url: Set(url),
        username: Set(username),
        password_encrypted: Set(encrypt_password(password.as_deref(), encryption_key)?),
        sync_interval_seconds: Set(sync_interval_seconds),
        next_sync_at: Set(Some(next_sync)),
        last_sync_at: Set(None),
        last_sync_error: Set(None),
        status: Set(STATUS_ACTIVE.to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    // Losing the `UNIQUE(mirrors.repo_id)` race is the same outcome the read
    // above reports, reached a moment later: someone else registered the mirror
    // first. That is the caller's answer, not a server fault. Only that one
    // loss is folded — a foreign-key failure or a database outage stays an
    // error, because telling a client to fix a request that was never the
    // problem is exactly the misattribution this costs.
    match rg_db::ops::mirror_ops::create(db, model).await {
        Ok(mirror) => Ok(mirror),
        Err(error) if rg_db::is_unique_violation_anyhow(&error) => Err(mirror_already_exists()),
        Err(error) => Err(error),
    }
}

/// The one answer both the pre-read and the losing insert give, so a caller
/// cannot tell which of the two noticed. Carries no constraint or `db:` text —
/// this message reaches the client verbatim.
///
/// A `Conflict`, not an `InvalidRequest`: the request named a real repository
/// and a valid remote, and the only thing wrong with it is that this repository
/// already has a mirror. Nothing the caller can edit fixes that — deleting the
/// existing mirror does — which is what separates 409 from 400 here.
fn mirror_already_exists() -> anyhow::Error {
    crate::error::conflict("mirror already exists for this repository")
}

/// Get mirror for a repository.
pub async fn get_mirror(db: &DatabaseConnection, repo_id: i64) -> Result<Option<Mirror>> {
    rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id).await
}

/// Update mirror settings.
///
/// `password` follows the same rule as on create: plaintext in, ciphertext
/// stored. An explicit empty string clears the stored credential — without it
/// there would be no way to take a credential back off a mirror short of
/// deleting the whole row.
#[allow(clippy::too_many_arguments)]
pub async fn update_mirror(
    db: &DatabaseConnection,
    repo_id: i64,
    url: Option<String>,
    username: Option<String>,
    password: Option<String>,
    sync_interval_seconds: Option<i64>,
    status: Option<String>,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
) -> Result<Mirror> {
    update_mirror_after_read(
        db,
        repo_id,
        url,
        username,
        password,
        sync_interval_seconds,
        status,
        transport_policy,
        encryption_key,
        || std::future::ready(Ok(())),
    )
    .await
}

/// Testable boundary between the repository-scoped read and conditional write.
#[allow(clippy::too_many_arguments)]
async fn update_mirror_after_read<F, Fut>(
    db: &DatabaseConnection,
    repo_id: i64,
    url: Option<String>,
    username: Option<String>,
    password: Option<String>,
    sync_interval_seconds: Option<i64>,
    status: Option<String>,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
    after_read: F,
) -> Result<Mirror>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if let Some(seconds) = sync_interval_seconds {
        check_sync_interval(seconds)?;
    }

    let existing = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .ok_or_else(|| crate::error::not_found("mirror"))?;

    // Validate the effective remote on every PATCH, not only when `url` is in
    // the body. A row written before this policy may still be `http://`; letting
    // an interval-only or credential-only update succeed would preserve that
    // unsafe configuration and, in the credential case, attach a fresh secret
    // to it. The sync boundary also refuses the row, but request-time feedback
    // is what tells the operator how to repair it.
    transport_policy.validate_url_static(url.as_deref().unwrap_or(&existing.url))?;

    let mut updated_url = None;
    let mut updated_username = None;
    let mut updated_password = None;
    if let Some(v) = url {
        // …and the same split, for the same reason. Whatever the URL carried is
        // written to the credential fields first, so an explicit `username` /
        // `password` in this same request still overwrites it below.
        let remote = crate::net::split_url_credentials(&v).context("invalid mirror URL")?;
        updated_url = Some(remote.url);
        if let Some(lifted) = remote.username {
            updated_username = Some(Some(lifted));
        }
        if let Some(lifted) = remote.password {
            updated_password = Some(encrypt_password(Some(&lifted), encryption_key)?);
        }
    }
    if let Some(v) = username {
        // Empty means none, the same way it does for the password: the settings
        // form sends every field it displays on every save, so a username the
        // operator deleted arrives as `""` and has to reach the column as NULL
        // rather than as an empty name nothing can tell apart from one.
        updated_username = Some(Some(v).filter(|v| !v.is_empty()));
    }
    if let Some(v) = password {
        updated_password = Some(encrypt_password(Some(&v), encryption_key)?);
    }
    let updated_status = if let Some(v) = status {
        // The half of `status` a caller owns is the switch, and a switch has
        // two positions. `error` is the sweep's to write and `last_sync_error`
        // is where the reason lives, so accepting it here would let a caller
        // describe a pass that never happened; anything else is a typo that
        // would otherwise be stored verbatim and silently read as "switched
        // on" by every later sweep.
        if v != STATUS_ACTIVE && v != STATUS_INACTIVE {
            return Err(crate::error::invalid_request(format!(
                "`status` is the mirror's on/off switch and accepts \
                 `{STATUS_ACTIVE}` or `{STATUS_INACTIVE}`; the outcome of the \
                 last sync is reported in `status`/`last_sync_error` and is not \
                 settable"
            )));
        }
        Some(v)
    } else {
        None
    };

    after_read().await?;
    rg_db::ops::mirror_ops::update_settings(
        db,
        existing.id,
        updated_url,
        updated_username,
        updated_password,
        sync_interval_seconds,
        updated_status,
        Utc::now(),
    )
    .await?
    .ok_or_else(|| crate::error::not_found("mirror"))
}

/// Delete a mirror, and with it the clone it owns on disk.
///
/// The lookup and the `DELETE` are two statements, so a concurrent delete can
/// empty the row out from under this one; zero rows reports `not_found` rather
/// than confirming a deletion this call did not perform.
///
/// ## The bytes
///
/// The row is metadata *for* `<repo_root>/<repo_id>.mirror` — a full clone of a
/// third-party upstream, the largest thing a repository owns after its own Git
/// tree. Deleting the row alone left it on disk forever: no sweep anywhere walks
/// the filesystem, and the directory is named after a `repo_id` no later
/// namespace ever collides with, so nothing would find it again. Worse, the next
/// mirror configured on this repository inherited it: `run_sync_pass` branches
/// on whether `HEAD` exists, found the orphan, and ran `git remote update` — which
/// takes its remote from the inherited clone's own config, not from the new
/// `mirrors.url`. The new remote was never contacted, while the row reported
/// `status=active` with a fresh `last_sync_at`, and the SSRF guard cleared a URL
/// `git` never dialled (`card_8ee32201d626`).
///
/// So the same cross-store shape the repository's deletion uses: rename the
/// directory aside, mutate the row, remove the tombstone only once the row is
/// gone, and move the directory back if the row will not move. A tombstone that
/// cannot be removed is reported as an error rather than folded into the `204` —
/// bytes still on disk are not a completed deletion.
///
/// ## The pass in flight
///
/// A mirror pass owns that directory by absolute path for as long as its `git`
/// subprocess runs, so this refuses while the sync lease is held, exactly as
/// `ensure_repository_deletion_is_quiescent` does for the repository. The early
/// read is the cheap half — it costs no staging;
/// [`rg_db::ops::mirror_ops::delete_by_id_unless_syncing`] is the load-bearing
/// one, repeating the read inside the transaction that removes the row.
pub async fn delete_mirror(db: &DatabaseConnection, repo_id: i64, repo_root: &Path) -> Result<()> {
    use rg_db::ops::mirror_ops::{MirrorRetirement, SYNC_LEASE_STALE_AFTER};

    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .ok_or_else(|| crate::error::not_found("mirror"))?;

    if rg_db::ops::mirror_ops::sync_lease_in_flight(
        db,
        repo_id,
        Utc::now() - SYNC_LEASE_STALE_AFTER,
    )
    .await
    .context("failed to check for a mirror sync in flight before deleting the mirror")?
    .is_some()
    {
        return Err(mirror_sync_in_flight());
    }

    let deletion_id = uuid::Uuid::new_v4().simple().to_string();
    // The clone directory leaves the live namespace by rename, and a rename is
    // only reversible by a process that lives to reverse it. The journal is what
    // makes it reversible by the *next* process instead: declared before the
    // move, marked when the row is gone, and dropped by whichever ending this
    // call reaches. Without it a `SIGKILL` here leaves a live mirror row whose
    // clone is sitting one name away.
    let journal = crate::deletion_recovery::journal_at(repo_root);
    let planned = crate::repo::service::plan_repository_filesystem_directories(
        vec![crate::repo::service::RepositoryFilesystemDirectory {
            live: mirror_clone_path(repo_root, repo_id),
            kind: "mirror clone directory",
            hint: crate::platform::fs::REPO_ROOT_HINT,
        }],
        repo_id,
        &deletion_id,
    )?;
    crate::deletion_recovery::open(
        &journal,
        &deletion_id,
        "mirror clone directory",
        crate::repo::service::planned_filesystem_journal(&planned)?,
    )
    .await?;

    let staged =
        match crate::repo::service::stage_repository_filesystem_directories(planned, repo_id) {
            Ok(staged) => staged,
            Err(error) => {
                crate::deletion_recovery::close(&journal, &deletion_id).await;
                return Err(error);
            }
        };

    let retirement = rg_db::ops::mirror_ops::delete_by_id_unless_syncing(
        db,
        mirror.id,
        repo_id,
        Utc::now() - SYNC_LEASE_STALE_AFTER,
    )
    .await;
    let abandon = async |staged: &[crate::repo::service::StagedRepositoryFilesystemDirectory]| {
        crate::repo::service::restore_repository_filesystem_directories(staged, repo_id);
        crate::deletion_recovery::close(&journal, &deletion_id).await;
    };
    let retirement = match retirement {
        Ok(retirement) => retirement,
        Err(error) => {
            abandon(&staged).await;
            return Err(error);
        }
    };
    match retirement {
        MirrorRetirement::Deleted => {}
        MirrorRetirement::NotFound => {
            abandon(&staged).await;
            return Err(crate::error::not_found("mirror"));
        }
        MirrorRetirement::MirrorSyncInFlight => {
            abandon(&staged).await;
            return Err(mirror_sync_in_flight());
        }
    }

    // The row is gone, so the tombstone may be destroyed rather than put back.
    // Reported but not fatal for the same reason as everywhere else this marker
    // is written: see `StagedPackageVersion::retire`.
    let mut cleanup_error =
        match crate::deletion_recovery::mark_committed(&journal, &deletion_id).await {
            Ok(()) => None,
            Err(error) => {
                tracing::warn!(
                    repo_id,
                    error = %format!("{error:#}"),
                    "mirror row is deleted, but the deletion could not be marked committed"
                );
                Some(error)
            }
        };
    if let Err(error) =
        crate::repo::service::retire_repository_filesystem_directories(staged, repo_id)
    {
        cleanup_error.get_or_insert(error);
    }
    // Closed only when the clone directory is actually gone — see
    // `StagedPackageVersion::retire`.
    if cleanup_error.is_none() {
        crate::deletion_recovery::close(&journal, &deletion_id).await;
    }
    cleanup_error.map_or(Ok(()), Err)
}

/// The refusal both sync-in-flight checks give, so a caller cannot tell which of
/// the two noticed — the early one that costs no staging, or the one inside the
/// transaction that removes the row.
fn mirror_sync_in_flight() -> anyhow::Error {
    crate::error::conflict(
        "this mirror is syncing right now; wait for that pass to finish before deleting it",
    )
}

/// The full clone a mirror keeps on disk, under the repository root.
///
/// Keyed by `repo_id` rather than by `<namespace>/<name>`, so it survives a
/// rename and a transfer without moving. Exported because it is repository-owned
/// storage exactly like the LFS and legacy release-asset roots: whoever retires
/// a repository has to retire this directory with it
/// ([`crate::repo::service::delete_repo`]), and a copy of the `format!` string
/// in the deletion path is a copy that can drift.
pub fn mirror_clone_path(repo_root: &Path, repo_id: i64) -> std::path::PathBuf {
    repo_root.join(format!("{repo_id}.mirror"))
}

/// Why a call to [`sync_mirror`] did or did not spend a `git` subprocess.
///
/// A `bool` used to carry this, and it stopped being enough once a refusal could
/// mean three different things — "switched off", "its repository is gone" and
/// "another pass holds it" are three different answers for the operator and only
/// one of them is repaired by touching the mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncOutcome {
    /// A pass ran. Whether it succeeded is recorded on the row.
    Ran,
    /// The operator has this mirror switched off.
    SwitchedOff,
    /// The repository that owns the mirror has been deleted.
    RepositoryGone,
    /// The mirror itself has been deleted since this pass was selected.
    MirrorGone,
    /// Another pass holds this mirror's sync lease right now.
    AlreadySyncing,
}

/// Sync a single mirror: clone (first time) or fetch (subsequent).
///
/// The guard tests for *switched off* and nothing else. It used to require
/// `status == "active"`, which made it refuse exactly the mirrors this function
/// had itself marked [`STATUS_ERROR`] on the previous pass — a broken mirror
/// could then never be repaired by a later sync, because no later sync would
/// run (card_770723efaa96). A failed pass is a reason to retry, not a reason to
/// stop.
///
/// ## The lease
///
/// The row outlives its repository: `delete_repo` soft-deletes, so nothing
/// cascades to `mirrors`, and a pass that ran anyway would `git clone` the whole
/// upstream straight back into the directory the deletion just retired — bytes
/// nothing owns, with no sweep that walks the filesystem to find them again
/// (card_374998ffebc1). `list_due_sync` keeps such rows out of the sweep's
/// selection, and this function used to re-read the repository immediately
/// before `git` for the gap after that selection.
///
/// Re-reading is not enough, because the pass does not end at the check. `git
/// clone --mirror` runs for minutes, writing to the absolute path it was handed,
/// and a deletion that starts in that window retires the directory *underneath*
/// a live subprocess which then re-creates it (card_a1f2a20281af). No column of
/// `mirrors` distinguishes "a pass is running" from "a pass finished an hour
/// ago" — `last_sync_at` is written after the fact — so the pass declares itself
/// instead: [`rg_db::ops::mirror_ops::bid_for_sync_lease`] takes a lease that
/// covers the whole subprocess and verifies the repository's lifecycle under the
/// same transaction, and `delete_repo` reads it and backs off retryably.
pub async fn sync_mirror(
    db: &DatabaseConnection,
    mirror: &Mirror,
    repo_root: &Path,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
) -> Result<SyncOutcome> {
    use rg_db::ops::mirror_ops::SyncLeaseBid;

    if mirror.status == STATUS_INACTIVE {
        return Ok(SyncOutcome::SwitchedOff);
    }

    let token = uuid::Uuid::new_v4().simple().to_string();
    let bid = rg_db::ops::mirror_ops::bid_for_sync_lease(
        db,
        mirror.repo_id,
        &token,
        Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
    )
    .await
    .context("bid for this mirror's sync lease")?;
    match bid {
        SyncLeaseBid::Granted | SyncLeaseBid::TakenOver => {}
        SyncLeaseBid::Busy => {
            tracing::debug!(
                repo_id = mirror.repo_id,
                mirror_id = mirror.id,
                "skipping mirror sync: another pass is already syncing this mirror"
            );
            return Ok(SyncOutcome::AlreadySyncing);
        }
        SyncLeaseBid::RepositoryGone => {
            tracing::debug!(
                repo_id = mirror.repo_id,
                mirror_id = mirror.id,
                "skipping mirror sync: the repository that owns it is deleted"
            );
            return Ok(SyncOutcome::RepositoryGone);
        }
        SyncLeaseBid::MirrorGone => {
            tracing::debug!(
                repo_id = mirror.repo_id,
                mirror_id = mirror.id,
                "skipping mirror sync: the mirror was deleted and its clone directory retired \
                 with it"
            );
            return Ok(SyncOutcome::MirrorGone);
        }
    }

    let pass = run_sync_pass(db, mirror, repo_root, transport_policy, encryption_key).await;

    // Released however the pass went, including on the error paths above it:
    // holding it any longer would keep the repository undeletable for the whole
    // staleness horizon. A release that fails is logged rather than propagated —
    // the pass itself is what the caller asked about, and the lease expires by
    // itself.
    match rg_db::ops::mirror_ops::release_sync_lease(db, mirror.repo_id, &token).await {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            repo_id = mirror.repo_id,
            mirror_id = mirror.id,
            "this mirror's sync lease was taken over while its pass was still running"
        ),
        Err(error) => tracing::warn!(
            repo_id = mirror.repo_id,
            mirror_id = mirror.id,
            error = %format!("{error:#}"),
            "the mirror sync lease could not be released and will block this repository's \
             deletion until it goes stale"
        ),
    }

    pass.map(|()| SyncOutcome::Ran)
}

/// One mirror pass, from the credential to the row that records how it went.
///
/// Split out of [`sync_mirror`] so there is exactly one acquire and one release
/// of the sync lease however this returns — the same shape
/// `move_repository_storage_and_commit` gives the transfer lease.
async fn run_sync_pass(
    db: &DatabaseConnection,
    mirror: &Mirror,
    repo_root: &Path,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
) -> Result<()> {
    let repo_path = mirror_clone_path(repo_root, mirror.repo_id);

    // Decrypt before the guard so a credential that can no longer be read is
    // reported as such, rather than as a plain authentication failure from the
    // remote. Like every other failure here it lands in `last_sync_error`.
    let credentials = load_credentials(mirror, encryption_key);

    // SSRF guard (with DNS resolution) immediately before the git subprocess.
    // Re-checked here — not only at create/update — so a URL that resolved
    // public earlier, an old mirror predating this guard, or a DNS-rebind to an
    // internal address is caught right before the network call. A failure is
    // recorded as a normal sync error below (status=error), not propagated.
    let (credentials, result) = match credentials {
        Ok(credentials) => match transport_policy.destination(&mirror.url).await {
            Ok(remote) => {
                crate::blocking::run_blocking_git("mirror repository sync", move || {
                    let result = if repo_path.join("HEAD").exists() {
                        // Existing mirror: git remote update
                        run_git_remote_update(
                            transport_policy,
                            &repo_path,
                            &remote,
                            credentials.as_ref(),
                        )
                    } else {
                        // First time: git clone --mirror
                        run_git_clone_mirror(
                            transport_policy,
                            &remote,
                            &repo_path,
                            credentials.as_ref(),
                        )
                    };
                    Ok((credentials, result))
                })
                .await?
            }
            Err(e) => (
                credentials,
                Err(e.context("mirror remote URL failed transport/SSRF validation")),
            ),
        },
        // `anyhow::Error` is not `Clone`, and the borrow above needs the
        // credentials to stay put, so re-word the failure instead of moving it.
        Err(_) => (
            None,
            Err(anyhow::anyhow!(
                "the stored credential for this mirror could not be decrypted \
                 (it predates encryption at rest, or the server's secret changed) — \
                 re-enter it in the mirror settings"
            )),
        ),
    };

    let now = Utc::now();
    let next_sync = now + chrono::Duration::seconds(effective_sync_interval(mirror));

    let mut model: ActiveModel = mirror.clone().into();
    model.last_sync_at = Set(Some(now));
    model.next_sync_at = Set(Some(next_sync));
    model.updated_at = Set(now);

    match result {
        Ok(()) => {
            model.last_sync_error = Set(None);
            model.status = Set(STATUS_ACTIVE.to_string());
        }
        Err(e) => {
            // `{e}` printed the outermost `.context(...)` only, both in the
            // persisted field the UI shows and in the log — under it sits the
            // `git clone --mirror` failure that actually explains the outage
            // (card_a997f30c142c).
            //
            // Belt and braces on the way out: the credential is kept out of
            // argv and out of the URL, so git has nothing to echo — but this
            // string is persisted and rendered in the settings UI, which is
            // the last place a secret should surface if that ever stops
            // holding.
            let reason = mask_credential(&format!("{e:#}"), Some(&credentials));
            model.last_sync_error = Set(Some(reason.clone()));
            model.status = Set(STATUS_ERROR.to_string());
            tracing::error!(repo_id = mirror.repo_id, error = %reason, "mirror sync failed");
        }
    }

    rg_db::ops::mirror_ops::update(db, model).await?;
    Ok(())
}

/// Sync all due mirrors (called by background task / cron).
pub async fn sync_due_mirrors(
    db: &DatabaseConnection,
    repo_root: &Path,
    limit: u64,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
) -> Result<usize> {
    let mirrors = rg_db::ops::mirror_ops::list_due_sync(db, limit).await?;
    let mut count = 0;
    for mirror in &mirrors {
        match sync_mirror(db, mirror, repo_root, transport_policy, encryption_key).await {
            Ok(SyncOutcome::Ran) => count += 1,
            // Switched off, repository gone, or already being synced by someone
            // else — none of the three is this sweep's to report.
            Ok(_) => {}
            Err(e) => {
                tracing::error!(mirror_id = %mirror.id, error = %format!("{e:#}"), "mirror sync failed")
            }
        }
    }
    Ok(count)
}

/// Manually trigger a sync for a mirror.
///
/// A mirror the operator has switched off is a refusal, not a quiet no-op: this
/// is the "Sync now" button, and answering it with the same success the real
/// thing gets is how a mirror that never moves looks like one that just synced
/// (card_770723efaa96). `409`, because nothing about the request is malformed —
/// the mirror's own state is what makes it unanswerable, and flipping `status`
/// back is what fixes it.
pub async fn trigger_sync(
    db: &DatabaseConnection,
    repo_id: i64,
    repo_root: &Path,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
) -> Result<()> {
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .ok_or_else(|| crate::error::not_found("mirror"))?;
    // Three reasons now decline a pass, and the operator has to be told which
    // one: a deleted repository is not something flipping `status` repairs, and
    // a pass already running is not something to repair at all.
    // (Reaching this with a deleted repository takes the row outliving its
    // repository *and* a caller that resolved it some other way — every HTTP
    // route here resolves the repository first.)
    match sync_mirror(db, &mirror, repo_root, transport_policy, encryption_key).await? {
        SyncOutcome::Ran => Ok(()),
        SyncOutcome::SwitchedOff => Err(crate::error::conflict(
            "this mirror is switched off — set its status to `active` before syncing it",
        )),
        SyncOutcome::RepositoryGone => Err(crate::error::conflict(
            "the repository this mirror belongs to has been deleted; the mirror can no longer be \
             synced",
        )),
        // Between the lookup above and the bid, `DELETE /mirror` removed the row
        // and retired its clone. There is nothing left to sync, and the honest
        // answer is the one the next request would get anyway.
        SyncOutcome::MirrorGone => Err(crate::error::not_found("mirror")),
        SyncOutcome::AlreadySyncing => Err(crate::error::conflict(
            "this mirror is already syncing; wait for the pass in flight to finish before \
             starting another",
        )),
    }
}

// ── Credentials ─────────────────────────────────────────────────────────

/// Encrypt an operator-supplied password for storage.
///
/// `None` and `Some("")` both mean "no credential" — the empty string is how
/// the API clears one.
fn encrypt_password(password: Option<&str>, encryption_key: &str) -> Result<Option<String>> {
    let Some(password) = password.filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let key = crate::auth::encryption::derive_key(encryption_key);
    crate::auth::encryption::encrypt(password, &key)
        .context("failed to encrypt the mirror credential")
        .map(Some)
}

/// Read the stored credential back for a sync.
///
/// A mirror with a username but no password is *not* a credential: git would
/// be handed half an answer, be refused, and (with prompting disabled) fail
/// with a confusing error instead of the honest anonymous-access one.
fn load_credentials(mirror: &Mirror, encryption_key: &str) -> Result<Option<GitCredentials>> {
    let Some(ciphertext) = mirror.password_encrypted.as_deref() else {
        return Ok(None);
    };
    let key = crate::auth::encryption::derive_key(encryption_key);
    let password = crate::auth::encryption::decrypt(ciphertext, &key)
        .context("mirror credential could not be decrypted")?;
    Ok(Some(GitCredentials::new(mirror.username.clone(), password)))
}

/// Replace the credential in a message that is about to be persisted or logged.
///
/// Two sources, because a mirror has two places a credential can come from: the
/// one this sync loaded out of `password_encrypted`, and one still written into
/// a URL that the message quotes — git echoes the remote it failed to reach,
/// and a row predating the create-time split still carries it.
fn mask_credential(message: &str, credentials: Option<&Option<GitCredentials>>) -> String {
    let message = crate::net::mask_url_credentials(message);
    match credentials.and_then(Option::as_ref) {
        Some(credentials) => {
            crate::auth::encryption::mask_values(&message, &[credentials.password().to_string()])
        }
        None => message,
    }
}

/// Move a credential typed into `mirrors.url` into the columns that protect it.
///
/// The create/update path splits every URL it is handed, but a row written
/// before it did still holds `https://user:token@host/repo.git` in a plaintext
/// column. Run by `plombir-git serve` right after the key preflight — the first
/// point in the boot where the schema and the at-rest key both exist, and
/// before any sync can quote such a URL into `last_sync_error`. Returns how
/// many rows it rewrote.
///
/// Idempotent: a URL with no userinfo is left alone, so a restart costs one
/// query and rewrites nothing.
///
/// Two cases are converted rather than kept:
///
/// * `user:token@` — the login goes to `username`, the secret to
///   `password_encrypted`, unless the row already has one of its own (an
///   operator's explicit entry outranks a pasted URL, and dropping the URL copy
///   is the point).
/// * a lone `user@` on `http(s)` — dropped. Git was never given a password to
///   pair it with and prompting is off, so it authenticated nothing; promoting
///   it into `username` would only move a possible token from one plaintext
///   column to another.
pub async fn lift_legacy_url_credentials(
    db: &DatabaseConnection,
    encryption_key: &str,
) -> Result<usize> {
    use rg_db::entities::mirror;
    use sea_orm::{ActiveModelTrait, ColumnTrait, PaginatorTrait, QueryFilter, QueryOrder};

    let mut pages = mirror::Entity::find()
        .filter(mirror::Column::Url.contains("@"))
        .order_by_asc(mirror::Column::Id)
        .paginate(db, 500);

    let mut lifted_rows = 0_usize;
    while let Some(mirrors) = pages
        .fetch_and_next()
        .await
        .context("read mirror remote URLs")?
    {
        for mirror in mirrors {
            let lifted = crate::net::strip_url_credentials(&mirror.url);
            if !lifted.is_present() {
                // An `@` elsewhere in the URL — a scoped path, an scp-like
                // `git@host:path` remote. Nothing to take out.
                continue;
            }

            let id = mirror.id;
            let had_password = mirror.password_encrypted.is_some();
            let had_username = mirror.username.is_some();
            let mut model: mirror::ActiveModel = mirror.into();
            model.url = Set(lifted.url);

            match lifted.password {
                Some(password) if !had_password => {
                    model.password_encrypted =
                        Set(encrypt_password(Some(&password), encryption_key)?);
                    if let (false, Some(username)) = (had_username, lifted.username) {
                        model.username = Set(Some(username));
                    }
                }
                Some(_) => tracing::warn!(
                    mirror_id = id,
                    "mirror {id} had a credential in its URL and one of its own; the URL copy \
                     was dropped and the stored credential kept"
                ),
                None => tracing::warn!(
                    mirror_id = id,
                    "mirror {id} had a credential in its URL with no password half; it could \
                     not have authenticated anything and was dropped — re-enter it in the \
                     mirror settings if the remote needs one"
                ),
            }

            model
                .update(db)
                .await
                .with_context(|| format!("rewrite the remote URL of mirror {id}"))?;
            lifted_rows += 1;
        }
    }

    if lifted_rows > 0 {
        tracing::info!(
            count = lifted_rows,
            "moved credentials out of mirror remote URLs"
        );
    }
    Ok(lifted_rows)
}

// ── Git helpers ─────────────────────────────────────────────────────────

fn run_git_clone_mirror(
    transport_policy: MirrorTransportPolicy,
    remote: &crate::net::GuardedGitRemote,
    path: &Path,
    credentials: Option<&GitCredentials>,
) -> Result<()> {
    transport_policy.require_confidential_transport(remote.url())?;
    // `create mirror dir` named the operation but never the directory, and the
    // directory — `repo_root` — is the only thing an operator can act on when
    // the mirror row shows nothing but `Permission denied (os error 13)`.
    let parent = path
        .parent()
        .context("mirror path has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|error| {
        crate::platform::fs::path_error(
            "mirror directory",
            parent,
            &error,
            crate::platform::fs::REPO_ROOT_HINT,
        )
    })?;

    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let invocation = remote.bind_invocation(credential_invocation(credentials))?;
    let destination = path.to_string_lossy();
    invocation
        .run(
            git,
            &["clone", "--mirror", remote.url(), &destination],
            None,
        )?
        .ensure_success()
        .context("git clone --mirror")
}

/// Refresh an existing mirror clone from `url`.
///
/// The remote is pinned to the row's URL before the fetch, and that is not
/// housekeeping. `git remote update` takes its address from the clone's own
/// `remote.origin.url`, written once by the `git clone --mirror` that created
/// the directory — so every later change to `mirrors.url` was silently ignored
/// for as long as the clone survived. Two ways in: an operator repointing a
/// mirror through `PATCH /mirror`, and a mirror re-created on a *different* URL
/// on top of a clone the previous mirror left behind. Both reported
/// `status=active` with a fresh `last_sync_at` while fetching from the old
/// upstream, and both made `guard_git_url`'s verdict describe a call that never
/// happened: the guard cleared the new URL, `git` dialled the old one
/// (`card_8ee32201d626`).
///
/// Pinning here rather than at the point the URL changes is deliberate — this is
/// the one place the row and the directory are both in hand, so the clone follows
/// the row no matter which path moved it, including rows that drifted before
/// this existed.
fn run_git_remote_update(
    transport_policy: MirrorTransportPolicy,
    path: &Path,
    remote: &crate::net::GuardedGitRemote,
    credentials: Option<&GitCredentials>,
) -> Result<()> {
    transport_policy.require_confidential_transport(remote.url())?;
    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let invocation = remote.bind_invocation(credential_invocation(credentials))?;
    invocation
        .run(
            git,
            &["remote", "set-url", "origin", remote.url()],
            Some(path),
        )?
        .ensure_success()
        .context("git remote set-url origin")?;
    invocation
        .run(git, &["remote", "update", "--prune"], Some(path))?
        .ensure_success()
        .context("git remote update")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        spawn_authenticating_remote, spawn_rebinding_git_remotes, spawn_redirecting_git_remotes,
    };
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use sea_orm::{ConnectOptions, Database, NotSet};

    const SECRET: &str = "test-secret-key";

    fn credentials(username: Option<&str>, password: &str) -> GitCredentials {
        GitCredentials::new(username.map(str::to_string), password.to_string())
    }

    fn mirror_row(username: Option<&str>, password_encrypted: Option<String>) -> Mirror {
        Mirror {
            id: 1,
            repo_id: 7,
            url: "https://example.com/upstream.git".to_string(),
            username: username.map(str::to_string),
            password_encrypted,
            sync_interval_seconds: 3600,
            next_sync_at: None,
            last_sync_at: None,
            last_sync_error: None,
            status: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    async fn update_fixture() -> (DatabaseConnection, i64, i64) {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options).await.expect("connect test db");
        rg_db::run_migrations(&db).await.expect("migrate test db");

        let owner = rg_db::ops::user_ops::create_user(
            &db,
            "mirror-race-owner",
            "mirror-race-owner@example.invalid",
            "",
            "Owner",
        )
        .await
        .expect("create owner");
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("mirror-race-repo".to_string()),
                description: Set(None),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .expect("create repository");
        let mirror = rg_db::ops::mirror_ops::create(
            &db,
            ActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                url: Set("https://example.com/upstream.git".to_string()),
                username: Set(None),
                password_encrypted: Set(None),
                sync_interval_seconds: Set(3600),
                next_sync_at: Set(None),
                last_sync_at: Set(None),
                last_sync_error: Set(None),
                status: Set(STATUS_ACTIVE.to_string()),
                created_at: Set(now),
                updated_at: Set(now),
            },
        )
        .await
        .expect("create mirror");

        (db, repo.id, mirror.id)
    }

    #[tokio::test]
    async fn delete_after_the_service_read_is_typed_not_found() {
        let (db, repo_id, mirror_id) = update_fixture().await;

        let error = update_mirror_after_read(
            &db,
            repo_id,
            None,
            Some("sync-bot".to_string()),
            None,
            Some(7200),
            Some(STATUS_INACTIVE.to_string()),
            MirrorTransportPolicy::default(),
            SECRET,
            || async {
                assert_eq!(
                    rg_db::ops::mirror_ops::delete_by_id_unless_syncing(
                        &db,
                        mirror_id,
                        repo_id,
                        Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
                    )
                    .await
                    .expect("the competing mirror delete succeeds"),
                    rg_db::ops::mirror_ops::MirrorRetirement::Deleted
                );
                Ok(())
            },
        )
        .await
        .expect_err("a winning delete must not become a successful mirror update");

        let typed = error
            .downcast_ref::<crate::error::NotFound>()
            .expect("the lost race must stay classifiable as HTTP 404");
        assert_eq!(typed.resource, "mirror");
    }

    /// The column is named `password_encrypted`; this is the test that keeps the
    /// name honest at the one place that fills it.
    #[test]
    fn a_stored_password_is_ciphertext_and_reads_back() {
        let stored = encrypt_password(Some("hunter2"), SECRET)
            .expect("encrypt")
            .expect("a password produces a value");
        assert_ne!(stored, "hunter2", "the password was stored verbatim");
        assert!(!stored.contains("hunter2"));

        let loaded = load_credentials(&mirror_row(Some("sync-bot"), Some(stored)), SECRET)
            .expect("decrypt")
            .expect("a stored credential is readable");
        assert_eq!(loaded.password(), "hunter2");
        assert_eq!(loaded.username(), Some("sync-bot"));
    }

    /// Two encryptions of one password differ (fresh nonce), so the column can't
    /// be used as an oracle for "do these two mirrors share a password?".
    #[test]
    fn the_same_password_encrypts_differently_every_time() {
        let first = encrypt_password(Some("hunter2"), SECRET).unwrap().unwrap();
        let second = encrypt_password(Some("hunter2"), SECRET).unwrap().unwrap();
        assert_ne!(first, second);
    }

    /// card_3d4c7b8b27c8: `next_sync_at` is written as `now + interval` and
    /// `list_due_sync` selects on `next_sync_at <= now`, so a zero or negative
    /// interval makes the row permanently due — every tick of the sweep spawns
    /// a `git` subprocess against a third-party host, and the schedule never
    /// converges to a schedule.
    #[test]
    fn an_interval_the_scheduler_cannot_honour_is_refused() {
        for refused in [0, -1, -86_400, MIN_SYNC_INTERVAL_SECONDS - 1] {
            let error = check_sync_interval(refused).expect_err("interval {refused} was accepted");
            assert!(
                error
                    .downcast_ref::<crate::error::InvalidRequest>()
                    .is_some(),
                "a bad interval must be the caller's mistake, not a server fault: {error:#}"
            );
            let rendered = format!("{error:#}");
            assert!(
                rendered.contains("sync_interval_seconds"),
                "the message has to name the field: {rendered}"
            );
        }

        check_sync_interval(MIN_SYNC_INTERVAL_SECONDS).expect("the minimum itself is allowed");
        check_sync_interval(3600).expect("an hour is allowed");
    }

    /// The other end of the same guard: rows written before it existed still
    /// have to leave the sweep's selection after a pass, so the schedule is
    /// computed from the floor rather than from the stored number.
    #[test]
    fn a_pass_schedules_a_row_stored_below_the_floor_past_now_anyway() {
        let mut legacy = mirror_row(None, None);
        legacy.sync_interval_seconds = 0;
        assert_eq!(effective_sync_interval(&legacy), MIN_SYNC_INTERVAL_SECONDS);

        legacy.sync_interval_seconds = -3600;
        assert_eq!(effective_sync_interval(&legacy), MIN_SYNC_INTERVAL_SECONDS);

        let now = Utc::now();
        assert!(
            now + chrono::Duration::seconds(effective_sync_interval(&legacy)) > now,
            "the pass left the mirror due at the moment it finished"
        );

        let configured = mirror_row(None, None);
        assert_eq!(
            effective_sync_interval(&configured),
            configured.sync_interval_seconds,
            "a legitimate interval must not be rewritten by the floor"
        );
    }

    #[test]
    fn an_empty_password_is_no_credential_at_all() {
        assert_eq!(encrypt_password(Some(""), SECRET).unwrap(), None);
        assert_eq!(encrypt_password(None, SECRET).unwrap(), None);
        assert!(
            load_credentials(&mirror_row(Some("sync-bot"), None), SECRET)
                .unwrap()
                .is_none()
        );
    }

    /// A credential written under a different server secret must fail loudly at
    /// sync time rather than be silently treated as a usable password.
    #[test]
    fn a_credential_from_another_secret_is_refused() {
        let stored = encrypt_password(Some("hunter2"), "some-other-secret")
            .unwrap()
            .unwrap();
        // `GitCredentials` deliberately has no `Debug`, so that no stray
        // `{:?}` can ever print a password — which is also why this unwraps by
        // hand instead of reaching for `expect_err`.
        let error = match load_credentials(&mirror_row(None, Some(stored)), SECRET) {
            Err(error) => error,
            Ok(_) => panic!("a credential that cannot be decrypted must not be usable"),
        };
        assert!(format!("{error:#}").contains("could not be decrypted"));
    }

    #[test]
    fn a_sync_error_never_carries_the_password_onward() {
        let credentials = Some(Some(credentials(None, "hunter2")));
        let masked = mask_credential(
            "fatal: could not read Password for 'https://x': hunter2",
            credentials.as_ref(),
        );
        assert!(!masked.contains("hunter2"), "{masked}");
    }

    /// The other half of the same net: the secret is not always the one this
    /// sync loaded. git echoes the remote it failed to reach, and a row written
    /// before the create-time split still carries the credential in it — with
    /// nothing in `password_encrypted` for `mask_values` to match on.
    #[test]
    fn a_sync_error_never_carries_a_credential_out_of_the_url_either() {
        let masked = mask_credential(
            "fatal: unable to access 'https://bot:ghp_SECRET@example.com/o/r.git/': 403",
            None,
        );
        assert!(!masked.contains("ghp_SECRET"), "{masked}");
        assert!(
            masked.contains("example.com/o/r.git"),
            "the remote must still be identifiable: {masked}"
        );
    }

    /// Run one `git` command in `path` through the same disarmed invocation the
    /// mirror passes use, so the fixture cannot pick up the host's git config.
    fn git_in(path: &Path, args: &[&str]) {
        let git = global_gateway().as_ref().expect("the git gateway");
        credential_invocation(None)
            .run(git, args, Some(path))
            .expect("run git")
            .ensure_success()
            .unwrap_or_else(|error| panic!("git {args:?} failed: {error:#}"));
    }

    #[test]
    fn mirror_clone_connects_only_to_the_checked_dns_answer() {
        use std::sync::atomic::Ordering;

        let sinks = spawn_rebinding_git_remotes();
        let remote =
            crate::net::guard_git_url_with_addresses(&sinks.url, vec![sinks.checked_ip], |ip| {
                ip == "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
            })
            .expect("the public-answer stand-in is allowed");
        let directory = tempfile::tempdir().expect("tempdir");

        let outcome = run_git_clone_mirror(
            MirrorTransportPolicy::new(true),
            &remote,
            &directory.path().join("7.mirror"),
            None,
        );
        assert!(
            outcome.is_err(),
            "the checked sink deliberately returns 403"
        );
        assert!(
            sinks.checked_requests.load(Ordering::SeqCst) > 0,
            "git ignored the checked DNS answer"
        );
        assert_eq!(
            sinks.rebound_requests.load(Ordering::SeqCst),
            0,
            "git resolved localhost again and reached the rebound sink"
        );
    }

    #[test]
    fn mirror_clone_cannot_redirect_outside_the_checked_host_and_port() {
        use std::sync::atomic::Ordering;

        let sinks = spawn_redirecting_git_remotes();
        let remote =
            crate::net::guard_git_url_with_addresses(&sinks.url, vec![sinks.checked_ip], |ip| {
                ip == "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
            })
            .expect("the public-answer stand-in is allowed");
        let directory = tempfile::tempdir().expect("tempdir");

        let outcome = run_git_clone_mirror(
            MirrorTransportPolicy::new(true),
            &remote,
            &directory.path().join("7.mirror"),
            None,
        );
        assert!(
            outcome.is_err(),
            "the checked sink returns a refused redirect"
        );
        assert!(
            sinks.checked_requests.load(Ordering::SeqCst) > 0,
            "git never reached the checked endpoint"
        );
        assert_eq!(
            sinks.rebound_requests.load(Ordering::SeqCst),
            0,
            "git followed a redirect outside the checked host/port binding"
        );
    }

    #[test]
    fn mirror_update_connects_only_to_the_checked_dns_answer() {
        use std::sync::atomic::Ordering;

        let sinks = spawn_rebinding_git_remotes();
        let remote =
            crate::net::guard_git_url_with_addresses(&sinks.url, vec![sinks.checked_ip], |ip| {
                ip == "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
            })
            .expect("the public-answer stand-in is allowed");
        let directory = tempfile::tempdir().expect("tempdir");
        git_in(directory.path(), &["init", "--bare", "--quiet"]);
        git_in(
            directory.path(),
            &["remote", "add", "origin", "https://stale.invalid/repo.git"],
        );

        let outcome = run_git_remote_update(
            MirrorTransportPolicy::new(true),
            directory.path(),
            &remote,
            None,
        );
        assert!(
            outcome.is_err(),
            "the checked sink deliberately returns 403"
        );
        assert!(
            sinks.checked_requests.load(Ordering::SeqCst) > 0,
            "git ignored the checked DNS answer"
        );
        assert_eq!(
            sinks.rebound_requests.load(Ordering::SeqCst),
            0,
            "git remote update resolved localhost again and reached the rebound sink"
        );
    }

    /// card_8ee32201d626: `git remote update` reads the address out of the
    /// clone's own config, written once by the `git clone --mirror` that created
    /// the directory. Every later change to `mirrors.url` was therefore ignored
    /// for as long as the clone survived — an operator repointing the mirror, or
    /// a mirror re-created on a different URL on top of an inherited clone, kept
    /// fetching the *old* upstream while the row reported `status=active`.
    ///
    /// The pass is the one place the row and the directory are both in hand, so
    /// the pass is what pins them together. Asserted on the config git will
    /// actually dial, not on the return value: the fetch below fails (the remote
    /// does not exist), and pinning has to have happened anyway.
    #[test]
    fn a_refresh_points_the_clone_at_the_row_s_url_before_it_fetches() {
        let directory = tempfile::tempdir().expect("tempdir");
        let clone = directory.path().join("7.mirror");
        std::fs::create_dir_all(&clone).expect("create the clone directory");

        // The state an inherited clone is in: a bare mirror whose remote is the
        // *previous* mirror's upstream.
        git_in(&clone, &["init", "--bare", "--quiet"]);
        git_in(
            &clone,
            &["remote", "add", "origin", "file:///stale/upstream.git"],
        );

        let fresh = "file:///fresh/upstream.git";
        let remote = crate::net::GuardedGitRemote::unbound_for_test(fresh);
        assert!(
            run_git_remote_update(MirrorTransportPolicy::default(), &clone, &remote, None).is_err(),
            "the fixture's remote does not exist — a successful fetch would mean the test is \
             measuring something else"
        );

        let git = global_gateway().as_ref().expect("the git gateway");
        let configured = credential_invocation(None)
            .run(git, &["config", "--get", "remote.origin.url"], Some(&clone))
            .expect("read the clone's remote")
            .stdout;
        assert_eq!(
            String::from_utf8_lossy(&configured).trim(),
            fresh,
            "the pass fetched from the address the directory happened to carry, not the one the \
             mirror row names"
        );
    }

    /// The acceptance check of card_c29cb3416941: a mirror of a *private* remote
    /// gets as far as authenticating. Before the fix the credential was stored
    /// and then dropped on the floor — `git` was handed a bare URL and every
    /// private remote answered 401 forever.
    ///
    /// This drives `run_git_clone_mirror` directly rather than `sync_mirror`,
    /// because the SSRF guard in front of it (rightly) refuses a loopback
    /// remote; the guard has its own tests in `crate::net`.
    #[test]
    fn a_private_remote_receives_the_stored_credential() {
        let (address, _requests, seen) = spawn_authenticating_remote();
        let directory = tempfile::tempdir().expect("tempdir");
        let credentials = credentials(Some("sync-bot"), "hunter2");
        let remote = crate::net::GuardedGitRemote::unbound_for_test(&format!(
            "http://{address}/upstream.git"
        ));

        let outcome = run_git_clone_mirror(
            MirrorTransportPolicy::new(true),
            &remote,
            &directory.path().join("7.mirror"),
            Some(&credentials),
        );
        assert!(
            outcome.is_err(),
            "the stub remote refuses everyone — the clone cannot succeed"
        );

        let seen = seen.lock().expect("lock");
        let expected = format!("Basic {}", STANDARD.encode("sync-bot:hunter2"));
        assert!(
            seen.contains(&expected),
            "the remote never received the stored credential; it saw {seen:?}"
        );
    }

    /// The last sink repeats the HTTP confidentiality rule, so even a future
    /// caller that forgets request-time validation cannot emit a probe or a
    /// credential before the secure-default policy refuses it.
    #[test]
    fn plaintext_http_is_refused_before_the_live_sink_receives_anything() {
        use std::sync::atomic::Ordering;

        let (address, requests, seen) = spawn_authenticating_remote();
        let directory = tempfile::tempdir().expect("tempdir");
        let credentials = credentials(Some("sync-bot"), "hunter2");
        let remote = crate::net::GuardedGitRemote::unbound_for_test(&format!(
            "http://{address}/upstream.git"
        ));

        let error = run_git_clone_mirror(
            MirrorTransportPolicy::default(),
            &remote,
            &directory.path().join("7.mirror"),
            Some(&credentials),
        )
        .expect_err("secure default must stop plaintext HTTP before git runs");

        assert!(format!("{error:#}").contains("allow_insecure_http"));
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "the rejected remote still received a network request"
        );
        assert!(
            seen.lock().expect("lock").is_empty(),
            "the rejected remote received the stored credential"
        );
    }

    /// Native Git has no encrypted variant and no mirror opt-in. The final sink
    /// refuses it before even preparing the clone directory, which is the
    /// deterministic local witness that no subprocess or network call started.
    #[test]
    fn native_git_protocol_is_refused_before_the_live_sink_starts() {
        let directory = tempfile::tempdir().expect("tempdir");
        let parent = directory.path().join("blocked");
        let destination = parent.join("7.mirror");
        let remote =
            crate::net::GuardedGitRemote::unbound_for_test("git://127.0.0.1:9/upstream.git");

        let error = run_git_clone_mirror(
            MirrorTransportPolicy::new(true),
            &remote,
            &destination,
            None,
        )
        .expect_err("the HTTP opt-in must not admit native Git");

        assert!(format!("{error:#}").contains("git://"));
        assert!(
            !parent.exists(),
            "the sink prepared its destination before rejecting the transport"
        );
    }
}
