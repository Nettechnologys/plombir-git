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

    #[test]
    fn test_normalize_path() {
        let path = Path::new("/tmp/./test/../other");
        let normalized = normalize_path(path);

        #[cfg(unix)]
        assert_eq!(normalized, PathBuf::from("/tmp/other"));
    }
}
