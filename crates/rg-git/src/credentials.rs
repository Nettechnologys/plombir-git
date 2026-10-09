//! Handing a secret to an outbound `git` subprocess without leaking it.
//!
//! Two callers speak to remotes the user supplied — mirror sync and repository
//! import — and both start from the same kind of secret: a password or a
//! platform token typed into a form. This module is the only sanctioned way in,
//! so the two cannot drift apart on the question of where the secret ends up.
//!
//! The rule it encodes: the secret travels through the **environment**, read
//! back by an inline credential helper. Never through argv, never through the
//! remote URL.

use std::ffi::{OsStr, OsString};
use std::net::IpAddr;
use std::path::Path;

use anyhow::Result;

use crate::cli_gateway::{GitCommandGateway, GitOutput};

/// Environment variables the credential helper reads the secret out of. Named
/// after the product so they cannot collide with something the operator has
/// already exported for their own git usage.
pub const USERNAME_ENV: &str = "PLOMBIR_GIT_GIT_USERNAME";
pub const PASSWORD_ENV: &str = "PLOMBIR_GIT_GIT_PASSWORD";

/// A path which cannot contain user configuration or credential files.
///
/// Outbound git only needs a home directory to discover ambient authority. A
/// real directory under `/tmp` would be both unnecessary and open to planting
/// if its ownership ever drifted; `/dev/null` is stable, root-owned, and makes
/// every attempted child path fail closed with `ENOTDIR`.
const DISARMED_HOME: &str = "/dev/null";

/// Render the shell expansion through which the inline credential helper reads
/// one of the explicit environment entries below. Keeping this as a named call
/// makes the cross-process read visible to the workspace environment census.
fn credential_helper_env_read(name: &str) -> String {
    format!("${name}")
}

/// One complete invocation policy for a git remote selected by a user.
///
/// Keeping the argument, explicit-environment, and inherited-environment
/// policies together makes it impossible for import and mirror sync to pick up
/// only the credential half and silently omit the transport hardening half.
#[must_use = "an outbound git invocation must be run to apply its environment policy"]
pub struct OutboundGitInvocation {
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl OutboundGitInvocation {
    /// Prevent libcurl from resolving or redirecting an HTTP(S) remote outside
    /// the destination decision made by the caller.
    ///
    /// `http.curloptResolve=` clears every inherited resolve rule before the
    /// caller adds its checked host. Redirects are disabled because a redirect
    /// to another host or port would otherwise leave that checked mapping and
    /// return to libcurl's ordinary resolver.
    pub fn lock_http_destination(mut self) -> Self {
        self.args.push("-c".to_string());
        self.args.push("http.curloptResolve=".to_string());
        self.args.push("-c".to_string());
        self.args.push("http.followRedirects=false".to_string());
        self
    }

    /// Add the DNS answers approved for one HTTP(S) git remote.
    ///
    /// Git passes `http.curloptResolve` to libcurl's `CURLOPT_RESOLVE`. The URL
    /// itself remains unchanged, so HTTP `Host`, TLS SNI, and certificate
    /// verification still use `host`; only the socket address is replaced.
    pub fn bind_http_host(mut self, host: &str, port: u16, addresses: &[IpAddr]) -> Result<Self> {
        if host.is_empty()
            || host
                .bytes()
                .any(|byte| matches!(byte, b':' | b',' | b'\r' | b'\n'))
        {
            anyhow::bail!("invalid HTTP git host for address binding");
        }
        if port == 0 {
            anyhow::bail!("HTTP git destination has no effective port");
        }
        if addresses.is_empty() {
            anyhow::bail!("HTTP git destination has no checked addresses");
        }

        let addresses = addresses
            .iter()
            .map(|address| match address {
                IpAddr::V4(address) => address.to_string(),
                IpAddr::V6(address) => format!("[{address}]"),
            })
            .collect::<Vec<_>>()
            .join(",");
        self.args.push("-c".to_string());
        self.args
            .push(format!("http.curloptResolve={host}:{port}:{addresses}"));
        Ok(self)
    }

    /// Run the outbound operation under this policy.
    pub fn run(
        &self,
        git: &GitCommandGateway,
        args: &[&str],
        repo_path: Option<&Path>,
    ) -> Result<GitOutput> {
        self.run_with_disk_budget(git, args, repo_path, None)
    }

    /// [`Self::run`], with a ceiling on what the command may write under
    /// `disk_budget_dir`.
    ///
    /// A clone of a remote somebody else controls is bounded by its timeout in
    /// time, not in bytes: within two minutes it can fill the volume. The
    /// caller names the directory the operation is expected to fill — the
    /// staging clone or mirror destination, never a directory holding other
    /// repositories — and the ceiling it may reach there.
    pub fn run_under_disk_budget(
        &self,
        git: &GitCommandGateway,
        args: &[&str],
        repo_path: Option<&Path>,
        disk_budget_dir: &Path,
        max_bytes: u64,
    ) -> Result<GitOutput> {
        self.run_with_disk_budget(git, args, repo_path, Some((disk_budget_dir, max_bytes)))
    }

    fn run_with_disk_budget(
        &self,
        git: &GitCommandGateway,
        args: &[&str],
        repo_path: Option<&Path>,
        disk_budget: Option<(&Path, u64)>,
    ) -> Result<GitOutput> {
        let mut full_args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        full_args.extend_from_slice(args);
        let env: Vec<(&str, &str)> = self
            .env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let inherited_env_to_remove: Vec<OsString> = std::env::vars_os()
            .filter_map(|(key, _)| is_ambient_transport_env(&key).then_some(key))
            .collect();

        match disk_budget {
            Some((disk_budget_dir, max_bytes)) => git.run_with_env_removed_under_disk_budget(
                &full_args,
                repo_path,
                &env,
                &inherited_env_to_remove,
                disk_budget_dir,
                max_bytes,
            ),
            None => git.run_with_env_removed(&full_args, repo_path, &env, &inherited_env_to_remove),
        }
    }
}

/// Whether an inherited variable can change, authenticate, or observe the
/// transport used for a remote chosen by somebody other than the operator.
///
/// The `GIT_` family is intentionally handled as a namespace, not as a frozen
/// list: besides the obvious proxy/TLS/SSH knobs it includes indexed
/// `GIT_CONFIG_KEY_<n>` injections and trace destinations capable of recording
/// credentials. `SSH_` covers agents and askpass programs. libcurl's proxy
/// namespace is open-ended by URL scheme, hence the suffix match.
fn is_ambient_transport_env(key: &OsStr) -> bool {
    let Some(key) = key.to_str() else {
        return false;
    };
    let key = key.to_ascii_uppercase();

    key.starts_with("GIT_")
        || key.starts_with("SSH_")
        || key.ends_with("_PROXY")
        || matches!(
            key.as_str(),
            "CURL_SSL_BACKEND" | "NETRC" | "SSL_CERT_DIR" | "SSL_CERT_FILE" | "SSLKEYLOGFILE"
        )
}

/// A remote's credential, in plaintext, for the duration of a single git
/// invocation.
///
/// Deliberately has no `Debug` and no field access: a password that cannot be
/// printed by a stray `{:?}` and cannot be pasted into a URL by the next caller
/// is one fewer way to leak it.
pub struct GitCredentials {
    username: Option<String>,
    password: String,
}

impl GitCredentials {
    /// A username/password pair as an operator typed it.
    ///
    /// An empty username is the same as none: git handed half an answer is
    /// refused by the remote, and with prompting disabled the failure reads as
    /// a protocol error rather than the honest "no credential" it is.
    pub fn new(username: Option<String>, password: String) -> Self {
        Self {
            username: username.filter(|username| !username.is_empty()),
            password,
        }
    }

    /// A platform token, which travels in the password field of HTTP Basic.
    ///
    /// The username is the fixed placeholder the platform documents
    /// (`x-access-token` for GitHub, `oauth2` for GitLab); it carries no
    /// information, but Basic auth has nowhere to put a lone token.
    pub fn token(username: &str, token: &str) -> Self {
        Self::new(Some(username.to_string()), token.to_string())
    }

    /// The secret, for masking it out of text that is about to be logged or
    /// persisted.
    pub fn password(&self) -> &str {
        &self.password
    }

    /// The optional HTTP Basic username paired with the password or token.
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }
}

/// Extra `git` arguments and environment that let the subprocess authenticate.
///
/// The secret is passed through the environment and read by an inline
/// credential helper, so:
///
/// - it never appears in argv (`ps` on a shared host would show it, and the
///   gateway echoes the command line into its error text and trace span);
/// - it never appears in the remote URL, which git copies verbatim into
///   `.git/config` — an on-disk plaintext copy that outlives the operation.
///
/// Everything below the credential itself is set on **both** branches — with a
/// credential and without one — because the anonymous branch is the one that
/// needs it most: there the remote is whatever address the user typed, and the
/// authority git would reach for is the host's, not ours.
///
/// - the empty `credential.helper=` in front resets the helper list, so a helper
///   configured system- or user-wide on the host can neither answer first nor be
///   handed this credential to store;
/// - `GIT_CONFIG_NOSYSTEM=1` and `GIT_CONFIG_GLOBAL` pointed at nothing drop
///   `/etc/gitconfig` and `~/.gitconfig`, which carry more than helpers: a
///   `url.<base>.insteadOf` there rewrites the remote *after* the SSRF guard has
///   already approved the URL the user supplied;
/// - `HOME` and `XDG_CONFIG_HOME` pointed at `/dev/null` put `~/.netrc`,
///   `~/.gitconfig`, and other per-user files outside the subprocess's reach;
/// - inherited `GIT_*`, `SSH_*`, proxy, and TLS environment is removed before
///   these explicit values are applied, so neither branch can acquire a proxy,
///   client certificate, askpass program, ssh-agent, or injected Git config
///   from the server process;
/// - `GIT_TERMINAL_PROMPT=0`, because nothing here runs with a terminal behind
///   it: a remote that asks for authentication must fail fast instead of
///   blocking until the gateway's timeout.
///
/// `/dev/null` is git's documented way to say "no global config"; on a platform
/// without it the path simply fails to open, which git treats as an empty
/// config — the same outcome by a different route.
///
/// SSH and scp-like remotes are rejected by `rg_core::net::check_git_url_static`:
/// an isolated home would hide `~/.ssh`, but an inherited ssh-agent would remain
/// ambient authority. Supporting private SSH remotes therefore needs a future
/// explicit identity contract rather than another process-wide default.
pub fn credential_invocation(credentials: Option<&GitCredentials>) -> OutboundGitInvocation {
    let mut args = vec!["-c".to_string(), "credential.helper=".to_string()];
    let mut env = vec![
        ("HOME".to_string(), DISARMED_HOME.to_string()),
        ("XDG_CONFIG_HOME".to_string(), DISARMED_HOME.to_string()),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
    ];
    let Some(credentials) = credentials else {
        return OutboundGitInvocation { args, env };
    };

    // git runs a `!`-prefixed helper through `sh -c '<value> "$@"' <value> get`,
    // so the trailing `f` becomes the call and takes the operation as `$1`.
    let mut helper = String::from("!f() { ");
    if credentials.username.is_some() {
        helper.push_str(&format!(
            "echo username=\"{}\"; ",
            credential_helper_env_read(USERNAME_ENV)
        ));
    }
    helper.push_str(&format!(
        "echo password=\"{}\"; }}; f",
        credential_helper_env_read(PASSWORD_ENV)
    ));

    args.push("-c".to_string());
    args.push(format!("credential.helper={helper}"));
    if let Some(username) = &credentials.username {
        env.push((USERNAME_ENV.to_string(), username.clone()));
    }
    env.push((PASSWORD_ENV.to_string(), credentials.password.clone()));
    OutboundGitInvocation { args, env }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the environment hand-off: `ps` (and the gateway's own
    /// error text, which quotes the command line) must never see the password.
    #[test]
    fn the_password_never_reaches_the_command_line() {
        let credentials = GitCredentials::token("sync-bot", "hunter2");
        let invocation = credential_invocation(Some(&credentials));
        let args = &invocation.args;
        let env = &invocation.env;

        let command_line = args.join(" ");
        assert!(
            !command_line.contains("hunter2"),
            "the password leaked into argv: {command_line}"
        );
        assert!(
            !command_line.contains("sync-bot"),
            "the username leaked into argv: {command_line}"
        );
        // The helper list is reset first, then our helper is installed.
        assert_eq!(args[0], "-c");
        assert_eq!(args[1], "credential.helper=");
        assert!(args[3].contains(USERNAME_ENV) && args[3].contains(PASSWORD_ENV));

        let env: std::collections::HashMap<_, _> = env.iter().cloned().collect();
        assert_eq!(env.get(PASSWORD_ENV).map(String::as_str), Some("hunter2"));
        assert_eq!(env.get(USERNAME_ENV).map(String::as_str), Some("sync-bot"));
        assert_eq!(
            env.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
    }

    #[test]
    fn checked_http_addresses_are_command_scoped_without_rewriting_the_url() {
        let invocation = credential_invocation(None)
            .lock_http_destination()
            .bind_http_host(
                "git.example.test",
                443,
                &[
                    "203.0.113.7".parse().unwrap(),
                    "2001:db8::7".parse().unwrap(),
                ],
            )
            .expect("bind checked addresses");

        assert!(invocation
            .args
            .windows(2)
            .any(|args| args == ["-c", "http.curloptResolve="]));
        assert!(invocation
            .args
            .windows(2)
            .any(|args| args == ["-c", "http.followRedirects=false"]));
        assert!(invocation.args.windows(2).any(|args| {
            args == [
                "-c",
                "http.curloptResolve=git.example.test:443:203.0.113.7,[2001:db8::7]",
            ]
        }));
        assert!(
            !invocation
                .args
                .iter()
                .any(|arg| arg.contains("/owner/repo")),
            "address binding must not synthesize or rewrite a remote URL"
        );
    }

    #[test]
    fn an_empty_checked_address_set_is_refused() {
        let result = credential_invocation(None)
            .lock_http_destination()
            .bind_http_host("git.example.test", 443, &[]);
        assert!(result.is_err());
    }

    /// With no credential there is nothing to hand over — and that is precisely
    /// the branch that must still disarm the host: the remote is an address the
    /// user chose, so a helper (or an `insteadOf` rewrite) sitting in the
    /// server's own git config would be answering on their behalf.
    ///
    /// Prompting stays off for the older reason: an authenticating remote would
    /// otherwise hang the caller until the gateway's timeout instead of failing
    /// with "authentication required".
    #[test]
    fn an_anonymous_remote_disarms_the_host_config_too() {
        let invocation = credential_invocation(None);
        let args = &invocation.args;
        let env = &invocation.env;
        assert_eq!(
            args,
            &vec!["-c".to_string(), "credential.helper=".to_string()],
            "an anonymous invocation left the host's helper list in place"
        );
        let env: std::collections::HashMap<_, _> = env.iter().cloned().collect();
        assert_eq!(
            env.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            env.get("GIT_CONFIG_NOSYSTEM").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.get("GIT_CONFIG_GLOBAL").map(String::as_str),
            Some("/dev/null")
        );
        assert_eq!(env.get("HOME").map(String::as_str), Some(DISARMED_HOME));
        assert_eq!(
            env.get("XDG_CONFIG_HOME").map(String::as_str),
            Some(DISARMED_HOME)
        );
        // Nothing else: no credential means no secret in the environment.
        assert!(!env.contains_key(USERNAME_ENV) && !env.contains_key(PASSWORD_ENV));
    }

    /// The two branches must leave with the same disarmed environment, or the
    /// invariant is one refactor away from being true on one of them only —
    /// which is exactly how it was lost the first time.
    #[test]
    fn both_branches_disarm_the_host_identically() {
        let credentials = GitCredentials::token("sync-bot", "hunter2");
        let with_credentials = credential_invocation(Some(&credentials));
        let without_credentials = credential_invocation(None);

        assert_eq!(
            with_credentials.args[..2],
            without_credentials.args[..2],
            "the helper reset differs between the two branches"
        );
        let ambient = |env: &[(String, String)]| -> Vec<(String, String)> {
            env.iter()
                .filter(|(key, _)| key != USERNAME_ENV && key != PASSWORD_ENV)
                .cloned()
                .collect()
        };
        assert_eq!(
            ambient(&with_credentials.env),
            ambient(&without_credentials.env)
        );
    }

    /// A username the caller left empty is not a username: the helper must
    /// answer with the password alone rather than echo an empty one, which the
    /// remote would take as a real (and wrong) identity.
    #[test]
    fn an_empty_username_is_no_username() {
        let credentials = GitCredentials::new(Some(String::new()), "hunter2".to_string());
        let invocation = credential_invocation(Some(&credentials));
        let args = &invocation.args;
        let env = &invocation.env;

        assert!(!args[3].contains(USERNAME_ENV), "{}", args[3]);
        let env: std::collections::HashMap<_, _> = env.iter().cloned().collect();
        assert!(!env.contains_key(USERNAME_ENV));
        assert_eq!(env.get(PASSWORD_ENV).map(String::as_str), Some("hunter2"));
    }

    #[test]
    fn the_transport_denylist_covers_namespaces_not_a_frozen_sample() {
        for key in [
            "GIT_PROXY_COMMAND",
            "git_ssl_no_verify",
            "GIT_CONFIG_KEY_17",
            "GIT_TRACE_CURL_NO_DATA",
            "SSH_AUTH_SOCK",
            "ssh_askpass",
            "http_proxy",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "CURL_SSL_BACKEND",
            "SSL_CERT_FILE",
            "SSLKEYLOGFILE",
            "NETRC",
        ] {
            assert!(
                is_ambient_transport_env(OsStr::new(key)),
                "ambient transport variable escaped the denylist: {key}"
            );
        }

        for key in ["PATH", "LANG", "PLOMBIR_GIT_GIT_PASSWORD", "DATABASE_URL"] {
            assert!(
                !is_ambient_transport_env(OsStr::new(key)),
                "unrelated server variable was removed from outbound git: {key}"
            );
        }
    }
}
