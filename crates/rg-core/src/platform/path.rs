//! The traversal check every git entry point runs on a path component.
//!
//! Was "cross-platform path handling utilities" and had four more helpers —
//! `temp_dir`, `repo_path`, `expand_home`, `to_platform_string` — none of which
//! anything called; the repository root comes from configuration, not from a
//! guessed home directory. See [`super`] for the decision.

use anyhow::Result;

/// Validate a repository path component to prevent path traversal attacks (H-02).
///
/// Rejects strings that contain:
/// - `..` (parent directory traversal)
/// - `//` (double-slash traversal)
/// - Leading `/` (Unix absolute path injection)
/// - Leading `\` (Windows absolute path injection)
pub fn validate_repo_path(path: &str) -> Result<()> {
    if path.contains("..") {
        anyhow::bail!("repository path contains '..' (path traversal)");
    }
    if path.contains("//") {
        anyhow::bail!("repository path contains '//' (path traversal)");
    }
    if path.starts_with('/') {
        anyhow::bail!("repository path starts with '/' (absolute path)");
    }
    if path.starts_with('\\') {
        anyhow::bail!("repository path starts with '\\' (absolute path)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_repo_path_rejects_dotdot() {
        assert!(validate_repo_path("../etc").is_err());
        assert!(validate_repo_path("foo/../bar").is_err());
    }

    #[test]
    fn test_validate_repo_path_rejects_absolute() {
        assert!(validate_repo_path("/etc/passwd").is_err());
    }

    #[test]
    fn test_validate_repo_path_rejects_double_slash() {
        assert!(validate_repo_path("foo//bar").is_err());
    }

    #[test]
    fn test_validate_repo_path_accepts_normal() {
        assert!(validate_repo_path("owner/repo").is_ok());
        assert!(validate_repo_path("owner/repo.git").is_ok());
        assert!(validate_repo_path("owner").is_ok());
    }
}
