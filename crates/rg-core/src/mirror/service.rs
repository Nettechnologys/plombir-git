//! Mirror service — business logic for repository mirroring.
//!
//! Supports creating a mirror of an external Git repository, periodic
//! sync via cron-like scheduling, and manual sync triggers.
//!
//! ## The remote's credential
//!
//! `mirrors.password_encrypted` holds the password/token for the upstream
//! remote. It is AES-256-GCM ciphertext, keyed the same way as every other
//! secret at rest in ForgeKeep (`derive_key(encryption_key)` — see
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
//! are converted at startup by [`lift_legacy_url_credentials`], and
//! [`mask_credential`] is the last net in front of anything persisted or
//! logged.

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
pub async fn create_mirror(
    db: &DatabaseConnection,
    repo_id: i64,
    url: String,
    username: Option<String>,
    password: Option<String>,
    sync_interval_seconds: i64,
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

    // Reject an obviously-internal / non-git-transport remote at registration
    // for immediate operator feedback; sync re-checks with DNS resolution.
    crate::net::check_git_url_static(&url).context("invalid mirror URL")?;

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
    encryption_key: &str,
) -> Result<Mirror> {
    let existing = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .ok_or_else(|| crate::error::not_found("mirror"))?;

    let mut model: ActiveModel = existing.into();
    if let Some(v) = url {
        // Same static SSRF/scheme guard as create; sync re-checks with DNS.
        crate::net::check_git_url_static(&v).context("invalid mirror URL")?;
        // …and the same split, for the same reason. Whatever the URL carried is
        // written to the credential fields first, so an explicit `username` /
        // `password` in this same request still overwrites it below.
        let remote = crate::net::split_url_credentials(&v).context("invalid mirror URL")?;
        model.url = Set(remote.url);
        if let Some(lifted) = remote.username {
            model.username = Set(Some(lifted));
        }
        if let Some(lifted) = remote.password {
            model.password_encrypted = Set(encrypt_password(Some(&lifted), encryption_key)?);
        }
    }
    if let Some(v) = username {
        model.username = Set(Some(v));
    }
    if let Some(v) = password {
        model.password_encrypted = Set(encrypt_password(Some(&v), encryption_key)?);
    }
    if let Some(v) = sync_interval_seconds {
        model.sync_interval_seconds = Set(v);
    }
    if let Some(v) = status {
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
        model.status = Set(v);
    }
    model.updated_at = Set(Utc::now());

    rg_db::ops::mirror_ops::update(db, model).await
}

/// Delete a mirror.
///
/// The lookup and the `DELETE` are two statements, so a concurrent delete can
/// empty the row out from under this one; zero rows reports `not_found` rather
/// than confirming a deletion this call did not perform.
pub async fn delete_mirror(db: &DatabaseConnection, repo_id: i64) -> Result<()> {
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .ok_or_else(|| crate::error::not_found("mirror"))?;
    if rg_db::ops::mirror_ops::delete_by_id(db, mirror.id).await? {
        Ok(())
    } else {
        Err(crate::error::not_found("mirror"))
    }
}

/// Sync a single mirror: clone (first time) or fetch (subsequent).
///
/// Returns `Ok(true)` if a pass ran (successful or not — the outcome is on the
/// row), `Ok(false)` if the operator has this mirror switched off.
///
/// The guard tests for *switched off* and nothing else. It used to require
/// `status == "active"`, which made it refuse exactly the mirrors this function
/// had itself marked [`STATUS_ERROR`] on the previous pass — a broken mirror
/// could then never be repaired by a later sync, because no later sync would
/// run (card_770723efaa96). A failed pass is a reason to retry, not a reason to
/// stop.
pub async fn sync_mirror(
    db: &DatabaseConnection,
    mirror: &Mirror,
    repo_root: &Path,
    encryption_key: &str,
) -> Result<bool> {
    if mirror.status == STATUS_INACTIVE {
        return Ok(false);
    }

    let repo_path = repo_root.join(format!("{}.mirror", mirror.repo_id));

    // Decrypt before the guard so a credential that can no longer be read is
    // reported as such, rather than as a plain authentication failure from the
    // remote. Like every other failure here it lands in `last_sync_error`.
    let credentials = load_credentials(mirror, encryption_key);

    // SSRF guard (with DNS resolution) immediately before the git subprocess.
    // Re-checked here — not only at create/update — so a URL that resolved
    // public earlier, an old mirror predating this guard, or a DNS-rebind to an
    // internal address is caught right before the network call. A failure is
    // recorded as a normal sync error below (status=error), not propagated.
    let result = match &credentials {
        Ok(credentials) => match crate::net::guard_git_url(&mirror.url).await {
            Ok(()) => {
                if repo_path.join("HEAD").exists() {
                    // Existing mirror: git remote update
                    run_git_remote_update(&repo_path, credentials.as_ref())
                } else {
                    // First time: git clone --mirror
                    run_git_clone_mirror(&mirror.url, &repo_path, credentials.as_ref())
                }
            }
            Err(e) => Err(e.context("mirror remote URL failed SSRF validation")),
        },
        // `anyhow::Error` is not `Clone`, and the borrow above needs the
        // credentials to stay put, so re-word the failure instead of moving it.
        Err(_) => Err(anyhow::anyhow!(
            "the stored credential for this mirror could not be decrypted \
             (it predates encryption at rest, or the server's secret changed) — \
             re-enter it in the mirror settings"
        )),
    };

    let now = Utc::now();
    let next_sync = now + chrono::Duration::seconds(mirror.sync_interval_seconds);

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
            let reason = mask_credential(&format!("{e:#}"), credentials.as_ref().ok());
            model.last_sync_error = Set(Some(reason.clone()));
            model.status = Set(STATUS_ERROR.to_string());
            tracing::error!(repo_id = mirror.repo_id, error = %reason, "mirror sync failed");
        }
    }

    rg_db::ops::mirror_ops::update(db, model).await?;
    Ok(true)
}

/// Sync all due mirrors (called by background task / cron).
pub async fn sync_due_mirrors(
    db: &DatabaseConnection,
    repo_root: &Path,
    limit: u64,
    encryption_key: &str,
) -> Result<usize> {
    let mirrors = rg_db::ops::mirror_ops::list_due_sync(db, limit).await?;
    let mut count = 0;
    for mirror in &mirrors {
        match sync_mirror(db, mirror, repo_root, encryption_key).await {
            Ok(true) => count += 1,
            Ok(false) => { /* inactive, skip */ }
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
    encryption_key: &str,
) -> Result<()> {
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(db, repo_id)
        .await?
        .ok_or_else(|| crate::error::not_found("mirror"))?;
    if !sync_mirror(db, &mirror, repo_root, encryption_key).await? {
        return Err(crate::error::conflict(
            "this mirror is switched off — set its status to `active` before syncing it",
        ));
    }
    Ok(())
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
/// column. Run by `forgekeep serve` right after the key preflight — the first
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
    url: &str,
    path: &Path,
    credentials: Option<&GitCredentials>,
) -> Result<()> {
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
    let invocation = credential_invocation(credentials);
    let destination = path.to_string_lossy();
    invocation
        .run(git, &["clone", "--mirror", url, &destination], None)?
        .ensure_success()
        .context("git clone --mirror")
}

fn run_git_remote_update(path: &Path, credentials: Option<&GitCredentials>) -> Result<()> {
    let git = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    credential_invocation(credentials)
        .run(git, &["remote", "update", "--prune"], Some(path))?
        .ensure_success()
        .context("git remote update")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::spawn_authenticating_remote;
    use base64::{engine::general_purpose::STANDARD, Engine as _};

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
        let (address, seen) = spawn_authenticating_remote();
        let directory = tempfile::tempdir().expect("tempdir");
        let credentials = credentials(Some("sync-bot"), "hunter2");

        let outcome = run_git_clone_mirror(
            &format!("http://{address}/upstream.git"),
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
}
