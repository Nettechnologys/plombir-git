//! Push mirrors — the repository sends its branches and tags to another forge.
//!
//! The opposite direction of [`crate::mirror::service`]: there the remote is
//! authoritative and this repository is a read-only copy; here this repository
//! is where people work and the remote is kept up to date with it.
//!
//! ## When a push happens
//!
//! Every ref move this server accepts — a push over HTTP or SSH, a web edit, a
//! merge — runs the post-push hooks, and those call
//! [`after_ref_move`](crate::mirror::push::after_ref_move): each
//! enabled push mirror of the repository is marked as behind
//! (`pending_since`), and the ones that sync on commit become due at once and
//! wake the scheduler. The push itself runs in the background
//! ([`crate::mirror::push_scheduler`]), never on the request that moved the ref: an
//! unreachable mirror must not fail or slow down the push into this server.
//! Mirrors that do not sync on commit go out on their interval, and "Sync now"
//! makes any mirror due.
//!
//! ## What is pushed
//!
//! `refs/heads/*` and `refs/tags/*` — never the server's own refs (fork
//! scratch refs, merge-queue groups, pull request heads). By default only
//! fast-forwards: a branch rewritten here, or one somebody moved on the remote,
//! is reported as a rejected update and left alone, and a branch deleted here
//! stays on the remote. `force` makes the remote an exact copy — rewritten
//! branches are overwritten and deleted ones removed — and is a switch the
//! owner turns on, never the default, because it destroys whatever the remote
//! held that this repository does not.
//!
//! LFS objects are not uploaded: the remote receives the pointer files.
//!
//! ## The remote and its credential
//!
//! The same rules as the pull mirror, through the same helpers: HTTPS only
//! (plain HTTP only with `[mirror].allow_insecure_http`; SSH remotes need an
//! identity contract this server does not have yet), the SSRF guard with DNS
//! resolution right before every `git` subprocess and the resolved address
//! pinned for it, a credential typed into the URL moved into the encrypted
//! column, and the secret handed to `git` through the environment only. Every
//! message that is stored or logged goes through the same mask.

use anyhow::{Context, Result};
use chrono::Utc;
use rg_db::entities::push_mirror::{ActiveModel, Model as PushMirror};
use rg_git::cli_gateway::global_gateway;
use rg_git::credentials::{credential_invocation, GitCredentials};
use sea_orm::ActiveValue::Set;
use sea_orm::DatabaseConnection;
use std::path::Path;

use super::transport::MirrorTransportPolicy;

/// The shortest interval a push mirror may be given; `0` means "no interval"
/// — the mirror is pushed on commit and on request only.
pub const MIN_SYNC_INTERVAL_SECONDS: i64 = 600;

/// Interval a new push mirror gets when the request names none.
pub const DEFAULT_SYNC_INTERVAL_SECONDS: i64 = 8 * 60 * 60;

/// How many push mirrors one repository may have. Each is a `git push` to a
/// third-party host after every ref move, so the number is a fan-out bound.
pub const MAX_PUSH_MIRRORS_PER_REPO: u64 = 10;

/// When a failed pass is tried again, whatever the interval says: a mirror
/// that syncs on commit must not wait for the next push to be retried.
pub const RETRY_AFTER_FAILURE: chrono::Duration = chrono::Duration::minutes(10);

/// A new push mirror, as the owner asked for it.
pub struct NewPushMirror {
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub sync_on_commit: bool,
    pub sync_interval_seconds: i64,
    pub force: bool,
}

/// A change to a push mirror. `None` leaves a field as it is; an empty
/// `username` / `password` clears it.
#[derive(Default)]
pub struct PushMirrorChanges {
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub sync_on_commit: Option<bool>,
    pub sync_interval_seconds: Option<i64>,
    pub force: Option<bool>,
    pub enabled: Option<bool>,
}

fn check_sync_interval(seconds: i64) -> Result<()> {
    if seconds != 0 && seconds < MIN_SYNC_INTERVAL_SECONDS {
        return Err(crate::error::invalid_request(format!(
            "`sync_interval_seconds` must be 0 (push on commit and on request only) or at \
             least {MIN_SYNC_INTERVAL_SECONDS}"
        )));
    }
    Ok(())
}

fn next_scheduled(interval_seconds: i64) -> Option<chrono::DateTime<Utc>> {
    (interval_seconds > 0).then(|| Utc::now() + chrono::Duration::seconds(interval_seconds))
}

/// The remote as it will be stored: checked against the transport policy and
/// the SSRF guard's static half, with a credential written into it lifted out.
struct Remote {
    url: String,
    username: Option<String>,
    password: Option<String>,
}

fn accept_remote(
    url: &str,
    username: Option<String>,
    password: Option<String>,
    transport_policy: MirrorTransportPolicy,
) -> Result<Remote> {
    transport_policy.validate_url_static(url)?;
    let split = crate::net::split_url_credentials(url).context("invalid push mirror URL")?;
    let username = username
        .filter(|value| !value.is_empty())
        .or(split.username);
    let password = password
        .filter(|value| !value.is_empty())
        .or(split.password);
    crate::net::check_remote_url_length(&split.url)?;
    crate::net::check_remote_username_length(username.as_deref())?;
    Ok(Remote {
        url: split.url,
        username,
        password,
    })
}

/// Add a push mirror to a repository.
pub async fn create_push_mirror(
    db: &DatabaseConnection,
    repo_id: i64,
    new: NewPushMirror,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
) -> Result<PushMirror> {
    check_sync_interval(new.sync_interval_seconds)?;
    let remote = accept_remote(&new.url, new.username, new.password, transport_policy)?;
    if rg_db::ops::push_mirror_ops::count_for_repo(db, repo_id).await? >= MAX_PUSH_MIRRORS_PER_REPO
    {
        return Err(crate::error::conflict(format!(
            "this repository already has {MAX_PUSH_MIRRORS_PER_REPO} push mirrors; delete one \
             before adding another"
        )));
    }
    let now = Utc::now();
    rg_db::ops::push_mirror_ops::create(
        db,
        ActiveModel {
            repo_id: Set(repo_id),
            url: Set(remote.url),
            username: Set(remote.username),
            password_encrypted: Set(super::service::encrypt_password(
                remote.password.as_deref(),
                encryption_key,
            )?),
            sync_on_commit: Set(new.sync_on_commit),
            sync_interval_seconds: Set(new.sync_interval_seconds),
            force: Set(new.force),
            enabled: Set(true),
            // A new mirror has never received anything, so it is behind from
            // the start and goes out on the scheduler's next pass.
            next_sync_at: Set(Some(rg_db::ops::push_mirror_ops::due_now())),
            pending_since: Set(Some(now)),
            pending_revision: Set(0),
            last_sync_at: Set(None),
            last_success_at: Set(None),
            last_sync_error: Set(None),
            lease_token: Set(None),
            lease_acquired_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
    )
    .await
    .inspect(|_| wake_scheduler())
}

pub async fn list_push_mirrors(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<PushMirror>> {
    rg_db::ops::push_mirror_ops::list_for_repo(db, repo_id).await
}

fn not_found() -> anyhow::Error {
    crate::error::not_found("push mirror")
}

/// Change a push mirror's settings.
pub async fn update_push_mirror(
    db: &DatabaseConnection,
    repo_id: i64,
    id: i64,
    changes: PushMirrorChanges,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
) -> Result<PushMirror> {
    let current = rg_db::ops::push_mirror_ops::find_for_repo(db, repo_id, id)
        .await?
        .ok_or_else(not_found)?;
    let mut model: ActiveModel = current.clone().into();

    if let Some(url) = changes.url {
        let remote = accept_remote(&url, None, None, transport_policy)?;
        // The stored credential was given for one remote. Carrying it to
        // another host would hand somebody's token to an address the next
        // editor picked, so a move to a new origin has to bring its own
        // credential — or say, with an empty `password`, that it needs none.
        if current.password_encrypted.is_some()
            && !super::same_origin(&current.url, &remote.url)
            && remote.password.is_none()
            && changes.password.is_none()
        {
            return Err(crate::error::invalid_request(
                "the new URL points at another host, and the stored credential was given for \
                 the old one; send `password` (and `username`) for the new remote, or an \
                 empty `password` to push without one",
            ));
        }
        model.url = Set(remote.url);
        if let Some(username) = remote.username {
            model.username = Set(Some(username));
        }
        if let Some(password) = remote.password {
            model.password_encrypted = Set(super::service::encrypt_password(
                Some(&password),
                encryption_key,
            )?);
        }
    }
    if let Some(username) = changes.username {
        crate::net::check_remote_username_length(Some(&username))?;
        model.username = Set((!username.is_empty()).then_some(username));
    }
    if let Some(password) = changes.password {
        model.password_encrypted = Set(super::service::encrypt_password(
            Some(&password),
            encryption_key,
        )?);
    }
    if let Some(sync_on_commit) = changes.sync_on_commit {
        model.sync_on_commit = Set(sync_on_commit);
    }
    if let Some(seconds) = changes.sync_interval_seconds {
        check_sync_interval(seconds)?;
        model.sync_interval_seconds = Set(seconds);
        model.next_sync_at = Set(match current.pending_since {
            Some(_) => Some(rg_db::ops::push_mirror_ops::due_now()),
            None => next_scheduled(seconds),
        });
    }
    if let Some(force) = changes.force {
        model.force = Set(force);
    }
    if let Some(enabled) = changes.enabled {
        model.enabled = Set(enabled);
        if enabled && !current.enabled {
            // Whatever moved while it was off has not reached the remote.
            let now = Utc::now();
            model.pending_since = Set(Some(current.pending_since.unwrap_or(now)));
            model.next_sync_at = Set(Some(rg_db::ops::push_mirror_ops::due_now()));
        }
    }
    model.updated_at = Set(Utc::now());
    let updated = rg_db::ops::push_mirror_ops::update(db, model).await?;
    wake_scheduler();
    Ok(updated)
}

pub async fn delete_push_mirror(db: &DatabaseConnection, repo_id: i64, id: i64) -> Result<()> {
    if rg_db::ops::push_mirror_ops::delete_for_repo(db, repo_id, id).await? {
        Ok(())
    } else {
        Err(not_found())
    }
}

/// "Sync now": make the mirror due and wake the scheduler.
pub async fn request_push_mirror_sync(
    db: &DatabaseConnection,
    repo_id: i64,
    id: i64,
) -> Result<()> {
    if !rg_db::ops::push_mirror_ops::request_sync(db, repo_id, id).await? {
        let exists = rg_db::ops::push_mirror_ops::find_for_repo(db, repo_id, id)
            .await?
            .is_some();
        return Err(if exists {
            crate::error::conflict("this push mirror is switched off; enable it first")
        } else {
            not_found()
        });
    }
    wake_scheduler();
    Ok(())
}

/// The post-push hooks' half: `repo_id`'s refs moved.
///
/// Best effort by design. The ref move is already accepted, and a mirror that
/// could not be marked is caught by its interval; the failure is logged so an
/// operator can tell why a mirror lagged.
pub async fn after_ref_move(db: &DatabaseConnection, repo_id: i64) {
    match rg_db::ops::push_mirror_ops::mark_ref_moved(db, repo_id).await {
        Ok(0) => {}
        Ok(marked) => {
            tracing::debug!(repo_id, marked, "push mirrors marked behind");
            wake_scheduler();
        }
        Err(error) => tracing::warn!(
            repo_id,
            error = %format!("{error:#}"),
            "could not mark this repository's push mirrors behind; they catch up on their \
             interval"
        ),
    }
}

static WAKE: std::sync::LazyLock<tokio::sync::Notify> =
    std::sync::LazyLock::new(tokio::sync::Notify::new);

/// Ask the scheduler for a pass now rather than at its next tick.
fn wake_scheduler() {
    WAKE.notify_one();
}

/// Resolves when [`wake_scheduler`] was called since the last wait.
pub(super) async fn woken() {
    WAKE.notified().await;
}

/// Push every due mirror, up to `batch_size` of them. Returns how many passes
/// ran; a mirror's own failure is recorded on its row, not returned.
pub async fn push_due_mirrors(
    db: &DatabaseConnection,
    repo_root: &Path,
    batch_size: u64,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
) -> Result<usize> {
    let due = rg_db::ops::push_mirror_ops::list_due(db, batch_size).await?;
    let mut ran = 0;
    for mirror in due {
        if push_one(db, &mirror, repo_root, transport_policy, encryption_key).await {
            ran += 1;
        }
    }
    Ok(ran)
}

/// One pass for one mirror. `false` when another pass holds it.
async fn push_one(
    db: &DatabaseConnection,
    mirror: &PushMirror,
    repo_root: &Path,
    transport_policy: MirrorTransportPolicy,
    encryption_key: &str,
) -> bool {
    let token = uuid::Uuid::new_v4().simple().to_string();
    match rg_db::ops::push_mirror_ops::acquire_lease(db, mirror.id, &token).await {
        Ok(true) => {}
        Ok(false) => return false,
        Err(error) => {
            tracing::warn!(
                mirror_id = mirror.id,
                error = %format!("{error:#}"),
                "could not take a push mirror's lease"
            );
            return false;
        }
    }
    let credentials = super::service::decrypt_credentials(
        mirror.username.as_deref(),
        mirror.password_encrypted.as_deref(),
        encryption_key,
    );
    let (credentials, result) = match credentials {
        Ok(credentials) => {
            let (credentials, result) =
                push_pass(db, mirror, repo_root, transport_policy, credentials).await;
            (Some(credentials), result)
        }
        Err(_) => (
            None,
            Err(anyhow::anyhow!(
                "the stored credential for this push mirror could not be decrypted (the \
                 server's secret changed) — re-enter it in the mirror settings"
            )),
        ),
    };

    let recorded = match result {
        Ok(()) => {
            rg_db::ops::push_mirror_ops::record_success(
                db,
                mirror.id,
                &token,
                mirror.pending_revision,
                next_scheduled(mirror.sync_interval_seconds),
            )
            .await
        }
        Err(error) => {
            let message =
                super::service::mask_credential(&format!("{error:#}"), credentials.as_ref());
            tracing::warn!(
                repo_id = mirror.repo_id,
                mirror_id = mirror.id,
                error = %message,
                "push mirror pass failed"
            );
            rg_db::ops::push_mirror_ops::record_failure(
                db,
                mirror.id,
                &token,
                &message,
                Utc::now() + RETRY_AFTER_FAILURE,
            )
            .await
        }
    };
    match recorded {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            mirror_id = mirror.id,
            "a push mirror's lease was taken over while its pass ran; the pass is not recorded"
        ),
        Err(error) => tracing::warn!(
            mirror_id = mirror.id,
            error = %format!("{error:#}"),
            "could not record a push mirror pass"
        ),
    }
    true
}

/// The pass itself. Hands the credential back so the caller can mask it out of
/// whatever the failure says.
async fn push_pass(
    db: &DatabaseConnection,
    mirror: &PushMirror,
    repo_root: &Path,
    transport_policy: MirrorTransportPolicy,
    credentials: Option<GitCredentials>,
) -> (Option<GitCredentials>, Result<()>) {
    let served = match crate::repo::service::repository_identity(db, mirror.repo_id).await {
        Ok((owner, name)) => repo_root.join(&owner).join(format!("{name}.git")),
        Err(error) => return (credentials, Err(error)),
    };
    // The full guard, with DNS, immediately before the subprocess — not only
    // when the URL was stored: the name may resolve somewhere else by now.
    let remote = match transport_policy.destination(&mirror.url).await {
        Ok(remote) => remote,
        Err(error) => {
            return (
                credentials,
                Err(error.context("push mirror remote URL failed transport/SSRF validation")),
            )
        }
    };
    let force = mirror.force;
    let pass = crate::blocking::run_blocking_git("push mirror", move || {
        let result = run_git_push(
            transport_policy,
            &served,
            &remote,
            credentials.as_ref(),
            force,
        );
        Ok((credentials, result))
    })
    .await;
    match pass {
        Ok((credentials, result)) => (credentials, result),
        Err(error) => (None, Err(error)),
    }
}

/// The refspecs a pass pushes: branches and tags only, never the server's own
/// refs, with `+` only when the owner asked for an exact copy.
fn refspecs(force: bool) -> [&'static str; 2] {
    if force {
        ["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"]
    } else {
        ["refs/heads/*:refs/heads/*", "refs/tags/*:refs/tags/*"]
    }
}

fn run_git_push(
    transport_policy: MirrorTransportPolicy,
    served: &Path,
    remote: &crate::net::GuardedGitRemote,
    credentials: Option<&GitCredentials>,
    force: bool,
) -> Result<()> {
    transport_policy.require_confidential_transport(remote.url())?;
    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let invocation = remote.bind_invocation(credential_invocation(credentials))?;
    let mut args = vec!["push", "--porcelain", "--no-verify"];
    if force {
        args.push("--prune");
    }
    args.push(remote.url());
    args.extend(refspecs(force));
    let output = invocation.run(git, &args, Some(served))?;
    let Err(error) = output.ensure_success() else {
        return Ok(());
    };
    // `--porcelain` writes one line per ref to stdout, `!` for a refused one:
    // `!<TAB>refs/heads/main:refs/heads/main<TAB>[rejected] (non-fast-forward)`.
    // The failure git reports on stderr does not say which ref, so the message
    // is built from these. `[rejected]` is git's own refusal of a ref that
    // would not fast-forward; `[remote rejected]` is the remote's — a
    // protected branch, a hook — and needs a different answer.
    let stdout = output.stdout_str();
    let refused: Vec<(&str, &str)> = stdout
        .lines()
        .filter(|line| line.starts_with('!'))
        .filter_map(|line| {
            let mut fields = line.split('\t').skip(1);
            let spec = fields.next()?;
            let summary = fields.next().unwrap_or_default();
            Some((spec.split_once(':').map_or(spec, |(_, to)| to), summary))
        })
        .collect();
    let diverged: Vec<&str> = refused
        .iter()
        .filter(|(_, summary)| summary.starts_with("[rejected]"))
        .map(|(refname, _)| *refname)
        .collect();
    let declined: Vec<String> = refused
        .iter()
        .filter(|(_, summary)| !summary.starts_with("[rejected]"))
        .map(|(refname, summary)| format!("{refname} {summary}"))
        .collect();
    let mut error = error.context("git push to the push mirror");
    if !declined.is_empty() {
        error = error.context(format!("the remote declined {}", declined.join(", ")));
    }
    if !force && !diverged.is_empty() {
        error = error.context(format!(
            "the remote has commits this repository does not on {} (a rewritten or diverged \
             branch, or a tag that points elsewhere). Those refs were left as they are; turn on \
             force to make the mirror an exact copy",
            diverged.join(", ")
        ));
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{migrated_memory_database, spawn_git_http_backend};
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use rg_db::entities::repository;
    use sea_orm::NotSet;

    const KEY: &str = "push-mirror-test-key";

    #[test]
    fn only_branches_and_tags_are_pushed_and_force_is_opt_in() {
        assert_eq!(
            refspecs(false),
            ["refs/heads/*:refs/heads/*", "refs/tags/*:refs/tags/*"]
        );
        assert_eq!(
            refspecs(true),
            ["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"]
        );
    }

    #[test]
    fn an_interval_is_zero_or_at_least_the_floor() {
        assert!(check_sync_interval(0).is_ok());
        assert!(check_sync_interval(MIN_SYNC_INTERVAL_SECONDS).is_ok());
        assert!(check_sync_interval(MIN_SYNC_INTERVAL_SECONDS - 1).is_err());
        assert!(check_sync_interval(-1).is_err());
        assert_eq!(next_scheduled(0), None);
    }

    /// The git CLI through the gateway, with the identity a commit needs.
    fn git(dir: &Path, args: &[&str]) -> String {
        let mut full = vec![
            "-c",
            "user.name=Push Mirror",
            "-c",
            "user.email=pm@example.invalid",
            "-c",
            "init.defaultBranch=main",
        ];
        full.extend_from_slice(args);
        let output = global_gateway()
            .as_ref()
            .expect("git gateway")
            .run(&full, Some(dir))
            .expect("run git");
        assert!(
            output.success(),
            "git {args:?} failed: {}",
            output.stderr_str()
        );
        output.stdout_str().trim().to_string()
    }

    fn rev(repo: &Path, refname: &str) -> Option<String> {
        let output = global_gateway()
            .as_ref()
            .expect("git gateway")
            .run(&["rev-parse", "--verify", "--quiet", refname], Some(repo))
            .expect("run git");
        output
            .success()
            .then(|| output.stdout_str().trim().to_string())
    }

    /// A served bare repository with `main`, `feature`, tag `v1` and a ref of
    /// the server's own; the work tree it was pushed from.
    fn served_repository(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let work = root.join("work");
        let served = root.join("served.git");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q"]);
        std::fs::write(work.join("README.md"), "one\n").unwrap();
        git(&work, &["add", "README.md"]);
        git(&work, &["commit", "-q", "-m", "one"]);
        git(&work, &["tag", "v1"]);
        git(&work, &["branch", "feature"]);
        git(root, &["init", "-q", "--bare", "served.git"]);
        git(
            &work,
            &[
                "push",
                "-q",
                served.to_str().unwrap(),
                "main",
                "feature",
                "v1",
            ],
        );
        git(&served, &["update-ref", "refs/pull/1/head", "main"]);
        (served, work)
    }

    /// The whole outbound half against a remote that accepts pushes: the
    /// stored credential reaches it, branches and tags land, the server's own
    /// refs do not, and a rewritten branch only goes out with `force`.
    #[test]
    fn a_push_reaches_the_remote_with_its_credential_and_force_is_what_rewrites() {
        let temp = tempfile::tempdir().unwrap();
        let backend_root = temp.path().join("remote");
        std::fs::create_dir_all(&backend_root).unwrap();
        git(&backend_root, &["init", "-q", "--bare", "target.git"]);
        let target = backend_root.join("target.git");
        let backend = spawn_git_http_backend(&backend_root);
        let remote =
            crate::net::GuardedGitRemote::unbound_for_test(&format!("{}/target.git", backend.url));
        let credentials = GitCredentials::new(Some("mirror-bot".into()), "s3cret".into());
        let policy = MirrorTransportPolicy::new(true);
        let (served, work) = served_repository(temp.path());

        run_git_push(policy, &served, &remote, Some(&credentials), false).expect("first push");
        assert_eq!(
            rev(&target, "refs/heads/main"),
            rev(&served, "refs/heads/main")
        );
        assert_eq!(
            rev(&target, "refs/heads/feature"),
            rev(&served, "refs/heads/feature")
        );
        assert_eq!(rev(&target, "refs/tags/v1"), rev(&served, "refs/tags/v1"));
        assert_eq!(
            rev(&target, "refs/pull/1/head"),
            None,
            "a ref of the server's own reached the mirror"
        );
        let expected = format!("Basic {}", STANDARD.encode("mirror-bot:s3cret"));
        assert!(
            backend.authorizations.lock().unwrap().contains(&expected),
            "the remote never saw the stored credential"
        );

        // Rewrite `main` and delete `feature` here.
        git(&work, &["commit", "-q", "--amend", "-m", "rewritten"]);
        git(
            &work,
            &["push", "-q", "--force", served.to_str().unwrap(), "main"],
        );
        git(&served, &["update-ref", "-d", "refs/heads/feature"]);
        let before = rev(&target, "refs/heads/main");

        let error = run_git_push(policy, &served, &remote, Some(&credentials), false)
            .expect_err("a rewritten branch must not go out without force");
        assert!(format!("{error:#}").contains("turn on force"), "{error:#}");
        assert_eq!(
            rev(&target, "refs/heads/main"),
            before,
            "the remote's history moved"
        );
        assert!(
            rev(&target, "refs/heads/feature").is_some(),
            "a branch was deleted on the remote without force"
        );

        run_git_push(policy, &served, &remote, Some(&credentials), true).expect("forced push");
        assert_eq!(
            rev(&target, "refs/heads/main"),
            rev(&served, "refs/heads/main")
        );
        assert_eq!(
            rev(&target, "refs/heads/feature"),
            None,
            "force did not make the remote an exact copy"
        );

        // The remote's own refusal is not a divergence, and force is not the
        // answer to it.
        let hook = target.join("hooks").join("pre-receive");
        std::fs::write(&hook, "#!/bin/sh\necho 'main is protected' >&2\nexit 1\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(work.join("README.md"), "two\n").unwrap();
        git(&work, &["commit", "-q", "-am", "two"]);
        git(&work, &["push", "-q", served.to_str().unwrap(), "main"]);
        let error = run_git_push(policy, &served, &remote, Some(&credentials), false)
            .expect_err("the remote's hook refused the push");
        let message = format!("{error:#}");
        assert!(
            message.contains("the remote declined refs/heads/main [remote rejected]"),
            "{message}"
        );
        assert!(!message.contains("turn on force"), "{message}");
    }

    /// A credential is given for one remote: moving the mirror to another host
    /// must bring a new one, while a new path on the same host keeps it.
    #[tokio::test]
    async fn a_new_host_does_not_inherit_the_stored_credential() {
        let db = migrated_memory_database().await;
        let repo_id = seeded_repo(&db).await;
        let mut new = new_mirror("https://github.example/org/repo.git");
        new.username = Some("mirror-bot".into());
        new.password = Some("s3cret".into());
        let mirror = create_push_mirror(&db, repo_id, new, MirrorTransportPolicy::default(), KEY)
            .await
            .unwrap();
        let change = |url: &str, password: Option<&str>| PushMirrorChanges {
            url: Some(url.to_string()),
            password: password.map(str::to_string),
            ..Default::default()
        };

        let error = update_push_mirror(
            &db,
            repo_id,
            mirror.id,
            change("https://collector.example/org/repo.git", None),
            MirrorTransportPolicy::default(),
            KEY,
        )
        .await
        .expect_err("the stored token followed the mirror to another host");
        assert!(format!("{error:#}").contains("another host"), "{error:#}");

        let moved = update_push_mirror(
            &db,
            repo_id,
            mirror.id,
            change("https://github.example/org/renamed.git", None),
            MirrorTransportPolicy::default(),
            KEY,
        )
        .await
        .expect("a new path on the same host keeps its credential");
        assert!(moved.password_encrypted.is_some());

        let rehomed = update_push_mirror(
            &db,
            repo_id,
            mirror.id,
            change("https://collector.example/org/repo.git", Some("")),
            MirrorTransportPolicy::default(),
            KEY,
        )
        .await
        .expect("an explicit empty password moves the mirror without a credential");
        assert_eq!(rehomed.password_encrypted, None);
    }

    async fn seeded_repo(db: &DatabaseConnection) -> i64 {
        let owner = rg_db::ops::user_ops::create_user(
            db,
            "push-mirror-owner",
            "push-mirror-owner@example.invalid",
            "",
            "Owner",
        )
        .await
        .expect("create owner");
        let now = Utc::now();
        rg_db::ops::repo_ops::create(
            db,
            repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("pushed".to_string()),
                description: Set(None),
                is_private: Set(true),
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
        .expect("create repository")
        .id
    }

    fn new_mirror(url: &str) -> NewPushMirror {
        NewPushMirror {
            url: url.to_string(),
            username: None,
            password: None,
            sync_on_commit: true,
            sync_interval_seconds: DEFAULT_SYNC_INTERVAL_SECONDS,
            force: false,
        }
    }

    /// A credential typed into the URL is stored sealed, never in the URL.
    #[tokio::test]
    async fn a_credential_in_the_url_is_sealed_and_lifted_out() {
        let db = migrated_memory_database().await;
        let repo_id = seeded_repo(&db).await;
        let mirror = create_push_mirror(
            &db,
            repo_id,
            new_mirror("https://mirror-bot:s3cret@example.com/org/repo.git"),
            MirrorTransportPolicy::default(),
            KEY,
        )
        .await
        .unwrap();
        assert_eq!(mirror.url, "https://example.com/org/repo.git");
        assert_eq!(mirror.username.as_deref(), Some("mirror-bot"));
        let sealed = mirror
            .password_encrypted
            .clone()
            .expect("a sealed credential");
        assert!(!sealed.contains("s3cret"));
        let opened = super::super::service::decrypt_credentials(None, Some(&sealed), KEY)
            .unwrap()
            .unwrap();
        assert_eq!(opened.password(), "s3cret");
        assert!(mirror.pending_since.is_some(), "a new mirror starts behind");
    }

    /// The race the revision exists for: a ref move while a pass runs must
    /// keep the mirror behind and due, not be cleared by that pass.
    #[tokio::test]
    async fn a_move_during_a_pass_keeps_the_mirror_behind_and_due() {
        let db = migrated_memory_database().await;
        let repo_id = seeded_repo(&db).await;
        let mirror = create_push_mirror(
            &db,
            repo_id,
            new_mirror("https://example.com/org/repo.git"),
            MirrorTransportPolicy::default(),
            KEY,
        )
        .await
        .unwrap();
        assert!(
            rg_db::ops::push_mirror_ops::acquire_lease(&db, mirror.id, "first")
                .await
                .unwrap()
        );
        assert!(
            !rg_db::ops::push_mirror_ops::acquire_lease(&db, mirror.id, "second")
                .await
                .unwrap(),
            "two passes held one mirror"
        );
        let started = mirror.pending_revision;
        after_ref_move(&db, repo_id).await;
        let later = Utc::now() + chrono::Duration::hours(8);
        assert!(rg_db::ops::push_mirror_ops::record_success(
            &db,
            mirror.id,
            "first",
            started,
            Some(later)
        )
        .await
        .unwrap());
        let row = rg_db::ops::push_mirror_ops::find_for_repo(&db, repo_id, mirror.id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            row.pending_since.is_some(),
            "the move during the pass was forgotten"
        );
        assert!(
            row.next_sync_at.is_some_and(|next| next <= Utc::now()),
            "a sync-on-commit mirror with a missed move must be due again: {row:?}"
        );
        assert!(row.last_success_at.is_some());
        assert_eq!(row.lease_token, None);

        assert!(
            rg_db::ops::push_mirror_ops::acquire_lease(&db, mirror.id, "third")
                .await
                .unwrap()
        );
        assert!(rg_db::ops::push_mirror_ops::record_success(
            &db,
            mirror.id,
            "third",
            row.pending_revision,
            Some(later)
        )
        .await
        .unwrap());
        let row = rg_db::ops::push_mirror_ops::find_for_repo(&db, repo_id, mirror.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.pending_since, None,
            "a caught-up mirror still reads behind"
        );
        assert!(row.next_sync_at.is_some_and(|next| next > Utc::now()));

        // Switched off, a mirror is neither marked nor swept.
        update_push_mirror(
            &db,
            repo_id,
            mirror.id,
            PushMirrorChanges {
                enabled: Some(false),
                ..Default::default()
            },
            MirrorTransportPolicy::default(),
            KEY,
        )
        .await
        .unwrap();
        after_ref_move(&db, repo_id).await;
        let off = rg_db::ops::push_mirror_ops::find_for_repo(&db, repo_id, mirror.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(off.pending_revision, row.pending_revision);
        let error = request_push_mirror_sync(&db, repo_id, mirror.id)
            .await
            .unwrap_err();
        assert!(
            error.downcast_ref::<crate::error::Conflict>().is_some(),
            "{error:#}"
        );
    }

    /// An unreachable mirror is recorded and retried; nothing about it fails
    /// the caller, and the stored secret never reaches the recorded error.
    #[tokio::test]
    async fn an_unreachable_mirror_records_a_masked_failure_and_stays_behind() {
        let db = migrated_memory_database().await;
        let repo_id = seeded_repo(&db).await;
        let now = Utc::now();
        let sealed = super::super::service::encrypt_password(Some("s3cret"), KEY).unwrap();
        // A row written before the create-time checks: credential still in the
        // URL, and an address the SSRF guard refuses.
        let mirror = rg_db::ops::push_mirror_ops::create(
            &db,
            ActiveModel {
                repo_id: Set(repo_id),
                url: Set("https://mirror-bot:s3cret@127.0.0.1:9/target.git".to_string()),
                username: Set(Some("mirror-bot".to_string())),
                password_encrypted: Set(sealed),
                sync_on_commit: Set(true),
                sync_interval_seconds: Set(DEFAULT_SYNC_INTERVAL_SECONDS),
                force: Set(false),
                enabled: Set(true),
                next_sync_at: Set(Some(now)),
                pending_since: Set(Some(now)),
                pending_revision: Set(0),
                last_sync_at: Set(None),
                last_success_at: Set(None),
                last_sync_error: Set(None),
                lease_token: Set(None),
                lease_acquired_at: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let repo_root = tempfile::tempdir().unwrap();

        let ran = push_due_mirrors(
            &db,
            repo_root.path(),
            10,
            MirrorTransportPolicy::default(),
            KEY,
        )
        .await
        .expect("a failing mirror is not the caller's error");
        assert_eq!(ran, 1);
        let row = rg_db::ops::push_mirror_ops::find_for_repo(&db, repo_id, mirror.id)
            .await
            .unwrap()
            .unwrap();
        let error = row
            .last_sync_error
            .clone()
            .expect("the failure is recorded");
        assert!(
            !error.contains("s3cret"),
            "the secret reached the stored error: {error}"
        );
        assert!(row.pending_since.is_some(), "a failed pass cleared the lag");
        assert!(
            row.next_sync_at
                .is_some_and(|next| next > Utc::now() + chrono::Duration::minutes(9)),
            "the retry is not scheduled: {row:?}"
        );
        assert_eq!(row.lease_token, None, "the lease outlived the pass");
    }
}
