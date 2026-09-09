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
//! * the **environment** is disarmed, which is what puts `/etc/gitconfig`,
//!   `~/.gitconfig` and injected `GIT_CONFIG_*` out of reach. That half is not
//!   stated here: `cli_gateway::DISARMED_ENV` applies it to every
//!   child the gateway spawns, on both the synchronous and the streaming path,
//!   so a call site cannot arrive without it and this module cannot be the
//!   reason one did.
//! * the settings ForgeKeep decides are stated on the command line, where they
//!   outrank every configuration file. `OWNED_SETTINGS` below is that list, and
//!   every entry there is git's own default — an unconfigured host therefore
//!   behaves exactly as it did before.
//!
//! The second half is not made redundant by the first. A disarmed environment
//! says only that the *host* did not decide; it leaves the decision to whatever
//! git's built-in default happens to be in the version installed, and it says
//! nothing about configuration written inside the repository the command runs
//! in. Stating the values is what makes the answer ForgeKeep's own.
//!
//! This is the policy for repository-local operations ForgeKeep performs on its
//! own repositories. Talking to a remote the *user* named is a different
//! contract with a different threat model, and lives in
//! [`crate::credentials::credential_invocation`].

use std::path::Path;

use anyhow::Result;

use crate::cli_gateway::{GitCommandGateway, GitOutput};

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
}

/// The policy above, bound to `git`.
pub fn local(git: &GitCommandGateway) -> LocalGitInvocation<'_> {
    let mut args = Vec::with_capacity(OWNED_SETTINGS.len() * 2);
    for setting in OWNED_SETTINGS {
        args.push("-c".to_string());
        args.push((*setting).to_string());
    }

    LocalGitInvocation { git, args }
}

impl LocalGitInvocation<'_> {
    /// Run a git command under this policy.
    pub fn run(&self, args: &[&str], repo_path: Option<&Path>) -> Result<GitOutput> {
        self.run_with_env(args, repo_path, &[])
    }

    /// Run a git command under this policy with extra environment variables.
    ///
    /// The gateway applies the extras *after* its own disarming, so a caller can
    /// hand the subprocess an identity (`GIT_AUTHOR_NAME` and friends) without
    /// the removal of inherited `GIT_*` taking it away again.
    pub fn run_with_env(
        &self,
        args: &[&str],
        repo_path: Option<&Path>,
        env: &[(&str, &str)],
    ) -> Result<GitOutput> {
        let mut full_args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        full_args.extend_from_slice(args);

        self.git.run_with_env(&full_args, repo_path, env)
    }
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

    /// The policy is the `-c` list and nothing else: an invocation that also
    /// carried its own copy of the environment half would go on passing after a
    /// mutation removed the gateway's, and the single point this module's
    /// documentation promises would quietly be two.
    #[test]
    fn the_policy_states_settings_and_leaves_the_environment_to_the_gateway() {
        let gateway = GitCommandGateway::default();
        let invocation = local(&gateway);
        assert_eq!(
            invocation.args.len(),
            OWNED_SETTINGS.len() * 2,
            "the local policy carries arguments beyond its own `-c` settings"
        );
    }
}
