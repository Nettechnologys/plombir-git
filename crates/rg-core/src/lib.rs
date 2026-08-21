//! ForgeKeep core business logic.
//!
//! Handles users, repositories, authentication, access control,
//! issues, pull requests, wiki, LFS, webhooks, code reviews,
//! branch protection, collaborators, organizations, and notifications.
//!
//! ## Module Groups (for future crate splits)
//!
//! - **Identity**: auth, user, org
//! - **Collaboration**: repo, issue, pull_request, wiki, review, collaborator,
//!   label, board, time_tracking, branch_protection, webhook, notification
//! - **Delivery & CI**: ci, release, package_registry, mirror, import
//! - **Infrastructure**: search, lfs, email, audit, platform

// ── Identity & Auth ─────────────────────────────────
pub mod attachment;
pub mod attestation;
pub mod auth;
pub mod org;
pub mod user;

// ── Collaboration ───────────────────────────────────
pub mod board;
pub mod branch_protection;
pub mod collaborator;
pub mod issue;
pub mod issue_template;
pub mod label;
pub mod notification;
pub mod pull_request;
pub mod push_hooks; // Transport-neutral post-push hooks (HTTP + SSH both call these)
pub mod repo;
pub mod review;
pub mod time_tracking;
pub mod webhook;
pub mod wiki;

// ── Delivery & CI ───────────────────────────────────
pub mod ci;
pub mod import;
pub mod mirror;
pub mod package_registry;
pub mod release;

// ── Infrastructure ──────────────────────────────────
pub mod audit;
pub mod backup; // Scheduled SQLite snapshots, so "are there backups?" is a config answer
pub mod blob_storage;
pub mod email;
pub mod instance;
pub mod lfs;
pub mod namespace; // Which first path segments the application already answers for
pub mod net; // SSRF-hardened outbound HTTP for user-supplied URLs
pub mod platform;
pub mod search; // Cross-platform abstractions
pub mod task_tracker; // Drain-aware tracker for detached fire-and-forget delivery tasks

pub(crate) mod db_retry; // One retry policy for the bounded database-write loops
pub mod error; // Domain error types (CoreError)
pub mod metrics_hook; // Observer hooks so the HTTP layer can meter core-crate events

#[cfg(test)]
pub(crate) mod test_support; // Fixtures shared by unit tests of several services

use anyhow::Result;

/// The rule for a name that addresses the `/{owner}/…` namespace.
///
/// Deliberately a re-export rather than a second implementation. Usernames and
/// organisation names are two holders of *one* namespace — `/{owner}/{repo}`
/// resolves either — and this used to be a separate, looser copy of the rule:
/// up to 39 characters against 30, `char::is_alphanumeric` (so Cyrillic passed)
/// against ASCII, and no requirement to start with an alphanumeric character.
/// An organisation could therefore take a name no person could register, which
/// is a homograph waiting to happen in a namespace shared with usernames.
///
/// Every failure carries [`error::InvalidRequest`]: a handler that funnels the
/// result through `AppError::from` answers `400` with the rule that was broken
/// instead of blaming itself with a `500`.
pub use user::service::validate_username;

/// The suffix the git transport treats as decoration rather than as part of the
/// name.
///
/// Both transports strip exactly one of these off the last path segment —
/// `rg_http::git_http::strip_git_suffix` and `rg_ssh::parse_repo_owner_name` —
/// so that `owner/repo.git` and `owner/repo` reach the same bare repository,
/// which is what every git client expects.
const GIT_SUFFIX: &str = ".git";

/// Check if a repository name is valid.
///
/// Typed like [`validate_username`] — the request is what is wrong, and no
/// retry of it can succeed.
pub fn validate_repo_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(error::invalid_request("repository name cannot be empty"));
    }
    if name.len() > 100 {
        return Err(error::invalid_request(
            "repository name too long (max 100 characters)",
        ));
    }
    for c in name.chars() {
        if !c.is_alphanumeric() && c != '-' && c != '_' && c != '.' {
            return Err(error::invalid_request(format!(
                "repository name contains invalid character: {c}"
            )));
        }
    }
    // `.` and `..` are path segments with a meaning of their own. A repository
    // called either is addressed at `/{owner}/.`, which a browser and a git
    // client both resolve away before the request is sent — the row exists and
    // nothing can reach it. The dot itself stays legal: `my.project` is a name,
    // these two are not.
    if name == "." || name == ".." {
        return Err(error::invalid_request(
            "repository name cannot be '.' or '..' — a path segment of that name addresses a              directory rather than a repository",
        ));
    }
    // A name ending in `.git` is one the transport cannot address. Both git
    // transports strip the suffix, so `foo.git` is asked for as `foo`: with no
    // neighbour of that name the clone fails on a repository whose page opens
    // perfectly well, and *with* one it silently hands over the neighbour's
    // code. The second is the reason this is refused rather than documented —
    // a wrong result with nothing in the answer saying so.
    //
    // Refused whatever the case, because the rule an operator can hold in their
    // head is "a repository name may not end in `.git`". Keying it to the
    // lowercase spelling the strippers happen to use would make the answer
    // depend on knowing that `strip_suffix` is case-sensitive, which is not a
    // rule anybody can state.
    //
    // Only new names: `fork_repo` and `transfer_repo` carry an existing one, so
    // a repository created before this rule keeps working — with the ambiguity
    // it already had. `namespace::report_repositories_the_transport_cannot_address`
    // names those at boot instead of leaving them to be discovered by a bad
    // clone.
    if name.to_ascii_lowercase().ends_with(GIT_SUFFIX) {
        return Err(error::invalid_request(format!(
            "repository name cannot end in '{GIT_SUFFIX}': the git transport strips that suffix, \
             so '{name}' would be requested as '{}' and clone the wrong repository",
            &name[..name.len() - GIT_SUFFIX.len()]
        )));
    }
    Ok(())
}

#[cfg(test)]
mod repo_name_tests {
    use super::validate_repo_name;

    fn refusal(name: &str) -> String {
        format!(
            "{:#}",
            validate_repo_name(name).expect_err("this name must be refused")
        )
    }

    /// The defect: both git transports strip one `.git` off the last path
    /// segment, so a repository called `foo.git` is asked for as `foo`. With no
    /// `foo` next to it the clone fails on a repository whose page opens
    /// perfectly well; **with** one it hands over the neighbour's code and says
    /// nothing. `foo.git` is not a contrived name either — it is what
    /// `git clone --bare` produces, so a mirror or an import is the likely way
    /// it reaches the database.
    #[test]
    fn a_name_the_git_transport_would_strip_is_refused_by_name() {
        for name in ["foo.git", "foo.GIT", "foo.Git", ".git"] {
            let refusal = refusal(name);
            assert!(
                refusal.contains(".git") && refusal.contains(name),
                "the refusal must name the rule and what was typed: {refusal}"
            );
        }
    }

    /// The rule is the suffix, not the four letters anywhere in the name — a
    /// rule that took `git` away from everybody would be a worse trade than the
    /// ambiguity it prevents.
    #[test]
    fn a_name_that_merely_contains_git_is_still_a_name() {
        for name in ["git", "gitea", "foo.git.example", "my.git.hub", "dotgit"] {
            validate_repo_name(name)
                .unwrap_or_else(|error| panic!("`{name}` must stay valid: {error:#}"));
        }
    }

    /// `.` and `..` are path segments with a meaning of their own: a browser and
    /// a git client both resolve `/{owner}/..` away before the request is sent,
    /// so the row exists and nothing can reach it. The dot itself stays legal.
    #[test]
    fn the_two_names_that_are_directory_traversal_are_refused() {
        for name in [".", ".."] {
            assert!(
                refusal(name).contains("path segment"),
                "the refusal must say why a bare dot is not a name: {}",
                refusal(name)
            );
        }
        for name in ["...", "a.b", ".hidden"] {
            validate_repo_name(name)
                .unwrap_or_else(|error| panic!("`{name}` must stay valid: {error:#}"));
        }
    }
}
