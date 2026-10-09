//! The traversal check every git entry point runs on a path component.
//!
//! Was "cross-platform path handling utilities" and had four more helpers —
//! `temp_dir`, `repo_path`, `expand_home`, `to_platform_string` — none of which
//! anything called; the repository root comes from configuration, not from a
//! guessed home directory. See [`super`] for the decision.

use anyhow::Result;

/// Longest file name an upload may carry, in bytes — the `NAME_MAX` of every
/// filesystem the server is deployed on.
pub const MAX_UPLOAD_FILENAME_LEN: usize = 255;

/// Whether `name` is exactly one ordinary path component — a file name, not a
/// path.
///
/// This is the shape every stored upload name is later joined onto a
/// server-owned directory in, and `Path::join` offers no safety of its own:
/// an absolute name replaces the directory outright and a `..` component is
/// kept, not normalised. The check is spelled through `Path::components` so
/// that `.`, `..` and the empty string are refused by the same rule that
/// refuses a separator, and the component is compared back to the whole name
/// so nothing the parser dropped (a trailing `/`, a redundant `.`) slips by.
pub fn is_single_path_component(name: &str) -> bool {
    let mut components = std::path::Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(component)), None) if component == name
    )
}

/// Reject an uploaded file name before it is stored or joined onto a path.
///
/// Release assets and attachments both keep the uploader's name and both
/// build a path from it on the way back out (a blob key, and for release
/// assets the pre-migration `<id>/<name>` layout under `repo_root`). A name
/// that is a path therefore has to be refused at the door, in one place, by a
/// rule the two cannot drift apart on. `what` names the upload kind in the
/// message, which is fixed text that reaches the client verbatim (H-05):
/// echoing the rejected name back would put `/etc/…` into a response body.
///
/// Every rejection here is the uploader's to fix, so each one is typed as
/// [`crate::error::InvalidRequest`] — a real `400`, never a 5xx.
pub fn validate_upload_filename(what: &str, filename: &str) -> Result<()> {
    if filename.is_empty()
        || filename.len() > MAX_UPLOAD_FILENAME_LEN
        || filename.chars().any(char::is_control)
    {
        return Err(crate::error::invalid_request(format!(
            "invalid {what} filename"
        )));
    }
    // `\` is not a separator on the server, but it is on the client that
    // uploaded the file, and a name the two sides read differently is not a
    // name worth storing.
    if filename.contains('/') || filename.contains('\\') || !is_single_path_component(filename) {
        return Err(crate::error::invalid_request(format!(
            "{what} filename must not contain a path"
        )));
    }
    Ok(())
}

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

    /// Every name a legitimate client sends has to keep working: non-ASCII,
    /// spaces, the quoting characters `content_disposition` neutralises on the
    /// way out, and a `;` inside a quoted plain `filename`.
    #[test]
    fn upload_filenames_that_are_plain_names_are_accepted() {
        for name in [
            "payload.bin",
            "пакет-1.0.tgz",
            "my file (final).tar.gz",
            "a\";filename=\"evil.exe",
            "release;notes.txt",
            "..hidden-but-a-name",
            "a..b",
            ".dotfile",
            &"x".repeat(MAX_UPLOAD_FILENAME_LEN),
        ] {
            assert!(
                validate_upload_filename("release asset", name).is_ok(),
                "a plain file name must be accepted: {name:?}"
            );
            assert!(is_single_path_component(name), "{name:?}");
        }
    }

    /// The shapes `Path::join` turns into a different directory, plus the
    /// ones no filesystem can hold.
    #[test]
    fn upload_filenames_that_are_paths_are_refused_as_the_client_s_mistake() {
        let overlong = "x".repeat(MAX_UPLOAD_FILENAME_LEN + 1);
        for name in [
            "",
            "/etc/passwd",
            "/data/encryption_key",
            "../../x",
            "..",
            ".",
            "a/b",
            "a\\b",
            "..\\..\\x",
            "a\0b",
            "a\nb",
            "trailing/",
            overlong.as_str(),
        ] {
            let error = validate_upload_filename("release asset", name)
                .expect_err(&format!("a path must be refused: {name:?}"));
            assert!(
                error
                    .downcast_ref::<crate::error::InvalidRequest>()
                    .is_some(),
                "the rejection must be typed as the client's mistake: {name:?} -> {error:#}"
            );
            let message = error.to_string();
            assert!(
                !message.contains("etc") && !message.contains(".."),
                "the message must not echo the rejected name back: {message}"
            );
        }
    }

    /// The component check on its own — the predicate the legacy release
    /// asset lookup re-runs on a stored name.
    #[test]
    fn a_single_path_component_is_a_name_and_nothing_else() {
        for path in [
            "",
            "/etc/passwd",
            "../../x",
            "..",
            ".",
            "a/b",
            "trailing/",
            "./a",
        ] {
            assert!(!is_single_path_component(path), "{path:?}");
        }
    }

    #[test]
    fn the_message_names_the_upload_kind() {
        assert_eq!(
            validate_upload_filename("attachment", "a/b")
                .unwrap_err()
                .to_string(),
            "attachment filename must not contain a path"
        );
        assert_eq!(
            validate_upload_filename("attachment", "")
                .unwrap_err()
                .to_string(),
            "invalid attachment filename"
        );
    }
}
