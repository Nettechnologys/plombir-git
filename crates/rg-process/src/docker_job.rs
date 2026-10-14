//! The `docker run` a CI job executes in, built once for both runners.
//!
//! The embedded runner (`rg-ci`) and the external one (`rg-runner`) each run a
//! job's script in a container, and each used to spell the argument vector out
//! on its own. The two copies had already drifted: one refused a workspace path
//! that is not UTF-8, the other silently mangled it with `to_string_lossy`, and
//! the two built the environment differently. Everything that decides what a job
//! container may do lives here, so a hardening flag or a rule about the
//! environment cannot be applied on one side only.

use std::path::Path;

/// Hard cap on the number of processes a job container may spawn (fork-bomb guard).
pub const PIDS_LIMIT: &str = "512";
/// Memory ceiling for a job container.
pub const MEMORY_LIMIT: &str = "2g";
/// CPU quota for a job container.
pub const CPU_LIMIT: &str = "2";

/// `HOME` a job sees inside its container: a directory every uid can write.
///
/// The host's `HOME` is the runner's workspace, which does not exist inside the
/// container, and the Docker CLI's own `HOME` holds the client's registry
/// credentials, which must not be forwarded either.
const CONTAINER_HOME: &str = "HOME=/tmp";

/// Why a job container's `docker run` was not built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobContainerRefused {
    /// The job's workspace path cannot be handed to `docker -v` without
    /// changing it.
    WorkspacePathNotUtf8,
    /// The job's `image:` is not an image reference; the rule it broke.
    Image(String),
}

impl std::fmt::Display for JobContainerRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkspacePathNotUtf8 => f.write_str("repo path is not valid UTF-8"),
            Self::Image(rule) => f.write_str(rule),
        }
    }
}

impl std::error::Error for JobContainerRefused {}

/// Everything a job container's `docker run` depends on.
pub struct JobContainer<'a> {
    /// `--name`, so a timeout or an interrupted job can remove it by name.
    pub name: &'a str,
    pub image: &'a str,
    pub script: &'a str,
    /// The job's workspace, mounted at `/workspace`.
    pub workspace: &'a Path,
    /// Uid and gid the job runs as, from [`crate::container_user`].
    pub user: Option<String>,
    /// The job's variables. Only their names reach the argv; values are
    /// inherited from the Docker CLI's environment (see [`apply_environment`]).
    pub variables: &'a [(String, String)],
}

/// The argument vector for `docker run` of one job container.
///
/// SECURITY (CWE-269 privilege escalation): the container is confined so a
/// malicious job cannot break out onto the runner host.
/// - `--cap-drop=ALL` strips every Linux capability (no raw sockets, no mount…).
/// - `--security-opt=no-new-privileges` blocks setuid/gain-privilege via execve.
/// - `--pids-limit` / `--memory` / `--cpus` bound resource exhaustion.
///
/// The Docker socket is never mounted and `--privileged` is never passed, so the
/// job has no path to the daemon or to host devices. Secret values never appear
/// on the command line: only variable names do.
///
/// The image is the one argument the repository author picks. It is checked
/// against the image reference grammar and placed after `--`, so a value like
/// `--privileged` can neither be read as a flag nor reach `docker` at all —
/// whether it came from a file accepted before the check existed or from a
/// caller that skipped it.
pub fn run_args(job: &JobContainer<'_>) -> Result<Vec<String>, JobContainerRefused> {
    crate::image_reference::validate_image_reference(job.image)
        .map_err(JobContainerRefused::Image)?;
    let workspace = job
        .workspace
        .to_str()
        .ok_or(JobContainerRefused::WorkspacePathNotUtf8)?;
    let mut args: Vec<String> = [
        "run",
        "--rm",
        "--name",
        job.name,
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        PIDS_LIMIT,
        "--memory",
        MEMORY_LIMIT,
        "--cpus",
        CPU_LIMIT,
        "-v",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    args.push(format!("{workspace}:/workspace"));
    args.extend(["-w".to_string(), "/workspace".to_string()]);
    // The workspace is created owner-only and container root holds no capability
    // to override that, so the job runs as the workspace's owner, which keeps
    // everything it builds removable by the cleanup.
    if let Some(user) = &job.user {
        args.extend(["--user".to_string(), user.clone()]);
    }
    for (key, _) in job.variables {
        if key != "HOME" {
            args.extend(["-e".to_string(), key.clone()]);
        }
    }
    args.extend([
        "-e".to_string(),
        CONTAINER_HOME.to_string(),
        "--".to_string(),
        job.image.to_string(),
        "sh".to_string(),
        "-c".to_string(),
        job.script.to_string(),
    ]);
    Ok(args)
}

/// Give a `docker` command the job's variables as its environment.
///
/// The values travel in the environment of the Docker CLI, so the daemon can
/// forward them with `-e NAME` and they never show up in the host's process
/// arguments. A job's `HOME` is skipped: it is the host path, and the CLI reads
/// its own configuration from its own `HOME`.
pub fn apply_environment(command: &mut tokio::process::Command, variables: &[(String, String)]) {
    for (key, value) in variables {
        if key != "HOME" {
            command.env(key, value);
        }
    }
}

/// A `docker` client that is dropped with its future.
///
/// A timeout that drops `output()` must kill the Docker CLI as well; otherwise a
/// wedged client keeps running after the job has been recorded as failed. Every
/// call the runners make to the daemon goes through this, including `docker info`.
pub fn client(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command.kill_on_drop(true);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job<'a>(workspace: &'a Path, variables: &'a [(String, String)]) -> JobContainer<'a> {
        JobContainer {
            name: "plombir-git-runner-job-7",
            image: "alpine:3.20",
            script: "echo hi",
            workspace,
            user: Some("1000:1000".to_string()),
            variables,
        }
    }

    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    }

    #[test]
    fn hardening_flags_are_always_present() {
        let variables = [("CI_JOB_TOKEN".to_string(), "secret".to_string())];
        let args = run_args(&job(Path::new("/srv/repo"), &variables)).unwrap();

        assert!(has_pair(&args, "--cap-drop", "ALL"), "{args:?}");
        assert!(
            has_pair(&args, "--security-opt", "no-new-privileges"),
            "{args:?}"
        );
        assert!(has_pair(&args, "--pids-limit", PIDS_LIMIT), "{args:?}");
        assert!(has_pair(&args, "--memory", MEMORY_LIMIT), "{args:?}");
        assert!(has_pair(&args, "--cpus", CPU_LIMIT), "{args:?}");
        assert!(!args.iter().any(|arg| arg == "--privileged"), "{args:?}");
        assert!(
            !args.iter().any(|arg| arg.contains("docker.sock")),
            "{args:?}"
        );
    }

    #[test]
    fn variable_names_reach_the_argv_and_values_do_not() {
        let variables = [("CI_JOB_TOKEN".to_string(), "secret".to_string())];
        let args = run_args(&job(Path::new("/srv/repo"), &variables)).unwrap();

        assert!(has_pair(&args, "-e", "CI_JOB_TOKEN"), "{args:?}");
        assert!(
            !args.iter().any(|arg| arg == "secret"),
            "secret leaked: {args:?}"
        );
    }

    #[test]
    fn the_job_home_is_replaced_and_the_host_home_never_forwarded() {
        let variables = [("HOME".to_string(), "/srv/runner/jobs/7".to_string())];
        let args = run_args(&job(Path::new("/srv/repo"), &variables)).unwrap();

        assert!(
            !args.iter().any(|arg| arg == "/srv/runner/jobs/7"),
            "{args:?}"
        );
        assert!(!args.iter().any(|arg| arg == "HOME"), "{args:?}");
        assert!(has_pair(&args, "-e", CONTAINER_HOME), "{args:?}");
    }

    #[test]
    fn the_image_and_script_terminate_the_invocation() {
        let variables: [(String, String); 0] = [];
        let args = run_args(&job(Path::new("/srv/repo"), &variables)).unwrap();

        assert_eq!(
            &args[args.len() - 5..],
            &["--", "alpine:3.20", "sh", "-c", "echo hi"]
        );
    }

    /// card_857c0d4ab7a6: the image used to follow the runner's flags with
    /// nothing in between, so `image: --privileged` was a flag to `docker`.
    #[test]
    fn an_image_that_is_not_a_reference_never_reaches_the_argv() {
        let variables: [(String, String); 0] = [];
        for image in ["--privileged", "-v/:/host", "--env-file=/etc/passwd"] {
            let mut container = job(Path::new("/srv/repo"), &variables);
            container.image = image;
            match run_args(&container) {
                Err(JobContainerRefused::Image(rule)) => {
                    assert!(rule.contains(image), "{rule}")
                }
                other => panic!("`{image}` was not refused: {other:?}"),
            }
        }
    }

    #[test]
    fn the_workspace_is_mounted_and_the_job_runs_as_its_owner() {
        let variables: [(String, String); 0] = [];
        let args = run_args(&job(Path::new("/srv/repo"), &variables)).unwrap();

        assert!(has_pair(&args, "-v", "/srv/repo:/workspace"), "{args:?}");
        assert!(has_pair(&args, "-w", "/workspace"), "{args:?}");
        assert!(has_pair(&args, "--user", "1000:1000"), "{args:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_workspace_that_is_not_utf8_is_refused_instead_of_mangled() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"/srv/repo-\xff"));
        let variables: [(String, String); 0] = [];

        assert_eq!(
            run_args(&job(path, &variables)),
            Err(JobContainerRefused::WorkspacePathNotUtf8)
        );
    }
}
