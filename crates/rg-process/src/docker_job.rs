//! The `docker run` a CI job executes in, built once for both runners.
//!
//! The embedded runner (`rg-ci`) and the external one (`rg-runner`) each run a
//! job's script in a container, and each used to spell the argument vector out
//! on its own. The two copies had already drifted: one refused a workspace path
//! that is not UTF-8, the other silently mangled it with `to_string_lossy`, and
//! the two built the environment differently. Everything that decides what a job
//! container may do lives here, so a hardening flag or a rule about the
//! environment cannot be applied on one side only.
//!
//! A job runs in two steps, and they get different environments ([`prepare`]):
//!
//! 1. The image is found or pulled by the operator's client alone —
//!    `docker image inspect`, then `docker pull` if it is missing. A pull may
//!    start a credential helper (ECR, GCR, `pass`, …), and a helper reads its
//!    configuration and its keys from the environment it inherits.
//! 2. The container starts with `docker run --pull never` on the image ID that
//!    step found. That client carries the job's variables, so it is never one
//!    that can pull: a plain `docker run <tag>` pulls a missing image itself,
//!    and its helper then saw the job's `AWS_ACCESS_KEY_ID` next to the
//!    operator's `AWS_PROFILE` (card_031aaa2348fe). An ID is not a tag either,
//!    so with `DOCKER_CONTENT_TRUST` on the run does not go to notary — the
//!    signature is checked by the pull in step 1, which then always runs.

use std::path::Path;

/// Default cap on the number of processes a job container may spawn
/// (fork-bomb guard), when the operator sets none.
pub const PIDS_LIMIT: u32 = 512;
/// Default memory ceiling for a job container, in `docker run --memory` spelling.
pub const MEMORY_LIMIT: &str = "2g";
/// Default CPU quota for a job container, in `docker run --cpus` spelling.
pub const CPU_LIMIT: &str = "2";

/// Docker refuses a memory limit under 6 MiB.
const MEMORY_FLOOR_BYTES: u64 = 6 << 20;
/// No runner host carries this much; past it the value is a typo.
const MEMORY_CEILING_BYTES: u64 = 1 << 40;
/// Docker's smallest quota is a hundredth of a CPU.
const CPU_FLOOR_HUNDREDTHS: u64 = 1;
const CPU_CEILING: u64 = 1024;
/// `PID_MAX_LIMIT` on 64-bit Linux: a pids limit past it limits nothing.
const PIDS_CEILING: u32 = 4_194_304;

/// The resource ceiling of every job container a runner starts.
///
/// Set by the runner's operator — `[ci]` `job_memory` / `job_cpus` /
/// `job_pids` for the embedded runner, `--job-memory` / `--job-cpus` /
/// `--job-pids` or `runner.toml` for `plombir-git-runner` — and by nobody else.
/// A workflow cannot reach it: the job hands [`run_args`] an image, a script
/// and variables, and none of them is read as a flag. Letting a repository
/// raise its own ceiling would be `container.options` by another door, which
/// is refused on purpose (`docs/gitea-actions.md`).
///
/// The default is the ceiling every job had before the knob existed, so an
/// upgrade that sets nothing produces the same `docker run` byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobLimits {
    memory: String,
    cpus: String,
    pids: u32,
}

impl Default for JobLimits {
    fn default() -> Self {
        Self {
            memory: MEMORY_LIMIT.to_string(),
            cpus: CPU_LIMIT.to_string(),
            pids: PIDS_LIMIT,
        }
    }
}

impl JobLimits {
    /// Replace the memory ceiling with an operator's value.
    ///
    /// Each knob is checked when the runner starts, so a typo is a startup
    /// error naming the setting as the operator wrote it (`setting`:
    /// `ci.job_memory`, `--job-memory`, …) rather than every job failing on a
    /// `docker run` refusal.
    pub fn with_memory(mut self, setting: &str, value: &str) -> Result<Self, String> {
        self.memory = parse_memory(value)
            .map_err(|why| format!("{setting} = {value:?} is not a memory limit: {why}"))?;
        Ok(self)
    }

    /// Replace the CPU quota with an operator's value; see [`Self::with_memory`].
    pub fn with_cpus(mut self, setting: &str, value: &str) -> Result<Self, String> {
        self.cpus = parse_cpus(value)
            .map_err(|why| format!("{setting} = {value:?} is not a CPU quota: {why}"))?;
        Ok(self)
    }

    /// Replace the process limit with an operator's value; see
    /// [`Self::with_memory`]. There is no "unlimited": the limit is the
    /// fork-bomb guard, and a job container always runs with one.
    pub fn with_pids(mut self, setting: &str, value: u64) -> Result<Self, String> {
        self.pids = u32::try_from(value)
            .ok()
            .filter(|pids| (1..=PIDS_CEILING).contains(pids))
            .ok_or_else(|| {
                format!(
                    "{setting} = {value} is not a process limit: it has to be between 1 and \
                     {PIDS_CEILING}; a job container always runs with one"
                )
            })?;
        Ok(self)
    }

    /// `--memory`, as `docker run` takes it.
    pub fn memory(&self) -> &str {
        &self.memory
    }

    /// `--cpus`, as `docker run` takes it.
    pub fn cpus(&self) -> &str {
        &self.cpus
    }

    /// `--pids-limit`.
    pub fn pids(&self) -> u32 {
        self.pids
    }

    /// The quota in hundredths of a CPU.
    pub fn cpu_hundredths(&self) -> u64 {
        cpu_hundredths(&self.cpus).unwrap_or(0)
    }

    /// The CPUs this machine has, when the quota asks for more.
    ///
    /// Docker refuses `--cpus` above what its host has, and it refuses per job,
    /// so every job would fail on its `docker run`. The daemon may sit on
    /// another machine (`DOCKER_HOST`), so the caller warns rather than refuses.
    pub fn cpus_beyond_this_host(&self) -> Option<usize> {
        let host = std::thread::available_parallelism().ok()?.get();
        (self.cpu_hundredths() > (host as u64).saturating_mul(100)).then_some(host)
    }
}

impl std::fmt::Display for JobLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "memory={} cpus={} pids={}",
            self.memory, self.cpus, self.pids
        )
    }
}

/// `<digits>[b|k|m|g]`, the spelling `docker run --memory` accepts, lowercased.
fn parse_memory(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    let (digits, unit) = match value.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((at, _)) => value.split_at(at),
        None => (value.as_str(), ""),
    };
    let shift = match unit {
        "" | "b" => 0,
        "k" => 10,
        "m" => 20,
        "g" => 30,
        _ => return Err("write a whole number with an optional b, k, m or g, like \"8g\"".into()),
    };
    let bytes = digits
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(1 << shift))
        .ok_or("write a whole number with an optional b, k, m or g, like \"8g\"")?;
    if bytes < MEMORY_FLOOR_BYTES {
        return Err("docker refuses less than 6m".into());
    }
    if bytes > MEMORY_CEILING_BYTES {
        return Err("more than 1024g is not a runner host".into());
    }
    Ok(value)
}

/// A decimal with at most two places, as `docker run --cpus` accepts it.
fn parse_cpus(value: &str) -> Result<String, String> {
    let value = value.trim();
    let hundredths = cpu_hundredths(value)
        .ok_or("write a decimal number of CPUs with at most two places, like \"8\" or \"1.5\"")?;
    if hundredths < CPU_FLOOR_HUNDREDTHS {
        return Err("docker refuses less than 0.01".into());
    }
    if hundredths > CPU_CEILING * 100 {
        return Err(format!("more than {CPU_CEILING} is not a runner host"));
    }
    Ok(value.to_string())
}

fn cpu_hundredths(value: &str) -> Option<u64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(fraction) || fraction.len() > 2 {
        return None;
    }
    if value.ends_with('.') {
        return None;
    }
    let fraction = format!("{fraction:0<2}").parse::<u64>().ok()?;
    whole
        .parse::<u64>()
        .ok()?
        .checked_mul(100)?
        .checked_add(fraction)
}

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
    /// inherited from the Docker CLI's environment (see [`prepare`]).
    pub variables: &'a [(String, String)],
    /// The runner operator's ceiling, see [`JobLimits`].
    pub limits: &'a JobLimits,
}

/// The argument vector for `docker run` of one job container, started from
/// `image_id` — the content ID [`resolve_image`] found for `job.image`.
///
/// SECURITY (CWE-269 privilege escalation): the container is confined so a
/// malicious job cannot break out onto the runner host.
/// - `--cap-drop=ALL` strips every Linux capability (no raw sockets, no mount…).
/// - `--security-opt=no-new-privileges` blocks setuid/gain-privilege via execve.
/// - `--pids-limit` / `--memory` / `--cpus` bound resource exhaustion, at the
///   operator's [`JobLimits`].
///
/// The Docker socket is never mounted and `--privileged` is never passed, so the
/// job has no path to the daemon or to host devices. Secret values never appear
/// on the command line: only variable names do.
///
/// The image is the one argument the repository author picks. It is checked
/// against the image reference grammar and placed after `--`, so a value like
/// `--privileged` can neither be read as a flag nor reach `docker` at all —
/// whether it came from a file accepted before the check existed or from a
/// caller that skipped it. What actually starts is `image_id`, under
/// `--pull never`, so this client never fetches anything (see the module docs).
pub fn run_args(
    job: &JobContainer<'_>,
    image_id: &str,
) -> Result<Vec<String>, JobContainerRefused> {
    let workspace = validate(job)?;
    if !is_image_id(image_id) {
        return Err(JobContainerRefused::Image(format!(
            "docker reported '{}' as the ID of image '{}', which is not sha256:<64 hex>",
            image_id.escape_debug(),
            job.image.escape_debug()
        )));
    }
    let mut args: Vec<String> = [
        "run",
        "--rm",
        "--pull",
        "never",
        "--name",
        job.name,
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    args.extend([
        "--pids-limit".to_string(),
        job.limits.pids().to_string(),
        "--memory".to_string(),
        job.limits.memory().to_string(),
        "--cpus".to_string(),
        job.limits.cpus().to_string(),
        "-v".to_string(),
    ]);
    args.push(format!("{workspace}:/workspace"));
    args.extend(["-w".to_string(), "/workspace".to_string()]);
    // The workspace is created owner-only and container root holds no capability
    // to override that, so the job runs as the workspace's owner, which keeps
    // everything it builds removable by the cleanup.
    if let Some(user) = &job.user {
        args.extend(["--user".to_string(), user.clone()]);
    }
    for (key, _) in job.variables {
        if crate::job_environment::reaches_container(key) {
            args.extend(["-e".to_string(), key.clone()]);
        }
    }
    args.extend([
        "-e".to_string(),
        CONTAINER_HOME.to_string(),
        "--".to_string(),
        image_id.to_string(),
        "sh".to_string(),
        "-c".to_string(),
        job.script.to_string(),
    ]);
    Ok(args)
}

/// The reference and the workspace a job container can be built from.
fn validate<'a>(job: &JobContainer<'a>) -> Result<&'a str, JobContainerRefused> {
    crate::image_reference::validate_image_reference(job.image)
        .map_err(JobContainerRefused::Image)?;
    job.workspace
        .to_str()
        .ok_or(JobContainerRefused::WorkspacePathNotUtf8)
}

/// `sha256:<64 lowercase hex>`, the form `docker image inspect` reports an ID
/// in, and the one `docker run` parses as an ID rather than a name.
fn is_image_id(id: &str) -> bool {
    id.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Give the `docker run` client its environment: the operator's, reduced to
/// what the CLI itself reads, plus the job's variables.
///
/// The values travel in the environment of the Docker CLI, so the daemon can
/// forward them with `-e NAME` and they never show up in the host's process
/// arguments. That environment belongs to a host process, so it starts empty
/// and a job variable that would reconfigure the CLI (`LD_PRELOAD`,
/// `DOCKER_HOST`, …) is left out — see [`crate::job_environment`] and
/// [`left_out_variables`]. What only a credential helper reads is neither
/// inherited nor refused here: this client never pulls, so no helper runs.
fn apply_run_environment(command: &mut tokio::process::Command, variables: &[(String, String)]) {
    command
        .env_clear()
        .envs(crate::job_environment::docker_run_environment());
    for (key, value) in variables {
        if crate::job_environment::reaches_container(key) {
            command.env(key, value);
        }
    }
}

/// A `docker` client for finding or pulling an image: the operator's
/// environment only, whatever the job declared.
fn image_client(program: &std::ffi::OsStr) -> tokio::process::Command {
    let mut command = client(program);
    command
        .env_clear()
        .envs(crate::job_environment::docker_cli_environment());
    command
}

/// The image a job container starts from.
pub struct ResolvedImage {
    /// `sha256:<hex>`, what `docker run` gets instead of the reference.
    pub id: String,
    /// What `docker pull` printed, when it had to run; empty otherwise. The job
    /// log used to show the pull because `docker run` did it.
    pub pull_log: String,
}

/// Why a job's image could not be made available locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageUnavailable {
    /// The failing client's exit code, or -1 when it did not run or exit.
    pub exit_code: i32,
    /// What it printed, with the step that failed named.
    pub log: String,
}

impl std::fmt::Display for ImageUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.log)
    }
}

impl std::error::Error for ImageUnavailable {}

/// The local ID of `image`, or `None` when the daemon does not hold it (or
/// cannot be asked — the pull that follows says which).
async fn local_image_id(program: &std::ffi::OsStr, image: &str) -> Option<String> {
    let mut command = image_client(program);
    command.args(["image", "inspect", "--format", "{{.Id}}", "--", image]);
    let output = crate::output_ci_job_in_process_tree(&mut command)
        .await
        .ok()?;
    let id = output.stdout.bytes()?;
    let id = String::from_utf8(id).ok()?;
    let id = id.trim();
    (output.status.success() && is_image_id(id)).then(|| id.to_string())
}

/// Find `image` in the local daemon, pulling it first if it is missing — or
/// always, with `DOCKER_CONTENT_TRUST` on, so its signature is checked on
/// every job as `docker run <tag>` used to. Both clients run in the operator's
/// environment: this is the step that may start a credential helper.
///
/// `image` must already be a valid reference ([`prepare`] checks it first).
pub async fn resolve_image(
    program: impl AsRef<std::ffi::OsStr>,
    image: &str,
) -> Result<ResolvedImage, ImageUnavailable> {
    let program = program.as_ref();
    let trust = crate::job_environment::content_trust_enabled();
    if !trust {
        if let Some(id) = local_image_id(program, image).await {
            return Ok(ResolvedImage {
                id,
                pull_log: String::new(),
            });
        }
    }

    let mut pull = image_client(program);
    pull.args(["pull", "--", image]);
    let pulled = crate::output_ci_job_in_process_tree(&mut pull)
        .await
        .map_err(|error| ImageUnavailable {
            exit_code: -1,
            log: format!("Failed to run docker pull {image}: {error}"),
        })?;
    let pull_log = pulled.log();
    if !pulled.status.success() {
        let exit_code = pulled.status.code().unwrap_or(-1);
        return Err(ImageUnavailable {
            exit_code,
            log: format!("docker pull {image} exited with code {exit_code}\n{pull_log}")
                .trim_end()
                .to_string(),
        });
    }
    match local_image_id(program, image).await {
        Some(id) => Ok(ResolvedImage { id, pull_log }),
        None => Err(ImageUnavailable {
            exit_code: -1,
            log: format!(
                "docker pull {image} succeeded, but docker image inspect does not report a \
                 sha256 ID for it\n{pull_log}"
            )
            .trim_end()
            .to_string(),
        }),
    }
}

/// Why a job container was not started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareFailed {
    /// The job itself is not runnable as written.
    Refused(JobContainerRefused),
    /// The image could not be found or pulled.
    ImageUnavailable(ImageUnavailable),
}

impl std::fmt::Display for PrepareFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refused) => refused.fmt(f),
            Self::ImageUnavailable(unavailable) => unavailable.fmt(f),
        }
    }
}

impl std::error::Error for PrepareFailed {}

/// A job container ready to start.
pub struct PreparedRun {
    /// `docker run`, with its argv and environment; the caller runs it.
    pub command: tokio::process::Command,
    /// For the head of the job log; see [`ResolvedImage::pull_log`].
    pub pull_log: String,
}

/// Everything before a job container starts: check the job, make its image
/// available with the operator's credentials, and build a `docker run` that
/// carries the job's variables and cannot pull. See the module docs for why
/// those are two clients.
pub async fn prepare(
    program: impl AsRef<std::ffi::OsStr>,
    job: &JobContainer<'_>,
) -> Result<PreparedRun, PrepareFailed> {
    let program = program.as_ref();
    validate(job).map_err(PrepareFailed::Refused)?;
    let image = resolve_image(program, job.image)
        .await
        .map_err(PrepareFailed::ImageUnavailable)?;
    let args = run_args(job, &image.id).map_err(PrepareFailed::Refused)?;
    let mut command = client(program);
    command.args(&args);
    apply_run_environment(&mut command, job.variables);
    Ok(PreparedRun {
        command,
        pull_log: image.pull_log,
    })
}

/// Variables both runners give every job for running on the host: the host's
/// `PATH` and `LANG`, and the workspace as `HOME`. A container takes these from
/// its image, so leaving them out is not something to report.
const RUNNER_PROVIDED_VARIABLES: &[&str] = &["PATH", "LANG", "HOME"];

/// The job's variables a container does not receive, by name, sorted.
pub fn left_out_variables(variables: &[(String, String)]) -> Vec<String> {
    variables
        .iter()
        .map(|(key, _)| key)
        .filter(|key| {
            !crate::job_environment::reaches_container(key)
                && !RUNNER_PROVIDED_VARIABLES.contains(&key.as_str())
        })
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The line a job log gets for [`left_out_variables`], if there are any. Each
/// runner prefixes it the way it marks its own lines.
///
/// The pipeline still runs: refusing it for `env: HTTP_PROXY` or `LANG` would
/// break configurations that are common in the wild, and the container never
/// needed the runner's copy. The author reads the log, so that is where the
/// names go.
pub fn left_out_variables_notice(variables: &[(String, String)]) -> Option<String> {
    let left_out = left_out_variables(variables);
    (!left_out.is_empty()).then(|| {
        format!(
            "WARNING: not passed to the container: {}. These names configure the runner's own \
             docker client or are not valid variable names; set the value under another name and \
             export it in the script if the job needs it.",
            left_out.join(", ")
        )
    })
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

    static DEFAULT_LIMITS: std::sync::LazyLock<JobLimits> =
        std::sync::LazyLock::new(JobLimits::default);

    fn job<'a>(workspace: &'a Path, variables: &'a [(String, String)]) -> JobContainer<'a> {
        JobContainer {
            name: "plombir-git-runner-job-7",
            image: "alpine:3.20",
            script: "echo hi",
            workspace,
            user: Some("1000:1000".to_string()),
            variables,
            limits: &DEFAULT_LIMITS,
        }
    }

    const IMAGE_ID: &str =
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    }

    #[test]
    fn hardening_flags_are_always_present() {
        let variables = [("CI_JOB_TOKEN".to_string(), "secret".to_string())];
        let args = run_args(&job(Path::new("/srv/repo"), &variables), IMAGE_ID).unwrap();

        assert!(has_pair(&args, "--cap-drop", "ALL"), "{args:?}");
        assert!(
            has_pair(&args, "--security-opt", "no-new-privileges"),
            "{args:?}"
        );
        assert!(
            has_pair(&args, "--pids-limit", &PIDS_LIMIT.to_string()),
            "{args:?}"
        );
        assert!(has_pair(&args, "--memory", MEMORY_LIMIT), "{args:?}");
        assert!(has_pair(&args, "--cpus", CPU_LIMIT), "{args:?}");
        assert!(!args.iter().any(|arg| arg == "--privileged"), "{args:?}");
        assert!(
            !args.iter().any(|arg| arg.contains("docker.sock")),
            "{args:?}"
        );
    }

    /// An operator who sets no limit gets the `docker run` every job had
    /// before the knob existed — spelled out here rather than read from the
    /// constants, so changing a default is a visible edit to this list.
    #[test]
    fn without_operator_limits_the_argv_is_the_one_jobs_always_had() {
        let variables: [(String, String); 0] = [];
        let args = run_args(&job(Path::new("/srv/repo"), &variables), IMAGE_ID).unwrap();
        let head: Vec<&str> = args.iter().take(18).map(String::as_str).collect();
        assert_eq!(
            head,
            [
                "run",
                "--rm",
                "--pull",
                "never",
                "--name",
                "plombir-git-runner-job-7",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--pids-limit",
                "512",
                "--memory",
                "2g",
                "--cpus",
                "2",
                "-v",
                "/srv/repo:/workspace",
            ]
        );
    }

    /// The operator's limits replace the defaults, once each. Nothing the job
    /// writes — its script, its variables, its image — becomes a second
    /// `--memory`: the repository cannot raise its own ceiling.
    #[test]
    fn operator_limits_reach_the_argv_and_the_job_cannot_add_its_own() {
        let limits = JobLimits::default()
            .with_memory("ci.job_memory", "12g")
            .and_then(|limits| limits.with_cpus("ci.job_cpus", "8"))
            .and_then(|limits| limits.with_pids("ci.job_pids", 4096))
            .unwrap();
        let variables = [
            ("MEMORY".to_string(), "--memory=64g".to_string()),
            ("--cpus".to_string(), "64".to_string()),
        ];
        let mut container = job(Path::new("/srv/repo"), &variables);
        container.limits = &limits;
        container.script = "docker run --memory 64g --cpus 64 --pids-limit -1 x";
        let args = run_args(&container, IMAGE_ID).unwrap();
        let options = &args[..args.iter().position(|arg| arg == "--").unwrap()];

        for (flag, value) in [
            ("--memory", "12g"),
            ("--cpus", "8"),
            ("--pids-limit", "4096"),
        ] {
            assert!(has_pair(options, flag, value), "{flag} {value}: {args:?}");
            assert_eq!(
                options.iter().filter(|arg| arg.as_str() == flag).count(),
                1,
                "{flag} appears more than once before `--`: {args:?}"
            );
        }
        assert!(
            !options.iter().any(|arg| arg.contains("64")),
            "a job-supplied value reached docker's options: {args:?}"
        );
    }

    #[test]
    fn operator_limits_are_validated_and_name_the_setting() {
        let accepted = JobLimits::default()
            .with_memory("job_memory", " 16G ")
            .and_then(|limits| limits.with_cpus("job_cpus", "1.5"))
            .and_then(|limits| limits.with_pids("job_pids", 1))
            .unwrap();
        assert_eq!(
            (accepted.memory(), accepted.cpus(), accepted.pids()),
            ("16g", "1.5", 1)
        );
        assert_eq!(accepted.cpu_hundredths(), 150);
        for memory in [
            "",
            "g",
            "12x",
            "1.5g",
            "-1",
            "5m",
            "2048g",
            "99999999999999999999",
        ] {
            let error = JobLimits::default()
                .with_memory("ci.job_memory", memory)
                .unwrap_err();
            assert!(error.starts_with("ci.job_memory = "), "{memory:?}: {error}");
        }
        for cpus in ["", "0", "0.001", "1.", ".5", "1,5", "-2", "2048", "two"] {
            let error = JobLimits::default()
                .with_cpus("--job-cpus", cpus)
                .unwrap_err();
            assert!(error.starts_with("--job-cpus = "), "{cpus:?}: {error}");
        }
        for pids in [0, 4_194_305, u64::MAX] {
            let error = JobLimits::default()
                .with_pids("ci.job_pids", pids)
                .unwrap_err();
            assert!(error.starts_with("ci.job_pids = "), "{pids}: {error}");
        }
    }

    #[test]
    fn variable_names_reach_the_argv_and_values_do_not() {
        let variables = [("CI_JOB_TOKEN".to_string(), "secret".to_string())];
        let args = run_args(&job(Path::new("/srv/repo"), &variables), IMAGE_ID).unwrap();

        assert!(has_pair(&args, "-e", "CI_JOB_TOKEN"), "{args:?}");
        assert!(
            !args.iter().any(|arg| arg == "secret"),
            "secret leaked: {args:?}"
        );
    }

    /// card_a79579f79c29: a job variable became the environment of the host
    /// `docker` CLI, so `LD_PRELOAD` or `DOCKER_HOST` reconfigured a process
    /// outside the container.
    #[test]
    fn a_host_sensitive_job_variable_reaches_neither_the_argv_nor_the_notice_silently() {
        let variables = [
            ("DEPLOY_TARGET".to_string(), "prod".to_string()),
            ("LD_PRELOAD".to_string(), "/workspace/evil.so".to_string()),
            (
                "DOCKER_HOST".to_string(),
                "tcp://203.0.113.9:2375".to_string(),
            ),
            ("node-version".to_string(), "20".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/srv/runner/jobs/7".to_string()),
        ];
        let args = run_args(&job(Path::new("/srv/repo"), &variables), IMAGE_ID).unwrap();
        assert!(has_pair(&args, "-e", "DEPLOY_TARGET"), "{args:?}");
        for name in ["LD_PRELOAD", "DOCKER_HOST", "node-version", "PATH"] {
            assert!(!has_pair(&args, "-e", name), "{name}: {args:?}");
        }
        assert_eq!(
            left_out_variables(&variables),
            ["DOCKER_HOST", "LD_PRELOAD", "node-version"]
        );
        let notice = left_out_variables_notice(&variables).expect("a notice");
        assert!(
            notice.contains("WARNING") && notice.contains("DOCKER_HOST, LD_PRELOAD, node-version"),
            "{notice}"
        );
        assert_eq!(
            left_out_variables_notice(&variables[..1]),
            None,
            "nothing was left out, so nothing is reported"
        );
    }

    /// The CLI's environment is built, not inherited: the runner's own
    /// variables stay behind unless the CLI needs them, and a job variable is
    /// only there when a container may receive it.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_docker_client_environment_is_the_allow_list_plus_container_variables() {
        assert!(
            std::env::var_os("CARGO_PKG_NAME").is_some(),
            "cargo sets this in every test process; it is the runner-side canary"
        );
        let variables = [
            ("DEPLOY_TARGET".to_string(), "prod".to_string()),
            ("LD_PRELOAD".to_string(), "/workspace/evil.so".to_string()),
            (
                "DOCKER_HOST".to_string(),
                "tcp://203.0.113.9:2375".to_string(),
            ),
        ];
        let mut command = client("env");
        apply_run_environment(&mut command, &variables);
        let output = command.output().await.expect("run env");
        let environment = String::from_utf8(output.stdout).expect("utf-8 env");
        let names: Vec<&str> = environment
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name))
            .collect();
        assert!(names.contains(&"DEPLOY_TARGET"), "{names:?}");
        for name in ["LD_PRELOAD", "CARGO_PKG_NAME"] {
            assert!(
                !names.contains(&name),
                "{name} reached the docker client: {names:?}"
            );
        }
        assert!(
            !environment.contains("203.0.113.9"),
            "the job's DOCKER_HOST reached the docker client"
        );
    }

    #[test]
    fn the_job_home_is_replaced_and_the_host_home_never_forwarded() {
        let variables = [("HOME".to_string(), "/srv/runner/jobs/7".to_string())];
        let args = run_args(&job(Path::new("/srv/repo"), &variables), IMAGE_ID).unwrap();

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
        let args = run_args(&job(Path::new("/srv/repo"), &variables), IMAGE_ID).unwrap();

        assert_eq!(
            &args[args.len() - 5..],
            &["--", IMAGE_ID, "sh", "-c", "echo hi"]
        );
        assert!(has_pair(&args, "--pull", "never"), "{args:?}");
        assert!(!args.iter().any(|arg| arg == "alpine:3.20"), "{args:?}");
    }

    /// The ID comes out of a host process's stdout; anything else in that
    /// position would be a name `docker run` could still pull.
    #[test]
    fn only_a_sha256_image_id_is_started() {
        let variables: [(String, String); 0] = [];
        for id in [
            "alpine:3.20",
            "sha256:0123",
            "--privileged",
            "sha256:0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
            "",
        ] {
            assert!(
                matches!(
                    run_args(&job(Path::new("/srv/repo"), &variables), id),
                    Err(JobContainerRefused::Image(_))
                ),
                "{id:?} was accepted as an image ID"
            );
        }
    }

    /// card_857c0d4ab7a6: the image used to follow the runner's flags with
    /// nothing in between, so `image: --privileged` was a flag to `docker`.
    #[test]
    fn an_image_that_is_not_a_reference_never_reaches_the_argv() {
        let variables: [(String, String); 0] = [];
        for image in ["--privileged", "-v/:/host", "--env-file=/etc/passwd"] {
            let mut container = job(Path::new("/srv/repo"), &variables);
            container.image = image;
            match run_args(&container, IMAGE_ID) {
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
        let args = run_args(&job(Path::new("/srv/repo"), &variables), IMAGE_ID).unwrap();

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
            run_args(&job(path, &variables), IMAGE_ID),
            Err(JobContainerRefused::WorkspacePathNotUtf8)
        );
    }

    /// A stand-in `docker` that records, per subcommand, its argv and the
    /// environment it was started with. The image is missing until `pull`
    /// runs, so both the inspect and the pull path are taken.
    #[cfg(unix)]
    fn recording_docker(state: &Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let program = state.join("docker");
        std::fs::write(
            &program,
            format!(
                r#"#!/bin/sh
state=$(dirname "$0")
step=$1
[ "$1" = image ] && step=inspect
env > "$state/$step.env"
printf '%s\n' "$@" > "$state/$step.args"
case "$step" in
  inspect)
    [ -e "$state/pulled" ] || {{ echo "Error: No such image" >&2; exit 1; }}
    echo {IMAGE_ID}
    ;;
  pull)
    : > "$state/pulled"
    echo "Status: Downloaded newer image"
    ;;
  run)
    echo ran
    ;;
esac
"#
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    #[cfg(unix)]
    fn recorded_names(state: &Path, step: &str) -> Vec<String> {
        std::fs::read_to_string(state.join(format!("{step}.env")))
            .unwrap_or_else(|error| panic!("docker {step} did not run: {error}"))
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_string()))
            .collect()
    }

    /// card_031aaa2348fe: `docker run <tag>` pulled a missing image itself, so
    /// the credential helper it started saw the job's AWS keys. Every client
    /// that can pull now runs without a single job variable, and the one that
    /// carries them starts an ID under `--pull never`.
    #[cfg(unix)]
    #[tokio::test]
    async fn no_client_that_can_pull_carries_a_job_variable() {
        let state = tempfile::tempdir().unwrap();
        let program = recording_docker(state.path());
        let variables = [
            ("AWS_PROFILE".to_string(), "deploy".to_string()),
            ("AWS_ACCESS_KEY_ID".to_string(), "AKIAJOBKEY".to_string()),
            (
                "AWS_SECRET_ACCESS_KEY".to_string(),
                "job-secret".to_string(),
            ),
            ("CI_JOB_TOKEN".to_string(), "job-token".to_string()),
            (
                "DOCKER_HOST".to_string(),
                "tcp://203.0.113.9:2375".to_string(),
            ),
        ];
        let workspace = tempfile::tempdir().unwrap();
        let mut prepared = prepare(&program, &job(workspace.path(), &variables))
            .await
            .unwrap_or_else(|error| panic!("prepare failed: {error}"));
        assert!(
            prepared.pull_log.contains("Downloaded newer image"),
            "{}",
            prepared.pull_log
        );
        let output = prepared.command.output().await.expect("run docker");
        assert!(output.status.success(), "{output:?}");

        // A name the operator's own environment carries may be there; the
        // job's value never.
        for step in ["inspect", "pull"] {
            let names = recorded_names(state.path(), step);
            let environment =
                std::fs::read_to_string(state.path().join(format!("{step}.env"))).unwrap();
            for (name, value) in &variables {
                assert!(
                    std::env::var_os(name).is_some() || !names.contains(name),
                    "docker {step} could start a credential helper and saw the job's {name}: \
                     {names:?}"
                );
                assert!(
                    !environment.contains(value.as_str()),
                    "docker {step} saw the job's value of {name}"
                );
            }
        }

        let run = recorded_names(state.path(), "run");
        for name in [
            "AWS_PROFILE",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "CI_JOB_TOKEN",
        ] {
            assert!(
                run.contains(&name.to_string()),
                "{name} missing from docker run: {run:?}"
            );
        }
        let run_environment = std::fs::read_to_string(state.path().join("run.env")).unwrap();
        assert!(
            !run_environment.contains("203.0.113.9"),
            "the job's DOCKER_HOST reached docker run"
        );
        let run_args: Vec<String> = std::fs::read_to_string(state.path().join("run.args"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert!(has_pair(&run_args, "--pull", "never"), "{run_args:?}");
        assert!(has_pair(&run_args, "--", IMAGE_ID), "{run_args:?}");
        assert!(has_pair(&run_args, "-e", "AWS_PROFILE"), "{run_args:?}");
        assert!(
            has_pair(&run_args, "-e", "AWS_ACCESS_KEY_ID"),
            "{run_args:?}"
        );
        assert!(
            !run_args.iter().any(|arg| arg == "alpine:3.20"),
            "{run_args:?}"
        );
    }

    /// An image the daemon already holds is not pulled again, and a pull that
    /// fails is the job's failure with docker's own words in the log — not a
    /// `docker run` of a name.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_local_image_is_not_pulled_and_a_failed_pull_stops_the_job() {
        let state = tempfile::tempdir().unwrap();
        let program = recording_docker(state.path());
        let variables: [(String, String); 0] = [];
        let workspace = tempfile::tempdir().unwrap();

        std::fs::write(state.path().join("pulled"), b"").unwrap();
        let prepared = prepare(&program, &job(workspace.path(), &variables))
            .await
            .unwrap_or_else(|error| panic!("prepare failed: {error}"));
        assert_eq!(prepared.pull_log, "");
        assert!(
            !state.path().join("pull.args").exists(),
            "a local image was pulled"
        );

        std::fs::remove_file(state.path().join("pulled")).unwrap();
        std::fs::write(
            &program,
            "#!/bin/sh\n[ \"$1\" = pull ] && { echo 'denied: requested access' >&2; exit 18; }\nexit 1\n",
        )
        .unwrap();
        match prepare(&program, &job(workspace.path(), &variables)).await {
            Err(PrepareFailed::ImageUnavailable(unavailable)) => {
                assert_eq!(unavailable.exit_code, 18);
                assert!(
                    unavailable.log.contains("docker pull alpine:3.20"),
                    "{}",
                    unavailable.log
                );
                assert!(
                    unavailable.log.contains("denied: requested access"),
                    "{}",
                    unavailable.log
                );
            }
            Err(other) => panic!("unexpected refusal: {other}"),
            Ok(_) => panic!("a failed pull still produced a docker run"),
        }
    }

    /// The image is checked before any client runs: `--privileged` as an
    /// image must not reach `docker image inspect` or `docker pull` either.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_invalid_image_reaches_no_docker_client() {
        let state = tempfile::tempdir().unwrap();
        let program = recording_docker(state.path());
        let variables: [(String, String); 0] = [];
        let workspace = tempfile::tempdir().unwrap();
        let mut container = job(workspace.path(), &variables);
        container.image = "--privileged";
        assert!(matches!(
            prepare(&program, &container).await,
            Err(PrepareFailed::Refused(JobContainerRefused::Image(_)))
        ));
        assert!(!state.path().join("inspect.args").exists());
        assert!(!state.path().join("pull.args").exists());
    }
}
