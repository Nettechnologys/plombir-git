//! Reporting a file-system operation that failed, and cleaning up after one.
//!
//! Not the "cross-platform file system utilities" the first line used to claim:
//! the permission half of this module (`set_executable`, `get_permissions`,
//! `is_executable`, `create_dir_executable`, `copy_preserve_permissions`,
//! `executable_extension`, `is_absolute`, `normalize_path`) had no caller in the
//! workspace, while fifteen call sites reached for `PermissionsExt` directly.
//! See [`super`] for the decision and its evidence.

use std::fs;
use std::path::Path;

/// Ownership / permission diagnostic for a path the process failed to use.
///
/// Container deployments hit this constantly: the container user and the host
/// user share a *name* but not a *uid*, so a bind-mounted directory created by
/// the host user is unusable inside the container and the failure surfaces as a
/// bare `Permission denied (os error 13)` with no hint of which side is wrong.
/// This turns that into one line naming both sides of the mismatch plus the
/// `chown` that fixes it.
///
/// When `path` itself does not exist yet (the usual case for a directory the
/// server is about to create), the nearest existing ancestor is reported —
/// that is the directory whose permissions actually blocked the operation.
///
/// Returns `None` on non-unix platforms and when no ancestor can be stat'd.
pub fn ownership_hint<P: AsRef<Path>>(path: P) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let path = path.as_ref();
        let (blocking, metadata) = path
            .ancestors()
            .find_map(|candidate| fs::metadata(candidate).ok().map(|m| (candidate, m)))?;

        // SAFETY: `geteuid`/`getegid` are always-succeeding libc calls that read
        // the calling process's own credentials and touch no memory.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let (owner_uid, owner_gid) = (metadata.uid(), metadata.gid());
        let mode = metadata.mode() & 0o7777;
        let shown = blocking.display();

        // Wrong owner and wrong mode need opposite fixes, and telling an
        // operator to `chown` a directory they already own reads as nonsense.
        let remedy = if owner_uid == uid {
            format!(
                "the owner already matches, so it is the mode that denies access: `chmod -R u+rwX {shown}`"
            )
        } else {
            format!(
                "fix it with `chown -R {uid}:{gid} {shown}` — for a Docker bind-mount run that on \
                 the host against the host-side directory, since the container uid is unrelated to \
                 the host user of the same name"
            )
        };

        Some(format!(
            "this process runs as uid={uid} gid={gid}, but {shown} is owned by uid={owner_uid} \
             gid={owner_gid} (mode {mode:04o}); {remedy}"
        ))
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Format `error` on `path` as a single actionable line: the failure, the
/// remediation for the caller's situation, and — when the error is
/// permission-related — the uid/ownership diagnostic from [`ownership_hint`].
///
/// `what` names the thing in operator terms (`"repo_root"`, `"SSH host key"`),
/// `remedy` is the caller-specific advice appended when the failure is *not* a
/// permission problem (a missing parent, a bind-mounted directory, …).
pub fn describe_path_error(
    what: &str,
    path: &Path,
    error: &std::io::Error,
    remedy: &str,
) -> String {
    let mut message = format!("{what} {} is unusable: {error}", path.display());
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        if let Some(hint) = ownership_hint(path) {
            message.push_str(&format!("\n  hint: {hint}"));
            return message;
        }
    }
    if !remedy.is_empty() {
        message.push_str(&format!("\n  hint: {remedy}"));
    }
    message
}

/// Remediation for a filesystem failure under the repository storage root.
///
/// The server derives every repository path from `repo_root` and creates the
/// `<owner>/` level below it on demand, so neither directory is ever named by
/// the request that failed — the operator only sees `os error 13` from a clone
/// or an import.
pub const REPO_ROOT_HINT: &str =
    "repositories live under the `[server].repo_root` directory (`--repo-root` / \
     `FORGEKEEP_REPO_ROOT`); that directory, and the `<owner>/` level the server creates below \
     it, must be writable by the user running forgekeep";

/// Remediation for a filesystem failure on a CI cache archive.
///
/// The cache lives in `_ci_cache/<repo_id>/` under the repository storage root
/// and is touched from both ends — the runner writes and reads the archive, the
/// server stores and serves it — so both halves have to name the same directory
/// or an operator gets a different story depending on which side failed first.
pub const CI_CACHE_DIR_HINT: &str =
    "CI cache archives live in `_ci_cache/<repo_id>/` next to the repository storage root; \
     that directory must be writable by the user running forgekeep";

/// Remediation for a filesystem failure inside the blob storage backend.
///
/// The local backend is rooted at `[server].repo_root` and derives every object
/// path from a [`BlobKey`](crate::blob_storage::BlobKey), so neither the root
/// nor the object path is named by the request that failed — an artifact
/// upload, an LFS push, a package publish and a `docker push` all surface the
/// same anonymous errno.
pub const BLOB_STORAGE_HINT: &str =
    "artifacts, packages, OCI images and LFS objects are stored under the `[server].repo_root` \
     directory; that directory must be writable by the user running forgekeep";

/// Remediation for a filesystem failure on an on-disk LFS object.
///
/// The directory is derived from the repository, never echoed back, and the oid
/// is sharded below it, so nothing on either side of the failure names a path:
/// the HTTP handlers compute it from the request, and the background compressor
/// from a database row. Both halves share this hint so an operator gets the same
/// story whichever one failed first.
pub const LFS_STORAGE_HINT: &str =
    "LFS objects live in `<owner>.lfs/<repo>/` under the `[server].repo_root` directory; that \
     directory must be writable by the user running forgekeep";

/// [`describe_path_error`] as a ready-to-propagate [`anyhow::Error`].
///
/// The `?` on a bare `std::fs` call discards the path — the io error only
/// carries the errno — so every caller that computes its path internally has to
/// re-attach it. This is that one line.
pub fn path_error(what: &str, path: &Path, error: &std::io::Error, remedy: &str) -> anyhow::Error {
    anyhow::anyhow!("{}", describe_path_error(what, path, error, remedy))
}

/// Refuse a file that grants access by itself and that another local account
/// can read.
///
/// The question this answers is not "can this process read the file" — that is
/// what a successful `File::open` already says — but "can a *different* local
/// account read it". A credential that answers yes to the second is already
/// shared with every other account on the host, and no amount of care further
/// down the call chain takes that back. Any group or world bit is therefore a
/// refusal, not a warning: the material behind these paths (a JWT signing
/// secret, an at-rest encryption key, a runner token, a TLS or SSH private key)
/// is enough on its own to impersonate the instance or decrypt its traffic.
///
/// `what` names the thing in operator terms (`"config file"`, `"TLS private
/// key"`), and the message carries the observed mode plus the `chmod` that
/// fixes it — a bare "permission problem" would send the operator the opposite
/// way, towards *widening* the file.
///
/// Unix permission bits have no portable equivalent, so non-Unix targets keep
/// whatever validation the caller does around this and rely on their platform
/// ACLs.
pub fn ensure_owner_only(path: &Path, what: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(path)
            .map_err(|error| {
                path_error(
                    what,
                    path,
                    &error,
                    "the file must exist and be readable by the user running forgekeep",
                )
            })?
            .permissions()
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "{what} {} has mode {mode:04o}; run chmod 600 {}",
                path.display(),
                path.display()
            );
        }
    }

    #[cfg(not(unix))]
    {
        let _ = (path, what);
    }

    Ok(())
}

/// Warn when a directory the server keeps state in can be entered by another
/// local account.
///
/// The question is asked of the *directory*, not of each artefact under it,
/// because the directory is where it can be answered once. A bare repository
/// is a tree of `0644` objects, `VACUUM INTO` writes its snapshot `0644` by
/// construction, and the audit archive and the SQLite database are ordinary
/// files — every one of them inherits its reachability from the directory that
/// holds them, so `chmod 700` on the parent settles the lot and no future kind
/// of artefact has to remember its own mode.
///
/// A warning and not a refusal, unlike [`ensure_owner_only`]. That one guards
/// single files whose contents are a credential: exposure is immediate and
/// total, and the operator can fix it in one `chmod` before the server is of
/// any use. A state directory, by contrast, was created by whoever deployed
/// this instance — often under a stock `umask`, often long before this version
/// existed — and turning an upgrade into a start-up failure for all of them
/// would cost more than the leak it closes. So the line names the path, the
/// mode observed and the `chmod` that fixes it, and the server starts.
///
/// A path that cannot be stat'd is silent: the caller has just created it, and
/// a second failure to read what it created is not this function's story to
/// tell.
pub fn warn_if_others_can_reach(what: &str, path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let Ok(metadata) = fs::metadata(path) else {
            return;
        };
        let mode = metadata.permissions().mode() & 0o7777;
        if mode & 0o077 == 0 {
            return;
        }

        let shown = path.display();
        tracing::warn!(
            path = %shown,
            mode = %format!("{mode:04o}"),
            "{what} {shown} has mode {mode:04o}, so every other local account on this host can \
             read what the server keeps there; run `chmod 700 {shown}` (the server does not \
             narrow a directory an operator created, so this is a warning and not a refusal)"
        );
    }

    #[cfg(not(unix))]
    {
        let _ = (what, path);
    }
}

/// The levels a `create_dir_all` of `path` would have to make itself.
///
/// `ancestors()` walks from `path` upwards and existence is monotone along that
/// walk — a directory cannot exist under one that does not — so the missing
/// levels are exactly the leading run of the walk, deepest first.
///
/// The empty-path guard is not a formality: a relative `path` such as
/// `data/backups` ends its ancestor walk at `""`, which exists as nothing at
/// all, so without the guard the caller would go on to `chmod` a path that is
/// not there and turn a created directory into an `ENOENT`.
///
/// A `candidate.exists()` that fails (an unreadable parent) reads as missing,
/// which is the safe way round: the `create_dir_all` that follows fails on the
/// same directory and returns before any mode is set.
#[cfg(unix)]
fn levels_to_create(path: &Path) -> Vec<&Path> {
    path.ancestors()
        .take_while(|candidate| !candidate.as_os_str().is_empty() && !candidate.exists())
        .collect()
}

/// Create `path`, and every level missing above it, reachable by the owner only.
///
/// [`warn_if_others_can_reach`] is the answer for a directory somebody else
/// made: it is stated, not narrowed, because an operator's directory may be
/// shared with a backup job on purpose. This is the answer for the other half
/// of the same question — a directory that did not exist a syscall ago, which
/// nobody can be sharing with anything, and whose only claim to `0755` is the
/// `umask` of whoever happened to start the server. A state directory the
/// server made itself is created right the first time instead of being
/// complained about on every boot afterwards.
///
/// Only the levels *this call* created are narrowed. `create_dir_all` never
/// reports which those were, so they are read off before it runs: `ancestors()`
/// walks up from `path` and existence is monotone along that walk, so the
/// missing levels are exactly its leading run. An existing directory anywhere
/// in the chain is left with the mode it had — narrowing it is the thing
/// [`warn_if_others_can_reach`] deliberately does not do.
///
/// The mode is set twice on purpose. `mkdir(2)` masks the requested mode with
/// the process `umask`, which can only *clear* bits, so passing `0o700` to the
/// builder means the directory is never even briefly wider than that — no
/// window between creation and a `chmod` for another account to walk in
/// through. The explicit `set_permissions` afterwards is what makes the result
/// `0700` exactly rather than "`0700` minus whatever the `umask` also took",
/// and it clears an inherited setgid bit while it is there.
///
/// Returns the raw [`std::io::Error`] rather than a formatted one: the callers
/// each have their own `what` and their own remediation hint, and
/// [`path_error`] is where those meet.
///
/// See [`levels_to_create`] for which levels count as this call's own.
pub fn create_dir_all_owner_only(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        use std::os::unix::fs::PermissionsExt;

        let created = levels_to_create(path);

        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;

        for directory in created {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }

        Ok(())
    }

    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Create a file that no other local account can read, and refuse to touch one
/// that is already there.
///
/// The file-level counterpart of [`create_dir_all_owner_only`], for the
/// material [`ensure_owner_only`] refuses to load: a key or a token whose bytes
/// are enough on their own to impersonate the instance. Both halves of the
/// signature matter, and each answers a failure the obvious spelling has:
///
/// * `std::fs::write` takes the ambient `umask`, so the secret is born `0644`
///   on a stock host and is only narrowed by whatever `chmod` the caller
///   remembers to make next. That is not merely a window: a crash, a `SIGKILL`
///   or a full disk between the two leaves the file readable *permanently*,
///   because the caller that generates a key generates it exactly once and
///   every later start finds the file present and steps aside. Passing the mode
///   to `open(2)` instead means the file is never wider than `0600`, not even
///   for an instant — `O_CREAT` masks the requested mode with the `umask`, and
///   a `umask` can only clear bits.
/// * `create_new` makes the creation itself the exclusion, so two first starts
///   racing cannot each generate a secret and have the loser silently replace
///   the winner's. The caller sees [`std::io::ErrorKind::AlreadyExists`] and
///   can read back what the other one wrote — which is the same shape
///   `ensure_key_file` in the CLI already uses for the at-rest encryption key.
///
/// The explicit `set_permissions` afterwards is what makes the result `0600`
/// exactly rather than "`0600` minus whatever the `umask` also took", and
/// unlike the `chmod` this function exists to replace it can never widen
/// anything: `create_new` has just proved the file did not exist, so it is this
/// call's own file whose mode is being pinned.
///
/// Returns the open handle rather than taking the bytes: a caller that writes a
/// secret wants `sync_all` before it reports success, and where that goes is
/// the caller's story.
///
/// Non-Unix targets get the exclusive create without the mode, and rely on the
/// platform's own inheritance the same way [`ensure_owner_only`] does.
pub fn create_new_owner_only(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let file = options.open(path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }

    Ok(file)
}

/// [`create_new_owner_only`] for a caller already inside the runtime.
///
/// Offloaded for the same reason as the directory twin: the whole point of that
/// helper is that the mode travels with the `open(2)`, and `tokio::fs` has no
/// spelling that carries it. One `open` is all that moves off the reactor, and
/// the handle comes back as a `tokio::fs::File` so the caller's writes stay
/// async.
pub async fn create_new_owner_only_async(path: &Path) -> std::io::Result<tokio::fs::File> {
    let owned = path.to_path_buf();
    let file = tokio::task::spawn_blocking(move || create_new_owner_only(&owned))
        .await
        // A panicked or cancelled join is not a filesystem verdict, but the
        // caller's error channel is `io::Error` and losing the reason would
        // leave the file unexplained.
        .map_err(|error| std::io::Error::other(error.to_string()))??;
    Ok(tokio::fs::File::from_std(file))
}

/// Narrow a file *another program* created to owner-only.
///
/// This is the create-wide-then-chmod shape [`create_new_owner_only`] exists to
/// replace, and it is here for the one case that cannot use that helper: a file
/// this process asked something else to write. SQLite's `VACUUM INTO` opens the
/// snapshot itself — so it lands at the ambient `umask` — and refuses a path
/// that already exists, so there is no descriptor to hand it and no mode to
/// pass. What is still available is the *order*: the snapshot is written under
/// a temporary name, narrowed here, and only then renamed, so the name an
/// operator's tooling reads never exists at any mode but `0600`, and a run that
/// dies in between leaves a temp file the next rotation deletes rather than a
/// world-readable copy of the database.
///
/// For a file this process opens, the mode belongs on the open. Reaching for
/// this instead is the defect `scripts/secret-file-mode-contract-check.mjs`
/// sweeps for.
pub async fn restrict_to_owner_async(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, fs::Permissions::from_mode(0o600)).await
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// [`create_dir_all_owner_only`] for a caller already inside the runtime.
///
/// A background loop re-creates its state directory on every run — it can be
/// removed or unmounted while the server is up — and that call must not be the
/// one place where the mode goes back to the ambient `umask`.
///
/// Offloaded rather than rewritten against `tokio::fs`: which levels the call
/// created is the subtle half of the sync version, and a second copy of that
/// walk is a second thing to keep true. A `mkdir` plus one `chmod` per level is
/// what is being moved off the reactor, so the hop costs more than the work —
/// it buys a single definition instead.
pub async fn create_dir_all_owner_only_async(path: &Path) -> std::io::Result<()> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || create_dir_all_owner_only(&owned))
        .await
        // A panicked or cancelled join is not a filesystem verdict, but the
        // caller's error channel is `io::Error` and losing the reason would
        // leave the directory unexplained.
        .map_err(|error| std::io::Error::other(error.to_string()))?
}

/// Report the outcome of a best-effort cleanup without failing on it.
///
/// Cleanup of a temporary artifact runs on both the success and the error path,
/// and on the error path it must never replace the failure the caller is about
/// to return — so it cannot use `?`. Discarding the result outright is the
/// other extreme, and the one this project kept reaching for: an orphaned
/// staging file or working tree keeps its bytes until somebody notices the
/// volume is full, with nothing in the log connecting the two.
///
/// An already-absent path is the normal outcome of a cleanup that ran twice (an
/// error path that unwinds through a second `discard_*`), so `NotFound` stays
/// silent.
fn report_discard(what: &str, path: &Path, outcome: std::io::Result<()>) {
    match outcome {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            path = %path.display(),
            %error,
            "failed to remove {what}; it stays on disk until an operator removes it"
        ),
    }
}

/// Best-effort removal of a temporary file the caller is done with.
///
/// `what` names the thing in operator terms — `"attachment staging file"`,
/// `"uncompressed LFS object"` — because the path alone is a UUID nothing else
/// records. See [`report_discard`] for why the failure is warned rather than
/// returned or swallowed.
pub fn discard_file(what: &str, path: &Path) {
    report_discard(what, path, fs::remove_file(path));
}

/// Best-effort removal of a temporary directory tree the caller is done with.
///
/// The directory counterpart of [`discard_file`]; same reporting contract.
pub fn discard_dir(what: &str, path: &Path) {
    report_discard(what, path, fs::remove_dir_all(path));
}

/// [`discard_file`] for call sites already inside an async context.
///
/// A blocking `remove_file` on a request-handling task stalls the whole runtime
/// thread, so async callers get the tokio variant rather than the sync one.
pub async fn discard_file_async(what: &str, path: &Path) {
    report_discard(what, path, tokio::fs::remove_file(path).await);
}

/// [`discard_dir`] for call sites already inside an async context.
pub async fn discard_dir_async(what: &str, path: &Path) {
    report_discard(what, path, tokio::fs::remove_dir_all(path).await);
}

/// The `umask` guard the mode assertions across this crate share.
///
/// Lives beside the code rather than inside one `mod tests` because three
/// modules need it — this one, `backup`, and `audit::archiver` — and the lock
/// it holds only serialises anything if there is exactly one of it.
#[cfg(all(test, unix))]
pub(crate) mod test_umask {
    /// Serialises the tests that need a known `umask`. The value is
    /// process-wide, so two of them running at once would each see the other's,
    /// and every other test in this binary would create files through whichever
    /// one happened to be installed.
    pub(crate) static UMASK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Holds the process `umask` at a deliberately permissive value, and puts
    /// back what was there when it drops.
    ///
    /// Without this a mode assertion is only as strong as the `umask` of
    /// whoever ran `cargo test`: on a `umask 0077` developer box a plain
    /// `create_dir_all` already produces `0700`, and the test would pass
    /// against the very bug it exists to catch.
    pub(crate) struct WideUmask {
        previous: libc::mode_t,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl WideUmask {
        /// `0o002` is the shape this was found in — a group-writable stock host
        /// where `create_dir_all` lands on `0775`.
        pub(crate) fn hold() -> Self {
            let lock = UMASK_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // SAFETY: `umask` is an always-succeeding libc call that swaps a
            // value in the calling process's own credentials and touches no
            // memory. It is process-wide, which is what `UMASK_LOCK` serialises.
            let previous = unsafe { libc::umask(0o002) };
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for WideUmask {
        fn drop(&mut self) {
            // SAFETY: as above — restoring the value this guard displaced.
            unsafe { libc::umask(self.previous) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use super::test_umask::WideUmask;

    #[cfg(unix)]
    #[test]
    fn ownership_hint_names_both_sides_of_the_uid_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let hint = super::ownership_hint(dir.path()).expect("unix hint");

        assert!(hint.contains("this process runs as uid="), "{hint}");
        assert!(hint.contains("is owned by uid="), "{hint}");
        assert!(hint.contains(&dir.path().display().to_string()), "{hint}");
    }

    /// A directory the process already owns cannot be fixed by `chown`, and
    /// telling an operator to chown to the uid they are is how a diagnostic
    /// loses its credibility.
    #[cfg(unix)]
    #[test]
    fn ownership_hint_blames_the_mode_when_the_owner_already_matches() {
        let dir = tempfile::tempdir().unwrap();
        let hint = super::ownership_hint(dir.path()).expect("unix hint");

        assert!(hint.contains("chmod -R u+rwX"), "{hint}");
        assert!(!hint.contains("chown"), "{hint}");
    }

    /// The blocking directory is the one to `chown`, so a path that does not
    /// exist yet must report its nearest existing ancestor rather than nothing.
    #[cfg(unix)]
    #[test]
    fn ownership_hint_falls_back_to_the_nearest_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("repos").join("deep");

        let hint = super::ownership_hint(&missing).expect("unix hint");

        assert!(hint.contains(&dir.path().display().to_string()), "{hint}");
        assert!(!hint.contains("deep"), "{hint}");
    }

    #[test]
    fn describe_path_error_appends_the_caller_remedy_for_non_permission_errors() {
        let error = std::io::Error::new(std::io::ErrorKind::IsADirectory, "Is a directory");
        let message = super::describe_path_error(
            "config file",
            Path::new("/app/forgekeep.toml"),
            &error,
            "create the file before starting the container",
        );

        assert!(message.contains("/app/forgekeep.toml"), "{message}");
        assert!(message.contains("Is a directory"), "{message}");
        assert!(
            message.contains("create the file before starting the container"),
            "{message}"
        );
    }

    /// A real `std::fs` failure is the interesting input: the io error alone
    /// knows the errno and nothing else, so `path_error` is the only thing
    /// standing between the operator and an unqualified `os error 20`.
    #[test]
    fn path_error_names_the_path_the_io_error_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("occupied");
        fs::write(&not_a_dir, b"file").unwrap();
        let target = not_a_dir.join("alice");

        let error = fs::create_dir_all(&target).expect_err("a file cannot host a subdirectory");
        let rendered =
            super::path_error("repo owner directory", &target, &error, REPO_ROOT_HINT).to_string();

        assert!(
            rendered.contains(&target.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("[server].repo_root"), "{rendered}");
    }

    /// Sink that keeps every formatted log line so a test can assert on what
    /// the operator would actually have seen. A best-effort cleanup returns
    /// nothing — the log line *is* its whole interface.
    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogs {
        type Writer = CapturedLogs;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    impl CapturedLogs {
        fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
            let logs = Self::default();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_max_level(tracing::Level::WARN)
                .with_ansi(false)
                .finish();
            let guard = tracing::subscriber::set_default(subscriber);
            (logs, guard)
        }

        fn rendered(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    /// The warning is the whole interface — nothing is returned and the server
    /// starts either way — so it has to carry the three things an operator
    /// needs to act: which directory, what is wrong with it, and the command
    /// that fixes it. A line that says only "permissions" sends them towards
    /// `chmod 755`, which is the direction that caused this.
    #[cfg(unix)]
    #[test]
    fn a_group_readable_state_directory_is_named_with_its_mode_and_its_chmod() {
        use std::os::unix::fs::PermissionsExt;

        let (logs, _guard) = CapturedLogs::capture();
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        super::warn_if_others_can_reach("repo_root", dir.path());

        let rendered = logs.rendered();
        assert!(rendered.contains("repo_root"), "{rendered}");
        assert!(
            rendered.contains(&dir.path().display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("0755"), "{rendered}");
        assert!(
            rendered.contains(&format!("chmod 700 {}", dir.path().display())),
            "{rendered}"
        );
    }

    /// A group bit alone is enough — the usual shape of this is `0750` from a
    /// `umask 027` host, where nothing is world-readable and the directory is
    /// still shared with every account in the group.
    #[cfg(unix)]
    #[test]
    fn a_group_only_bit_still_warns() {
        use std::os::unix::fs::PermissionsExt;

        let (logs, _guard) = CapturedLogs::capture();
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o750)).unwrap();

        super::warn_if_others_can_reach("backup dir", dir.path());

        assert!(logs.rendered().contains("0750"), "{}", logs.rendered());
    }

    /// A correctly deployed instance runs this on every start of every state
    /// directory it has. One line per boot per directory about nothing is how
    /// the log stops being read, and with it the warning above.
    #[cfg(unix)]
    #[test]
    fn an_owner_only_state_directory_stays_silent() {
        use std::os::unix::fs::PermissionsExt;

        let (logs, _guard) = CapturedLogs::capture();
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();

        super::warn_if_others_can_reach("audit archive_dir", dir.path());

        assert_eq!(
            logs.rendered(),
            "",
            "an owner-only directory is the correct state, not a warning"
        );
    }

    /// The caller has just created the directory and reported its own failure
    /// if that did not work. A second complaint here would only add a line
    /// about a path that does not exist to an error that already explains why.
    #[test]
    fn an_absent_directory_produces_no_second_complaint() {
        let (logs, _guard) = CapturedLogs::capture();
        let dir = tempfile::tempdir().unwrap();

        super::warn_if_others_can_reach("repo_root", &dir.path().join("never-created"));

        assert_eq!(logs.rendered(), "");
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    /// The directory the server makes for its own state must not depend on the
    /// `umask` of whoever started the server. `0700` exactly, and on *every*
    /// level the call created: `[audit].archive_dir` and `[backup].dir` default
    /// to siblings of `repo_root` rather than children, so the intermediate
    /// `data/` is created here too and is just as much a way in.
    #[cfg(unix)]
    #[test]
    fn a_state_directory_the_server_creates_is_owner_only() {
        let _umask = WideUmask::hold();
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("data").join("audit-archive");

        super::create_dir_all_owner_only(&archive).unwrap();

        for level in [archive.as_path(), archive.parent().unwrap()] {
            assert_eq!(
                mode_of(level),
                0o700,
                "{} was created {:04o}, so every other local account on the host can read it",
                level.display(),
                mode_of(level)
            );
        }
    }

    /// A directory that was already there belongs to whoever made it. Narrowing
    /// it could cut off a backup job that reaches it by group — which is why
    /// [`warn_if_others_can_reach`] states the problem instead of fixing it —
    /// and that warning has to keep firing on exactly this case.
    #[cfg(unix)]
    #[test]
    fn an_existing_directory_keeps_the_mode_its_operator_chose() {
        use std::os::unix::fs::PermissionsExt;

        let _umask = WideUmask::hold();
        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("backups");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o775)).unwrap();

        super::create_dir_all_owner_only(&shared).unwrap();

        assert_eq!(
            mode_of(&shared),
            0o775,
            "an operator's directory must not be narrowed underneath them"
        );

        let (logs, _guard) = CapturedLogs::capture();
        super::warn_if_others_can_reach("backup dir", &shared);
        assert!(
            logs.rendered().contains("0775"),
            "the directory this call left alone must still be reported: {}",
            logs.rendered()
        );
    }

    /// The mixed case, and the one a "narrow everything" implementation would
    /// get wrong in the dangerous direction and a "narrow nothing" one in the
    /// useless direction: the operator's level keeps its mode, the level this
    /// call brought into being does not inherit it.
    #[cfg(unix)]
    #[test]
    fn only_the_levels_this_call_created_are_narrowed() {
        use std::os::unix::fs::PermissionsExt;

        let _umask = WideUmask::hold();
        let root = tempfile::tempdir().unwrap();
        let operators = root.path().join("data");
        fs::create_dir(&operators).unwrap();
        fs::set_permissions(&operators, fs::Permissions::from_mode(0o755)).unwrap();
        let ours = operators.join("audit-archive");

        super::create_dir_all_owner_only(&ours).unwrap();

        assert_eq!(mode_of(&operators), 0o755, "the existing level moved");
        assert_eq!(mode_of(&ours), 0o700, "the created level took the umask");
    }

    /// A key is born owner-only or it is not owner-only at all. `0o002` here is
    /// the stock host that made the directory half of this module necessary;
    /// under it a `std::fs::write` lands on `0644`, and the `chmod` that would
    /// follow is exactly the step a crash gets to skip.
    #[cfg(unix)]
    #[test]
    fn a_secret_file_the_server_creates_is_owner_only() {
        use std::io::Write;

        let _umask = WideUmask::hold();
        let root = tempfile::tempdir().unwrap();
        let key = root.path().join("ssh_host_key");

        let mut file = super::create_new_owner_only(&key).unwrap();
        file.write_all(b"PRIVATE KEY").unwrap();
        file.sync_all().unwrap();
        drop(file);

        assert_eq!(
            mode_of(&key),
            0o600,
            "{} was created {:04o}, so every other local account on the host can read the key",
            key.display(),
            mode_of(&key)
        );
        assert_eq!(fs::read(&key).unwrap(), b"PRIVATE KEY");
    }

    /// The other half of the signature. Two first starts of the same instance
    /// race here: both find no key, both generate one, and with a plain write
    /// the loser's key silently replaces the winner's — after the winner has
    /// already advertised its fingerprint. The exclusion has to be the creation
    /// itself, so the loser is told and can read back what is there.
    #[test]
    fn an_existing_secret_file_is_refused_rather_than_replaced() {
        let root = tempfile::tempdir().unwrap();
        let key = root.path().join("ssh_host_key");
        fs::write(&key, b"the key another start generated").unwrap();

        let error = super::create_new_owner_only(&key).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(&key).unwrap(),
            b"the key another start generated",
            "the existing secret must survive the call that refused to make a new one"
        );
    }

    /// `[server].repo_root` defaults to the relative `./repos` on bare metal,
    /// whose ancestor walk ends at `""` — a path that exists as nothing. A
    /// `chmod` of it would turn a directory that was created perfectly well
    /// into an `ENOENT` the operator cannot act on.
    #[cfg(unix)]
    #[test]
    fn a_relative_path_does_not_walk_past_its_own_root() {
        let levels = super::levels_to_create(Path::new("forgekeep-no-such-dir/repos"));

        assert_eq!(
            levels,
            vec![
                Path::new("forgekeep-no-such-dir/repos"),
                Path::new("forgekeep-no-such-dir"),
            ],
            "the empty ancestor must not be handed to set_permissions"
        );
    }

    /// The background archiver and the backup scheduler re-create their
    /// directory on every run — it can be removed or unmounted while the server
    /// is up — so the async path is the one that runs for the rest of the
    /// instance's life, and it must not be where the mode goes back to the
    /// ambient `umask`.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_async_twin_narrows_what_it_creates_too() {
        let _umask = WideUmask::hold();
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("audit-archive");

        super::create_dir_all_owner_only_async(&archive)
            .await
            .unwrap();

        assert_eq!(mode_of(&archive), 0o700);
    }

    #[test]
    fn discard_removes_the_file_and_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("staged.upload");
        fs::write(&file, b"staged").unwrap();
        let tree = dir.path().join("worktree");
        fs::create_dir_all(tree.join("nested")).unwrap();

        super::discard_file("attachment staging file", &file);
        super::discard_dir("file-edit working tree", &tree);

        assert!(!file.exists(), "the staging file survived the discard");
        assert!(!tree.exists(), "the working tree survived the discard");
    }

    /// An error path that unwinds through a second cleanup discards an already
    /// absent path, and a warning per unwind is how a log stops being read.
    #[test]
    fn discard_of_an_absent_path_stays_silent() {
        let (logs, _guard) = CapturedLogs::capture();
        let dir = tempfile::tempdir().unwrap();

        super::discard_file("attachment staging file", &dir.path().join("gone"));
        super::discard_dir("file-edit working tree", &dir.path().join("gone-too"));

        assert_eq!(
            logs.rendered(),
            "",
            "an already-absent path is the normal outcome, not a warning"
        );
    }

    /// The whole point of the helper: a cleanup that fails must leave the path
    /// and the operator-facing name of the orphan in the log, because nothing
    /// downstream records either — the temp names are UUIDs.
    #[test]
    fn discard_failure_names_what_was_orphaned_and_where() {
        let (logs, _guard) = CapturedLogs::capture();
        let dir = tempfile::tempdir().unwrap();
        // A regular file is not a directory tree (ENOTDIR) and a directory is
        // not an unlinkable file (EISDIR/EPERM) — two non-`NotFound` failures
        // that need no permission games to reproduce, so they hold under root.
        let file = dir.path().join("staged.upload");
        fs::write(&file, b"staged").unwrap();
        let tree = dir.path().join("worktree");
        fs::create_dir_all(&tree).unwrap();

        super::discard_dir("file-edit working tree", &file);
        super::discard_file("attachment staging file", &tree);

        let rendered = logs.rendered();
        assert!(
            rendered.contains("failed to remove file-edit working tree"),
            "the orphaned tree is unnamed: {rendered}"
        );
        assert!(
            rendered.contains(&file.display().to_string()),
            "the orphaned tree's path is missing: {rendered}"
        );
        assert!(
            rendered.contains("failed to remove attachment staging file"),
            "the orphaned staging file is unnamed: {rendered}"
        );
        assert!(
            rendered.contains(&tree.display().to_string()),
            "the orphaned staging file's path is missing: {rendered}"
        );
    }

    /// The async variants exist so a request-handling task does not block the
    /// runtime thread on `remove_file`; they must otherwise report identically.
    #[tokio::test]
    async fn async_discard_reports_the_same_failure_as_the_sync_one() {
        let (logs, _guard) = CapturedLogs::capture();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("staged.upload");
        fs::write(&file, b"staged").unwrap();

        let tree = dir.path().join("upload");
        fs::create_dir_all(&tree).unwrap();
        super::discard_dir_async("OCI upload directory", &tree).await;
        assert!(!tree.exists(), "the upload directory survived the discard");

        super::discard_file_async("attachment staging file", &file).await;
        assert!(!file.exists(), "the staging file survived the discard");
        super::discard_file_async("attachment staging file", &file).await;
        super::discard_dir_async("OCI upload directory", &tree).await;
        assert_eq!(
            logs.rendered(),
            "",
            "a repeated discard of an absent path must stay silent"
        );

        // ENOTDIR: a regular file is not a tree, and the tokio backend must
        // surface that the same way the sync one does.
        fs::write(&file, b"staged").unwrap();
        super::discard_dir_async("OCI upload directory", &file).await;
        let rendered = logs.rendered();
        assert!(
            rendered.contains("failed to remove OCI upload directory"),
            "the async failure went unreported: {rendered}"
        );
        assert!(
            rendered.contains(&file.display().to_string()),
            "the async failure lost the path: {rendered}"
        );
    }
}
