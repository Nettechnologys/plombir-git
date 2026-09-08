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

use std::ffi::OsString;
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

    #[error("I/O error running git: {0}")]
    Io(#[from] std::io::Error),
}

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
    /// - Returns `GitOutput` with the captured stdout, stderr, and status.
    pub fn run(&self, args: &[&str], repo_path: Option<&Path>) -> Result<GitOutput> {
        self.run_inner(args, repo_path, None, &[])
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
        self.run_inner(args, repo_path, Some(env), &[])
    }

    /// Run with explicit environment overrides after removing selected values
    /// inherited from the server process.
    ///
    /// This is deliberately crate-private: which ambient settings are unsafe is
    /// a property of *what the command is for*, not of the call site, so the
    /// decision is made by one of the two invocation policies this crate
    /// exports and by nothing else. `credentials` states it for a remote the
    /// user named; `invocation` states it for ForgeKeep's own repositories.
    pub(crate) fn run_with_env_removed(
        &self,
        args: &[&str],
        repo_path: Option<&Path>,
        env: &[(&str, &str)],
        inherited_env_to_remove: &[OsString],
    ) -> Result<GitOutput> {
        self.run_inner(args, repo_path, Some(env), inherited_env_to_remove)
    }

    /// Core implementation shared by the synchronous invocation variants.
    fn run_inner(
        &self,
        args: &[&str],
        repo_path: Option<&Path>,
        env: Option<&[(&str, &str)]>,
        inherited_env_to_remove: &[OsString],
    ) -> Result<GitOutput> {
        let full_cmd = self.build_command_line(args, repo_path);
        let command_str = full_cmd.join(" ");

        let _span = tracing::debug_span!("git_cli", cmd = %command_str).entered();

        let mut builder = Command::new("git");
        builder.args(&full_cmd);
        for key in inherited_env_to_remove {
            builder.env_remove(key);
        }
        if let Some(envs) = env {
            for (k, v) in envs {
                builder.env(k, v);
            }
        }
        let output =
            match rg_process::output_in_process_tree_with_timeout(&mut builder, self.timeout)
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
    pub async fn spawn_async(
        &self,
        args: &[&str],
        repo_path: Option<&Path>,
    ) -> Result<tokio::process::Child> {
        let full_cmd = self.build_command_line(args, repo_path);
        let command_str = full_cmd.join(" ");

        let _span = tracing::debug_span!("git_cli_async", cmd = %command_str).entered();

        let child = tokio::process::Command::new("git")
            .args(&full_cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
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
