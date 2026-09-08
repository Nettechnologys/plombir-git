//! The configuration under which ForgeKeep runs the `git` binary.
//!
//! The subprocess twin of [`crate::repository::open`]. That one states the
//! permissions ForgeKeep opens a repository under in-process; this one states
//! the configuration ForgeKeep runs `git` under, so the two halves of the same
//! server answer the same way.
//!
//! Without it, `git` answers to the machine. `/etc/gitconfig` and the
//! `~/.gitconfig` of whichever account the server process runs under are read
//! by every invocation, and so is that process's own `GIT_*` environment. An
//! operator who set `apply.whitespace = fix` for their own convenience is then
//! quietly rewriting the *content* of other people's pull requests; one who set
//! `core.hooksPath` is running their own scripts inside every server-side
//! replay; one who set `commit.gpgsign` is turning every rebase merge into a
//! `500`. None of that is a setting the instance offered anybody — it is the
//! host steering what ForgeKeep does with somebody else's repository, and two
//! instances configured differently answer the same request differently
//! without either of them saying so. Measured on git 2.43.0, all three.
//!
//! Two mechanisms, and they answer different questions:
//!
//! * the **environment** is disarmed — `GIT_CONFIG_NOSYSTEM=1` drops
//!   `/etc/gitconfig`, `GIT_CONFIG_GLOBAL` pointed at nothing drops
//!   `~/.gitconfig`, an isolated `HOME` and `XDG_CONFIG_HOME` put the rest of
//!   the per-user files out of reach, and every inherited `GIT_*` variable is
//!   removed. That last one is not redundant: `GIT_CONFIG_COUNT` with its
//!   `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>` pairs injects configuration
//!   that `GIT_CONFIG_NOSYSTEM` does not suppress, and `GIT_DIR`,
//!   `GIT_WORK_TREE` and `GIT_INDEX_FILE` would redirect the operation itself.
//! * the settings ForgeKeep decides are stated on the command line, where they
//!   outrank every configuration file. `OWNED_SETTINGS` below is that list, and
//!   every entry there is git's own default — an unconfigured host therefore
//!   behaves exactly as it did before.
//!
//! On today's call site the environment is what closes the host: a temporary
//! rebase worktree carries no configuration but the one `git clone` just wrote.
//! The stated settings are what keeps the answer stated rather than defaulted,
//! so a command run inside a repository whose configuration ForgeKeep did not
//! write means the same thing — the placement the in-process half had to answer
//! with explicit merge options, because no isolation filters it out.
//!
//! This is the policy for repository-local operations ForgeKeep performs on its
//! own repositories. Talking to a remote the *user* named is a different
//! contract with a different threat model, and lives in
//! [`crate::credentials::credential_invocation`].

use std::ffi::{OsStr, OsString};
use std::path::Path;

use anyhow::Result;

use crate::cli_gateway::{GitCommandGateway, GitOutput};

/// A path which cannot contain user configuration or credential files.
///
/// `/dev/null` is stable, root-owned, and makes every attempted child path fail
/// closed with `ENOTDIR`, which a real empty directory under `/tmp` would not.
const DISARMED_HOME: &str = "/dev/null";

/// The git settings ForgeKeep states rather than inherits.
///
/// Every value here is git's own default, so this list changes nothing on a
/// host that configured nothing — it only takes the decision away from
/// `/etc/gitconfig`, `~/.gitconfig` and injected `GIT_CONFIG_*` environment. A
/// setting that genuinely ought to be operator-tunable belongs in
/// `forgekeep.toml`, where it is the instance's decision and is written down.
const OWNED_SETTINGS: &[&str] = &[
    // The bytes a replay writes. `apply.whitespace` is the one measured to
    // silently rewrite a pull request's content: with the `apply` backend and
    // `fix`, trailing whitespace is stripped out of somebody else's commit.
    "core.autocrlf=false",
    "core.eol=lf",
    "apply.whitespace=warn",
    // What a replay is allowed to run, and what it signs with. A host
    // `core.hooksPath` executes its own scripts three times during one rebase
    // merge (`post-checkout`, then `post-rewrite`); a host `commit.gpgsign`
    // fails the whole operation on a server that has no key.
    "core.hooksPath=/dev/null",
    "commit.gpgsign=false",
    // The *global* attributes file, which is the host's; `.gitattributes` inside
    // the repository is the user's own content and is left alone.
    "core.attributesFile=/dev/null",
    // How content is reconciled. The same knobs `forgekeep_merge_options` states
    // for the in-process half, so the CLI and `gix` paths cannot disagree.
    "diff.algorithm=myers",
    "diff.renames=true",
    "merge.renames=true",
    "merge.renormalize=false",
    "merge.conflictStyle=merge",
    // How a replay is performed. The backend decides which machinery applies the
    // commits, and `rebase.forkPoint` decides *which* commits are replayed at
    // all.
    "rebase.backend=merge",
    "rebase.autostash=false",
    "rebase.forkPoint=false",
    "rebase.updateRefs=false",
    // A name this code spells out: `clone.defaultRemoteName` on the host would
    // rename the remote the fetch and push below go on to address as `origin`.
    "clone.defaultRemoteName=origin",
];

/// One complete invocation policy for `git` run against ForgeKeep's own
/// repositories.
///
/// Built around a gateway rather than beside one so that a call site reads the
/// same as the gateway call it replaces: swapping `gateway.run(…)` for
/// `git.run(…)` is the whole change, and nothing can pick up half the policy.
pub struct LocalGitInvocation<'a> {
    git: &'a GitCommandGateway,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

/// The policy above, bound to `git`.
pub fn local(git: &GitCommandGateway) -> LocalGitInvocation<'_> {
    let mut args = Vec::with_capacity(OWNED_SETTINGS.len() * 2);
    for setting in OWNED_SETTINGS {
        args.push("-c".to_string());
        args.push((*setting).to_string());
    }

    LocalGitInvocation {
        git,
        args,
        env: vec![
            ("HOME".to_string(), DISARMED_HOME.to_string()),
            ("XDG_CONFIG_HOME".to_string(), DISARMED_HOME.to_string()),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
            ("GIT_CONFIG_GLOBAL".to_string(), DISARMED_HOME.to_string()),
            // Nothing here runs with a terminal behind it, so a prompt is a
            // hang until the gateway's timeout rather than a question.
            ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ],
    }
}

impl LocalGitInvocation<'_> {
    /// Run a git command under this policy.
    pub fn run(&self, args: &[&str], repo_path: Option<&Path>) -> Result<GitOutput> {
        self.run_with_env(args, repo_path, &[])
    }

    /// Run a git command under this policy with extra environment variables.
    ///
    /// The extras are applied *after* the disarming, so a caller can hand the
    /// subprocess an identity (`GIT_AUTHOR_NAME` and friends) without the
    /// removal of inherited `GIT_*` taking it away again.
    pub fn run_with_env(
        &self,
        args: &[&str],
        repo_path: Option<&Path>,
        env: &[(&str, &str)],
    ) -> Result<GitOutput> {
        let mut full_args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        full_args.extend_from_slice(args);

        let mut full_env: Vec<(&str, &str)> = self
            .env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        full_env.extend_from_slice(env);

        let inherited_env_to_remove: Vec<OsString> = std::env::vars_os()
            .filter_map(|(key, _)| is_inherited_git_env(&key).then_some(key))
            .collect();

        self.git
            .run_with_env_removed(&full_args, repo_path, &full_env, &inherited_env_to_remove)
    }
}

/// Whether an inherited variable can steer git's configuration or redirect the
/// operation.
///
/// Handled as a namespace rather than a frozen list: besides the obvious
/// `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` redirections it covers the
/// indexed `GIT_CONFIG_KEY_<n>` injection, `GIT_TEMPLATE_DIR`, and the editor
/// and pager variables a replay would otherwise be able to execute.
fn is_inherited_git_env(key: &OsStr) -> bool {
    key.to_str()
        .is_some_and(|key| key.to_ascii_uppercase().starts_with("GIT_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list is the contract: each entry is one `-c` pair, in a form git
    /// accepts, and none of them is a bare key whose value would then be taken
    /// from the next argument.
    #[test]
    fn every_owned_setting_is_a_complete_assignment() {
        for setting in OWNED_SETTINGS {
            let (key, value) = setting
                .split_once('=')
                .unwrap_or_else(|| panic!("`{setting}` states no value"));
            assert!(
                key.contains('.') && !key.is_empty(),
                "`{setting}` is not a `section.key` name"
            );
            assert!(!value.is_empty(), "`{setting}` assigns an empty value");
        }
    }

    /// `GIT_CONFIG_NOSYSTEM` does not suppress `GIT_CONFIG_COUNT`, which is why
    /// the removal of inherited `GIT_*` is a separate mechanism and not a
    /// belt-and-braces duplicate of the explicit environment.
    #[test]
    fn the_configuration_injection_variables_are_removed() {
        for key in [
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_TEMPLATE_DIR",
        ] {
            assert!(
                is_inherited_git_env(OsStr::new(key)),
                "`{key}` would be inherited by a repository-local git subprocess"
            );
        }
        assert!(
            !is_inherited_git_env(OsStr::new("PATH")),
            "removing PATH would leave git unable to find its own helpers"
        );
    }
}
