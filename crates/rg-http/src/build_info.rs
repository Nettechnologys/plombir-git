//! Which source this binary was built from — the half of the AGPL §13 offer
//! that only the build can know.
//!
//! The other half, *where* that source lives, is the operator's
//! `[server].source_url`. A fork that changes the code changes that one URL and
//! its users are pointed at the fork, at the very commit they are running.
//!
//! The commit is recorded at compile time from `FORGEKEEP_SOURCE_COMMIT`. It is
//! deliberately not read from `.git` by a build script: the image build copies
//! `crates/` alone, so a build script would find no repository exactly where
//! the production binary is made, and would quietly print nothing. The
//! `Dockerfile` takes the commit as a build argument instead. `option_env!`
//! puts the variable in rustc's dep-info, so changing it rebuilds this crate.

/// The raw compile-time value, before validation.
const RAW_SOURCE_COMMIT: Option<&str> = option_env!("FORGEKEEP_SOURCE_COMMIT");

/// What the build recorded about its own source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceCommit {
    /// A full object id, safe to put into a link.
    Known(&'static str),
    /// The build was not told which commit it came from (the variable was
    /// unset or empty, as with a plain `cargo build`).
    Unrecorded,
    /// The variable was set to something that is not a full object id — a
    /// short SHA, `unknown`, a branch name. Linking it would claim a commit the
    /// operator cannot vouch for, so it is reported and then ignored.
    Malformed(&'static str),
}

impl SourceCommit {
    /// The commit to link to, if the build recorded a usable one.
    pub fn known(self) -> Option<&'static str> {
        match self {
            Self::Known(commit) => Some(commit),
            Self::Unrecorded | Self::Malformed(_) => None,
        }
    }
}

/// The commit this binary was built from, as recorded by the build.
pub fn source_commit() -> SourceCommit {
    classify(RAW_SOURCE_COMMIT)
}

fn classify(raw: Option<&'static str>) -> SourceCommit {
    match raw.map(str::trim) {
        None | Some("") => SourceCommit::Unrecorded,
        Some(value) if is_full_object_id(value) => SourceCommit::Known(value),
        Some(value) => SourceCommit::Malformed(value),
    }
}

/// A SHA-1 (40) or SHA-256 (64) object id in the lowercase form `git rev-parse`
/// prints.
fn is_full_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The link a user follows to read the source of this build.
///
/// `source_url` is the repository, already validated and without a trailing
/// slash. With a known commit the link is `<source_url>/tree/<commit>` — the
/// layout of GitHub, GitLab and ForgeKeep itself. Without one it is the
/// repository: still the right project, but no claim about which commit.
pub fn source_link(source_url: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("{source_url}/tree/{commit}"),
        None => source_url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA1: &str = "87bd02a3c1f0e9d8b7a6c5d4e3f2a1b0c9d8e7f6";

    #[test]
    fn a_full_object_id_is_the_commit_the_link_names() {
        assert_eq!(classify(Some(SHA1)), SourceCommit::Known(SHA1));
        let sha256 = "a".repeat(64).leak() as &'static str;
        assert_eq!(classify(Some(sha256)), SourceCommit::Known(sha256));
        assert_eq!(
            source_link("https://example.com/fork", classify(Some(SHA1)).known()),
            format!("https://example.com/fork/tree/{SHA1}")
        );
    }

    /// Compose passes `${FORGEKEEP_SOURCE_COMMIT:-}`, so an operator who did not
    /// set it builds with an *empty* value, not an absent one. Both mean the
    /// same thing and neither is worth a warning.
    #[test]
    fn unset_and_empty_are_both_unrecorded() {
        assert_eq!(classify(None), SourceCommit::Unrecorded);
        assert_eq!(classify(Some("")), SourceCommit::Unrecorded);
        assert_eq!(classify(Some("  ")), SourceCommit::Unrecorded);
    }

    /// A value that only looks like a commit must not become a link that
    /// claims one. `git rev-parse --short HEAD` and a branch name are the two
    /// likely slips.
    #[test]
    fn anything_but_a_full_lowercase_object_id_is_malformed() {
        for raw in ["87bd02a", "main", "unknown", &SHA1.to_uppercase()] {
            let raw: &'static str = raw.to_string().leak();
            assert_eq!(classify(Some(raw)), SourceCommit::Malformed(raw), "{raw}");
            assert_eq!(classify(Some(raw)).known(), None, "{raw}");
        }
    }

    #[test]
    fn without_a_commit_the_link_is_the_repository_itself() {
        assert_eq!(
            source_link("https://example.com/fork", None),
            "https://example.com/fork"
        );
    }
}
