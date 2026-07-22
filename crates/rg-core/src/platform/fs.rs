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

    #[test]
    fn test_normalize_path() {
        let path = Path::new("/tmp/./test/../other");
        let normalized = normalize_path(path);

        #[cfg(unix)]
        assert_eq!(normalized, PathBuf::from("/tmp/other"));
    }
}
