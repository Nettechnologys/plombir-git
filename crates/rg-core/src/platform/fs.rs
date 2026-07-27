//! Cross-platform file system utilities.

use std::fs;
use std::path::Path;

/// Set file permissions to executable (cross-platform).
///
/// # Unix
/// Sets `chmod +x` (mode 0o755).
///
/// # Windows
/// On Windows, files are generally executable based on their extension
/// (.exe, .bat, .cmd, .ps1), so this is a no-op.
///
/// # Errors
/// Returns IoError if permissions cannot be set (Unix) or if the path
/// cannot be accessed.
pub fn set_executable<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = fs::metadata(&path)?;
        let mut perms = metadata.permissions();
        perms.set_mode(0o755); // rwxr-xr-x
        fs::set_permissions(&path, perms)?;
    }

    #[cfg(windows)]
    {
        // Windows: executability is determined by file extension
        // .exe, .bat, .cmd, .ps1 are executable
        // No need to set permissions explicitly
        // Just verify the file exists
        if !path.as_ref().exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "File not found",
            ));
        }
    }

    Ok(())
}

/// Get file permissions (cross-platform).
///
/// # Unix
/// Returns the mode bits (e.g., 0o755).
///
/// # Windows
/// Returns a placeholder value (0o644) since Windows doesn't have
/// Unix-style permissions.
pub fn get_permissions<P: AsRef<Path>>(path: P) -> std::io::Result<u32> {
    let metadata = fs::metadata(&path)?;
    let perms = metadata.permissions();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(perms.mode())
    }

    #[cfg(windows)]
    {
        // Windows doesn't have Unix-style permissions
        // Return a placeholder value
        Ok(0o644)
    }
}

/// Check if a file is executable (cross-platform).
///
/// # Unix
/// Checks if the file has the executable bit set.
///
/// # Windows
/// Checks if the file has an executable extension (.exe, .bat, .cmd, .ps1).
pub fn is_executable<P: AsRef<Path>>(path: P) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Ok(metadata) = fs::metadata(&path) {
            let perms = metadata.permissions();
            let mode = perms.mode();
            // Check owner, group, or other execute bit
            (mode & 0o111) != 0
        } else {
            false
        }
    }

    #[cfg(windows)]
    {
        if let Some(ext) = path.as_ref().extension() {
            let ext_str = ext.to_string_lossy().to_lowercase();
            matches!(
                ext_str.as_str(),
                "exe" | "bat" | "cmd" | "ps1" | "vbs" | "js"
            )
        } else {
            false
        }
    }
}

/// Create a directory with executable permissions (cross-platform).
///
/// # Unix
/// Creates directory with mode 0o755.
///
/// # Windows
/// Creates directory normally (Windows doesn't have executable bit for dirs).
pub fn create_dir_executable<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
    fs::create_dir_all(&path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms)?;
    }

    Ok(())
}

/// Copy a file preserving permissions (cross-platform).
///
/// # Unix
/// Preserves the executable bit.
///
/// # Windows
/// Copies the file normally.
pub fn copy_preserve_permissions<P: AsRef<Path>, Q: AsRef<Path>>(
    from: P,
    to: Q,
) -> std::io::Result<u64> {
    fs::copy(&from, &to)?;

    #[cfg(unix)]
    {
        if let Ok(metadata) = fs::metadata(&from) {
            let perms = metadata.permissions();
            fs::set_permissions(&to, perms)?;
        }
    }

    Ok(fs::metadata(&from)?.len())
}

/// Get the platform-specific executable extension.
///
/// # Returns
/// - Unix: `""` (empty string)
/// - Windows: `".exe"`
pub fn executable_extension() -> &'static str {
    #[cfg(unix)]
    {
        ""
    }

    #[cfg(windows)]
    {
        ".exe"
    }
}

/// Check if a path is absolute (cross-platform).
pub fn is_absolute(path: &Path) -> bool {
    path.is_absolute()
}

/// Normalize a path (cross-platform).
///
/// Converts all separators to the platform default,
/// removes `.` and `..` where possible.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            std::path::Component::CurDir => {
                // Skip `.`
                continue;
            }
            std::path::Component::ParentDir => {
                // Pop the last component if possible
                if !components.is_empty() {
                    components.pop();
                }
            }
            _ => {
                components.push(component.as_os_str().to_os_string());
            }
        }
    }

    let mut result = PathBuf::new();
    for component in components {
        result.push(component);
    }

    result
}

use std::path::PathBuf;

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

/// Remediation for a filesystem failure on a temporary git working tree.
///
/// Creating a repository, editing a file from the web UI and committing a batch
/// of files all stage a working tree in the system temp directory. A container
/// started with `read_only: true` has no writable `/tmp`, and the resulting
/// error names neither the directory nor the variable that moves it.
pub const TEMP_DIR_HINT: &str =
    "the server stages git working trees in the system temporary directory; point `TMPDIR` at a \
     writable directory (a container started with `read_only: true` has no writable `/tmp` \
     unless a tmpfs is mounted there)";

/// [`describe_path_error`] as a ready-to-propagate [`anyhow::Error`].
///
/// The `?` on a bare `std::fs` call discards the path — the io error only
/// carries the errno — so every caller that computes its path internally has to
/// re-attach it. This is that one line.
pub fn path_error(what: &str, path: &Path, error: &std::io::Error, remedy: &str) -> anyhow::Error {
    anyhow::anyhow!("{}", describe_path_error(what, path, error, remedy))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executable_extension() {
        let ext = executable_extension();
        #[cfg(unix)]
        assert_eq!(ext, "");

        #[cfg(windows)]
        assert_eq!(ext, ".exe");
    }

    #[test]
    fn test_is_absolute() {
        #[cfg(unix)]
        assert!(is_absolute(Path::new("/tmp/test")));

        #[cfg(windows)]
        assert!(is_absolute(Path::new("C:\\test")));
    }

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
        let rendered = super::path_error("repo owner directory", &target, &error, REPO_ROOT_HINT)
            .to_string();

        assert!(rendered.contains(&target.display().to_string()), "{rendered}");
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

    #[test]
    fn test_normalize_path() {
        let path = Path::new("/tmp/./test/../other");
        let normalized = normalize_path(path);

        #[cfg(unix)]
        assert_eq!(normalized, PathBuf::from("/tmp/other"));
    }
}
