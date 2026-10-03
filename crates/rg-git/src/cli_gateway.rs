//! Unified gateway for all `git` CLI invocations.
//!
//! Replaces ad-hoc `Command::new("git")` calls across the codebase with a
//! single entry point that provides:
//!
//! - **Version check** — validates `git --version` at construction time
//! - **Timeout** — all synchronous calls enforce a configurable deadline
//! - **Structured errors** — `GitCliError` distinguishes I/O, non-zero exit,
//!   timeout, and missing git
//! - **Tracing** — every invocation is wrapped in a `tracing::span`
//! - **Convenience** — automatic `-C <repo_path>` when a repo path is given
//! - **Async pipe support** — `spawn()` for pack-objects / index-pack streaming
//! - **Host configuration** — every child, on both paths, starts from the
//!   disarmed environment described at `DISARMED_ENV`

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Result};

// ── Errors ──────────────────────────────────────────────────────

/// Categorized error from a git CLI invocation.
#[derive(Debug, thiserror::Error)]
pub enum GitCliError {
    #[error("git not found or not executable: {0}")]
    NotFound(String),

    #[error("git command timed out after {timeout:?}: {command}")]
    Timeout { command: String, timeout: Duration },

    #[error("git command failed (exit {exit_code}): {command}")]
    Failed { command: String, exit_code: String },

    /// The child wrote past a caller-declared ceiling and was cut off before
    /// the run could hand that memory back. Reported as `InvalidRequest` by
    /// design: the size of what `git` was asked to print is a property of the
    /// repository, which is chosen by whoever can push.
    #[error(
        "git command produced too much output ({stream} passed the {limit_bytes}-byte limit \
         at {bytes_read} bytes): {command}"
    )]
    OutputTooLarge {
        command: String,
        stream: rg_process::LimitedStream,
        limit_bytes: u64,
        bytes_read: u64,
    },

    #[error("I/O error running git: {0}")]
    Io(#[from] std::io::Error),
}

// ── Output ceilings ─────────────────────────────────────────────

/// Default upper bound `run` accepts on captured stdout, in bytes.
///
/// A command written to `run` with no explicit ceiling still gets one — this
/// one — so a caller who never thought about output size cannot silently pay
/// for the size of what git chose to print. 16 MiB matches the largest budget
/// this workspace holds for a single read of committed data
/// (`MAX_TEMPLATE_TOTAL_BYTES`, `MAX_WORKFLOW_TOTAL_BYTES`,
/// `MAX_WIKI_TOTAL_BYTES`), so anything a legitimate reader here holds fits;
/// a call that legitimately needs more must ask for it with
/// [`GitCommandGateway::run_bounded`], and the ceiling becomes visible.
pub const DEFAULT_STDOUT_LIMIT_BYTES: u64 = 16 * 1024 * 1024;

/// Default upper bound on captured stderr, in bytes.
///
/// `stderr` funnels into the text of an error message rather than into any
/// reader's parsing loop, so its ceiling is separate — a legitimate git error
/// message never approaches this size, and a stream that does is the same
/// class of pathology as an oversized `stdout`.
pub const DEFAULT_STDERR_LIMIT_BYTES: u64 = 1024 * 1024;

// ── Output ──────────────────────────────────────────────────────

/// Wrapper around `std::process::Output` with extra helpers.
#[derive(Debug)]
pub struct GitOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: std::process::ExitStatus,
    pub command: String,
}

impl GitOutput {
    /// Check if the command exited successfully.
    pub fn success(&self) -> bool {
        self.status.success()
    }

    /// Get stdout as a string lossily.
    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).to_string()
    }

    /// Get stderr as a string lossily.
    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).to_string()
    }

    /// Ensure success; return `Err` with stderr if the command failed.
    pub fn ensure_success(&self) -> Result<()> {
        if self.status.success() {
            Ok(())
        } else {
            bail!(
                "git {} failed ({}): {}",
                self.command,
                self.status
                    .code()
                    .map_or("signal".into(), |c| c.to_string()),
                self.stderr_str().trim()
            )
        }
    }
}

// ── Host configuration ──────────────────────────────────────────

/// A path which cannot contain user configuration or credential files.
///
/// `/dev/null` is stable, root-owned, and makes every attempted child path fail
/// closed with `ENOTDIR`, which a real empty directory under `/tmp` would not.
const DISARMED_HOME: &str = "/dev/null";

/// The environment every `git` this gateway starts is given, before anything a
/// caller adds on top.
///
/// This is the gateway's own answer to "whose configuration is this?", and it is
/// applied on **both** paths — [`GitCommandGateway::run`] and
/// [`GitCommandGateway::spawn_async`] — rather than when a call site remembers
/// to ask for it. A policy a caller has to opt into is a policy the next call
/// site added beside it does not have.
///
/// Without it every invocation reads `/etc/gitconfig` and the `~/.gitconfig` of
/// whichever account the server process happens to run under, and the host
/// decides what this instance does with somebody else's repository:
/// `transfer.fsckObjects` decides which pushes it accepts, `pack.window` and
/// `pack.threads` decide what `pack-objects` streams, `core.autocrlf` and
/// `tar.umask` decide the bytes — and therefore the checksum — of a release
/// tarball, `core.hooksPath` runs the operator's scripts inside a server-side
/// replay, and `url.<base>.insteadOf` rewrites a remote *after* the SSRF guard
/// has already approved the URL. Two instances configured differently answer
/// the same request differently, and neither of them says so.
///
/// What this deliberately does **not** state is which values git should use:
/// that is a policy question, it differs between a repository Plombir Git owns
/// and a remote the user named, and it lives in [`crate::invocation`] and
/// [`crate::credentials`] respectively. The gateway only takes the decision
/// away from the machine.
const DISARMED_ENV: &[(&str, &str)] = &[
    // `~/.netrc`, `~/.ssh`, `~/.config/git/*` — everything git reaches through
    // a home directory rather than through a configuration variable.
    ("HOME", DISARMED_HOME),
    ("XDG_CONFIG_HOME", DISARMED_HOME),
    // `/etc/gitconfig` and `~/.gitconfig`, which is where the settings above are
    // actually written.
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", DISARMED_HOME),
    // Nothing the server runs has a terminal behind it, so a prompt is a hang
    // until the timeout rather than a question.
    ("GIT_TERMINAL_PROMPT", "0"),
];

/// Whether an inherited variable can steer git's configuration or redirect the
/// operation.
///
/// Handled as a namespace rather than a frozen list: besides the obvious
/// `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` redirections it covers the
/// indexed `GIT_CONFIG_KEY_<n>` injection — which `GIT_CONFIG_NOSYSTEM` does
/// *not* suppress, so this is a second mechanism and not a duplicate of the
/// explicit values above — as well as `GIT_TEMPLATE_DIR` and the editor and
/// pager variables a replay would otherwise be able to execute.
fn is_host_git_env(key: &OsStr) -> bool {
    key.to_str()
        .is_some_and(|key| key.to_ascii_uppercase().starts_with("GIT_"))
}

/// The environment side of a `git` invocation, for the two builder types this
/// gateway spawns children with.
///
/// Both paths disarm through one function rather than through two similar
/// blocks: a mutation that drops the call is what the guards in this file look
/// for, and there is no third spelling for it to hide behind.
trait GitChildEnvironment {
    fn unset(&mut self, key: &OsStr);
    fn set(&mut self, key: &str, value: &str);
}

impl GitChildEnvironment for Command {
    fn unset(&mut self, key: &OsStr) {
        self.env_remove(key);
    }

    fn set(&mut self, key: &str, value: &str) {
        self.env(key, value);
    }
}

impl GitChildEnvironment for tokio::process::Command {
    fn unset(&mut self, key: &OsStr) {
        self.env_remove(key);
    }

    fn set(&mut self, key: &str, value: &str) {
        self.env(key, value);
    }
}

/// Take the host's configuration away from a `git` child.
///
/// Removal comes first and the explicit values second, because the two overlap:
/// `GIT_CONFIG_NOSYSTEM` is both a variable the server process may have
/// inherited and one this gateway states, and applying them the other way round
/// would delete the answer it had just written.
///
/// `also_remove` is the extra removal an invocation policy asks for — the
/// transport environment of an outbound remote, say — and it is applied in the
/// same phase for the same reason.
fn disarm_host_configuration<C: GitChildEnvironment>(builder: &mut C, also_remove: &[OsString]) {
    for (key, _) in std::env::vars_os().filter(|(key, _)| is_host_git_env(key)) {
        builder.unset(&key);
    }
    for key in also_remove {
        builder.unset(key);
    }
    for (key, value) in DISARMED_ENV {
        builder.set(key, value);
    }
}

// ── Gateway ─────────────────────────────────────────────────────

/// Unified gateway for git CLI invocations.
///
/// ```ignore
/// let git = GitCommandGateway::new()?;
/// let out = git.run(&["clone", "--bare", url], Some(repo_path))?;
/// out.ensure_success()?;
/// ```
#[derive(Debug, Clone)]
pub struct GitCommandGateway {
    /// Default timeout for synchronous commands.
    timeout: Duration,
}

impl Default for GitCommandGateway {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
        }
    }
}

impl GitCommandGateway {
    /// Create a new gateway, validating that `git` is available.
    pub fn new() -> Result<Self> {
        let gateway = Self::default();
        let version = gateway.run(&["--version"], None)?;
        let version_str = version.stdout_str();
        if !version_str.starts_with("git version") {
            bail!("unexpected git --version output: {}", version_str.trim());
        }
        tracing::debug!(git_version = %version_str.trim(), "GitCommandGateway initialized");
        Ok(gateway)
    }

    /// Create a new gateway with a custom timeout.
    pub fn with_timeout(timeout: Duration) -> Result<Self> {
        let gateway = Self { timeout };
        // Validate git availability (cheap call)
        let version = gateway.run(&["--version"], None)?;
        if !version.stdout_str().starts_with("git version") {
            bail!(
                "unexpected git --version output: {}",
                version.stdout_str().trim()
            );
        }
        tracing::debug!(?timeout, git_version = %version.stdout_str().trim(), "GitCommandGateway initialized");
        Ok(gateway)
    }

    /// Run a git command synchronously and capture its output.
    ///
    /// - `repo_path` — if `Some`, prepends `["-C", repo_path]` to `args`.
    /// - Enforces the configured timeout; kills the child on timeout.
    /// - Stdout / stderr are capped at [`DEFAULT_STDOUT_LIMIT_BYTES`] /
    ///   [`DEFAULT_STDERR_LIMIT_BYTES`]; a run that would grow past either
    ///   fails with [`GitCliError::OutputTooLarge`] rather than reading it
    ///   all. A caller that legitimately needs more calls [`Self::run_bounded`]
    ///   with an explicit ceiling — the size becomes visible in the source.
    /// - Returns `GitOutput` with the captured stdout, stderr, and status.
    pub fn run(&self, args: &[&str], repo_path: Option<&Path>) -> Result<GitOutput> {
        self.run_inner(
            args,
            repo_path,
            None,
            &[],
            DEFAULT_STDOUT_LIMIT_BYTES,
            DEFAULT_STDERR_LIMIT_BYTES,
        )
    }

    /// Run a git command with an explicit stdout ceiling.
    ///
    /// The declared limit is the entire point: any caller reading the output
    /// of a git command whose size scales with a repository's content — a
    /// listing, a blob, an author-set — chooses here how many bytes it is
    /// willing to hold. Beyond it the call returns [`GitCliError::OutputTooLarge`]
    /// naming the stream and the ceiling, and the child sees `EPIPE` on its
    /// next write. Stderr keeps [`DEFAULT_STDERR_LIMIT_BYTES`] — the error
    /// message is not what any caller reads as data.
    pub fn run_bounded(
        &self,
        args: &[&str],
        repo_path: Option<&Path>,
        stdout_limit_bytes: u64,
    ) -> Result<GitOutput> {
        self.run_inner(
            args,
            repo_path,
            None,
            &[],
            stdout_limit_bytes,
            DEFAULT_STDERR_LIMIT_BYTES,
        )
    }

    /// Run a git command with extra environment variables.
    ///
    /// Like [`Self::run`], but additionally sets the provided environment
    /// variables on the spawned process. Used for commands that need git identity
    /// (`GIT_AUTHOR_NAME` / `GIT_COMMITTER_EMAIL` etc.) without polluting the
    /// caller's environment.
    pub fn run_with_env(
        &self,
        args: &[&str],
        repo_path: Option<&Path>,
        env: &[(&str, &str)],
    ) -> Result<GitOutput> {
        self.run_inner(
            args,
            repo_path,
            Some(env),
            &[],
            DEFAULT_STDOUT_LIMIT_BYTES,
            DEFAULT_STDERR_LIMIT_BYTES,
        )
    }

    /// Run with explicit environment overrides after removing selected values
    /// inherited from the server process.
    ///
    /// The git configuration environment is removed from every child anyway
    /// (`DISARMED_ENV`); this widens that removal for a command whose threat
    /// model reaches past git's own variables — the proxy, TLS and ssh-agent
    /// settings an outbound remote would otherwise pick up.
    ///
    /// It is deliberately crate-private: how much further to go is a property
    /// of *what the command is for*, not of the call site, so the decision is
    /// made by one of the two invocation policies this crate exports and by
    /// nothing else. `credentials` states it for a remote the user named;
    /// `invocation` states it for Plombir Git's own repositories.
    pub(crate) fn run_with_env_removed(
        &self,
        args: &[&str],
        repo_path: Option<&Path>,
        env: &[(&str, &str)],
        inherited_env_to_remove: &[OsString],
    ) -> Result<GitOutput> {
        self.run_inner(
            args,
            repo_path,
            Some(env),
            inherited_env_to_remove,
            DEFAULT_STDOUT_LIMIT_BYTES,
            DEFAULT_STDERR_LIMIT_BYTES,
        )
    }

    /// Core implementation shared by the synchronous invocation variants.
    fn run_inner(
        &self,
        args: &[&str],
        repo_path: Option<&Path>,
        env: Option<&[(&str, &str)]>,
        inherited_env_to_remove: &[OsString],
        stdout_limit_bytes: u64,
        stderr_limit_bytes: u64,
    ) -> Result<GitOutput> {
        let full_cmd = self.build_command_line(args, repo_path);
        let command_str = full_cmd.join(" ");

        let _span = tracing::debug_span!("git_cli", cmd = %command_str).entered();

        let mut builder = Command::new("git");
        builder.args(&full_cmd);
        disarm_host_configuration(&mut builder, inherited_env_to_remove);
        // Applied last, so a caller can still hand the child an identity
        // (`GIT_AUTHOR_NAME` and friends) or state one of the disarmed values
        // itself — the removal above would otherwise take it away again.
        if let Some(envs) = env {
            for (k, v) in envs {
                builder.env(k, v);
            }
        }
        // The ceiling is the invariant: `output_in_process_tree_with_timeout`
        // with no limit buffered the child's whole stdout under `wait_with_output`,
        // and the only bound on the way was the 120-second deadline — for a
        // command whose output size is chosen by a repository's content, this
        // is exactly no bound. `_and_limit` stops reading at the caller's
        // ceiling and reports the refusal by variant, not by exit code.
        let output = match rg_process::output_in_process_tree_with_timeout_and_limit(
            &mut builder,
            self.timeout,
            stdout_limit_bytes,
            stderr_limit_bytes,
        )
        .map_err(|error| match error {
            rg_process::ProcessOutputError::Spawn(error)
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                GitCliError::NotFound(format!("{command_str}: {error}"))
            }
            rg_process::ProcessOutputError::Spawn(error)
            | rg_process::ProcessOutputError::Wait(error) => GitCliError::Io(error),
        })? {
            rg_process::TimedOutput::Completed(output) => output,
            rg_process::TimedOutput::TimedOut => {
                return Err(GitCliError::Timeout {
                    command: command_str,
                    timeout: self.timeout,
                }
                .into());
            }
            rg_process::TimedOutput::OutputTooLarge {
                stream,
                limit,
                bytes_read,
            } => {
                return Err(GitCliError::OutputTooLarge {
                    command: command_str,
                    stream,
                    limit_bytes: limit,
                    bytes_read,
                }
                .into());
            }
        };

        let result = GitOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            status: output.status,
            command: command_str,
        };

        if !result.success() {
            tracing::debug!(
                exit_code = ?result.status.code(),
                stderr = %result.stderr_str().trim(),
                "git command failed"
            );
        }

        Ok(result)
    }

    /// Run a git command, returning `()` on success or `bail!` with stderr.
    ///
    /// Convenience wrapper for the common pattern:
    /// `let out = git.run(...)?; out.ensure_success()?;`
    pub fn run_or_bail(&self, args: &[&str], repo_path: Option<&Path>) -> Result<()> {
        self.run(args, repo_path)?.ensure_success()
    }

    /// Spawn an async git command with piped stdin/stdout/stderr.
    ///
    /// Used for pack-objects, index-pack, etc. where data is streamed.
    /// Callers should use `tokio::time::timeout()` around the I/O loop.
    ///
    /// The child is disarmed exactly as the synchronous path's is — see
    /// `DISARMED_ENV`. This is the protocol hot path, so it is also the path
    /// where the host would have had the most to say: what `pack-objects`
    /// streams, which pushes `index-pack` accepts, and the bytes of every
    /// archive this server hands out.
    pub async fn spawn_async(
        &self,
        args: &[&str],
        repo_path: Option<&Path>,
    ) -> Result<tokio::process::Child> {
        let full_cmd = self.build_command_line(args, repo_path);
        let command_str = full_cmd.join(" ");

        let _span = tracing::debug_span!("git_cli_async", cmd = %command_str).entered();

        let mut builder = tokio::process::Command::new("git");
        builder
            .args(&full_cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        disarm_host_configuration(&mut builder, &[]);

        let child = builder
            .spawn()
            .map_err(|e| GitCliError::NotFound(format!("{command_str}: {e}")))?;

        Ok(child)
    }

    /// Build the full argument list (including optional `-C <repo_path>`).
    fn build_command_line(&self, args: &[&str], repo_path: Option<&Path>) -> Vec<String> {
        let mut full = Vec::new();
        if let Some(path) = repo_path {
            full.push("-C".to_string());
            full.push(path.to_string_lossy().to_string());
        }
        full.extend(args.iter().map(|s| s.to_string()));
        full
    }
}

// ── Global singleton ────────────────────────────────────────────

static GLOBAL_GATEWAY: OnceLock<Result<GitCommandGateway>> = OnceLock::new();

/// Get (or lazily initialize) the global `GitCommandGateway`.
///
/// Validates `git --version` on first access.  Returns the cached
/// `Result` on subsequent calls, so there's no repeated startup check.
pub fn global_gateway() -> &'static Result<GitCommandGateway> {
    GLOBAL_GATEWAY.get_or_init(GitCommandGateway::new)
}

/// Seed the global gateway with a configured command timeout.
///
/// Must be called **before** the first `global_gateway()` access to take
/// effect (e.g. at server startup). If the global gateway was already
/// initialized, this is a no-op and the previously configured timeout stays.
/// Returns an error only if `git` itself is unavailable.
pub fn init_global_gateway(timeout: Duration) -> Result<()> {
    GLOBAL_GATEWAY.get_or_init(|| GitCommandGateway::with_timeout(timeout));
    match global_gateway() {
        Ok(_) => Ok(()),
        Err(e) => bail!("failed to initialize git gateway: {e}"),
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    fn raw_git_command_lines(source: &str) -> Vec<(usize, &str)> {
        rust_source::call_sites(source, &["Command::new"])
            .into_iter()
            .filter(|call| {
                rust_source::first_string_argument(source, *call).as_deref() == Some("git")
            })
            .map(|call| (call.line, rust_source::source_line(source, call.line)))
            .collect()
    }

    #[test]
    fn test_version_check() {
        let gateway = GitCommandGateway::new().expect("git should be installed");
        let out = gateway.run(&["--version"], None).unwrap();
        assert!(out.stdout_str().contains("git version"));
        assert!(out.success());
    }

    #[test]
    fn test_successful_command() {
        let gateway = GitCommandGateway::new().unwrap();
        let out = gateway
            .run(&["rev-parse", "--git-dir"], Some(Path::new(".")))
            .unwrap();
        // In the project root, this should succeed or fail gracefully
        let _ = out.success();
    }

    #[test]
    fn test_failing_command() {
        let gateway = GitCommandGateway::new().unwrap();
        let out = gateway.run(&["this-command-does-not-exist"], None).unwrap();
        assert!(!out.success());
        assert!(out.ensure_success().is_err());
    }

    #[test]
    fn test_build_command_line() {
        let gateway = GitCommandGateway::new().unwrap();
        let args = gateway.build_command_line(&["clone", "--bare", "url"], Some(Path::new("/tmp")));
        assert_eq!(args, vec!["-C", "/tmp", "clone", "--bare", "url"]);
    }

    #[test]
    fn test_gateway_cache() {
        let g1 = global_gateway();
        let g2 = global_gateway();
        assert!(std::ptr::eq(g1.as_ref().unwrap(), g2.as_ref().unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn run_bounded_refuses_output_that_would_exceed_the_ceiling() {
        // A repository with `printf` on `$PATH` is enough — no committed
        // objects are needed to prove that `run` refuses a stdout that would
        // grow past the ceiling. `git -c help.alias=...` runs a shell
        // command inline.
        let gateway = GitCommandGateway::new().unwrap();
        // 2 MiB of `x` characters, more than the 64 KiB ceiling.
        let alias = "alias.emit=!printf 'x%.0s' $(seq 1 2097152)";

        let result = gateway.run_bounded(&["-c", alias, "emit"], None, 65_536);
        let error = result.expect_err("emitting 2 MiB with a 64 KiB ceiling must fail");

        // Downcast through anyhow to the typed variant so the assertion
        // fails if the refusal is dressed up as a timeout or a non-zero exit.
        let cli_error = error
            .downcast_ref::<GitCliError>()
            .expect("run_bounded should surface a typed GitCliError");
        match cli_error {
            GitCliError::OutputTooLarge {
                command,
                stream,
                limit_bytes,
                bytes_read,
            } => {
                assert_eq!(*stream, rg_process::LimitedStream::Stdout);
                assert_eq!(*limit_bytes, 65_536);
                assert!(
                    *bytes_read > *limit_bytes,
                    "bytes_read {bytes_read} should exceed the declared limit {limit_bytes}"
                );
                assert!(
                    command.contains("emit"),
                    "refusal should name the command that produced the output; got {command}"
                );
            }
            other => panic!("expected OutputTooLarge, got {other:?}"),
        }

        // The Display message must carry both the ceiling and the command,
        // so an operator reading logs sees which cap bound where.
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("65536"),
            "error message should name the limit; got: {rendered}"
        );
        assert!(
            rendered.contains("emit"),
            "error message should name the command; got: {rendered}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_stays_bounded_by_the_default_stdout_ceiling() {
        // Same shape as the bounded probe, but goes through `run` — the
        // gateway's default ceiling is the one being asserted here. Emit
        // significantly more than the default, then check `run` refuses.
        let gateway = GitCommandGateway::new().unwrap();
        // (DEFAULT_STDOUT_LIMIT_BYTES + 1 MiB) worth of characters.
        let over_default = DEFAULT_STDOUT_LIMIT_BYTES + 1024 * 1024;
        let alias = format!("alias.emit=!printf 'y%.0s' $(seq 1 {over_default})");

        let error = gateway
            .run(&["-c", &alias, "emit"], None)
            .expect_err("a stream past the default ceiling must fail");

        let cli_error = error
            .downcast_ref::<GitCliError>()
            .expect("run should surface a typed GitCliError");
        assert!(
            matches!(
                cli_error,
                GitCliError::OutputTooLarge {
                    stream: rg_process::LimitedStream::Stdout,
                    limit_bytes,
                    ..
                } if *limit_bytes == DEFAULT_STDOUT_LIMIT_BYTES
            ),
            "expected OutputTooLarge with the default stdout limit, got {cli_error:?}"
        );
    }

    /// Source-order guard: `run_inner` must dispatch through the bounded
    /// entry point of `rg_process`.
    ///
    /// A mutation that drops the ceilings — reading the pipe whole again, under
    /// whatever name — restores the buffering the card was filed against, and
    /// the RSS-anchored guard in `rg-process::bounded_tests` would red for it,
    /// but that failure travels through a whole chain of tests. This assertion
    /// reddens in the file that owns the invariant.
    ///
    /// Read through the named views rather than off the raw bytes: a raw
    /// `find` is satisfied by the comment inside `run_inner` that names the
    /// unbounded twin, and by this test's own words.
    #[test]
    fn run_inner_dispatches_through_the_bounded_rg_process_entrypoint() {
        let source = include_str!("cli_gateway.rs");
        let bounded = rust_source::production_function_call_sites(
            source,
            "run_inner",
            &["output_in_process_tree_with_timeout_and_limit"],
        );
        assert_eq!(
            bounded.len(),
            1,
            "`run_inner` must reach `rg_process` through the bounded entry point exactly once, \
             found {} call(s) — the ceiling `run` advertises is bypassed otherwise",
            bounded.len()
        );
        // The unbounded twin is deleted, so this half guards against it being
        // written back rather than against today's tree.
        let unbounded = rust_source::production_function_call_sites(
            source,
            "run_inner",
            &["output_in_process_tree_with_timeout"],
        );
        assert!(
            unbounded.is_empty(),
            "`run_inner` dispatches through an unbounded `rg_process` entry point at line(s) {:?}",
            unbounded.iter().map(|call| call.line).collect::<Vec<_>>()
        );
    }

    /// `GIT_CONFIG_NOSYSTEM` does not suppress `GIT_CONFIG_COUNT`, which is why
    /// the removal of inherited `GIT_*` is a separate mechanism and not a
    /// belt-and-braces duplicate of the explicit values in `DISARMED_ENV`.
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
                is_host_git_env(OsStr::new(key)),
                "`{key}` would be inherited by a git subprocess"
            );
        }
        assert!(
            !is_host_git_env(OsStr::new("PATH")),
            "removing PATH would leave git unable to find its own helpers"
        );
    }

    /// The four config levels the environment can open, each named by the
    /// variable that closes it. A value dropped from the list is a level the
    /// host gets back, which is invisible on a developer's machine and
    /// invisible on a host that configured nothing.
    #[test]
    fn every_configuration_level_the_environment_opens_is_closed() {
        let disarmed: std::collections::HashMap<_, _> = DISARMED_ENV.iter().copied().collect();
        assert_eq!(disarmed.get("GIT_CONFIG_NOSYSTEM"), Some(&"1"));
        for key in ["HOME", "XDG_CONFIG_HOME", "GIT_CONFIG_GLOBAL"] {
            assert_eq!(
                disarmed.get(key),
                Some(&DISARMED_HOME),
                "`{key}` no longer points at a path that cannot hold configuration"
            );
        }
        assert_eq!(disarmed.get("GIT_TERMINAL_PROMPT"), Some(&"0"));
        assert_eq!(
            disarmed.len(),
            DISARMED_ENV.len(),
            "a key is stated twice, so which value wins depends on iteration order"
        );
    }

    /// Source-order guard: both spawn paths must disarm.
    ///
    /// The behavioural halves live in `tests/host_configuration.rs`, which runs
    /// real `git` against a planted config. This one reddens in the file that
    /// owns the invariant, and it is what catches the asymmetric mutation — the
    /// synchronous path keeps its disarming, the streaming path quietly loses
    /// it, and every ordinary test goes on passing because none of them stream.
    #[test]
    fn both_spawn_paths_disarm_the_host_configuration() {
        let source = include_str!("cli_gateway.rs");
        for spawner in ["run_inner", "spawn_async"] {
            let disarmed = rust_source::production_function_call_sites(
                source,
                spawner,
                &["disarm_host_configuration"],
            );
            assert_eq!(
                disarmed.len(),
                1,
                "`{spawner}` must disarm the host's git configuration exactly once, found {} \
                 call(s) — every child it starts answers to the machine otherwise",
                disarmed.len()
            );
        }
    }

    #[cfg(unix)]
    fn process_is_running(pid: u32) -> bool {
        // A killed orphan may briefly remain as a zombie until its new parent
        // reaps it. It is no longer executing work and counts as stopped.
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            return stat
                .split_once(") ")
                .and_then(|(_, fields)| fields.chars().next())
                != Some('Z');
        }
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(unix)]
    fn assert_process_stops(pid: u32, description: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while process_is_running(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "{description} process {pid} survived the git command timeout"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    struct RecordedPidCleanup(Vec<std::path::PathBuf>);

    #[cfg(unix)]
    impl Drop for RecordedPidCleanup {
        fn drop(&mut self) {
            for path in &self.0 {
                let Ok(pid) = std::fs::read_to_string(path) else {
                    continue;
                };
                drop(
                    Command::new("kill")
                        .args(["-KILL", pid.trim()])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status(),
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn timeout_returns_near_deadline_and_kills_git_alias_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let shell_pid_file = temp.path().join("git-alias-shell.pid");
        let child_pid_file = temp.path().join("git-alias-child.pid");
        let _pid_cleanup = RecordedPidCleanup(vec![shell_pid_file.clone(), child_pid_file.clone()]);
        let shell_pid_path = shell_pid_file.to_string_lossy().into_owned();
        let child_pid_path = child_pid_file.to_string_lossy().into_owned();
        let alias = "alias.timeout-probe=!exec sh -c 'echo $$ > \"$RG_GIT_SHELL_PID\"; \
                     sleep 5 & child=$!; echo $child > \"$RG_GIT_CHILD_PID\"; wait $child' \
                     </dev/null >/dev/null 2>&1";

        let gateway = GitCommandGateway::with_timeout(Duration::from_millis(200)).unwrap();
        let started = std::time::Instant::now();
        let error = gateway
            .run_with_env(
                &["-c", alias, "timeout-probe"],
                None,
                &[
                    ("RG_GIT_SHELL_PID", shell_pid_path.as_str()),
                    ("RG_GIT_CHILD_PID", child_pid_path.as_str()),
                ],
            )
            .expect_err("the long-running git alias must time out");
        let elapsed = started.elapsed();

        assert!(
            error.to_string().contains("timed out after 200ms"),
            "unexpected gateway error: {error:#}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "200ms timeout returned only after {elapsed:?}"
        );

        let recorded_pid = |path: &Path| {
            std::fs::read_to_string(path)
                .unwrap_or_else(|read_error| {
                    panic!("fixture did not record {}: {read_error}", path.display())
                })
                .trim()
                .parse::<u32>()
                .unwrap_or_else(|parse_error| {
                    panic!(
                        "fixture recorded an invalid {}: {parse_error}",
                        path.display()
                    )
                })
        };
        assert_process_stops(recorded_pid(&shell_pid_file), "git alias shell");
        assert_process_stops(recorded_pid(&child_pid_file), "git alias descendant");
    }

    #[test]
    fn raw_git_command_scan_ignores_rust_data_and_keeps_real_string_arguments() {
        let source = r####"
// Command::new("git");
/* process::Command::new("git"); */
let normal = "Command::new(\"git\")";
let raw = r#"process::Command::new("git")"#;
let bytes = b"Command::new(\"git\")";
let shell = Command::new("sh");
let git = std::process::Command::new(
    r#"git"#,
);
let typed = Command::new::<&str>("git");
"####;

        let hits = raw_git_command_lines(source);
        assert_eq!(
            hits.len(),
            2,
            "non-code decoys were treated as git calls: {hits:?}"
        );
        assert_eq!(hits[0].0, 8);
        assert_eq!(hits[0].1.trim(), "let git = std::process::Command::new(");
        assert_eq!(hits[1], (11, "let typed = Command::new::<&str>(\"git\");"));
    }

    /// Regression guard: ensure no crates use raw `Command::new("git")` outside this file.
    #[test]
    fn test_no_raw_git_command_in_crates() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .unwrap();
        let mut violations = Vec::new();

        for entry in walkdir::WalkDir::new(workspace.join("crates"))
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            // Skip the gateway file itself (the only allowed location)
            if path.ends_with("cli_gateway.rs") {
                continue;
            }

            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            for (line_no, line) in raw_git_command_lines(&content) {
                let relative = path.strip_prefix(workspace).unwrap_or(path);
                violations.push(format!(
                    "{}:{}  =>  {}",
                    relative.display(),
                    line_no,
                    line.trim()
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "found {} raw git Command::new(\"git\") outside cli_gateway.rs:\n{}",
            violations.len(),
            violations.join("\n")
        );
    }
}
